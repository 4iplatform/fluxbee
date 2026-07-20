# WAN multi-hop reachability plane (Option B) — spec v1

**Status:** design LOCKED (user-approved 2026-07-20), not yet built.
**Closes:** EDGE-H4 (ingress→worker single WAN hop) for the star topology.
**Related:** `edge-ingress-spec-v6.md` (H4), `docs/onworking COA/opa-dual.md`, `src/router/system_policy.rs`, `policy/system.rego`.

## 1. Problem

The WAN control plane is single-hop by construction (verified in code):

- LSA is **non-transitive**: `build_local_lsa_payload` advertises only the local hive's nodes
  (`nodes` + LAN `peer_nodes`); the hub never re-advertises a spoke's nodes to another spoke
  (`src/router/mod.rs:1892-1919`).
- `apply_lsa_payload` rejects any LSA whose `payload.hive != authenticated-peer hive`
  (`LSA_REJECT_HIVE_MISMATCH`, `mod.rs:2106`) — the EDGE-01 bucket-binding security invariant. This
  is precisely what forbids the hub from relaying a foreign hive's LSA.

Two gates therefore force one hop, both rooted in "LSAs are adjacency-only":

1. **Origination gate** — a spoke cannot resolve a non-adjacent hive's node ⇒ `NODE_NOT_FOUND`
   before any WAN send (`resolve_by_name_inner`, `mod.rs:5523`; root cause `mod.rs:1892`).
2. **Terminal gate** — a relayed frame reaching a non-adjacent hive carries `src=<origin spoke>`,
   which the receiver cannot resolve, so `handle_wan_message` silently drops it (`mod.rs:2858`).

Topology reality (verified): the router on EVERY hive is named `RT.gateway@<hive>`, so
`is_gateway_name` makes **every router a gateway** (`src/config/mod.rs:352-356`). `forward_to_hive`'s
gateway branch therefore does `send_to_wan_peer(target_hive)` which requires a **direct** WAN session
to that exact hive (`mod.rs:1764-1801`); a spoke has no direct session to another spoke, so it emits
`WAN_UNAVAILABLE` — **the data-plane does NOT hub-relay today**. Option B must add **next-hop
forwarding**: a reachability entry points `next_hop` at the vouching hub (which IS a direct WAN peer),
so `forward_to_hive(next_hop)` succeeds and the hub re-resolves the unchanged `dst` onward to the
target hive (which the hub knows directly). Both `NODE_NOT_FOUND` (origination) and the silent
terminal drop are then closed, in both directions.

## 2. Design: hub-authored reachability plane, distinct from the identity-bearing LSA

Separate three things that are conflated today: **data** (which node lives on which hive),
**mechanism** (transport + forwarding), and **rules** (who may vouch, what a vouch grants).

### 2.1 Mechanism (router code)

- New message `MSG_WAN_REACHABILITY` authored by the **hub in its own authenticated bucket** (so it
  does NOT violate EDGE-01). Payload: `{ origin_hive, router_id, seq, timestamp, entries: [{ uuid,
  name, hive_id, vpn_id }] }`, one entry per spoke node the hub knows, with `hive_id` = the node's
  **origin** hive (the spoke it lives on, NOT the hub).
- The hub emits it alongside the LSA beacon (`mod.rs:2370-2375`), built from its `lsa_state`
  (all direct spokes), with **split-horizon**: a spoke is never sent its own nodes; each other
  spoke receives the union of the rest.
- Each spoke ingests into a **new `reachability` table** (`Arc<Mutex<HashMap<Uuid,
  ReachabilityEntry>>>`), kept SEPARATE from `lsa_state`/the LSA SHM snapshot so the
  identity-bearing snapshot is never polluted. Each `ReachabilityEntry` carries `{ name,
  origin_hive, next_hop_hive, vpn_id, last_seq, last_updated }` where `next_hop_hive` = the vouching
  hub (a DIRECT WAN peer of this spoke); entries are implicitly `via_hub` (transitively learned, not
  directly authenticated).
- Resolution consults the reachability table as a **fallback** after `nodes`/`peer_nodes`/`lsa`:
  - by NAME: `rebuild_fib` adds a `FibSource::ReachableNode` entry `{pattern: name, next_hop:
    Hive(next_hop_hive)}` at a higher `admin_distance` than LSA, so `resolve_by_name` yields
    `ForwardHive(next_hop_hive=hub)` — a direct peer — closing the origination gate.
  - by UUID: `find_reachable_node(reachability, uuid)` returns `RemoteNodeInfo { name,
    hive_id: origin_hive, next_hop_hive, via_hub: true }`; UUID-addressed forwards use
    `next_hop_hive`, while the canonical/authority binding uses `hive_id` (origin) + `via_hub`.
  Each hop re-resolves the unchanged `dst`; the hub, knowing the target directly, forwards onward.
- Terminal gate (`mod.rs:2858`): a frame whose `src` resolves only via the reachability table is
  **admitted for delivery** instead of dropped (terminal gate closed), carrying the `via_hub` flag
  to the authority decision.

### 2.2 Rules (OPA-system — `policy/system.rego`, new entrypoints; Rust fallback shadow-verified)

- `fluxbee/system/wan_reachability_admit` — may this authenticated peer VOUCH foreign reachability?
  Rule: only the primary hub (the gateway on `motherbee`). Replaces the hardcoded
  `LSA_REJECT_HIVE_MISMATCH` intent for the reachability plane with an explicit policy decision.
- `fluxbee/system/wan_src_admit` — is a hub-vouched `src` admissible for **delivery**? Rule: yes
  for the data plane.

Rust `system_policy` gains twin fallback fns (byte-identical, shadow-verified like `authority()`).
The wasm recompile (`sy-opa-rules compile-file policy/system.rego <entrypoint> policy/system.wasm`,
Linux-only Go tool on the build box) is a follow step; the Rust fallback is the immediate source of
truth per OPA-dual.

### 2.3 The security invariant (non-negotiable)

**Data-plane reachability is relaxed; SYSTEM authority stays strict and non-transitive.**

A `src` resolved via the reachability table (`directly_authenticated = false`) is admitted for
DELIVERY but is **denied SYSTEM authority**: `serialize_for_local_delivery` forces
`authorize_system = false` for any protected SYSTEM action when the frame was hub-vouched, WITHOUT
consulting `authority()`/`fluxbee/system/allow`. So a transitively-learned `SY.orchestrator@worker1`
gets no cross-hive control-plane power at another spoke. Consequence: a compromised hub can misroute
or deliver DATA (it is the gateway — it already could) but cannot FABRICATE SYSTEM authority between
spokes. The existing EDGE-01 canonicalization and the strict `allow` entrypoint are unchanged; the
hub-vouched path is simply excluded from authority.

## 3. Scope of this pass

- Build the reachability plane + resolution + terminal-gate relaxation + the authority exclusion.
- Add the two new `system.rego` entrypoints + Rust fallbacks + shadow-verify tests for B only.
- Do NOT migrate the pre-existing scattered rules (EDGE-01 `canonical_wan_src_name`,
  `vpn_allows_between`) into OPA-system yet — documented as future work (OPA-dual continuation).

## 4. Validation

- Unit: resolution via reachability yields `ForwardHive`; terminal admit for hub-vouched src;
  authority denied for hub-vouched protected action; split-horizon; loop/seq dedup.
- Lab (Proxmox): `IO.api` on worker1 published behind motherbee → `POST /e/<ich>` from ingress1
  Edge returns HTTP 200 (today: 502 `HANDLER_UNREACHABLE`), reply returns, and a spoofed
  `SY.orchestrator@worker1` relayed transitively is still denied a protected SYSTEM action.
