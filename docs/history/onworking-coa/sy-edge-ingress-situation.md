# SY.edge / INGRESS — Situation & Audit Report (for spec review)

**Status: 2026-07-05.** This is a *situation capture*, not a spec. It records (1) what is
built and validated, (2) what the deep audit surfaced, (3) the open design threads
(the `publish_endpoint` command + the edge config surface) with every edge case laid
out — so the design can be reviewed in one place and a proper spec written from it.

Related: `docs/edge-ingress-spec-v3.md` (the current spec, already coherent with A+Z),
memory `ingress-edge-v3-design.md`. Commit of the current code: `c1e827f`.

---

## 0. TL;DR

- The **INGRESS node = `SY.edge`**. Design pivoted (this session) away from a bespoke
  `http.req`/`http.res` L2 family (a "mutation") to:
  - **Option A** — forward under the target's OWN declared family (`inbound_family`).
  - **Option Z** — forward by the PRE-RESOLVED `handler_node` NAME cached in the endpoint
    row (resolve `ilk→handler_node` happens once, at publish time, at core; no request-time
    resolve; the router is left untouched).
- **Validated end-to-end** on the Proxmox lab: same-hive AND cross-hive, `curl → HTTP 200`
  with `nodes/test/ai-test-gov` as the echo handler.
- A **deep adversarial audit** found the core request-path SOLID, but surfaced **1 security
  gap (#1)** + **4 HIGH** (not deployable from scratch + plaintext door) + several MEDIUM/LOW.
- The agreed direction for #1: publishing an endpoint should be a **fluxbee operational
  command** driven by SY.architect/the user (not an IO-node tool), which inherits the admin
  authz plane — this dissolves #1. Design thread in §4.
- A **second surface** (design thread §5): the edge's OWN config (DNS, NIC/public-IP, listen,
  TLS, log) via `set/get_node_config` — distinct from the publish operation.

---

## 1. WHAT IS BUILT (and validated)

### 1.1 Design (Option A + Z)
- Edge = a public reverse-proxy node on a DMZ hive (role `Ingress`, 2 NICs: public `:443`,
  internal mesh WAN). No `SY.identity`, no vault, holds only its own TLS material.
- ilk + tenant **a fuego**: `self_ilk = deterministic_system_ilk_id("SY.edge@<hive>")`
  (SHA-256, no SHM, no ILK_REGISTER); tenant = `DEFAULT_ROOT_TENANT_ID`.
- **Forward** (per request): `meta.msg_type = inbound_family` (Option A),
  `routing.dst = Unicast(handler_node)` (Option Z, pre-resolved NAME),
  `meta.dst_ilk = ilk` (carried, not used for routing), `meta.src_ilk = <edge a-fuego ilk>`,
  `routing.src_l2_name = SY.edge@<hive>`, `context = {method, path, query, headers}`,
  `payload` = body passthrough (JSON/utf8/base64). 64 KiB envelope cap.
- **Reply** correlated purely by `trace_id` (`send_with_matcher` + `RouteMatch::Any` + an
  anti-shadowing guard for the router's UNREACHABLE/TTL frames). Reply payload wrapped as
  the HTTP 200 body.
- **Endpoint table**: `Arc<RwLock<HashMap<hash, EndpointEntry>>>`, seeded from
  `/etc/fluxbee/edge.endpoints.json`, **hot-swapped** via `NODE_CONFIG_SET`/`CONFIG_CHANGED`
  (subsystem `endpoints`/`node_config`, monotonic version, whole-map replace).
- Row = `{ hash, ilk, handler_node, inbound_family, auth_mode, secret?, methods?, tenant_id? }`.

### 1.2 Code surface (committed, `c1e827f`)
| File | What |
|---|---|
| `src/bin/sy_edge.rs` (~750 ln) | the edge. compiles, 9/9 unit tests. |
| `src/bin/sy_orchestrator.rs` | `add_ingress_hive_flow` (~17515), `IngressSection` (~180). 2/2 ingress tests. |
| `docs/edge-ingress-spec-v3.md` | spec, coherent with A+Z + SY.edge naming. |
| `Cargo.toml` | `hyper 1` + `hyper-util 0.1` (edge TLS serve loop). |
| `src/router/mod.rs` | **untouched** (Z needs no router change). |

### 1.3 Validated on the lab
- **Same-hive**: edge + `AI.test.gov` on motherbee → `curl POST /e/demo` → HTTP 200,
  handler saw `src_ilk = <edge a-fuego ilk>`.
- **Cross-hive** (real DMZ shape): edge on `ingress1` → `Unicast(AI.test.gov@motherbee)`
  over WAN → handler → reply back → HTTP 200. The reply-by-`routing.src` (uuid) routes back
  cross-hive because the router rewrites/floods node UUIDs into LSA (works for the **direct
  WAN neighbour** — see audit H4).

---

## 2. AUDIT FINDINGS (deep, adversarially verified)

> Coverage caveat: the audit ran as a 5-dimension workflow; **2 dimensions flaked**
> (edge-correctness died, edge-security returned a placeholder). The synthesis manually
> covered the #1 security question and reviewed the request path; #1 was re-verified by hand
> against code. So edge-correctness is only partially covered — a clean re-run of those two
> dimensions is advisable.

### ⚠️ #1 — CONFIG_CHANGED authz on the endpoint table (SECURITY, verified by hand)
The message that **replaces the edge's entire public routing table** is a
`SYSTEM`/`CONFIG_CHANGED`. Two facts combine:
1. **`sy_edge` does ZERO sender verification** — the system loop
   (`sy_edge.rs:259-283`) accepts any `CONFIG_CHANGED` with a matching subsystem +
   `version > applied_version` and swaps the registry. No origin / `node_name==self` check.
2. **The router's origin-authority allowlist does not cover `CONFIG_CHANGED`** —
   `PROTECTED_SYSTEM_ACTIONS` (`src/router/system_policy.rs`) contains `NODE_CONFIG_SET`
   but not `CONFIG_CHANGED`; the orchestrator even emits it with `src_l2_name: None`
   (`sy_orchestrator.rs:9861`).

**Delivery nuance (found while verifying):** the orchestrator emits `CONFIG_CHANGED` as
`Unicast(node_name)` (`sy_orchestrator.rs:9858-9888`). The router **swallows** a *peer-received*
`CONFIG_CHANGED` (`mod.rs:3578 return Ok(())`) — so a **direct cross-hive** push to the edge
never arrives. The correct/authenticated path is `set_node_config` (a protected admin action)
→ the target hive's orchestrator → a **local** `CONFIG_CHANGED` → the edge. **This means the
hot-swap is currently UNVALIDATED live**, and if delivered it is unauthenticated.

**Fix direction (agreed):** route the endpoint push through the admin command plane
(`set_node_config` substrate) so authz is inherited, AND add an edge-side origin gate
(`src_l2_name == SY.orchestrator@<edge-hive>` + `node_name==self`). See §4 (the
`publish_endpoint` command is the productized form of this).

### HIGH
- **H1 — `sy-edge` is never packaged.** `packaging/build-deb.sh`, `scripts/install.sh` omit
  it (these were reverted during the http.req cleanup). `add_ingress`'s manifest gate
  (`sy_orchestrator.rs:17677`) returns `MANIFEST_INVALID`; even bypassed there is no binary to
  push and `sy-edge.service` ExecStart points nowhere. **Not deployable from scratch.**
- **H2 — no ingress `system_nodes` template.** No shipped `hive.yaml` defines
  `system_nodes.ingress` (`config/hive.yaml`, `packaging/hive.yaml.example`);
  `system_nodes_for_role(_, Ingress)` → `Err → CONFIG_FAILED` before provisioning. The lab
  used a hand-edited yaml not in the repo.
- **H3 — TLS never wired → plaintext public door.** The generated `edge:` block writes only
  `listen` + `endpoints_path`; `IngressSection` has no TLS field, so `tls_requested=false` and
  `run_frontend` binds cleartext. A from-config `add_ingress` serves all public traffic —
  including bearer tokens — in the clear.
- **H4 — cross-hive is single-WAN-hop only.** LSA propagation is one-hop; a handler on any hive
  that is not the edge's direct WAN neighbour (the hub) is unreachable on both legs
  (NODE_NOT_FOUND → 502). Spec §6 labels multi-hop *deferred*, so this is arguably scoping —
  but must be stated: **ingress cross-hive works today only to the hub or same-hive.**

### MEDIUM
- **M1 — endpoints seed written unvalidated → silent all-404 edge.** Operator `endpoints_json`
  is written verbatim (`sy_orchestrator.rs:17779`); `EndpointRow` now requires
  `handler_node`+`inbound_family` (no defaults), so one malformed row → empty map → every
  `/e/<hash>` 404s, while `add_ingress` reports ok.
- **M2 — header denylist where spec requires allowlist.** `filter_request_headers`
  (`sy_edge.rs`) forwards every client header except a fixed stripped set; spec §12 (marked
  "not deferred") wants an allowlist. Client can inject `X-Forwarded-For`/`X-*` inward.
- **M3 — public edge inherits blanket system-node reachability.** `vpn_allows` returns true for
  any system node, so SY.edge (VPN 0) can address any node/VPN; `add_ingress` writes an empty
  `sy-config-routes.yaml`. `system_policy` still shields the 18 protected actions, but a
  compromised edge can emit arbitrary non-protected application messages cross-VPN. Spec §12
  says constrain before alpha. **Amplifies #1.**
- **M4 — return-leg silent 504 under LSA staleness** + a misleading comment (`sy_edge.rs:821`
  says "by name", routes by uuid). If the edge UUID ages out mid-flight, the handler's reply
  hits NODE_NOT_FOUND, the UNREACHABLE goes to the *handler*, and the edge blocks the full 30s →
  `HANDLER_TIMEOUT`, masking the cause.

### LOW / nit
- **L1** — pending map has timeout eviction but no concurrency cap; spec §12 wants bounded/
  admission-controlled "from day one" (add an `Arc<Semaphore>`/tower limit → 503).
- **L2** — reply matcher `first-frame-wins`: `RouteMatch::Any` resolves on the first frame
  with the trace_id; an ack-then-result handler returns the ack as the 200 body. Latent (no
  multi-frame handler today).
- **L3** — WS/SSE upgrade not 501'd (spec §3.3 promises it); a WS handshake is stripped and
  forwarded as an ordinary request.
- **L4** — rate-limit overclaimed in the spec (§2 "Does", §4 `rate_policy`) — neither exists.
- **nit** — §14 checklist entirely unchecked despite much done; line-293 item obsolete under Z.

### Rejected (noted)
- `wan-inbound-reply-silent-drop` (`mod.rs:2838`) — real unlogged drop but self-preventing;
  observability nit (add a `tracing::warn!`).

---

## 3. WHAT'S MISSING TO FINISH ALPHA

**(a) Blockers (needed for alpha):**
1. Authorize the endpoint-table push (#1) — the `publish_endpoint` command (§4).
2. Package `sy-edge` + systemd unit into the manifest (H1).
3. Ship an ingress `system_nodes` template (H2).
4. Wire public-door TLS end-to-end (H3).
5. Validate the endpoints seed at provisioning time (M1).
6. Header allowlist (M2, spec "not deferred").
7. Scope down the public edge's mesh reachability (M3, spec "not deferred").
8. Bounded/admission-controlled pending map (L1, spec "day one").
9. Decide + document cross-hive topology limit (H4).

**(b) Deferred by design (spec-labeled):** EDGE_REGISTER self-publication by the node itself,
lease/refresh, blob egress (§8), multi-hop cross-hive (§6), JWT/ACME, §11 control channel,
streaming/upgrade (only the 501 stub is arguably alpha, L3).

---

## 4. DESIGN THREAD A — `publish_endpoint` as a fluxbee operational command

**Origin of the idea (user):** publishing an endpoint is NOT an IO-node tool; it is a fluxbee
**command managed by SY.architect / the user**, one more among all the admin commands. And it
is an **OPERATION**, distinct from `set/get node config` (which manages a node's own config).

### 4.1 Why it fits (verified against code)
- Archi routes **every mutation through `fluxbee_plan_compiler`** (`sy_architect.rs:114`); a
  user says "publish this endpoint" in chat → AI compiles a 1-step plan → user `CONFIRM` →
  `executor_execute_plan` → `SY.admin` → orchestrator `handle_admin`. So a new command is added
  as an **SY.admin action + orchestrator flow** — *no new AI tool needed*.
- **Why OPERATION, not config:** `set_node_config` writes one node's own `config.json` scalars.
  `publish_endpoint` registers a **cross-node capability** — binds a public hash to *another*
  node's `{ilk, handler_node}`, needs a **publish-time identity resolve** (like `vault_put`'s
  `owner_node→ilk`, `sy_admin.rs:10885`), and its source of truth is a **master table owned by
  the authority**, not the edge's config. The `CONFIG_CHANGED` wire push is only the delivery
  transport. This is structurally identical to why `vault_put` is an operation, not a config write.

### 4.2 Naming (decided): `publish_endpoint` / `unpublish_endpoint` / `list_endpoints`
Follows the codebase verb_noun operation convention (`publish_runtime_package`, `create_tenant`,
`run_node`, `list_nodes`).

### 4.3 Command shapes (proposal)
```
publish_endpoint   {edge_node, owner_node, inbound_family, auth_mode, secret?, methods?, tenant_id?}
                   → {hash, public_url:"/e/<hash>", ilk, handler_node, version}
unpublish_endpoint {edge_node, hash} → {removed, version}
list_endpoints     {edge_node} → {endpoints:[...], version}   (read-only, like list_nodes)
```
Admin resolves `owner_node → {ilk, handler_node}` and mints the opaque `hash`; the caller never
supplies ilk or hash. Field shapes match the existing `EndpointRow` verbatim.

### 4.4 Full flow (proposal)
```
user chat → archi → fluxbee_plan_compiler → 1-step plan {action:"publish_endpoint", target:edge_node}
  → CONFIRM → executor_execute_plan → SY.admin@<hive>
     ├─ admin_origin_authorized gate (sy_admin.rs:2583/2637)               ← AUTHENTICATED
     ├─ resolve owner_node → {ilk, handler_node} (Option Z publish-time resolve;
     │     reuse normalize_vault_put_payload resolver, sy_admin.rs:10885) + mint hash
     └─ forward to SY.orchestrator@<ingress-hive> (forward_system_action_to_hive, sy_orch:11281)
  → orchestrator publish_endpoint_flow (NEW, next to set_node_config_flow:12541):
     1. load master table (see §4.5)
     2. apply delta (publish=insert / unpublish=remove) + bump monotonic version + atomic write
     3. push FULL row set via send_node_config_changed_signal (9852), subsystem="endpoints"
  → SY.edge system loop: subsystem match → version gate → rows_to_registry FULL replace
```

### 4.5 Master table (what §2's "where does it live" means)
The edge does a **whole-map replace** on each push, so **the authority must persist the complete
set of all published endpoints** and re-emit the whole table on every delta. That authoritative
complete set = the **master table**; the edge is only a cache. **Where:** recommended
`state_dir/edge/<edge_node>/endpoints.json` on the **ingress-hive orchestrator** (colocated with
SY.edge state → local `CONFIG_CHANGED` delivery, avoids the peer-swallow of §2). Alternative:
motherbee orchestrator holds it and forwards the whole table. **OPEN — user to decide.**

### 4.6 How this resolves #1 (three layers)
1. **Router**: add `EDGE_PUBLISH`/`EDGE_UNPUBLISH`/`EDGE_LIST` to `PROTECTED_SYSTEM_ACTIONS`
   (`system_policy.rs`) → only `SY.admin`/`SY.orchestrator` can drive them cross-hive.
2. **Admin door**: `admin_origin_authorized` restricts who invokes `publish_endpoint`.
3. **Edge-side origin gate**: the edge accepts an `endpoints` replace only from
   `SY.orchestrator@<edge-hive>` (requires the orchestrator to stop sending `src_l2_name: None`,
   `sy_orchestrator.rs:9861`). This is the layer that directly closes #1.

### 4.7 Files to add/touch (proposal checklist)
- `sy_admin.rs`: add actions to `INTERNAL_ADMIN_ACTIONS` (~78/99) + `INTERNAL_ACTION_REGISTRY`
  (~2941) + `ADMIN_EXECUTOR_PILOT_ACTIONS` (~67) + mutating list (~79-101); an admin normalize
  step (owner_node resolve + hash mint), factoring `normalize_vault_put_payload:10885` into a
  shared `resolve_owner_node`; action-help arms (~7104/7209/7383/7487/7808/8336).
- `sy_orchestrator.rs`: 3 `handle_admin` arms (~2000) + `publish/unpublish/list_endpoints_flow`
  (master-table load/persist, version bump ~12740, atomic write ~12813, cross-hive forward
  ~11281, push via a generalized `send_node_config_changed_signal` ~9852 that takes a `subsystem`
  and a real `src_l2_name`).
- `sy_edge.rs`: the origin check on `CONFIG_CHANGED` (the one required hardening; loop ~259-283).
- `src/router/system_policy.rs`: add the 3 `EDGE_*` to `PROTECTED_SYSTEM_ACTIONS` (~25) + fix the
  length assertion in the unit test (~156).

### 4.8 Open questions (thread A)
1. **Master table home** — ingress orchestrator (rec) vs motherbee (§4.5).
2. **Hash minting** — admin-computed opaque vs caller vanity slug; collision handling (the
   current `add_hive endpoints_json` path near `sy_orchestrator.rs:180` hasn't been read for
   the mint algorithm).
3. **`src_l2_name` mechanism** — does the router re-stamp an authoritative src on delivered
   `CONFIG_CHANGED`, or must the orchestrator set it (changing `:9861`)? The edge gate depends
   on it being un-forgeable.
4. **Archi surface** — rely on `fluxbee_plan_compiler` auto-discovery (canonical), or add a
   first-class `fluxbee_publish_endpoint` FunctionTool for UX? (rec: defer.)
5. **Lease** — decided: persist until `unpublish` (no TTL for now).

---

## 5. DESIGN THREAD B — the edge's OWN config surface (DNS / NIC / params)

Separate from A. The edge's own parameters are **config** (managed via `set/get_node_config`,
the generic mechanism), NOT an operation. The user's examples: **DNS** (public URL / ACME) and
**NIC / public-IP** ("lo que viene de la placa").

### 5.1 Current edge config (`EdgeSection` in `sy_edge.rs`)
`listen`, `endpoints_path`, `tls_cert`, `tls_key`, `tls_vault_key`, `vault_hive`.
Env knobs: `NODE_NAME`, `NODE_VERSION`, `ROUTER_SOCKET`, `UUID_PERSISTENCE_DIR`, `TTL`,
`HANDLER_TIMEOUT_MS`, `SY_EDGE_HTTP_LISTEN`, `SY_EDGE_ENDPOINTS`, `SY_EDGE_TLS_*`,
`SY_EDGE_VAULT_HIVE`, `CONFIG_DIR`.

### 5.2 Missing (per the DNS/NIC point + spec §9 `edge.conf`)
- `expected_public_ip_cidr` — the NIC / public-IP the edge binds/validates.
- **DNS** config — public URL / ACME management.
- `log_level`, `edge_id`.

### 5.3 The gap
Today the edge reads its config **only at BOOT** (`Config::load`). The only thing it applies live
is the endpoint table (the hot-swap). To make DNS/NIC/listen settable via `set_node_config`
without a reinstall, the edge needs a **`config` subsystem handler** (small, like `endpoints`).
Caveat: rebinding `:443` / changing NIC likely still needs a restart; DNS is more dynamic.

### 5.4 Open questions (thread B)
1. Which params are dynamically-settable-live vs restart-required?
2. Does DNS management live on the edge (ACME on the internet NIC) or at core (the spec leans
   ACME+DNS as the only genuinely-external deps, deferred for alpha with the wildcard cert)?
3. Is thread B alpha or post-alpha? (The wildcard cert sidesteps ACME/DNS for now.)

---

## 6. DECISIONS + OPEN QUESTIONS (consolidated)

**Decided this session:**
- INGRESS = `SY.edge`; Option A + Z; router untouched.
- Naming: `publish_endpoint` / `unpublish_endpoint` / `list_endpoints`.
- Lease: persist until unpublish (no TTL now).
- The publish path goes through the admin command plane (dissolves #1).
- The "only edge/orch/router" constraint is **revoked** — admin/system_policy/architect changes
  are now in scope (the command belongs there by design).

**Open (for the spec):**
- Master table home (ingress vs motherbee).
- Hash minting algorithm + collision handling.
- `src_l2_name` authoritative-stamp mechanism (router vs orchestrator).
- Thread B scope (edge config surface: DNS/NIC) — alpha or later.
- Cross-hive topology limit (H4): declare "adjacent-hub only" for alpha, or build relay.
- Whether to fold the audit MEDIUM/LOW fixes (M2/M3/L1) into the same pass.

---

## 7. PROPOSED BUILD ORDER (options, not a decision)

- **Option 1 — validate then build:** (1) live-validate #1/hot-swap with the existing
  `set_node_config` (push `{endpoints:[...]}`, confirm the edge applies it); (2) build thread A
  (`publish_endpoint`) → closes #1; (3) H1+H2 (deployable from scratch); (4) H3 (TLS);
  (5) M1/M2/M3/L1 (finish non-deferred alpha surface); (6) thread B (edge config); (7) spec cleanup.
- **Option 2 — deployable first:** H1+H2+H3 first (get a real from-scratch TLS ingress), then A,
  then the rest.

Both are viable; the spec should pick the order and the alpha scope line.

---

## 8. REFERENCE — key file:line anchors

- Edge forward + reply: `src/bin/sy_edge.rs` invoke (~716-885), hot-swap loop (~259-283),
  EndpointRow (~485), rows_to_registry (~523), fail-closed TLS (~205-245).
- add_ingress: `src/bin/sy_orchestrator.rs:17515` (flow), `:180` (IngressSection),
  set_node_config_flow `:12541`, send_node_config_changed_signal `:9852-9888`.
- Admin: `src/bin/sy_admin.rs` ADMIN_ACTIONS (~68-100), INTERNAL_ACTION_REGISTRY (~2941),
  handle_admin_command (~2251/10300), admin_origin_authorized (~2583/2637),
  normalize_vault_put_payload (~10885), action-help match blocks (~7104+).
- Archi: `src/bin/sy_architect.rs` register_tools (~920), plan_compiler (~4376),
  execute_executor_plan_with_context (~12974), system-prompt mutation rule (~114).
- Router authz: `src/router/system_policy.rs` PROTECTED_SYSTEM_ACTIONS (~25), authority (~69).
- Router CONFIG_CHANGED handling: `src/router/mod.rs:747` (local), `:3561/3578` (peer swallow).
- Spec: `docs/edge-ingress-spec-v3.md`.
