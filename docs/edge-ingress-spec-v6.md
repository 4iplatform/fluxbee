# SY.edge / INGRESS — Specification v6 (canonical)

**Status:** design-complete for alpha, grounded in code. Supersedes `edge-ingress-spec-v5.md`.

**What changed from v5 (the model was corrected against the code):**
- The public URL **is an `ICH`** — the channel identity that already exists in `SY.identity`
  (`IchEntry`, deterministic `ich:uuid` from `(tenant, channel_type, address)`, with the owning
  node's `owner_l2_name` stored in it). Not a bespoke minted "hash". The `ICH` is the channel and
  the discriminator; `url ↔ ICH`.
- Publication is renamed **externalize** and is **self-service by the OWNING IO node, sent DIRECTLY
  to `SY.admin`** (not via archi/operator, not to the edge). This mirrors how `orchestrator`/
  `frontdesk` go directly to `SY.identity` for `ILK_REGISTER`. Authorized by an `IO.`-prefix grant
  + `requester == the ICH's owner` — one-to-one, never anyone-to-anyone.
- The two planes are kept strictly apart: **identity creates the ilk/ICH** (`ILK_PROVISION`, node→
  `SY.identity`); **admin externalizes** (node→`SY.admin`). v5's confusion came from mixing them.
- The entry **token** is minted by **`SY.admin` during externalize** (vault writes are admin-only),
  stored in `SY.vault` owned by the node's ilk, placed in the edge row and returned to the node.
- The edge **carries** the `ICH` (an opaque tag it stamps into `meta.ich`) but never resolves it —
  correcting v5's over-strong "no ICH at the edge."

---

## 1. Goal

Expose selected internal **IO-node channels** at public HTTPS URLs, on a **disposable** DMZ box,
where each URL is a channel `ICH`, the exposure is **self-requested by the owning IO node** through
the normal admin command plane, with **no inbound path into the mesh control plane** and the edge
**holding nothing about identity and nothing irreplaceable**.

---

## 2. Invariants (load-bearing)

- **I1 — The edge only forwards to IO nodes.** Enforced at externalize by the `IO.`-prefix grant
  (§7). Everything to expose is modeled as an IO node (I7).
- **I2 — No path from the public internet to the admin plane.** Internet inbound terminates at an
  IO node; `SY.admin` is reachable only from inside the mesh.
- **I3 — The DMZ box holds nothing irreplaceable.** The edge may be reimaged at any time and must
  converge from an authority **inside** the mesh. It persists only its own TLS material and a
  rebuildable cache. **This is why externalize goes through admin, not to the edge** (§7.1).
- **I4 — One authority per fact.** The externalized-channel binding has exactly one authoritative
  holder inside the mesh; the *originator* of a change (the IO node, later archi) does not change
  who is authoritative.
- **I5 — The router stays dumb.** L2 routing knows `name → reachability`, nothing about auth/
  tenants/channels. The router is untouched by ingress.
- **I6 — The edge is outside the identity frontier.** It holds no `ilk`, no identity SHM, and never
  resolves `ilk`/`ICH`. It routes on `ICH → owner_l2_name` (a value handed to it) and stamps the
  `ICH` verbatim.
- **I7 — Special cases live on the IO side, never on the edge.** The edge has one behavior and
  never grows; new capabilities are absorbed by IO nodes.
- **I8 — Externalize is self-service by the owning IO node.** The node that owns a channel `ICH`
  requests its own exposure, directly to `SY.admin`, gated by `IO.`-prefix + `requester ==
  owner_l2_name`. One-to-one. A node can only externalize a channel it owns; it can never repoint
  or expose another node.

---

## 3. The identity frontier

`SY.identity` owns the entity registry: `ilk ↔ { type, node_name (L2), tenant, ICH…, definition }`,
in PostgreSQL (primary) + `jsr-identity` SHM (every hive **except ingress/egress**). Any in-mesh
node resolves `ilk`/`ICH` locally from SHM; the edge cannot (no SHM there).

- **Inside the frontier:** `ilk`/`ICH` exist and resolve.
- **Outside (the edge):** only `ICH → owner_l2_name`, handed down pre-resolved.

The `ilk` is used **once, inside, at externalize-time** to authorize + resolve; it **never crosses
to the edge**.

---

## 4. `ICH` — the channel identity (the central concept)

Grounded in `crates/fluxbee_sdk/src/identity.rs`:

- **An `ICH` is a unique channel.** `stable_ich_id(channel_type, address, tenant_id) =
  ich:uuid5(tenant:type:address)` (`identity.rs:396`) — deterministic, unique system-wide, opaque
  (a UUID; leaks no tenant/type/address).
- **The owning node is stored in the ICH.** `IchEntry { ich_id, ilk_id, tenant_id, channel_type[32],
  address[256], owner_l2_name[128], enabled, is_primary, … }` (`identity.rs:342`). So `ICH →
  owner_l2_name` is a **direct read** in identity SHM — no `ilk → L2` hop.
- **`ICH` already flows on messages:** `Meta.ich: Option<String>` (`protocol.rs:57`); io-nodes route
  channels by it (e.g. io-slack `meta.ich = "slack://T123"`).
- **`ICH` can be enabled/disabled:** `MSG_ICH_SET_ENABLED` (`identity.rs:27`) + the `enabled` flag —
  a natural home for an "externalized" state (§12 open item).

**`url ↔ ICH`.** The public URL is `/e/<ICH>`. The edge maps `ICH → owner_l2_name`, forwards to that
node, and stamps `meta.ich = <ICH>` so the node knows exactly which channel the request is for. The
`ICH` is the **discriminator**: one IO node can own many channels (many ICHs), each a distinct URL,
all disambiguated by `meta.ich` — with **zero** extra edge state.

---

## 5. Two planes (never mix them — the v5 poison)

| Plane | Action | Authority | Who initiates | Authz |
|---|---|---|---|---|
| **Identity** | create the node's `ilk`/`ICH` | `SY.identity` | the node | `is_authorized` prefix/exact (`ILK_PROVISION → ["IO."]`, `identity.rs:1762`) |
| **Command** | **externalize** an `ICH`; **config** the edge | `SY.admin` (the one command runner) | the node (externalize) / operator (config) | admin per-action grant (§7.2) |

The `ICH` already exists (identity plane) when the node externalizes it (command plane). Admin does
not create the channel; it exposes it.

---

## 6. The SY.edge node (aseptic)

- **Role/placement:** `role: Ingress`, dedicated DMZ hive, two NICs (public `:443`, internal WAN).
  The single public door. **No `SY.identity`, no vault beyond its own TLS material.**
- **Self label a fuego:** `self_ilk = deterministic_system_ilk_id("SY.edge@<hive>")` — a fixed
  self-label to sign its own frames; **not** a registry lookup (it can't reach identity). It is
  **untrusted at core** (the trust boundary is the router-stamped `src_l2_name`); may be dropped.
- **Born with ZERO endpoints.** No seed. The cache converges purely from admin pushes (I3).
- **Holds:** its own TLS cert/key; a live routing cache `ICH → { owner_l2_name, inbound_family,
  auth_mode, secret?, methods? }` (SHM, `Arc<RwLock<HashMap>>`, whole-map replace, monotonic
  version); its own node config (§9). **No `ilk`, no identity SHM, no authoritative set.**
- **Reachability constrained (M3):** its `sy-config-routes` lists only the IO handler nodes it
  forwards to — no blanket system-node reachability.

---

## 7. `externalize` — the operation (self-service, IO → admin)

### 7.1 Why direct to admin, never to the edge
By **I3** the edge holds nothing rebuildable-only-from-itself. If a node registered directly on the
edge, the edge would **become the authority** — a reimage would lose the bindings. And the edge is
dumb: no identity to resolve `ICH → owner`, nothing to authorize. So the durable authority lives
**inside** (`SY.admin`, which runs commands and has identity SHM); the node sends `externalize`
directly to admin; admin pushes the resolved cache to the edge; the edge converges.

This mirrors the existing pattern: `orchestrator`/`frontdesk` send `ILK_REGISTER` **directly to
`SY.identity`** (`allowed_exacts`, `identity.rs:1798`). Here IO nodes send `externalize` **directly
to `SY.admin`**.

### 7.2 Authz — an `IO.`-prefix grant + ownership
`SY.admin` today gates protected actions to `SY.admin` origin (`admin_origin_authorized`,
`sy_admin.rs:2583/2637`). `externalize` needs a **per-action grant** mirroring identity's
`allowed_prefixes` (`identity.rs:1762`):
- the router-stamped `src_l2_name` must start with `IO.` (I1: IO-only), **and**
- `resolve(ICH).owner_l2_name == src_l2_name` (I8: one-to-one — you externalize only your own
  channel; `src_l2_name` is un-forgeable, router-stamped at `mod.rs:975`).

**Extensibility:** later, to let archi externalize on behalf of something, add `SY.architect@` to the
grant's `allowed_exacts` — one line, no redesign.

### 7.3 Command shapes
```
externalize      { ich, inbound_family, auth_mode, methods? }
                 → { url:"/e/<ich>", owner_l2_name, token?, version }
unexternalize    { ich } → { removed, version }
list_externalized { }    → { channels:[ {ich, url, inbound_family, auth_mode, methods} ], version }
```
The caller passes only its **own** `ICH`. It never passes `owner_l2_name` (admin reads it),
never a hash (the ICH *is* the url), never an ilk (frontier).

### 7.4 Full flow
```
IO node (e.g. IO.cloud) ── externalize {ich, inbound_family, auth_mode} ──► SY.admin   (direct, in-mesh)
   router stamps src_l2_name = "IO.cloud@<hive>"                    ◄── un-forgeable (mod.rs:975)
   admin:
     ├─ grant check: src_l2_name starts with "IO."                 ◄── I1
     ├─ resolve ICH → owner_l2_name (identity SHM) ; owner == src_l2_name?   ◄── I8, one-to-one
     ├─ if auth_mode=shared-secret: mint token → vault_put owned-by node ilk (§8)
     ├─ record durable binding (inside the mesh, §12)
     └─ EDGE_OPEN_URL {row} ──► SY.edge@<ingress-hive>    # addressed service command (§7.6)
          admin BLOCKS on EDGE_OPEN_URL_RESPONSE           # the URL is "published" only once the edge acks
   edge: upsert the row by ich  →  EDGE_OPEN_URL_RESPONSE { ok, url }
   response to the node: { url:"/e/<ich>", token? }        # whole chain synchronous: IO ◄ admin ◄ edge
```
The externalize request travels the **internal mesh** (IO.cloud → SY.admin), never through the edge
— so there is no bootstrap chicken-and-egg (§10).

### 7.6 The admin→edge leg is a COMMAND, not a config push (lab-corrected)
Opening/closing a URL is a **verified service directive**, not the edge's config. It is delivered as
an **addressed request/response**: `EDGE_OPEN_URL` / `EDGE_CLOSE_URL` (Unicast to the edge, acked with
`*_RESPONSE`). Consequences:
- **Cross-hive works with zero router changes.** An addressed command routes like any node RPC
  (`ForwardHive`); it is NOT a `CONFIG_CHANGED` broadcast, which the peer router intentionally
  swallows (`mod.rs:3578`) — that swallow is correct for route/OPA config but silently dropped the
  endpoint push to a remote edge (the bug the ingress lab surfaced).
- **Synchronous confirmation.** Admin blocks on the ack, so the IO node is told `ok` only after the
  edge actually holds the row. No fire-and-forget.
- **Per-URL, not whole-table-replace.** Each `EDGE_OPEN_URL` upserts one `ich`; `EDGE_CLOSE_URL`
  removes one. (The old whole-map replace wiped every other URL on each externalize — gone.)
- **Distinct from `node_config` (§9).** `set/get_node_config` is the edge's OWN config; it never
  carried endpoints. The prior `CONFIG_CHANGED subsystem=endpoints`/`node_config` overload was the
  "under-the-table" muddle and is removed from the edge.

### 7.5 Request path (runtime)
Per external request to `/e/<ich>`:
```
meta.msg_type       = inbound_family                 # Option A — the target's own family
meta.ich            = <ich>                           # the channel discriminator (stamped verbatim)
routing.dst         = Unicast(owner_l2_name)          # Option Z — pre-resolved L2 name
routing.src_l2_name = SY.edge@<hive>                  # so the reply routes back by name
context             = { method, path, query, headers* }   # *allowlist (M2)
payload             = body passthrough (JSON / utf8 / base64)
```
**No `dst_ilk`, no `ilk`.** Envelope cap 64 KiB. Reply correlated purely by `trace_id`
(`send_with_matcher` + `RouteMatch::Any` + anti-shadowing guard for `UNREACHABLE`/`TTL`; return-leg
`UNREACHABLE` surfaced as a fast 502, not a blocked `HANDLER_TIMEOUT` — M4).

---

## 8. The entry token — minted by admin, stored in vault

If `auth_mode = shared-secret`, the channel needs an entry credential. Because **vault writes are
admin-only** (`handle_put` requires `is_well_known_admin`), the node cannot mint its own — **admin
mints it during externalize**:
- admin mints a random token → `vault_put` **owned by the node's ilk** (dedicated-owner: the node
  can read it back, `secret.ilk == caller.src_ilk`);
- admin places it in the edge row (`secret`) and returns it in the externalize response.

Token lands in three places, one flow: **edge** (door check), **response** (the node → its external
clients), **vault** (durable; survives an edge reimage via the re-push; node re-reads; rotation via
`vault_rotate`, an admin action).

- **Alpha:** the edge checks `Authorization: Bearer <token>` **locally** against the row secret.
- **Deferred (hardening):** the edge asks the vault per request so the token never sits on the DMZ.

---

## 9. `set/get_node_config` — the edge's own config (distinct surface)

The edge's **own** params (DNS resolver, NIC/public-IP `expected_public_ip_cidr`, `listen`, TLS
material, `log_level`) are ordinary **node config** via generic `set/get_node_config` — through
admin, like any node. Distinct from externalize (§5/§7.6): `node_config` is the edge configuring
**itself**; it NEVER carries the URL table (a service the edge gives to others).

**Corrected (lab):** the edge used to treat `CONFIG_CHANGED subsystem="endpoints"` **and**
`"node_config"` identically — both replaced the endpoint table. That conflation is removed; the
endpoint table is now driven only by `EDGE_OPEN_URL`/`EDGE_CLOSE_URL` (§7.6), and `node_config` is
free for its real purpose.

Gap (still open): today the edge reads its own config **only at boot** (`Config::load`). Add a
`node_config` handler so params apply live. Live: `log_level`, DNS resolver. Restart-required:
rebind `:443`, change NIC, swap TLS.

**DNS split:** the edge's own *resolver* is node config (here). The public *zone record*
(`fluxbee.ai/e/…` → edge IP) is an external DNS-zone op at core, deferred behind the wildcard cert.

---

## 10. `IO.cloud` — the cloud manager, in-mesh, and the bootstrap

- Same code species as `IO.linkedhelper` (an in-mesh IO adapter for an external system, here Fluxbee
  Cloud) — but a **different deployment shape**: it is a **SINGLETON, one per system, on the
  motherbee**, so it is **baked into the core `.deb`** (binary + `io-cloud.service`), NOT published as
  a per-tenant runtime package like `io-api`/`io-slack`. The unit is enabled everywhere but an
  `ExecCondition` gates it to `role: motherbee`, so it only ever runs on the motherbee. It comes up
  after `rt-gateway`/`SY.identity`/`SY.admin` and retries registration until the mesh is ready.
- **The first node to externalize.** On boot → gets its ilk (orchestrator-injected
  `FLUXBEE_NODE_ILK_ID` if spawned, else self-provisions via `ILK_PROVISION`) → registers its channel
  `ICH` (identity plane) → optionally sends `externalize {ich}` to `SY.admin` (command plane, opt-in
  via `IO_CLOUD_EDGE_NODE`; opening a public URL is deliberate, not automatic). All **over the
  internal mesh**, not through the edge — so there is **no chicken-and-egg**.
  Its `ICH` routes only to `IO.cloud`. Then external traffic reaches the mesh **only** as ordinary
  app traffic to `IO.cloud` (I2); `IO.cloud → SY.admin`/`archi`/`frontdesk` are internal calls.
- **Agent, not authority.** Holds no authoritative binding. In-mesh so it can read ilk-space, but
  it **stays in ilk/ICH-space**; it needs **no identity SHM whole-table view** — it speaks ICHs and
  lets the core resolve. Scope: a small allowlist (`externalize`/`unexternalize`/`list_externalized`
  for its own channels), never general admin — the one auditable operational firewall (I2 bound).
- Lives **inside the mesh, not in the DMZ hive** — must not share blast radius with the edge.
- Deferred: the end-user ilk as a *subject claim* inside the command, `SY.policy` over `(IO.cloud
  transport principal) + (user ilk subject)`.

---

## 11. Security model — why gap #1 dissolves

The audit's #1 (edge routing table replaceable with zero sender verification) was real **only under
the wrong model** (an operator binding an *arbitrary* ilk to a hash → repoint/expose any node). The
self-externalize model **deletes that primitive**:
- `owner_l2_name` = the requester itself (router-stamped) → **you cannot repoint or expose a third
  party**, only make a door to your own channel.
- `IO.`-prefix grant → non-IO cannot externalize (I1).
- Closed mesh (I2) → no external actor can externalize.
- The entry **token** gates who *enters* the door.

What remains is minor and matches the invariants: the edge trusts its **local relay** for cache
pushes (defense-in-depth; but the edge is a DMZ cache — if the DMZ is compromised the cache is moot,
and the **durable** authority is inside, I3). The heavy "protected-action + admin-origin + edge-gate"
stack proposed earlier was defending a threat this model removes.

### 11.1 ⏳ PENDING (DEFERRED BY DECISION) — the `externalize` authz gate is NOT built yet

**Status: the door is intentionally left OPEN for the first cut.** The `IO.`-prefix + owner-check
above is the *design*; it is **not implemented yet** — the first `externalize` accepts the request
**without** the origin/owner gate. Rationale (user decision, this session): the **admin plane must
complexify first** (more commands, planes, roles), and the authz doors get **closed properly then** —
not patched prematurely onto a chain that today does not even gate the `ADMIN_COMMAND` entry
(`sy_admin.rs:2730` dispatches with no origin check; the effective mutation gate is the orchestrator's
`is_allowed_admin_source_name`, and `SY.admin` relays under its own origin). The correct fix is a
**dedicated, explicit gate** added when admin's authz model matures.

> **⚠️ Until this closes, `externalize` is UNAUTHENTICATED. Must NOT ship to production.** This is
> the reopened form of audit gap #1 — tracked here on purpose, not forgotten.

---

## 12. Where the durable binding lives (the one open item)

The externalized-channel binding is authoritative **inside the mesh**. Two shapes, both inside, both
edge-invisible:
- **As an `ICH` attribute in `SY.identity`** — reuse `IchEntry.enabled`/flags for an "externalized"
  bit + the URL; free auto-propagation, single owner, `list_externalized` = an SHM read, `IO.cloud`
  reads it from identity. Cost: `auth_mode`/`secret`/`methods` are not identity → split to a small
  operational side-table.
- **As a separate operational table owned by admin/orchestrator** that *references* identity for
  resolution. Clean auth separation; cost: must react to identity `node_name` changes to re-render.

Parked consciously — the edge receives `ICH → owner_l2_name` either way.

---

## 13. `add_ingress` — provisioning (alpha deployability blockers)

**H1/H2/H3 RESOLVED + lab-validated (commit `1c2f507`).**

- **H1 — package `sy-edge` — DONE.** `packaging/build-deb.sh` adds `sy-edge:sy_edge` to
  `RUST_BINS` (staged into `/usr/bin` + `dist/core/bin`, auto-hashed into the core manifest) and a
  dedicated unit `After/Wants=rt-gateway.service sy-vault.service` (it fetches TLS from vault at
  boot). `deb-prerm` stops it; `scripts/install.sh` has dev-path parity. Verified inside the built
  `.deb`: binary + unit + manifest entry present — the `MANIFEST_INVALID` gate now passes.
- **H2 — ingress `system_nodes` template — DONE.** `config/hive.yaml` +
  `packaging/hive.yaml.example` ship `system_nodes.ingress = [SY.config.routes, SY.edge]`, so
  `system_nodes_for_role(_, Ingress)` resolves and `add_hive role=ingress` no longer aborts.
- **H3 — TLS on `IngressSection`, fail-closed — DONE.** `IngressSection` accepts
  `ingress.tls_vault_key` + `vault_hive` (YAML-scalar-guarded) and renders them into the remote
  `edge:` block; absent them the edge sets `tls_requested=false` and binds cleartext, so an HTTPS
  ingress **requires** `tls_vault_key` + the secret seeded (owner `SY.edge@<edge_hive>`).
  Lab-proof: real `*.fluxbee.ai` cert seeded into `SY.vault`, edge fetched it by its deterministic
  ilk and bound HTTPS; `curl https .../e/<ich>` → 200 with the Sectigo cert served.
- **Edge born zero:** the seed file is empty/absent; endpoints arrive only via externalize.
  (Kills M1's "malformed seed → silent all-404": there is no seed.)

---

## 14. Special cases live on the IO side (I7)

The edge is `ICH → forward`, forever. New kind of thing to expose → *which IO node adapts it?* —
`IO.web` for artifacts, `IO.blob` for blobs. Zero new edge code; all evolution in the cheap inside
nodes. This is the design valve, not a rule to remember.

---

## 15. Alpha scope line

**In scope:**
1. `externalize`/`unexternalize`/`list_externalized` as `SY.admin` commands, IO→admin direct,
   `IO.`-prefix grant + owner check (§7). Closes #1.
2. Edge row reshaped to `ICH → { owner_l2_name, inbound_family, auth_mode, secret?, methods? }`;
   stamp `meta.ich`; **remove `ilk`/`dst_ilk`** (§4, §7.5).
3. Edge **born zero** (no seed) + converge from pushes (§6).
4. Admin-minted entry token → vault owned-by-node, edge-local check (§8).
5. Package `sy-edge` (H1), ingress `system_nodes` template (H2), fail-closed public TLS (H3).
6. Header allowlist (M2), bounded pending map → 503 (L1), return-leg `UNREACHABLE` handling (M4),
   scope down edge reachability (M3).
7. Declare the cross-hive topology limit (H4): **adjacent-hub-or-same-hive only** (LSA is one WAN
   hop); multi-hop relay deferred.

**Deferred by design:** the durable-binding shape decision (§12); edge-asks-vault token check;
`IO.cloud` end-user-ilk subject authz; blob egress; multi-hop cross-hive; JWT/ACME + public-zone
DNS (wildcard sidesteps); WS/SSE upgrade (501 stub only); the edge `config` subsystem (thread B) if
not needed for the first proof.

---

## 16. Build order

1. **Reshape the edge row + forward** to the ICH model (`ICH → owner_l2_name`, stamp `meta.ich`,
   drop `ilk`/`dst_ilk`) and re-validate the forward pipe with an **IO handler** (not AI.test.gov).
2. **`externalize` command** in `SY.admin` + the `IO.`-prefix/owner grant + push to edge → closes #1
   and proves the real self-service flow.
3. **Admin-minted token** → vault owned-by-node, edge-local door check.
4. **H1 + H2 + H3** — a real from-scratch TLS-serving ingress that boots **zero** and converges.
5. **`IO.cloud`** — the thin in-mesh cloud manager; first real externalize (bootstrap). Can run in
   parallel with 2–4.
6. **M2/M3/M4/L1** — finish the non-deferred alpha surface.
7. Thread B (`config` subsystem) + spec/checklist cleanup.

---

## 17. Reference — file:line anchors

- **ICH:** `identity.rs` — `IchEntry` (342, `owner_l2_name`), `stable_ich_id` (396),
  `resolve_ich_mapping` (841/963), `list_ich_options` (859), `MSG_ICH_SET_ENABLED` (27); `Meta.ich`
  `protocol.rs:57`.
- **Self-service authz pattern:** `identity.rs` — `is_authorized` (2815), `allowed_prefixes`
  `ILK_PROVISION → ["IO."]` (1762), `allowed_exacts` `ILK_REGISTER` (1798); router stamps
  `src_l2_name` (`mod.rs:975`, WAN 2848/3614).
- **Admin command plane:** `sy_admin.rs` — `ADMIN_ACTIONS` (68–100), `INTERNAL_ACTION_REGISTRY`
  (2941), `handle_admin_command` (2251/10300), `admin_origin_authorized` (2583/2637),
  `normalize_vault_put_payload` owner resolve (10885), action-help (7104+).
- **Vault (admin-only write, dedicated-owner read):** `sy_vault.rs` `handle_put` (is_well_known_admin);
  `VaultClient.get` dedicated-owner.
- **Delivery (verified):** `sy_orchestrator.rs` `send_node_config_changed_signal` (9852–9888;
  generalize `subsystem` + real `src_l2_name`, drop `:9861` `None`); router LOCAL CONFIG_CHANGED
  **broadcasts to local nodes** (`mod.rs:795-801`), peer path **swallows** (`3578`).
- **Edge (to reshape):** `sy_edge.rs` — forward+reply (716–885), hot-swap loop (259–283),
  `EndpointRow`/`EndpointEntry` (485/500 — becomes `ICH → owner_l2_name`, drop `ilk`, add `meta.ich`
  stamp), fail-closed TLS (205–245), `filter_request_headers` (allowlist M2).
- **add_ingress:** `sy_orchestrator.rs` `add_ingress_hive_flow` (17515), `IngressSection` (180),
  manifest gate (17677); packaging `packaging/build-deb.sh`, `scripts/install.sh`; template
  `config/hive.yaml`, `packaging/hive.yaml.example`.
