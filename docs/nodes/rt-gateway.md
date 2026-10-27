# rt-gateway (the router)

`rt-gateway` is the router process every hive runs. Nodes connect to it over a local Unix
socket and send it framed JSON messages; it delivers each message to a local node, to another
router of the same hive, or across the WAN to another hive. It enforces the system policy on
protected system actions, applies the routes, VPNs and taps of SY.config.routes, asks the user
policy of SY.opa.rules where a message has no destination, and keeps a short on-disk stream of
what passes through it (the log plane) for SY.storage and SY.cognition.

Source: `src/main.rs`, `src/router/mod.rs` (routing, WAN, IRP), `src/router/stream.rs` (the
stream), `src/router/system_policy.rs` and `policy/system.rego` (system authority),
`src/opa.rs` (policy evaluation), `src/config/mod.rs` (settings), `src/shm/mod.rs` (shared
memory), and the SDK's `crates/fluxbee_sdk/src/mesh/` (WAN mTLS and the hive registry, feature
`mesh-tls`).

| | |
| --- | --- |
| Binary | `/usr/bin/rt-gateway`, built as `json-router` (`packaging/build-deb.sh`, `RUST_BINS` entry `rt-gateway:json-router`) |
| Node name | `RT.gateway@<hive_id>` (`wan.gateway_name`, or `JSR_ROUTER_NAME`) |
| Role | every role |
| systemd unit | `rt-gateway.service` (`Restart=always`, `RestartSec=5`, `TimeoutStopSec=15`) |
| Listens on | `/var/run/fluxbee/routers/<uuid>.sock` (nodes), `.../irp-<uuid>.sock` (sibling routers), `wan.listen` TCP on the gateway |
| Writes | its own SHM region `/jsr-<uuid>`, and on the gateway the LSA region `/jsr-lsa-<hive>` |
| State | `/var/lib/fluxbee/state/<router name>/identity.yaml` and `.../stream/` |

## Where it runs and how it starts

- Every hive runs one: the packaged unit has no role gate, and `add_hive` writes an
  `rt-gateway` unit on the spokes it provisions (`src/bin/sy_orchestrator.rs`). One router per
  hive is the supported topology ([architecture.md](../reference/architecture.md) §4.2). The
  orchestrator starts it before any SY service and waits up to 30 s for it (`bootstrap_local`,
  `wait_for_router_ready`). On the motherbee it first makes sure the mesh CA and the hive's own
  certificate exist (`ensure_motherbee_mesh_tls`).
- The router's name comes from `JSR_ROUTER_NAME`, else `wan.gateway_name`, else `RT.gateway`,
  with `@<hive_id>` added (`src/config/mod.rs`). A router is the gateway when its base name
  equals `gateway_name`'s, so the packaged router is the gateway. Only the gateway loads TLS,
  runs the WAN and writes the LSA region.
- At first start it creates `/var/lib/fluxbee/state/<router name>/identity.yaml` with its UUID
  and SHM name; later starts reuse them, and with them the SHM region the previous process left.
  The router takes that region over before it accepts a node: it empties the node table and writes
  its PID, start time, name and gateway flag (`RouterRegionWriter::open_or_create`). Nodes appear
  in it again as they reconnect, so a node that does not come back is gone for the readers too
  (SY.orchestrator, sibling routers).
- Settings read from `/etc/fluxbee/hive.yaml`: `hive_id`, `role` (default `worker`),
  `wan.listen`, `wan.uplinks[].address`, `wan.gateway_name`, `wan.mtls`,
  `government.identity_frontdesk`, `ai`. The environment: `JSR_LOG_LEVEL` (default `info`) and
  `JSR_ROUTER_NAME`. Timers are constants: HELLO 10 s, dead 40 s, heartbeat 5 s, stale 30 s
  (`DEFAULT_*` in `src/config/mod.rs`).
- It refuses to start if the baked system policy cannot be loaded
  (`ensure_system_policy_loaded`; there is no fallback table).

## What it listens on

- **Node socket** `/var/run/fluxbee/routers/<uuid hex>.sock`, mode 0600; a peer whose uid is not
  the router's own (root in production) is refused (`local_peer_admitted`). Frames are a 4-byte
  big-endian length plus JSON, at most 128 KiB (`MAX_FRAME_SIZE` in
  `crates/fluxbee_sdk/src/socket/connection.rs`). The first frame must be a `HELLO` whose
  `payload.uuid` equals `routing.src`. `RT.*` names are accepted only from the router's own
  process (`rt_name_allowed`, by the peer's PID).
- **IRP socket** `irp-<uuid hex>.sock` in the same directory, mode 0600: the link between
  routers of the same hive, which only the unsupported several-routers mode uses. Every 5 s a
  router looks in `/dev/shm` for live regions of other routers of its hive and the one with the
  lower UUID connects.
- **WAN** (gateway only): TCP on `wan.listen` when set; there is no default address in code. The
  motherbee template sets `0.0.0.0:9000` (`packaging/hive.yaml.example`); spokes have no
  `wan.listen` and connect out to `wan.uplinks`. There is no HTTP or status port.

## WAN between hives

- `wan.mtls`: `disabled` (also when missing or unknown), `permissive` or `required`. With
  `required` the server demands a client certificate signed by the mesh CA, the peer's hive is
  taken from its certificate, and a `HELLO` that claims another hive is dropped
  (`crates/fluxbee_sdk/src/mesh/tls.rs`). With `required` and no TLS material on disk the router
  logs `WAN FAIL-CLOSED` and rejects every peer; `permissive` falls back to plaintext with a
  warning. TLS files are `/var/lib/fluxbee/tls/<hive_id>/` (`tls_dir`).
- The WAN `HELLO` carries the protocol version `fluxbee/1.16` (`MESH_PROTOCOL_VERSION`); a
  mismatch is only logged, never rejected (comment on the constant in
  `crates/fluxbee_sdk/src/protocol.rs`).
- **Admission on the motherbee (A-48).** The motherbee (`role: motherbee`) admits a hive only
  while the hive registry lists it: the directory `/var/lib/fluxbee/hives/<hive_id>/`, which
  SY.orchestrator creates when it accepts the join and `remove_hive` deletes after the hive's
  cleanup. The hive is the one bound to the peer's certificate. Any other gets `WAN_REJECT`
  `HIVE_NOT_REGISTERED` and the log line `wan peer rejected: hive not registered`. An admitted
  session looks again every 10 s and closes when the directory is gone
  (`wan peer session closed: hive no longer registered`), so the link of a removed hive ends by
  itself (`wan_peer_admission`, `hive_unregistered`; the registry's loose reading,
  `HiveRegistry::lists` in `crates/fluxbee_sdk/src/mesh/registry.rs`). Both lines carry
  `reason=HIVE_NOT_REGISTERED`. The registry is the only list. Spokes keep none and listen on no
  WAN port; a spoke that is rejected logs the reason (`wan uplink error`) and backs off like any
  failed attempt.
- Each gateway announces its nodes, routes, VPNs and taps to its peers in an LSA
  (`build_local_lsa_payload`). The name a WAN message claims is rewritten to `<role>@<the
  hive it came from>` (EDGE-01), so a hive cannot speak for another.
- An uplink dials again 5 s after a link that got past `WAN_ACCEPT` ends. An attempt that never
  got there (no connection, a TLS or handshake error, a `WAN_REJECT`) is retried after 5 s,
  doubling up to 60 s (`UplinkRetry`).
- **Multi-hop:** a hub gateway with two or more peers sends `WAN_REACHABILITY`, so a spoke can
  reach another spoke through it. A spoke accepts this only from the motherbee's gateway (rule 6
  of `policy/system.rego`), and messages from hosts learned that way never get system authority
  (`prepare_local_delivery`).

## Routing

- **Origin binding.** A frame whose `routing.src` is not the UUID that said `HELLO` on that
  connection is dropped with `router dropped a message whose routing.src is not the sending
  node`. The router overwrites `routing.src_l2_name` with the sender's registered name, so every
  node can trust it.
- **Unicast** by UUID or by name. Names resolve through a forwarding table built from local
  nodes, sibling routers, the static routes of SY.config.routes, the LSA and the reachability
  table (`rebuild_fib`). `name@*` tries the sender's hive first.
- **Broadcast** reaches the nodes connected at that moment (minus the sender), filtered by VPN
  and by `meta.target`; TTL 2 also reaches sibling routers and TTL 3 also crosses the WAN.
  Duplicates are dropped by `trace_id` for 30 s.
- **Resolve** (no destination): first the system policy's frontdesk rule sends a message with no
  ILK, or with a `temporary` one, to `government.identity_frontdesk`; otherwise the user policy
  of SY.opa.rules picks the target (`resolve_target_with_identity`).
- **VPNs:** both ends must share a `vpn_id`, except system nodes (`SY.*`, `RT.*`) and `SYSTEM`
  messages with scope `global`, which cross VPNs; traffic to and from `SY.edge` is limited to
  edge control, IO data, vault reads, and SY.vault's notices about the edge's own secrets, which
  reach it only as a unicast: a broadcast never does (`vpn_allows_between`,
  `vpn_allows_broadcast_between`; A-78).
- **Taps** copy matching unicast messages to a third node, fire-and-forget (see
  [SY.config.routes](sy-config-routes.md)).
- **Errors to the sender:** `UNREACHABLE {original_dst, reason}` with reasons `NODE_NOT_FOUND`,
  `VPN_BLOCKED`, `PEER_UNAVAILABLE`, `WAN_UNAVAILABLE`, `GATEWAY_UNAVAILABLE`, `OPA_ERROR`,
  `OPA_NO_TARGET`; and `TTL_EXCEEDED` when the TTL runs out (every IRP and WAN hop decrements it).

### System authority

32 actions are protected (`PROTECTED_SYSTEM_ACTIONS` in `src/router/system_policy.rs`: node
lifecycle, `CONFIG_GET`/`CONFIG_SET`/`CONFIG_CHANGED`, `SYSTEM_UPDATE`, edge commands, ...).
At local delivery the router asks the baked `policy/system.wasm` whether the sender may perform
the action (`authorize_system`); a denial drops the message with
`router dropped protected SYSTEM action from unauthorized origin`. The rules
(`policy/system.rego`): edge commands and node control only from `SY.admin@motherbee`; the
orchestrator's mutating actions only from `SY.orchestrator@motherbee` (`ADD_HIVE_FINALIZE` and
`REMOVE_HIVE_CLEANUP` never on the motherbee itself), its reads also from the same hive's
`SY.orchestrator`; every other protected action from the
same hive's `SY.admin`; `NODE_STATUS_GET` also from the same hive's `SY.vault`. The policy is
compiled into the binary (`include_bytes!`) and changes only with a new build.

### User policy

The router reads `/jsr-opa-<hive>` on its heartbeat and on `OPA_RELOAD`, and loads a new
version when it changes; `OPA_RELOAD` must carry the matching sha256. An invalid policy leaves
the previous one in place and is reported in the region. See [SY.opa.rules](sy-opa-rules.md).

## The router stream (`RT.<router>.stream@<hive>`)

The stream is the log plane (header of `src/router/stream.rs`). It is a node of its own router,
for example `RT.gateway.stream@motherbee`, that keeps a short window of records on disk and
pushes them in acknowledged batches to their sinks.

- **Records:**
  - `storage.turns`: a copy of each message a local node sends, except `SYSTEM`, `admin` and
    `query_response` messages (`should_publish_turn`);
  - `cognition.turns`: messages that local `AI.*` agents send or receive with a thread id;
  - `STREAM_APPEND` from nodes: `storage.cognition.*` only from `SY.cognition@<hive>`, and
    `storage.usage` (LLM usage records) from nodes, with SY.cognition accepted only on motherbee
    and worker hives and SY.frontdesk.gov, SY.architect and SY.admin only on the motherbee
    (`subject_allowed`). A refused append is counted.
- **Sinks** (`sinks_for`): `storage.*` goes to `SY.storage@motherbee` from every hive;
  `cognition.turns` goes to the hive's own `SY.cognition`, on motherbee and worker hives only.
- **Window:** a record waits at most 120 s for its sink (`STREAM_MAX_AGE_SECS`) and the stream
  keeps at most 32 MiB (`STREAM_MAX_BYTES`), in 4 MiB segments under
  `/var/lib/fluxbee/state/<router name>/stream/`, synced every 250 ms. Past the cap new records
  are dropped; older ones expire.
- **Usage reserve:** node appends may use 25 % of the window (`STREAM_NODE_APPENDS_SHARE_PERCENT`);
  of that share, 1 MiB (`STREAM_USAGE_RESERVE_BYTES`) and 128 of the 1024 queue slots
  (`STREAM_USAGE_QUEUE_RESERVE`) are kept for `storage.usage`, so cognition's batches cannot
  crowd out usage records.
- **Delivery:** `STREAM_BATCH` of at most 256 records or 96 KiB, one batch in flight per sink,
  `STREAM_ACK` expected within 5 s, retries every 0.5 to 8 s.
- **Never blocking:** the router only hands a record to a bounded queue (4096 turns, 1024 node
  appends, 16 MiB in total) and never waits. If the stream directory cannot be used, records are
  kept in memory only (`router stream cannot use its directory`).
- **Status:** `STREAM_STATUS` returns counters, backlog and per-sink cursors;
  `GET /hives/{hive}/streams` on SY.admin asks every stream the LSA lists for that hive
  (`handle_list_streams`).

### What it logs that `lab/ops.py health` counts

At most once every 30 s (`STREAM_ALARM_INTERVAL_SECS`) the stream logs what it lost since the
last line, by reason (`AlarmSnapshot::since`, `usage_since`):

```text
router stream lost records in the last 30 s: dropped (queue full): N, dropped (cap): N, refused appends: N, expired: N, oversize: N, disk errors: N
router stream lost usage records in the last 30 s: N
```

`lab/ops.py health` reads `journalctl -u rt-gateway` (and `-u sy-storage`) since a given time on
every hive and reports, per hive (`HEALTH_SCRIPT`):

| Field | What it counts | Emitted by |
| --- | --- | --- |
| `failed` | `systemctl --failed` units | systemd |
| `denials` | lines with `dropped protected SYSTEM action` | `prepare_local_delivery` |
| `srcdrops` | lines with `routing.src is not the sending node` | the node connection loop in `src/router/mod.rs` |
| `streamlost` | the sum of the counts on `router stream lost records` lines | `src/router/stream.rs` |
| `usagelost` | the sum on `router stream lost usage records` lines | `src/router/stream.rs` |
| `usagerej` | SY.storage's `usage records refused, mismatched or unverified` | `src/bin/sy_storage.rs` |

A hive is healthy only when all six are 0. A journal that cannot be read is reported, never
counted as zero.

## Failure behavior

- The process exits, and systemd restarts it, when `hive.yaml` or `identity.yaml` is missing or
  invalid, when the baked system policy cannot load, when its own SHM region or a socket cannot
  be created. The orchestrator's 5 s watchdog also restarts it when the unit is not active.
- Missing shared regions (config, LSA, OPA, identity, memory) do not stop it: it reopens them
  when they appear or when their heartbeat is older than 30 s. Without the config region a hive
  routes with no static routes, no taps and everything in VPN 0. The identity region is opened
  afresh on every read (`read_identity_snapshot`), so the router follows a region SY.identity
  created again (its limits changed in `hive.yaml`) at once; that region is empty until
  SY.identity's start-up rebuild fills it.
- A node frame over 128 KiB disconnects that node (`FRAME_TOO_LARGE`).
- A peer with a bad certificate fails its handshake (`wan connection error` /
  `wan uplink error`); the router keeps running. A silent peer is marked stale after 40 s.
- On SIGTERM it logs and exits cleanly.

## Health

- `systemctl status rt-gateway`. Startup log lines: `router listening`, `irp listening`,
  `wan mtls mode`, `wan listening`, `wan connected`, `router stream joined its router`.
- The orchestrator considers the router ready when the unit is active and its SHM heartbeat
  (written every 5 s) is at most 30 s old.
- `GET /hives/{hive}/opa/status` lists each router's loaded policy version and status;
  `GET /hives/{hive}/streams` shows the streams.
- `lab/ops.py health`, above.
- There is no router status message and no `/routers` route: SY.admin refuses the legacy
  `list_routers`/`get_routers` actions (`resolve_internal_action_spec`).

## Links

- [SY.config.routes](sy-config-routes.md), [SY.opa.rules](sy-opa-rules.md): the router's inputs.
- [SY.storage](sy-storage.md), [SY.cognition](sy-cognition.md): the stream's sinks.
- [SY.identity](sy-identity.md): the frontdesk rule.
- `docs/router-stream-v1.md`: the stream's design notes.
