# Routing and policy: reference

**Status:** the code on 2026-10-17 (0.1.79).
**Audience:** whoever writes routes, VPN rules or the user policy, or has to explain why a message
went where it went.

This page describes how a router delivers a message and which policies decide on the way. Every
statement names the code that backs it. `docs/history/04-routing.md` and `docs/history/06-regiones.md` (Spanish)
are older and partly wrong; where they disagree with this page, the code and this page win.

Main sources: `src/router/mod.rs` (the router), `src/router/system_policy.rs` and
`policy/system.rego` (the system policy), `src/opa.rs` (the OPA evaluator), `go/sy-opa-rules/`
(compiles and installs the user policy), `src/bin/sy_config_routes.rs` (routes, VPNs, taps),
`src/bin/sy_admin.rs` (the HTTP API), `crates/fluxbee_sdk/src/protocol.rs` (message shapes). The
shared-memory regions named here are described in [shared-memory.md](shared-memory.md); the
envelope in [protocol.md](protocol.md).

---

## 1. Basics

- **Node name.** `role@hive`, for example `AI.support.l1@motherbee`. A HELLO name without `@` gets
  `@<hive>` appended (`normalize_name`). Routing compares names as exact strings.
- **Message.** `routing {src, src_l2_name, dst, ttl, trace_id}`, `meta`, `payload` (`Message`,
  `Routing`, `Meta`). `routing.src` is the sender's UUID. On the wire `dst` is `null` (Resolve),
  `"broadcast"` (Broadcast) or any other string (Unicast: a UUID or a name) (`deserialize_dst`).
- **Routers.** A router is `RT.<name>@<hive>`. Its name comes from `JSR_ROUTER_NAME`, by default
  `wan.gateway_name` of `hive.yaml`, itself `RT.gateway` by default (`RouterConfig::load_from_env`,
  `src/config/mod.rs`). The router whose name before `@` equals `wan.gateway_name` is the
  **gateway** (`is_gateway_name`): only it opens WAN links, writes the LSA region and advertises
  the hive to the other hives.
- **One router per hive** is the supported topology, and that router is the gateway: the package
  ships one `rt-gateway` unit, the orchestrator starts and watches only that one, and SY.admin
  refuses `add_router` and `delete_router` ([architecture.md](architecture.md) §4.2). The code
  still has a mode with several routers in a hive, which is not supported. What this page says
  about sibling routers and IRP describes that mode; §10 lists its known defects.
- **Links.**
  - Nodes connect to `/var/run/fluxbee/routers/<uuid>.sock` (0600; the router UUID, no dashes).
    The router serves a connection only when the peer's uid (`SO_PEERCRED`) is its own, root in
    production; anyone else is refused with the warning `router refused a local connection`
    (`listen_local_socket`, `local_peer_admitted`). Every unit, managed node and Go node connects as
    root. The check also covers the moment between bind and chmod.
  - In the several-routers mode, the routers of one hive talk over IRP, `irp-<uuid>.sock` (0600,
    the same peer check) in the same directory. Every 5 s each router scans `/dev/shm` for router
    regions of its hive with a fresh heartbeat (`peer_discovery_loop`, `discover_peer_regions`); the
    router with the lower UUID dials the other (`should_initiate_peer`). A region counts only when
    the router's own user owns it and no group or world write bit is set, checked on the open
    descriptor (`open_read_only_owned_by`); another is skipped with the warning `router skipped a
    shared-memory region another user owns or may write`, since its nodes would enter the FIB.
  - Gateways talk over TCP (`wan.listen`, `wan.uplinks`). A gateway sends its LSA every 10 s and
    marks a hive stale after 40 s without one (`DEFAULT_HELLO_INTERVAL_MS`,
    `DEFAULT_DEAD_INTERVAL_MS`; fixed, not read from `hive.yaml`).

---

## 2. The sender's name is stamped by the router

Every decision below that depends on who sent a message uses `routing.src_l2_name` as the router
wrote it, never as the sender wrote it.

- **From a local node.**
  - The first frame must be a system `HELLO` whose `payload.uuid` equals `routing.src`.
  - An `RT.*` name is accepted only from the router's own process (peer PID, `rt_name_allowed`).
  - One live connection per UUID: a HELLO for a UUID that is still connected waits up to 1 s
    (`DUPLICATE_HELLO_GRACE`) and is then refused (`claim_node_uuid`).
  - The router answers `ANNOUNCE {uuid, name, status: "registered", vpn_id, router_name}`.
  - From then on, a frame whose `routing.src` is not the connection's UUID is dropped
    (`frame_is_from_connection`), and `src_l2_name` is overwritten with the registered name
    (`handle_message`).
- **Over IRP.** The name comes from the sibling router's region (`peer_nodes`). A sibling that is
  the gateway may hand over a node of another hive: then the name comes from the LSA, made
  canonical as below (`remote_source_from_gateway`). An unknown sender is dropped.
- **Over the WAN (EDGE-01).**
  - The sender is bound to the session. Its UUID is looked up only in the LSA bucket of the peer
    that sent the frame, then, for a frame the hub relays, among the nodes the hub vouches for, and
    only when that peer is the vouching hub (`wan_frame_source`; such a sender is `via_hub`).
    Anything else, a node of another hive sent by its UUID included, is dropped, counted as
    `foreign_source` and logged, at most every 30 s, as `router dropped a WAN frame: its sender is
    neither a node of the peer that sent it nor one that peer relays as the hub`. Until 0.1.83 the
    UUID was looked up in every bucket, so a spoke could send as another spoke's node.
  - The name becomes `<role>@<that bucket's hive>` (`canonical_wan_src_name`), so a peer can only
    speak as `role@<itself>`, or, as the hub, as the nodes it relays (data only, §6.3).
  - A peer fills only its own LSA bucket: `apply_lsa_payload` rejects an LSA whose `hive` is not the
    hive of the peer's WAN HELLO (`hive_mismatch`). With mTLS (`wan.mtls: required`, or
    `permissive` with a TLS peer) that HELLO hive must equal the hive in the peer's certificate
    (`handle_wan_connection`). The loader's default for `wan.mtls` is `disabled`; then the bucket
    rests on the HELLO alone.
  - A peer advertises only its own nodes: from hive P's LSA the router installs only the nodes
    named `<role>@P`, split at the first `@`. Any other name is dropped, counted under
    `foreign_node` and logged as `lsa nodes dropped` (`apply_lsa_payload`, `node_is_on_hive`); the
    rest of the LSA applies. What P's routes and taps may reach is in §3.2 and §8.3.
  - Nor may a peer take a UUID that is already a node's: what is addressed to a UUID, replies above
    all, would go to it. A node listed under the UUID of one of this hive's nodes is dropped. One
    listed under a UUID another hive lists is kept, and that UUID then resolves to neither hive
    (§3.1), whichever LSA came first: a duplicate claim can make a UUID unreachable, never
    redirect it. Both count as `duplicate_uuid` and are logged as `lsa nodes with a UUID already
    taken`. The hub vouches for no UUID that two hives list (`broadcast_reachability`), and a spoke
    drops a vouched UUID that is already taken, by one of its nodes or a node of an LSA, or that
    the hub lists twice (`apply_wan_reachability`, counted the same way).
  - The motherbee admits a hive that connects to it only while its hive registry lists it, the
    directory `/var/lib/fluxbee/hives/<hive>/` that `add_hive` creates and `remove_hive` deletes
    (FINDINGS A-48). Any other hive gets `WAN_REJECT` with `HIVE_NOT_REGISTERED`, and an admitted
    session closes within 10 s of its directory going away (`wan_peer_admission`,
    `hive_unregistered`). The other hives keep no registry, so this check does not apply to them.

Tests: `router_overwrites_spoofed_src_l2_name`,
`edge01_wan_canonicalization_defeats_cross_hive_name_forgery`,
`the_motherbee_drops_a_spokes_frame_sent_as_another_spokes_node`,
`a_hive_cannot_take_another_nodes_uuid_whichever_lsa_comes_first`.

---

## 3. Unicast

### 3.1 To a UUID

When `dst` parses as a UUID the router looks it up directly, in this order: a node on this router,
a node on a sibling router, a node in the LSA (another hive), a hub-vouched node. Nothing found:
`UNREACHABLE NODE_NOT_FOUND` (`handle_message`). Routes are not consulted. A UUID that two hives
of the LSA list, this one or a stale one included, resolves to neither (`find_remote_node`, the
warning `router refused a UUID that several hives list`, at most every 30 s).

### 3.2 To a name: the FIB

Names resolve through the FIB, a sorted list the router rebuilds whenever its nodes, its siblings,
the config region or the LSA change (`rebuild_fib`).

| Source (`FibSource`) | Comes from | Match | Next hop | Admin distance |
| --- | --- | --- | --- | --- |
| `LocalNode` | nodes connected to this router | EXACT name | this router | 0 |
| `PeerNode` | the sibling routers' regions | EXACT name | that router, over IRP | 0 |
| `StaticRoute` | this hive's `/jsr-config-<hive>` | the route's kind | `next_hop_hive`, or none | 1 |
| `LsaNode` | the LSA region: other hives' nodes, each named `<role>@<its hive>` (§2) | EXACT name | that hive | 2 |
| `LsaRoute` | the LSA region: other hives' routes, for the advertising hive's names and `name@*` only | the route's kind | `next_hop_hive`, else the hive that advertised it | 2 |
| `ReachableNode` | nodes the hub vouches for (on a spoke's gateway) | EXACT name | the hub | 3 |

**Order.** Most specific first; then admin distance, `priority`, `metric` and `installed_at`, all
ascending. Specificity is `(literal length, kind)` (`fib_specificity`): EXACT counts the whole
pattern, PREFIX the pattern without a trailing `.*`, GLOB the characters that are not `*`. At equal
length EXACT beats PREFIX beats GLOB.

**Match kinds** (`pattern_match_kind`):

- EXACT: string equality.
- PREFIX: strips a trailing `.*` and tests `starts_with`. There is no dot boundary: `AI.sales.*`
  also matches `AI.salesforce@motherbee`.
- GLOB: `*` matches any run of characters, `.` and `@` included (`wildcard_match`).

**Lookup.** The first entry that matches decides (`resolve_by_name_inner`):

- a node entry: the VPN check (§8.2); allowed, deliver here, hand to the sibling or forward to the
  hive; denied, `UNREACHABLE VPN_BLOCKED`, and the search stops;
- a route of another hive for a destination it does not apply to (below): skipped, the search
  goes on;
- a route with `DROP`: the message is discarded and the sender is not told;
- a route with `FORWARD` and a next-hop hive: forward to that hive;
- a route with `FORWARD` and no next hop: skipped, the search goes on;
- no entry: `UNREACHABLE NODE_NOT_FOUND`.

**What this means for routes.**

- A route never wins over a node of this hive that the FIB knows by name. The node's exact entry is
  at least as long as anything a route can match, and at equal specificity distance 0 beats 1. A
  `DROP` on `AI.internal.*` does not stop messages to a connected `AI.internal.db@motherbee`; it
  drops names that nobody serves, which would otherwise get `NODE_NOT_FOUND`.
- For a node of another hive, only an EXACT route on its full name wins (static 1 beats LSA 2).
- This hive's static routes apply to any name.
- Routes of another hive apply here only to that hive's names, `<role>@<that hive>`, and to a
  `name@*` destination (§3.3) (`LsaRoute` carries the advertising hive; `lsa_route_applies`). So a
  hive can neither redirect nor drop the traffic of another hive's nodes: a spoke's `DROP` on
  `SY.` or its `FORWARD` on `*` does nothing to `SY.admin@motherbee`. There an empty
  `next_hop_hive` means "to the hive that advertised it", while on its own hive a `FORWARD` without
  next hop does nothing.
- Routes never apply to a destination given as a UUID (§3.1).

### 3.3 Any hive: `name@*`

A destination ending in `@*` is tried first as `name@<sender's hive>`; any result other than
`NODE_NOT_FOUND` is final, a `VPN_BLOCKED` included. Otherwise the router takes the first FIB entry
whose node name before `@` equals `name` (a `*` inside `name` works as a wildcard), or whose route
pattern matches the literal string `name@*` (`resolve_by_name`, `resolve_by_name_any_hive`). The
pick follows FIB order: the longer full name first, then admin distance; not the nearest hive. In
the first step another hive's routes count only for a name of their own hive (§3.2); in the second
they count like this hive's.

### 3.4 Handing a message to another router

- **Sibling (IRP):** `send_to_peer_router`. No link to it: `PEER_UNAVAILABLE`.
- **Another hive** (`forward_to_hive`): on the gateway, the WAN link to that hive (`wan_peers`,
  keyed by hive), else `WAN_UNAVAILABLE`; on any other router, the sibling that is the gateway, over
  IRP, else `GATEWAY_UNAVAILABLE`.
- The message travels unchanged and the next router decides again with its own tables. A Resolve
  keeps `dst: null` (§5).
- A gateway that receives from the WAN a message for a node of a third hive forwards it again
  (`handle_wan_message`): that is how a hub relays between spokes.

### 3.5 TTL

- A message that arrives with `ttl == 0` gets `TTL_EXCEEDED` back.
- Local delivery does not touch the TTL. Every hand-over to another router, IRP or WAN, unicast or
  broadcast, takes one off (`send_to_peer_router`, `send_to_wan_peer`, `broadcast_to_peers`,
  `broadcast_to_wan`).
- Before handing a unicast to another router: `ttl <= 1` gives `TTL_EXCEEDED`, since it would
  arrive with 0. A message needs `ttl >= 2` to reach a sibling's node, and one more per extra hop.
- The SDK builds requests with `ttl: 16` (for example `crates/fluxbee_sdk/src/rpc.rs`); HELLO,
  ANNOUNCE and LSA carry 1 (`protocol.rs`).

### 3.6 UNREACHABLE and TTL_EXCEEDED

Both are system messages from the router's UUID to the sender's UUID, with the original `trace_id`
and `ttl: 16` (`build_unreachable`, `build_ttl_exceeded`). Payloads:
`UNREACHABLE {original_dst, reason}` and `TTL_EXCEEDED {original_dst, last_hop: "router:<uuid>"}`.
`original_dst` is the destination as sent: a name or UUID, `"broadcast"`, or `"null"` for a Resolve.

| `reason` | Meaning |
| --- | --- |
| `NODE_NOT_FOUND` | no FIB entry for the name; the UUID is unknown everywhere; or the node left between the FIB rebuild and the delivery |
| `VPN_BLOCKED` | the VPN rules, or the edge rules, forbid the pair (§8.2) |
| `PEER_UNAVAILABLE` | the destination is on a sibling router with no IRP link |
| `GATEWAY_UNAVAILABLE` | the destination is in another hive and no gateway sibling is linked; also a broadcast with `ttl >= 3` from such a router |
| `WAN_UNAVAILABLE` | this router is the gateway and has no WAN link to the next-hop hive |
| `OPA_ERROR` | Resolve: no user policy loaded, or its evaluation failed (§5, §7) |
| `OPA_NO_TARGET` | Resolve: the user policy gave no target |

A message that came from another router gets its error over the link it came in on; that router
hands it to the node when the node is its own (`handle_peer_message`, `handle_wan_message`).

### 3.7 Dropped without an answer

- a route with `DROP` (§3.2);
- a protected system action from an origin the system policy does not allow (§6.3);
- a frame whose `routing.src` is not the sending connection's UUID (§2);
- a broadcast whose `trace_id` is not a UUID or was seen in the last 30 s (§4);
- a frame over 128 KiB (`MAX_FRAME_SIZE`) on its way out: the router skips it, counts it and logs
  `router skipped a frame over the size limit; the link goes on` (`write_link_frames`). A node that
  sends such a frame is disconnected;
- a tap copy that cannot be delivered (§8.3);
- `STREAM_APPEND`, `OPA_RELOAD` and `WITHDRAW`, which the router consumes; `WITHDRAW` also closes the
  node's connection.

---

## 4. Broadcast (`dst: "broadcast"`)

At the router where it enters from a node (`handle_message`):

1. `trace_id` must be a UUID. A `trace_id` seen in the last 30 s is dropped (`BroadcastCache`).
2. Every local node except the sender gets it, when it passes the VPN check for a broadcast
   (§8.2, `vpn_allows_broadcast_between`) and, if `meta.target` is set, its name matches it
   (`pattern_match`: exact, or `*` wildcards).
3. `ttl >= 2`: a copy to every sibling router (`broadcast_to_peers`).
4. `ttl >= 3`: toward the WAN. The gateway sends it to every WAN peer; another router sends it to
   the gateway sibling (`forward_broadcast_to_wan`).

A sibling delivers it to its own nodes and, with `ttl >= 3`, sends it toward the WAN as in step 4. A
gateway that receives it from the WAN checks the same cache, delivers it to its nodes, and passes it
to its siblings (`ttl >= 2`) and to its WAN peers (`ttl >= 3`). `meta.target` is matched by every
router against its own nodes.

---

## 5. Resolve (`dst: null`)

The system step comes first; when it names a target, the user policy is not consulted
(`resolve_target_with_identity`).

1. **No `meta.src_ilk`** (absent or blank): the target is the hive's identity frontdesk. This is
   decided before anything is read (`route_to_frontdesk(None, false)`).
2. **Otherwise** the router reads `/jsr-identity-<hive>`. When it can:
   - an `src_ilk` with an active, unexpired alias is replaced by its canonical ILK in the copy the
     policy sees (`canonicalize_src_ilk_and_status`); the delivered message keeps the original;
   - an ILK whose `registration_status` is `temporary` goes to the frontdesk
     (`apply_identity_pre_resolve`).

   An `src_ilk` that is not `ilk:<uuid>`, or that the region does not list as active, is not sent to
   the frontdesk: it goes to the user policy. When the region cannot be read, the user policy runs
   without the identity data (§7.3).
3. **The user policy** (§7) gives the target. No user policy loaded, or an evaluation error:
   `UNREACHABLE OPA_ERROR`. No target: `UNREACHABLE OPA_NO_TARGET`.
4. The target is resolved like a unicast name, `@*` included (§3.2, §3.3), and checked against the
   sender's VPN.

- The frontdesk name is `government.identity_frontdesk` of `hive.yaml`, with `@<hive>` appended when
  missing; default `SY.frontdesk.gov@<hive>` (`RouterConfig::load_from_env`). The decision to send
  there is system policy (§6.1); the name is router configuration.
- When the target is on another router or hive, the message is forwarded with `dst` still `null`.
  The next router runs steps 1–4 again with its own identity region, policy and frontdesk name
  (`handle_peer_message`, `handle_wan_message`).

---

## 6. The system policy

### 6.1 What it is

`policy/system.rego` (package `fluxbee.system`) has two entry points. Each is compiled to its own
wasm and baked into the router binary with `include_bytes!` (`src/router/system_policy.rs`):

| Entry point | Wasm | Input | Answers |
| --- | --- | --- | --- |
| `fluxbee/system/allow` | `policy/system.wasm` | `{action, src_l2_name, hive_id}` | may this origin perform this protected action here |
| `fluxbee/system/frontdesk_route` | `policy/system_route.wasm` | `{src_ilk_present, registration_status}` | send this sender to the frontdesk (§5) |

- **Not editable at run time.** No region and no endpoint reaches it. It changes only by editing the
  Rego, compiling both wasms with `sy-opa-rules compile-file policy/system.rego <entrypoint>
  <out.wasm>` (`go/sy-opa-rules/main.go`), committing the three files and shipping a new router.
- **Fail closed.** `Router::run` calls `ensure_system_policy_loaded()`, which evaluates a probe on
  both wasms; if either fails, the router does not start. There is no Rust fallback table. At run
  time a lock or evaluation error denies (`authorize_system`) or does not force the frontdesk
  (`route_to_frontdesk`).

### 6.2 Protected actions and who may send them

The gate covers system messages (`meta.type` compared without case, `is_system_kind`) whose
`meta.msg` is one of the 30 `PROTECTED_SYSTEM_ACTIONS`. The input is that verb, the router-stamped
`src_l2_name` (§2) and the hive of the router doing the delivery.

| Rule in `system.rego` | Actions | Allowed origin |
| --- | --- | --- |
| (1) | `EDGE_OPEN_URL`, `EDGE_CLOSE_URL`, `EDGE_LIST_URLS`, `EDGE_PUBLISH_BLOB`, `EDGE_UNPUBLISH_BLOB`, `EDGE_TLS_STATUS`, `EDGE_TLS_RELOAD`, `EDGE_ACME_CHALLENGE` | `SY.admin@motherbee` |
| (2) | `CONFIG_GET`, `CONFIG_SET`, `SYSTEM_UPDATE`, `SYSTEM_SYNC_HINT`, `CONFIG_CHANGED` | `SY.admin@motherbee` |
| (3) mutations | `SPAWN_NODE`, `KILL_NODE`, `START_NODE`, `RESTART_NODE`, `REMOVE_NODE_INSTANCE`, `NODE_CONFIG_SET`, `SYSTEM_CORE_ROLLBACK`, `ADD_HIVE_FINALIZE`, `REMOVE_HIVE_CLEANUP` | `SY.orchestrator@motherbee`; `ADD_HIVE_FINALIZE` and `REMOVE_HIVE_CLEANUP` never on the motherbee itself |
| (3) reads | `NODE_CONFIG_GET`, `NODE_STATE_GET`, `NODE_STATUS_GET`, `LIST_NODES`, `GET_VERSIONS`, `GET_RUNTIMES`, `GET_RUNTIME` | `SY.orchestrator@motherbee`, `SY.orchestrator@<this hive>` |
| (4) | every protected action outside (1) and (2): the (3) lists and `INVENTORY_REQUEST`; never `ADD_HIVE_FINALIZE` or `REMOVE_HIVE_CLEANUP` on the motherbee | `SY.admin@<this hive>` |
| (5) | `NODE_STATUS_GET` | `SY.vault@<this hive>` |
| (6) | `WAN_REACHABILITY_VOUCH`: not a message, the router asks it before it accepts a hub's reachability advertisement | `RT.gateway@motherbee` |

- In short: node lifecycle verbs (SPAWN, KILL, …) come from the motherbee's orchestrator or from the
  admin of the same hive, and the node reads also from the same hive's orchestrator;
  `SYSTEM_UPDATE`, `SYSTEM_SYNC_HINT`, `CONFIG_*` and the edge commands come only from
  `SY.admin@motherbee`.
- Rule (3) has a direction (FINDINGS A-20, [control-path-hardening-v1.md](../control-path-hardening-v1.md)
  §3). Every orchestrator message that crosses hives goes from the motherbee to a spoke, and the
  watchdog acts locally, so a spoke's orchestrator, the DMZ ingress's included, commands no hive,
  not even its own. `ADD_HIVE_FINALIZE` and `REMOVE_HIVE_CLEANUP` run only on the hive being added
  or removed: on the motherbee they would restart or stop the core, so neither rule (3) nor rule
  (4) admits them there, from anyone, and its orchestrator refuses them too
  (`SPOKE_ONLY_SYSTEM_ACTIONS` in `src/bin/sy_orchestrator.rs`).
- The origin is split at the first `@` after trimming. The role must match exactly, so
  `SY.orchestrator.relay@h` is not `SY.orchestrator`; an empty name, or one without `@`, matches
  nothing.
- Nobody else is allowed. Since A-68, SY.wf-rules and the test name `WF.orch.diag` have no grant of
  their own; WF nodes are launched through SY.admin (rule (4) comment in `system.rego`). The tests
  in `src/router/system_policy.rs` load the baked wasm and pin this table.

### 6.3 Where the gate runs

- At the final local delivery, on the destination's router, on every path: from a local node, over
  IRP, over the WAN, and for tap copies (`prepare_local_delivery`, called by `deliver_locally` and
  `serialize_for_local_delivery`). A forbidden message crosses routers and hives and is dropped at
  the end.
- A denied message is dropped with the warning `router dropped protected SYSTEM action from
  unauthorized origin`. The sender gets nothing back.
- A sender known only through a hub's reachability advertisement (`via_hub`) is denied every
  protected action without evaluating the policy. So is a tap copy (`meta.via_tap`, §8.3): it
  keeps the original sender's name, but goes to the tap's target, which that sender never
  addressed.
- Other system verbs and every other kind pass the gate.
- Nodes add their own checks: SY.config.routes takes route, VPN and tap changes only from
  `SY.admin@motherbee` (`is_primary_admin_origin`); SY.opa.rules takes `CONFIG_CHANGED` only from
  `SY.admin@motherbee` (`handleMessage`).

### 6.4 Frozen markers in the route table

SY.config.routes appends two entries to the routes it writes: `SY.orchestrator@*` (GLOB) and
`SY.admin@<hive>` (EXACT), flagged `FLAG_ACTIVE | FLAG_FROZEN` (`frozen_system_authority_routes`).
The router drops `FLAG_FROZEN` entries when it loads routes (`refresh_config`), so they never route;
they only show part of the rule set in the region. They take 2 of the 256 route slots.

---

## 7. The user policy

### 7.1 Model

- **One policy for every hive.** The motherbee's SY.opa.rules compiles it and publishes the wasm;
  every hive's SY.opa.rules installs it into its own `/jsr-opa-<hive>`, where that hive's routers
  load it. The transport is described in [opa-distribution.md](../opa-distribution.md).
- **Package `router`, entry point `router/target`.** It is the default of SY.admin's `entrypoint`
  field (`handle_opa_http`) and of SY.opa.rules (`defaultEntrypoint`); the region records the one in
  use.
- **Selection only.** It runs only for Resolve, after the system step (§5). It cannot grant a
  protected action.
- **Build.** The OPA Go compiler v0.68.0 turns the rego into wasm (`compileRego`,
  `go/sy-opa-rules/main.go`), at most 4 MiB. The router runs it with wasmtime (`OpaResolver`,
  `src/opa.rs`). Builtins that the wasm leaves to the host are implemented in `eval_builtin`; any
  other one records `builtin not implemented: <name>`.

### 7.2 Input

`build_opa_input` (`src/opa.rs`):

```json
{
  "routing": { "src": "<sender uuid>", "src_l2_name": "IO.wapp.main@worker1" },
  "action": "<meta.msg, or empty>",
  "meta": { "type": "user", "src_ilk": "ilk:<uuid>", "thread_id": "…", "context": { } }
}
```

- `meta` is the message's whole `meta` as serialized; fields not set are absent. The payload is
  never included.
- `routing.src_l2_name` is the stamped name (§2). On a router that got the message from another hive
  it is the canonical `role@<that hive>`.
- `meta.src_ilk` is the canonical, lowercase `ilk:<uuid>` when the identity region was read and the
  value parses as `ilk:<uuid>` (§5); otherwise it is what the sender wrote.

### 7.3 Data

Each Resolve reads the identity region and passes it as `data` (`build_dynamic_identity_opa_data`,
`inject_identity_data` in `src/router/mod.rs`):

```json
{
  "identity": {
    "ilk:<uuid>": {
      "tenant_id": "tnt:<uuid>",
      "ilk_type": "human | agent | system | unknown",
      "registration_status": "temporary | partial | complete",
      "handler_node": "AI.sales@worker1",
      "role_hash": "<64 hex> or null",
      "skill_hashes": ["<64 hex>"],
      "handbook_hashes": ["<64 hex>"],
      "personality_hash": "<64 hex> or null"
    }
  },
  "identity_aliases": { "ilk:<old>": "ilk:<canonical>" }
}
```

- Only active ILKs are listed, and aliases only until they expire.
- `handler_node` is the node that serves the ILK (its `node_name`), `""` when none.
- An `ilk_type` byte out of range reads `"unknown"`, never `system`.
- **`tenant_id` is the tenant the ILK is charged to: a cost center, not a permission.** Do not allow
  or deny anything by it ([ilk-tenant-model-v1.md](../ilk-tenant-model-v1.md) §2.3 and its OPA
  note).
- There is no `tenant_ilk`, `type`, `human_subtype` or `capabilities` field.
- The router also loads `/var/lib/fluxbee/opa/current/data.json` as static data when it exists
  (`opa_data_bundle_path`); the identity data is merged over it. Nothing in the repository writes
  that file. When the identity region cannot be read, only that static data remains.

### 7.4 Output

The first string in the result is the target (`extract_target`). `null`, undefined or a non-string
means no target. A target can be any name the FIB understands, `name@*` included.

### 7.5 Example

```rego
package router

import rego.v1

default target := null

# The sender as this hive's SY.identity knows it. Undefined when the ILK is not in the
# region, and then the rules that use it do not match.
sender := data.identity[input.meta.src_ilk]

# One rule; the alternatives are tried top to bottom through `else`.
target := "AI.support.vip@motherbee" if {
    sender.ilk_type == "human"
    input.meta.context.tier == "vip"
} else := "AI.sales@worker1" if {
    startswith(input.routing.src_l2_name, "IO.wapp.")
} else := "AI.support@*" if {
    sender.registration_status == "complete"
}
```

- The same wasm runs on every hive. To branch by hive, use `input.routing.src_l2_name`.
- A message with no `src_ilk`, or a `temporary` one, never reaches this policy (§5).
- Two rules for `target` that give different values are a conflict: the compiled wasm stops with a
  runtime error (`var assignment conflict`, OPA v0.68 `internal/compiler/wasm`) and the sender gets
  `UNREACHABLE OPA_ERROR`. Keep the alternatives in one `else` chain.

### 7.6 Writing and reading it: SY.admin

| HTTP | What it does |
| --- | --- |
| `POST /opa/policy` `{rego, entrypoint?, version?}` | compile on the motherbee, apply there, publish, tell every hive |
| `POST /opa/policy/compile` | compile and stage on the motherbee; nothing runs it yet |
| `POST /opa/policy/check` | compile only, to validate: no version, SY.opa.rules stages nothing and applies nothing |
| `POST /opa/policy/apply` `{version}` | apply the staged policy; `version` is required and must be the staged one |
| `POST /opa/policy/rollback` | make the backup current again and publish it |
| `POST /opa/policy/clear` | remove current, staged and backup on every hive; the routers unload it; there is no rollback of a clear |
| `GET /opa/policy?hive=…`, `GET /hives/{hive}/opa/policy` | version, hash, entrypoint; the rego only on the motherbee |
| `GET /opa/status?hive=…`, `GET /hives/{hive}/opa/status` | what the hive runs, what was published to it, each router's loaded version and status, `in_sync` |

- `src/bin/sy_admin.rs`: `handle_opa_http`, `handle_opa_query`; SY.opa.rules: `handleOpaAction`,
  `statusView`. The internal actions are `opa_compile_apply`, `opa_compile`, `opa_check`,
  `opa_apply`, `opa_rollback`, `opa_clear`, `opa_get_policy`, `opa_get_status`.
- A write that names a hive other than `motherbee` gets 400 `INVALID_REQUEST`. Without `hive`, the
  GETs ask every hive.
- Compile and compile + apply take the next number from `/var/lib/fluxbee/opa-version.txt`, or a
  `version` you pass, which must be greater (`next_opa_version`). A check takes none: its answer
  is matched on a `check_id` it echoes (`send_opa_check`).
- A write that changed the policy reports the hives that run it and the ones still pending
  (`build_opa_rollout_response`).

### 7.7 How a router picks it up

- **OPA_RELOAD.** After writing the region, SY.opa.rules broadcasts `OPA_RELOAD {version, hash}`
  with `ttl: 2`. The router loads from the region only if `hash` equals the region's wasm hash; a
  missing or different hash is refused (`apply_opa_reload`). It passes the notice to its siblings.
- **Heartbeat.** Every 5 s the router compares the region's `policy_version` with the one it holds:
  different, with status ok, it loads; version 0 with no wasm, it unloads; loading or error, it
  waits (`maybe_refresh_opa_from_shm`).
- A load that fails keeps the previous policy and sets the status to error (`OpaResolver::reload`).
- Each router writes `opa_policy_version` and `opa_load_status` into its own region;
  `GET /hives/{hive}/opa/status` lists them under `routers` (`listRouterStatuses`).

---

## 8. SY.config.routes: routes, VPNs and taps

### 8.1 Where they live and how they reach the router

- **One per hive.** `SY.config.routes@<hive>` runs on every role (`config/hive.yaml`). It keeps the
  hive's rules in `/etc/fluxbee/sy-config-routes.yaml` (`{version, updated_at, routes, vpns, taps}`)
  and writes them into `/jsr-config-<hive>` at start and after each change (`apply_config`).
- **One change, one version.** It writes the routes plus the two frozen markers, then the VPN rules,
  then the taps. Only the tap write bumps the region's `config_version`, so a router sees the three
  together.
- **The routers of the hive** re-read the region on every frame from one of their nodes and on
  their 5 s heartbeat. When `config_version` changed, they replace routes, VPN rules and taps,
  reassign the VPN of their connected nodes and rebuild the FIB (`refresh_config`).
- **Other hives** get them in the gateway's LSA: every 10 s (`lsa_broadcast_loop`) and when a node of
  the gateway router connects or leaves (`broadcast_lsa_direct`).
- **Who changes them.**
  - SY.admin, with `meta.type: "admin"` and the action in `meta.action`. Changes are accepted only
    from `SY.admin@motherbee`, others get `FORBIDDEN` (`is_primary_admin_origin`); the lists are open.
  - `CONFIG_SET` (schema `sy_config_routes_v2`, `apply_mode: replace`, sections `routes`, `vpns`,
    `taps`), a protected action: `SY.admin@motherbee` only.
  - SY.config.routes no longer acts on `CONFIG_CHANGED`.

All endpoints are per hive. The top-level `/routes` and `/vpns` are gone (commit `aa206ac`, shipped
as 0.1.50 per `lab/DEPLOYMENTS.md`).

| Action | HTTP | Body or parameters |
| --- | --- | --- |
| `list_routes` | `GET /hives/{hive}/routes` | |
| `add_route` | `POST /hives/{hive}/routes` | `{prefix, action, match_kind?, next_hop_hive?, metric?, priority?}` |
| `delete_route` | `DELETE /hives/{hive}/routes/{prefix}` | the prefix, percent-decoded |
| `list_vpns` | `GET /hives/{hive}/vpns` | |
| `add_vpn` | `POST /hives/{hive}/vpns` | `{pattern, vpn_id, match_kind?, priority?}` |
| `delete_vpn` | `DELETE /hives/{hive}/vpns/{pattern}` | |
| `list_taps` | `GET /hives/{hive}/taps` | |
| `add_tap` | `POST /hives/{hive}/taps` | `{match_src, match_dst, target, mode?, enabled?}` |
| `delete_tap` | `DELETE /hives/{hive}/taps?match_src=…&match_dst=…&target=…` | |

**Defaults and checks** (`src/bin/sy_config_routes.rs`):

- `match_kind` `PREFIX`, `priority` 100, `metric` 0; tap `mode` `best_effort` (the only one) and
  `enabled` true.
- `match_kind` must be exactly `EXACT`, `PREFIX` or `GLOB`, and `action` exactly `FORWARD` or
  `DROP` (case matters).
- Errors: `DUPLICATE_PREFIX`, `DUPLICATE_PATTERN`, `DUPLICATE_TAP`, `NOT_FOUND`, `FORBIDDEN`,
  `UNKNOWN_ACTION`.
- An entry with another `match_kind`, `action` or `mode`, or one that goes past the slots, gets no
  answer: SY.config.routes adds it to its in-memory list, the region write fails, and every later
  change fails too until that entry is deleted or the node restarts. The YAML and the region keep
  the last good state.
- Limits: 256 routes in the region, the 2 markers included; 256 VPN rules; 256 taps. In the region,
  prefixes, patterns and tap names keep their first 256 bytes, `next_hop_hive` its first 32.

### 8.2 VPNs

**Assignment** (`assign_vpn`), when a node connects and whenever `config_version` changes
(`reassign_vpns`):

- names starting with `SY.` or `RT.` are in VPN 0, always;
- any other name takes the `vpn_id` of the first matching rule, rules sorted by `priority`
  ascending (equal priorities keep their order in the file), matched by their `match_kind` (§3.2);
- no match, or no config region: VPN 0.

Only the hive's own rules count. Rules of other hives travel in the LSA, but no router uses them to
assign. A node of another hive carries the `vpn_id` its own hive gave it, read from the LSA. To put
nodes of two hives in one VPN, load the same rule on both. A changed VPN is written to the router
region (`update_node_vpn`); the node is not told, ANNOUNCE carries it only at HELLO.

**The check** (`vpn_allows_between`), at every delivery decision: UUID or name, local, sibling or
remote, tap copies. A broadcast fan-out asks `vpn_allows_broadcast_between`, the same check
without the vault notice of rule 1.

1. If either end is `SY.edge`, only the edge rules apply: the edge commands of §6.2 rule (1) from
   `SY.admin@motherbee` and their `*_RESPONSE` back to it (`EDGE_SERVICE_COMMANDS`,
   `EDGE_SERVICE_RESPONSES`); non-system traffic between the edge and `IO.*` nodes; `VAULT_GET`
   and `VAULT_GET_RESPONSE` with `SY.vault`; and `VAULT_SECRET_CHANGED` from
   `SY.vault@motherbee` to the edge, addressed to it, never as a broadcast (A-78,
   `edge_vault_notice_allowed`). SY.vault's broadcast names every stored key, its tenant and its
   owners, so it stays out of the DMZ; SY.vault sends the edge the notices about its own secrets
   instead ([SY.vault](../nodes/sy-vault.md#vault_secret_changed)). Nothing else.
2. A system message with `meta.scope: "global"` passes, whoever sends it.
3. If either end's name starts with `SY.` or `RT.`, it passes.
4. Otherwise the two `vpn_id`s must be equal. VPN 0 is an ordinary VPN here: a node in VPN 0 and one
   in VPN 10 do not reach each other.

### 8.3 Taps

A tap copies traffic: a unicast message from `match_src` to `match_dst` (exact names, no wildcards)
also goes, as a copy, to `target` (`fanout_taps_for_unicast`).

- Taps are evaluated only by the router where the message enters from its node (`handle_message`),
  after the primary delivery.
- `match_src` is compared with the stamped sender name. `match_dst` is compared with the name the
  router knows for a UUID destination (local, sibling or LSA), or with the name as written: a tap on
  `AI.x@motherbee` does not see a message sent to `AI.x@*`. Resolve and broadcast messages are never
  tapped.
- This hive's taps (config region) apply to any message. Another hive's, from its LSA, apply only
  to a message that hive is a party to, its sender or its destination being `<role>@<that hive>`,
  and only with a target on that hive (`lsa_tap_applies`). So a hive cannot copy traffic it does
  not already see, nor hand a copy to another hive's node: a spoke's tap on `SY.admin@motherbee` to
  `SY.vault@motherbee` copies nothing, and one on a message to its own orchestrator cannot copy it
  to another spoke's. Targets are deduplicated.
- The router evaluates taps, this hive's and the LSA's, only while this hive's config region holds
  at least one tap (`handle_message`).
- The copy is the same message with `meta.via_tap: true`, a new `trace_id` and `dst` = `target`,
  resolved like any unicast name. It keeps the original `src` and `src_l2_name`, and never triggers
  taps itself.
- The target must pass the VPN check against the original sender (§8.2). A copy never carries a
  protected system action, whoever sent the original: the gate drops it at delivery, like a
  hub-relayed sender's (§6.3).
- Best effort: a copy that cannot go (unknown target, VPN, TTL, link down) is logged as
  `router tap copy: …` and dropped. The primary delivery does not wait for it.

---

## 9. What the router adds at delivery

- **Memory package.** To a message delivered to an `AI.*` node, with a `meta.thread_id` and a kind
  other than `system`, `admin` or `query_response`, the router attaches the package that
  `/jsr-memory-<hive>` holds for that thread and that node, if any (`package_for`). A package the
  sender set is removed on every delivery (`prepare_local_delivery`). See
  [router-stream-v1.md](../router-stream-v1.md) §6.2 and [cognition.md](cognition.md).
- **Stream copies.** The router a message enters by copies it, without waiting, to its stream
  `RT.<router>.stream@<hive>`: every kind except `system`, `admin` and `query_response` as
  `storage.turns` (`turn_copy`); a turn with a `thread_id` that a local `AI.*` node sends or
  receives, other than a tap copy or a broadcast, as `cognition.turns` (`cognition_copy`). Nodes add
  records with `STREAM_APPEND`, which the router consumes. See
  [router-stream-v1.md](../router-stream-v1.md) §2 and §7.

---

## 10. Known limits

These are defects of the several-routers mode, which is not supported (§1); they are not being
fixed. With one router per hive none of them applies.

- **Broadcast duplicates.** A broadcast reaches the gateway's nodes once per sibling router plus
  one, because copies that arrive over IRP are not checked against the broadcast cache
  (`handle_peer_message`). With a `ttl` of 4 or more, a broadcast that enters at another router
  reaches the gateway twice from that router, once with all its siblings and once toward the WAN
  (`handle_message`), and once more from every other sibling, which sends its copy on toward the
  WAN, that is to the gateway (`forward_broadcast_to_wan`). One that enters at the gateway, from
  one of its nodes or from the WAN, comes back once from each sibling the same way.
- **Errors from the WAN reach only the gateway's nodes.** An `UNREACHABLE` or `TTL_EXCEEDED` that
  comes back over the WAN is delivered only to a node of the gateway router: `handle_wan_message`
  looks its destination up only among the gateway's own nodes, so an error for a node of a sibling
  router is dropped.
- **Hub-vouched nodes need the gateway.** Only the gateway holds the reachability table, so a node
  vouched for by the hub cannot reach a node on a sibling router (`remote_source_from_gateway`
  comment), nor can a sibling resolve it.

---

## 11. Where to look

| Symptom | Look at |
| --- | --- |
| `NODE_NOT_FOUND` for a name | the full `role@hive`; the node connected (`GET /hives/{hive}/nodes`); its hive in the LSA and not stale; a `@*` destination |
| `VPN_BLOCKED` | `GET /hives/{hive}/vpns` on both hives; the `vpn_id` in the node's ANNOUNCE; the edge rules |
| A message vanished | a `DROP` route on a hive the message passed, or on its destination's hive (`GET /hives/{hive}/routes`; another hive's routes apply to its own names and to `name@*`, §3.2); the router log lines in §3.7 and §6.3 |
| `dst: null` lands on the frontdesk | `meta.src_ilk` absent, or the ILK `temporary` (§5) |
| `OPA_ERROR` | `GET /hives/{hive}/opa/status` (no policy, or a router in error); the router log `opa resolve failed` |
| `OPA_NO_TARGET` | the policy gave no string; `data.identity` lacks the sender (§7.3) |
| A protected verb did nothing | the sender's stamped name against §6.2 |
| No tap copy | exact names, the destination as written, the VPN, the log `router tap copy:`; another hive's tap copies only messages to or from that hive, to its own nodes; a protected system action is never copied (§8.3) |
| A node of another hive is unknown here | its name must be `<role>@<its hive>`; the log `lsa nodes dropped` (§2) |
| A message from another hive vanished | its sender must be a node of the peer it came from, or one the hub relays; the log `router dropped a WAN frame` (§2) |
| `NODE_NOT_FOUND` for a node's UUID | two hives list that UUID: the logs `router refused a UUID that several hives list` and `lsa nodes with a UUID already taken` (§2, §3.1) |
