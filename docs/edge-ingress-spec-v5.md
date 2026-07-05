# SY.edge / INGRESS — Specification v5

**Status:** design-complete for alpha. Supersedes `edge-ingress-spec-v4.md`.
**What changed from v4:** the identity model was corrected end-to-end. The edge is now defined as
**outside the identity frontier**: it holds no `ilk`, no `ICH`, no identity — only `hash → L2`. The
stray `meta.dst_ilk` the edge used to stamp is **removed** (it was never resolvable there and never
needed). The `ilk → L2` resolve and the binding authority are relocated to **the core, where
`SY.identity` lives**; the ingress-hive orchestrator is demoted to a pure last-mile relay. The
organizing principle of this revision — **special cases live on the IO node that connects to the
edge, never on the edge** — is stated explicitly (§13).

---

## 0. How to read this spec

The whole design falls out of one goal (§1), a set of invariants (§2), and one frontier (§3). The
frontier is the thing to internalize: **identity lives inside the mesh; the edge lives outside it.**
Almost every past confusion came from dragging identity (`ilk`, `ICH`) across that frontier to the
edge, where it cannot exist and is not needed. Read §1–§3 first; the rest is mechanism.

---

## 1. Goal (telos)

Expose selected internal handler nodes at public HTTPS URLs, on a **disposable** DMZ box, driven by
admin commands like the rest of Fluxbee, with **no inbound connections into the mesh** and the edge
**holding nothing irreplaceable and nothing about identity**.

---

## 2. Invariants

Load-bearing. Code that breaks one is wrong by definition.

- **I1 — The edge only forwards to IO nodes.** No exceptions. This is not a rule to remember; it is
  the design valve (§13): anything new to expose is modeled as an IO node, and the edge is untouched.
- **I2 — No path from the public internet to the admin plane.** Internet inbound terminates at an IO
  node, never at `SY.admin`. Admin is reachable only from inside the mesh.
- **I3 — The DMZ box holds nothing irreplaceable.** The edge may be reimaged/replaced at any time and
  must converge from an authority inside the mesh. It persists only its own TLS material and a
  rebuildable cache.
- **I4 — One authority per fact, origin-agnostic.** The published-endpoint binding has exactly one
  authoritative holder; it does not matter who *originated* a change (cloud, architect, operator).
- **I5 — The router stays dumb.** L2 routing knows `name → reachability` and nothing about auth,
  tenants, or endpoints.
- **I6 — The edge is outside the identity frontier (§3).** The edge holds no `ilk`, no `ICH`, no
  identity SHM. It routes on `hash → L2` only. Identity resolution never happens at the edge.
- **I7 — Special cases live on the IO side, never on the edge (§13).** Every new capability to expose
  (web/artifact, blob, anything) is absorbed by the IO node that fronts it. The edge never grows.

---

## 3. The identity frontier — the organizing principle

### 3.1 Who owns identity: `SY.identity`, not the orchestrator
`SY.identity` is the central registry of all entities (human / agent / system). It owns
`ilk ↔ { type, node_name (= the L2 name), tenant, ICH, definition }`. Motherbee runs the **PRIMARY**
(writes PostgreSQL + `jsr-identity` SHM); every worker runs a **REPLICA** (receives deltas over a
direct socket, writes local SHM, no DB). The full identity dataset lives in SHM in every hive **so
any node can resolve `ilk → L2` locally, with no round-trip** — this is what L3/OPA routing reads.

`SY.orchestrator` does **not** own identity. It is brute mechanics: it runs, sees, and configures
nodes, and it *calls* `ILK_REGISTER`/`ILK_UPDATE` when it spawns a node — a registrar on an
action-scoped allowlist, not an authority. `AI.frontdesk` is the same kind of caller for humans.
There is **one** identity store and **one** ilk-space; only the *action* is scoped
(`ILK_PROVISION` → IO, `ILK_REGISTER` → orchestrator/frontdesk).

### 3.2 Identity syncs by socket, not by `CONFIG_CHANGED`
Identity deltas can exceed 64 KB (a full ILK with history), so identity replicates over **direct
chunked sockets between `SY.identity` instances** — explicitly **not** router broadcast, **not**
`CONFIG_CHANGED`, **not** NATS. Identity is **not** a `CONFIG_CHANGED` subsystem. (This matters for
§6: the edge's endpoint push is a different mechanism, and inherits the 64 KB envelope cap.)

### 3.3 `SY.identity` is absent from ingress/egress → the edge is outside the frontier
`SY.identity` runs in every hive **except ingress and egress**. The edge therefore has **no identity
SHM** and **cannot resolve `ilk → L2` even if it wanted to.** This is not a limitation to work
around — it is the frontier that makes the design clean:

> **Inside the frontier** (mesh hives with identity SHM): `ilk` exists, `ICH` exists, the router
> resolves `ilk` at L3, auto-propagation keeps everyone current.
> **Outside the frontier** (the edge): only `hash → L2`. No `ilk`, no `ICH`, no identity.

### 3.4 The ilk dies at publish-time
The `ilk` is used **once, at the core, at publish-time**, to resolve `ilk → L2` against
`SY.identity` SHM. The resolved **L2 name** is what descends to the edge; the `ilk` stays inside and
**never crosses the frontier**. If a future need requires the router to perturb L3 for ICH-style
routing, **that happens inside the mesh** (core/router, where identity lives) — never at the edge.

```
cloud/architect publishes ── speaks ilk (the stable thing they know)
        │
core (has identity SHM) ──── resolves ilk → L2    ◄── the ilk DIES here
        │
ingress orchestrator ─────── relays (local CONFIG_CHANGED)
        │
edge ─────────────────────── receives hash → L2   ◄── the ilk never arrived
        │
runtime: GET /e/<hash> ───── forward to L2         ◄── pure L2, zero ilk
```

---

## 4. Model 1, and the deleted control channel

Two possible worlds for how an edge learns what to serve:

- **Model 2 (rejected):** edges are a cloud-managed *fleet* outside any mesh; a dedicated WSS/mTLS
  **control channel** (`edge-control-protocol`) carries cert/JWT-key/tenant assignments down.
- **Model 1 (chosen):** the edge is a normal **mesh node** (`role: Ingress`) on a DMZ hive, managed
  by the mesh's own admin plane. Fluxbee Cloud is not a special protocol peer — it is a privileged
  in-mesh IO node, `IO.cloud` (§8).

**Model 1 is chosen.** The **control channel is deleted**, not stubbed: its only purpose was managing
edges in no mesh (Model 2). Under Model 1 there is nothing left for it to do.

Three historically-tangled mechanisms, kept strictly apart:

| Mechanism | Layer | Purpose | Touches cloud? |
|---|---|---|---|
| **LSA flooding** | router / internal | node `UUID → reachability` | no |
| **Identity socket sync** | `SY.identity` ↔ `SY.identity` | replicate ilk/ich/tenant to every hive's SHM | no |
| **~~Control channel~~** | ~~edge ↔ FC.edge-manager~~ | **DELETED** | — |
| **`CONFIG_CHANGED`** | mesh internal | pushes the resolved endpoint table to the edge | no |

LSA also limits cross-hive reach to one WAN hop (§14, H4).

---

## 5. The SY.edge node (aseptic)

### 5.1 Role & placement
Public reverse-proxy, `role: Ingress`, on a dedicated **DMZ hive**. Two NICs: public `:443` and the
internal mesh WAN. The single public door.

### 5.2 Identity (a fuego — its own, not the registry's)
- `self_ilk = deterministic_system_ilk_id("SY.edge@<hive>")` (SHA-256; no SHM lookup, no
  `ILK_REGISTER`). This is a fixed self-label so the edge can sign its own frames — **not** a lookup
  into `SY.identity`, which the edge cannot reach.
- `tenant = DEFAULT_ROOT_TENANT_ID`.
- No `SY.identity` participation, no `SY.vault` beyond holding its own TLS key material.

### 5.3 What the edge holds — and does not
- **Holds:** its own TLS cert/key; a **live routing cache** (`hash → L2 + guard`, §6.1) in SHM; its
  own node config (§11).
- **Does NOT hold:** any `ilk` (of a caller or a target), any `ICH`, any identity SHM; the
  authoritative endpoint set (that is the core's, §6.2); anything that cannot be rebuilt from inside.
- **Reachability is constrained** (M3): the edge's `sy-config-routes` lists only the handler (IO)
  nodes it forwards to — no blanket system-node reachability. A compromised edge cannot address
  arbitrary nodes/VPNs.

The edge terminates TLS, authenticates the caller at **transport level only**, and forwards to an IO
node by L2 name. That is all it does and all it will ever do (I7).

---

## 6. Where things live (owners, corrected)

Every past "where does the routing list live" argument was several different things called one name.
There are four, with four owners. The frontier (§3) determines which side each lives on.

### 6.1 Live routing cache — **owned by the edge (SHM), outside the frontier**
`Arc<RwLock<HashMap<hash, EndpointEntry>>>`, served per request. **Volatile / rebuildable.** Seeded
at boot from `/etc/fluxbee/edge.endpoints.json`; hot-swapped at runtime by `CONFIG_CHANGED`
(subsystem `endpoints`, monotonic version, whole-map replace). Row — note **no `ilk`**:
`EndpointEntry { hash, handler_node (L2), inbound_family, auth_mode, secret?, methods?, tenant_id? }`.

### 6.2 Binding authority + `ilk → L2` resolve — **at the core, inside the frontier**
The authoritative answer to *"which endpoints exist, and to which ilk/handler each binds"* lives
**where `SY.identity` lives** (core: motherbee/worker with identity SHM), because minting and
maintaining the binding **requires** resolving `ilk → L2`, which needs identity SHM the ingress hive
does not have. The core resolves at publish-time and on change, and emits the **resolved** table
(`hash → L2 + guard`) downward. **This is the one open question of the whole design (§16):** whether
this authority *is* an `ICH` held by `SY.identity`, or a separate operational table that *references*
identity for resolution. Either way the edge is unaffected — it receives `hash → L2` regardless.

### 6.3 Last-mile relay — **the ingress-hive orchestrator**
The router **swallows a peer-received `CONFIG_CHANGED`** (`mod.rs:3578`), so the edge's push must be
**local** to its hive. The ingress-hive orchestrator receives the resolved table cross-hive (as a
normal forwarded system action) and re-emits it as a **local** `CONFIG_CHANGED` (subsystem
`endpoints`) to the edge. It holds **no authority**, does **no resolve**, needs **no identity SHM** —
pure relay. (This corrects v4, which wrongly located the master here.)

### 6.4 Cloud read view — **owned by `IO.cloud` (private), inside the frontier**
`IO.cloud` may cache its own reads to feed its UI. Private state, not in the authority chain, never
seeds the edge.

### 6.5 The ingress/egress mirror
Same shape the dev already ratified for egress: the worker's default-route `gateway_ip` is **declared
in `hive.yaml`** (intention) and **reconciled locally at each boot** — never a volatile
`ip route add` a reboot would lose.

| egress | ingress |
|---|---|
| `gateway_ip` in `hive.yaml` (intention) | binding authority at core (intention) |
| kernel route, volatile, reconciled at boot | edge SHM cache, volatile, reconciled at boot/push |
| worker reconciles locally | edge reconciles from local `CONFIG_CHANGED` (via relay) |

Intention/authority above (inside the frontier), volatile routing below (at the edge). The router is
untouched (Z, §7).

---

## 7. Request path

### 7.1 Option A — forward under the target's own family
Per request, `meta.msg_type = inbound_family` (the family the *target* declared). The edge does not
mint a bespoke `http.req`/`http.res` family.

### 7.2 Option Z — pre-resolved `handler_node` name (now mandatory, not a choice)
`ilk → handler_node` is resolved **once, at publish-time, at the core** (§9) and the resolved **L2
name** is cached in the row. At request time the edge sets `routing.dst = Unicast(handler_node)`.
**No request-time resolve; the router is untouched.** Z is now *obligatory* rather than a preference:
the edge is outside the identity frontier (§3.3), so it **cannot** resolve `ilk` and **must** receive
a pre-resolved name. This is valid because of I1 — the target is an IO node, whose `name →
reachability` the router floods.

### 7.3 Envelope shape (per request) — **no `dst_ilk`**
```
meta.msg_type      = inbound_family              # Option A
routing.dst        = Unicast(handler_node)       # Option Z, pre-resolved L2 name
meta.src_ilk       = <edge a-fuego self label>   # the edge signing itself, not a registry ilk
routing.src_l2_name = SY.edge@<hive>
context            = { method, path, query, headers* }   # *allowlist (M2)
payload            = body passthrough (JSON / utf8 / base64)
```
**Removed vs v4:** the edge no longer stamps `meta.dst_ilk`. It has no identity SHM to resolve one
and no need to carry one. If a handler needs to know *who* is calling, that identity resolution
happens **inside** the mesh (where identity SHM and L3 exist), not at the edge. Envelope cap 64 KiB;
`headers` is an allowlist, not a denylist.

### 7.4 Reply correlation
Correlated purely by `trace_id` (`send_with_matcher` + `RouteMatch::Any`) with an anti-shadowing
guard for router `UNREACHABLE`/`TTL` frames. Return leg must handle `UNREACHABLE` explicitly rather
than blocking to `HANDLER_TIMEOUT` (M4): surface a fast 502/504 with the real cause.

### 7.5 Staleness shrinks to exactly one node — the edge
Inside the frontier, `ilk → L2` **auto-propagates**: if a node moves/re-homes, `SY.identity` updates
its `node_name` and every identity-SHM node sees it on the next read, no restart, no re-publish. So
the "stale cached name" problem does **not** exist for any in-mesh consumer. It exists for the
**edge alone**, because the edge has no identity SHM. Therefore staleness is not a general design
flaw — it is precisely *"re-project the resolved name to the edge when it changes."* Alpha decision:
handler moves trigger a core re-resolve + re-push to the edge; the operator only re-publishes to
point at a **different ilk**.

---

## 8. `IO.cloud` — the cloud manager inside the mesh

### 8.1 Same species as any IO adapter
An IO node that adapts an external system already exists (`IO.linkedhelper`). `IO.cloud` is the same
species: the in-mesh adapter for the external system that happens to be Fluxbee Cloud. Not a new
concept — an instance of one that exists. That is why it is a simplification.

### 8.2 One door, terminates at an IO node (I2)
Cloud reaches the mesh only as ordinary app traffic to `IO.cloud`, through the edge, as a tenant.
`edge → IO.cloud` is the only inbound path; `IO.cloud → SY.admin` (or `→ archi`, `→ frontdesk`) is an
internal call. **Nothing from the internet reaches admin.**

### 8.3 Agent, not authority
`IO.cloud` **originates** operations (cloud asks → `IO.cloud` emits a scoped `publish_endpoint` toward
`SY.admin` → the normal flow, §9) and holds **no authoritative state**. It does not hold the binding
authority and does not push the edge directly. Since it is in-mesh, it **has identity SHM**, so it
reads ilk-space directly; but it reads via **scoped queries / its allowed SHM view**, not raw whole
tables — a compromise of the internet-adjacent cloud is bounded to its scope, not the full ilk-space.
Per the "cloud only knows ilk" rule, `IO.cloud` stays in **ilk-space**; the `ilk → L2` render lives
below it and is not cloud's concern.

### 8.4 Scope & the operational firewall
Small explicit allowlist (`register_io_node`, `list_endpoints`, `publish/unpublish_endpoint` for a
tenant) — never general admin. Permanently in the **operational lane** (tenant-scoped); never the
"what Fluxbee *is*" lane (identity, HARD premises → board escalation). This firewall bounds §8.5.

### 8.5 Residual risk (stated honestly)
A compromise originating from the internet is **bounded by `IO.cloud`'s policy scope and can never
exceed it**, because there is no path from the internet to admin except that node's scoped command
set. The scope firewall is load-bearing and lives in one auditable place.

### 8.6 Placement
`IO.cloud` lives **inside the mesh, not in the DMZ hive.** Edge disposable and public; `IO.cloud`
privileged and identity-bearing. They must not share blast radius.

### 8.7 Identity / OAuth (deferred, form captured)
Two identities potentially: `IO.cloud` (system principal) and the end-user ilk (the human cloud acts
for, minted by `AI.frontdesk`, a **subject**, never a routing destination). Alpha: cloud acts as
itself. Post-alpha: the human's ilk rides as a **claim inside** the command; `SY.policy` authorizes
over `(IO.cloud as transport principal) + (user ilk as subject)` — the `owner_node` pattern of
`vault_put`.

### 8.8 What `IO.cloud` dissolves
The only hole in I1 was ever *exposing archi*. With `IO.cloud` in-mesh, `IO.cloud → archi` is an
internal call; archi is never published and never touches the edge. **I1 has no exceptions.**

---

## 9. `publish_endpoint` — the operation (thread A)

### 9.1 Why an operation
It registers a **cross-node capability** — binds a public `hash` to a target's `ilk`, requires a
publish-time `ilk → L2` resolve against `SY.identity` SHM, and its source of truth is a binding
authority at the core. The `CONFIG_CHANGED` push is only delivery, not authority.

### 9.2 Command shapes
```
publish_endpoint   { edge_node, ilk, inbound_family, auth_mode, secret?, methods?, tenant_id? }
                   → { hash, public_url: "/e/<hash>", handler_node, version }
unpublish_endpoint { edge_node, hash } → { removed, version }
list_endpoints     { edge_node } → { endpoints: [...], version }
```
**Correction vs v4:** the input is the **`ilk`** (the stable thing cloud/architect speak), not an
`owner_node` L2 name. The core resolves `ilk → handler_node` against `SY.identity` SHM (Option Z
publish-time resolve) and mints the opaque `hash`. The caller never supplies `hash`, and the resolved
`handler_node` is a *derived, refreshable* value — the binding authority stores the stable `ilk`
(§16), the edge receives the derived L2.

### 9.3 Full flow (origin-agnostic — same for cloud and architect, I4)
```
originator ─────────────────────────────────────────────────────────┐
  • user chat → SY.architect → fluxbee_plan_compiler → 1-step plan   │ both converge on the
  • IO.cloud  → scoped publish_endpoint                              │ SAME admin door
                                                                     ▼
  → CONFIRM → executor_execute_plan → SY.admin  (on a hive WITH identity SHM)
       ├─ admin_origin_authorized gate
       ├─ resolve ilk → handler_node          ← via SY.identity SHM (NOT orchestrator)
       ├─ mint hash
       └─ record binding in the core authority (§16)
  → forward resolved table cross-hive → SY.orchestrator@<ingress-hive>   (relay, §6.3)
       └─ emit LOCAL CONFIG_CHANGED (subsystem "endpoints", real src_l2_name, §10)
  → SY.edge system loop: subsystem match → version gate → whole-map replace of SHM cache
```

### 9.4 Lease
Persist until `unpublish`. No TTL for alpha.

---

## 10. Security model — how gap #1 is dissolved

**The gap (#1):** the message that replaces the edge's entire public routing table was applied with
**zero sender verification** and was **not** in the router's origin-authority allowlist.

**Three layers:**
1. **Router allowlist.** Add the endpoint operations to `PROTECTED_SYSTEM_ACTIONS`
   (`src/router/system_policy.rs`) so only `SY.admin`/`SY.orchestrator` drive them cross-hive.
2. **Admin door.** `admin_origin_authorized` restricts who may invoke `publish_endpoint`.
3. **Edge origin gate (direct closure).** The edge accepts an `endpoints` whole-map replace **only**
   from `SY.orchestrator@<edge-hive>` (the relay, §6.3) **and** `node_name == self`. Requires the
   orchestrator to stop emitting `CONFIG_CHANGED` with `src_l2_name: None`
   (`sy_orchestrator.rs:9861`) and set a real, un-forgeable src (open item, §16).

The philosophically-clean answer (authority inside the frontier, edge as cache) and the
security-correct answer (edge trusts only its local relay; durable authority lives inside, not on the
DMZ box) are the same design.

---

## 11. `set/get_node_config` — the edge's own config (thread B)

### 11.1 Config vs operation
The edge's **own** params (DNS resolver, NIC/public-IP, listen, TLS, log level) are ordinary **node
config** via generic `set/get_node_config`. The edge is **not special** here: same config surface as
any node, plus edge-specific fields. Distinct from `publish_endpoint` (an operation, §9.1).

### 11.2 The `config` subsystem handler (the gap)
Today the edge reads config **only at boot** (`Config::load`); the only thing it applies live is the
endpoint table. To make DNS/NIC/listen/log settable without reinstall, add a **`config` subsystem
handler**, small and symmetric to `endpoints` (subsystem match → version gate → apply).

### 11.3 Live vs restart-required
- **Live:** `log_level`, DNS resolver override.
- **Restart / "pending restart":** rebinding `:443`, changing NIC/public-IP, swapping TLS material.

### 11.4 DNS means the resolver, not the public zone
- **Edge's own resolver** (which DNS server it uses / override to resolve internal names) → **node
  config, thread B.** Covered here.
- **Public zone record** (making `fluxbee.ai/e/...` point at the edge's public IP) → **not the edge's
  config.** External DNS-zone op, ACME-sibling, at core, **deferred behind the wildcard cert for
  alpha.**

---

## 12. `add_ingress` — provisioning

`add_ingress_hive_flow` + `IngressSection`. Four gaps block a from-scratch TLS-serving ingress (these
are the alpha deployability blockers, §14):

- **H3 — TLS field on `IngressSection`, fail-closed.** Today the generated `edge:` block writes only
  `listen` + `endpoints_path`; with no TLS field, `tls_requested=false` and the door binds
  **cleartext** — a from-config ingress serves bearer tokens in the clear. Carry TLS material; edge
  fails closed (no TLS → no listen).
- **H2 — ship an ingress `system_nodes` template.** No shipped `hive.yaml` defines
  `system_nodes.ingress`, so `system_nodes_for_role(_, Ingress)` errors to `CONFIG_FAILED`.
- **M1 — validate the endpoints seed.** A single malformed row (rows now require `handler_node` +
  `inbound_family`, no defaults) yields an empty map → every `/e/<hash>` 404s while `add_ingress`
  reports success. Validate at provisioning time.
- **H1 — package `sy-edge`** (binary + systemd unit); the manifest gate returns `MANIFEST_INVALID`
  and `sy-edge.service` `ExecStart` points nowhere.

---

## 13. Special cases live on the IO side, never on the edge (I7)

The edge has exactly one behavior — `hash → L2`, forward — and it will **never** grow. When a new
kind of thing must be exposed, the question is never "how does the edge handle it?" but "which IO
node adapts it?" The edge does not learn about it; it keeps seeing `hash → L2`.

- **Web page / Fluxbee artifact** → an `IO.web` (or `IO.cloud`) serves the artifact. The edge sees it
  as one more IO handler, `hash → L2`, identical to any other. Zero new edge code.
- **Blob** → an `IO.blob` adapts storage, does the streaming, handles `blob_ref`. All blob complexity
  (which is real) lives on the IO side, inside. The edge forwards to it like any other IO. Zero new
  edge code.

This turns I1 from a rule-to-remember into the **design valve**: everything to expose is modeled as
an IO node, and by construction the expensive public box stays trivial while all evolution happens in
the cheap inside nodes.

---

## 14. Alpha scope line

**In scope (blockers):**
1. `publish_endpoint` (input = `ilk`) + three-layer authz (§9, §10) — closes #1.
2. Package `sy-edge` + unit (H1).
3. Ingress `system_nodes` template (H2).
4. Public-door TLS, fail-closed (H3).
5. Endpoints-seed validation (M1).
6. Header allowlist (M2, §7.3).
7. Scope down edge mesh reachability (M3, §5.3).
8. Bounded pending map — `Arc<Semaphore>` → 503 on overflow (L1).
9. Return-leg `UNREACHABLE` handling + fix the "by name" comment (M4, §7.4).
10. Declare & document the cross-hive topology limit (H4).
11. Remove `meta.dst_ilk` stamping from the edge (this rev).

**Cross-hive topology (H4) — declared limit:** LSA is one WAN hop. Ingress cross-hive works only to
the direct WAN neighbour (hub) or same-hive. Alpha declares **"adjacent-hub-or-same-hive only."**
Multi-hop relay deferred.

**Deferred by design:** control channel (*deleted*, §4); `EDGE_REGISTER` self-publication;
lease/TTL; blob egress; multi-hop cross-hive; JWT/ACME + public-zone DNS (wildcard sidesteps for
alpha); WS/SSE upgrade (only the `501` stub, L3, is arguably alpha); per-user (end-user ilk) authz
(§8.7).

---

## 15. Build order

1. **Live-validate the hot-swap path** with the existing `set_node_config` (push `{endpoints:[...]}`,
   confirm whole-map replace) — de-risks §9 against code that already exists.
2. **`publish_endpoint` (input `ilk`)** + the **three authz layers** (§9, §10) — closes #1.
3. **H1 + H2 + H3** — a real from-scratch TLS-serving ingress.
4. **M1 / M2 / M3 / M4 / L1** + remove `dst_ilk` — finish the non-deferred alpha surface.
5. **`IO.cloud`** — stand up the thin in-mesh cloud manager (can proceed in parallel with 2–4).
6. **Thread B `config` subsystem handler** (§11).
7. Spec/checklist cleanup.

---

## 16. Open items

- **THE core open item — binding authority shape (§6.2).** Is a published endpoint an **`ICH` held by
  `SY.identity`** (reuse: auto-propagation, single owner sees `node_name` changes, `IO.cloud` reads it
  from identity SHM, `list_endpoints` becomes an SHM read; cost: `auth_mode`/`secret`/`methods` are
  not identity and would split to `SY.policy`), or a **separate operational table that references
  identity** for resolution (clean auth separation; cost: must subscribe to identity changes to
  re-render)? **This is entirely inside the frontier and does not affect the edge** — the edge
  receives `hash → L2` either way. Parked consciously.
- **`src_l2_name` un-forgeable stamp** (§10 layer 3): router re-stamp vs orchestrator sets it. The
  edge gate depends on it.
- **Endpoint-table > 64 KB.** The `CONFIG_CHANGED` whole-map replace inherits the 64 KB envelope cap.
  Fine for alpha (few endpoints); if the set grows, the edge push needs a chunked mechanism like
  identity's socket sync. Track.
- **Hash minting** algorithm + collision handling.

---

## 17. Reference — file:line anchors

- **Edge:** `src/bin/sy_edge.rs` — forward+reply (~716–885), hot-swap loop (~259–283), `EndpointRow`
  (~485, drop any `ilk`/`dst_ilk` field), `rows_to_registry` (~523), fail-closed TLS (~205–245),
  `filter_request_headers` (allowlist, M2), remove `dst_ilk` stamping in the forward path, return-leg
  comment (~821, M4).
- **add_ingress / orchestrator (relay role):** `src/bin/sy_orchestrator.rs` — `add_ingress_hive_flow`
  (~17515), `IngressSection` (~180), `set_node_config_flow` (~12541), `send_node_config_changed_signal`
  (~9852–9888; generalize `subsystem` + real `src_l2_name`), the `src_l2_name: None` emit (~9861),
  manifest gate (~17677), local relay emit, cross-hive forward (~11281).
- **Core resolve + authority:** wherever `SY.admin` runs with identity SHM — `ilk → handler_node`
  resolve reusing the identity SHM lookup (unique index on `node_name`); binding record per §16.
- **Admin:** `src/bin/sy_admin.rs` — `ADMIN_ACTIONS` (~68–100), `INTERNAL_ACTION_REGISTRY` (~2941),
  `handle_admin_command` (~2251/10300), `admin_origin_authorized` (~2583/2637), action-help arms
  (~7104+).
- **Archi:** `src/bin/sy_architect.rs` — `register_tools` (~920), `plan_compiler` (~4376),
  `execute_executor_plan_with_context` (~12974), mutation-through-plan-compiler rule (~114).
- **Identity (read-only from this spec's view):** `SY.identity` owns `identity_ilks.node_name`
  (unique index) = the `ilk → L2` source; socket sync, not `CONFIG_CHANGED`; absent in
  ingress/egress.
- **Router authz:** `src/router/system_policy.rs` — `PROTECTED_SYSTEM_ACTIONS` (~25), authority
  (~69), unit-test length assertion (~156).
- **Router CONFIG_CHANGED:** `src/router/mod.rs` — local delivery (~747), peer-swallow (~3561/3578).
- **Deleted:** the `edge-control-protocol` implementation and the `FC.edge-manager` control-channel
  client.
