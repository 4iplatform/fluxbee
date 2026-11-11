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
- It takes no argument: started with any (`--version` among them) it prints so and exits 2
  before it touches anything (FINDINGS A-177). The installed version is in `GET /versions`.
- At first start it creates `/var/lib/fluxbee/state/<router name>/identity.yaml` with its UUID
  and SHM name; later starts reuse them, and with them the SHM region the previous process left.
- **One process per router** (`hold_instance_lock`): before it opens its SHM region, its stream
  and its sockets, it takes an flock on `/var/run/fluxbee/<router name>.lock` (tmpfs, 0600, the
  holder's PID inside), held until the process ends; the kernel releases it however it ends. A
  second instance of the same router has the same UUID, so it would take over the live one's
  region, stream and sockets: it exits instead, naming the lock and the holder's PID. A lock that
  cannot be opened or taken for another reason is warned, and the router runs without it.
  The router takes that region over before it accepts a node: it empties the node table and writes
  its PID, start time, name and gateway flag (`RouterRegionWriter::open_or_create`). Nodes appear
  in it again as they reconnect, so a node that does not come back is gone for the readers too
  (SY.orchestrator, sibling routers).
- Settings read from `/etc/fluxbee/hive.yaml`: `hive_id`, `role` (default `worker`),
  `wan.listen`, `wan.uplinks[].address`, `wan.gateway_name`, `wan.mtls`,
  `government.identity_frontdesk` (`hive_settings_from`). No other key is parsed, so an old
  `hive.yaml` keeps its `ai` and `blob` whatever they hold. Until 0.1.124 the router parsed `ai`
  only to check it, and an invalid one stopped it at its start; the readers take `ai` from the
  installation file since 0.1.121 ([hive-config-update-v1](../hive-config-update-v1.md) §4). The
  environment: `JSR_LOG_LEVEL` (default `info`) and `JSR_ROUTER_NAME`. Timers are constants: HELLO
  10 s, dead 40 s, heartbeat 5 s, stale 30 s (`DEFAULT_*` in `src/config/mod.rs`).
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
  lower UUID connects. A link that ends, however it ends, takes the sibling out of the router's
  table, so a later look can link them again; until 0.1.124 one that ended on a read error stayed
  there, and the two were not linked again until that router restarted (A-203). Two links from
  one sibling at once register it once (A-208, since 0.1.132). What is fixed in that mode and what
  is still a known limit: [routing-and-policy.md](../reference/routing-and-policy.md) §10.
- **WAN** (gateway only): TCP on `wan.listen` when set; there is no default address in code. The
  motherbee template sets `0.0.0.0:9000` (`packaging/hive.yaml.example`); spokes have no
  `wan.listen` and connect out to `wan.uplinks`. There is no HTTP or status port.

## WAN between hives

- `wan.mtls`: `required`, `permissive` or `disabled`. An absent, empty or unknown value is
  `required`, an unknown one logged; `disabled` is logged as a warning (`wan_mtls_mode_from`,
  FINDINGS A-178: fail-closed as `identity.sync.auth` is, without its `off` and `none`, which
  here are unknown values and so `required`). With
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
- **Each step of the handshake has 30 s** (`WAN_HANDSHAKE_TIMEOUT`, since 0.1.124): the TCP
  connection, the TLS handshake, the peer's `HELLO`, on the side that dials and on the side that
  accepts. A peer silent in one (a black hole) no longer holds the uplink, or an accepted socket
  on the motherbee, until TCP gives up: the link ends with `wan handshake: <step> took over 30 s`
  (in `wan connect failed`, `wan uplink error` or `wan connection error`), and an uplink retries
  as after any failed attempt. The peer's `WAN_ACCEPT` closes the handshake and is otherwise
  ignored; until 0.1.124 it was counted and logged as a frame from a foreign source after every
  connection.
- **A new link's first frames** after the handshake are the router's LSA and, on the hub, its
  `WAN_REACHABILITY`: both are queued before the link goes into the gateway's WAN table, where
  another task could hand it a frame (`bring_wan_peer_up`, A-195, since 0.1.132). Until then a
  frame handed to the link in between went out before the LSA, and the peer dropped it, its sender
  not yet named.
- **A session's end (A-203, A-207).** A session ends on a clean close, a read error (a peer gone
  without TLS `close_notify`, as a router stopped for its update), an error handling a frame, a
  write error on its link (`wan peer session closed: its link no longer writes`), nothing heard
  from the peer for the dead interval, or on the motherbee the hive's removal.
  - **The dead interval** is the larger of the router's 40 s and the one the peer's `HELLO`
    declares. Every gateway sends each WAN peer its LSA every 10 s, so only a peer that is gone
    without closing (a host down, a network cut) stays silent that long:
    `wan peer session closed: nothing heard from the peer in the dead interval`, with the hive,
    the session's epoch and the interval.
  - Every end takes the hive out of the gateway's WAN table (`wan_peers`), but only the entry that
    session put there: a hive that connected again keeps its new session's. A frame for the hive
    is then answered `UNREACHABLE` `WAN_UNAVAILABLE` until it connects again, and an uplink dials
    again.
  - A read error or an error handling a frame is still logged as `wan connection error` on the
    side that accepted the link and `wan uplink error` on the side that dialed it. Until 0.1.124 a
    read error skipped that cleanup, and a silent peer kept its session, so every frame for the
    hive was lost without an answer until it connected again.
- **Multi-hop:** a hub gateway with two or more peers sends `WAN_REACHABILITY`, so a spoke can
  reach another spoke through it. A spoke accepts this only from the motherbee's gateway (rule 6
  of `policy/system.rego`), and messages from hosts learned that way never get system authority
  (`prepare_local_delivery`).
  - **When the hub sends it** (A-195, since 0.1.132): with its LSA every 10 s; to a spoke whose
    link comes up, right after its LSA; and to every spoke as soon as an LSA adds to the nodes it
    vouches for (a node new, or listed with another name or `vpn_id`; the nodes of a hive back from
    stale; or another hive's node whose UUID this hive no longer lists, which the hub vouches for
    again). The same nodes in another order are no change. A spoke drops a relayed frame whose
    sender the hub has not vouched for (`router dropped a WAN frame`), and the hub handles a
    spoke's frames in order, so every other spoke has the advertisement before the first frame the
    hub relays from a node that LSA added. Until 0.1.132 only the 10 s beacon sent it: a frame from
    a node that had just connected on another spoke, or any for up to 10 s after a spoke's link
    came up, was dropped without an answer. A node gone waits for the next advertisement, the
    beacon's at the latest: no frame waits for it (a spoke that routes to it meanwhile gets
    `NODE_NOT_FOUND` from the hub), and a node that restarts costs each spoke one advertisement, not
    two. A spoke takes each advertisement for the hub's whole set, so the hub builds and queues
    them one at a time, and each spoke gets them in the order they were built. It goes with no node
    too, so a spoke whose last vouched-for node is gone drops it; until 0.1.132 none went then, and
    the spoke kept routing to that node through the hub until its entry expired.
  - **Names on the hub** (A-265, since 0.1.132): the hub rebuilds its forwarding table as it
    applies an LSA, before the advertisement goes, so a frame another spoke sends by name to a node
    that LSA added goes on at once. Until 0.1.132 only its copy of the LSA changed, which a frame
    by UUID reads, and one by name got `NODE_NOT_FOUND` from the hub until its next rebuild, at a
    message of one of its own nodes or within 5 s.
  - **Errors across the hub** (A-245, since 0.1.132): a message between two spokes passes the
    hub both ways, and the far spoke's router answers it over the hub's link. The hub hands that
    `UNREACHABLE` or `TTL_EXCEEDED` on toward the spoke whose LSA lists the sender, one hop of TTL
    less, when the far router made it itself; never back where it came from, and never twice
    (`relay_far_answer`). Until 0.1.132 the hub gave it only to its own nodes, and a sender on
    another spoke waited out its own timeout, or, as SY.timer does when no answer comes in 30 s,
    counted the message delivered. An answer the router does not hand on is counted and logged, at
    most every 30 s, as `router dropped another router's answer from the WAN: it is for none of its
    nodes, and it does not hand it on`, with its trace_id and the reason:
    `not_made_by_the_peer_router`, `no_ttl_left`, `sender_not_listed`,
    `sender_on_the_answering_hive` or `no_link_to_the_senders_hive`. One handed on gets a debug
    line.

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
  [SY.config.routes](sy-config-routes.md)). A copy that cannot go is logged (`router tap copy:
  …`) and dropped, and never answered to the original's sender, who did not send it: neither by
  the router the original entered nor by a router the copy reaches, a sibling's or another
  hive's, whatever stopped it there (a full or gone link, an unknown target, no TTL left). Until
  0.1.129 one that could not go was answered under the copy's trace_id: by the router the
  original entered when the copy was for another hive, and by any router the copy reached, whose
  answer the sender's router relayed (A-209). Only the router marks a copy (`meta.via_tap`): on a
  node's own message the router clears it before anything reads it, and logs `router cleared
  meta.via_tap on a node's own message: only the router marks a tap copy` at most once every 30 s
  per node, so that message is tapped, copied to cognition and answered as any other.
- **Errors to the sender:** `UNREACHABLE {original_dst, reason}` with reasons `NODE_NOT_FOUND`,
  `VPN_BLOCKED`, `PEER_UNAVAILABLE`, `WAN_UNAVAILABLE`, `GATEWAY_UNAVAILABLE`, `OPA_ERROR`,
  `OPA_NO_TARGET`, `FRAME_TOO_LARGE`, `LINK_BUSY`; and `TTL_EXCEEDED` when the TTL runs out (every
  IRP and WAN hop decrements it). A router of another hive answers over the link the message came
  in on, and between two spokes the hub hands its answer on (Multi-hop, above; A-245).
  - **`FRAME_TOO_LARGE`** (since 0.1.124, FINDINGS A-202): the sender's frame passed its SDK's
    128 KiB check, but the router's additions took it over the limit (the sender's name, the action
    class, a resolve's trace entry; an agent's memory package is trimmed to fit). The router does
    not queue it for that link, local node, sibling router or WAN peer, and answers the sender over
    the link the message came in on, a broadcast once. It logs `router did not queue a frame over
    the size limit (FRAME_TOO_LARGE)` with the link, the destination, the size and the limit, at
    most once every 30 s (`frame_over_limit`). Until 0.1.124 the link's writer skipped the frame and
    the sender waited out its own timeout. SY.admin, SY.identity, SY.vault and SY.orchestrator keep
    their replies 1 KiB under the frame for these additions (A-198), so they never get it.
  - **A broadcast goes everywhere or nowhere:** its router measures its largest frame (a local
    node's, with the action class) before it sends it anywhere, so the other routers it would
    reach never answer it again (`broadcast_over_limit`).
  - **A link whose writer has stopped** counts as no link: `PEER_UNAVAILABLE`,
    `GATEWAY_UNAVAILABLE` or `WAN_UNAVAILABLE` for a sibling router or a hive
    (`send_to_peer_router`, `send_to_wan_peer`); for a local node, as for a node that left,
    `NODE_NOT_FOUND` to anything but a broadcast (`deliver_locally`; A-203).
  - **`LINK_BUSY`** (since 0.1.129, FINDINGS A-222): the link the message would take has its
    queue at the senders' bound (below). The message is not queued and its sender is answered at
    once, over the link it came in on; a broadcast goes to every other link, and its sender is
    answered once, by the router it entered.

### The link queue

Every frame for a link, a node's, a WAN peer's or a sibling router's, waits in that link's queue
until its writer hands it to the socket (`src/router/link.rs`, A-222, since 0.1.129).

- **The bound:** 1,024 frames or 8 MiB (`LINK_QUEUE_MAX_FRAMES`, `LINK_QUEUE_MAX_BYTES`). A
  sender's message gets 960 frames or 7 MiB of it (`LINK_QUEUE_SENDERS_FRAMES`,
  `LINK_QUEUE_SENDERS_BYTES`). The rest is for the router's own frames: its answers, LSAs,
  `WAN_REACHABILITY`, handshakes, another router's answer relayed to a node or, by the hub, toward
  another spoke. So senders that keep a link at their bound never crowd them out: the MiB they leave
  takes five LSAs at their largest.
- **The bytes are the heap a frame holds:** its buffer's capacity, not its length. The router
  builds frames with `serde_json::to_vec`, which can leave up to about twice the length allocated
  (a long string takes its exact size, then the buffer doubles for what follows), so a frame
  counts what it holds. It claims that number as it takes its place, and its writer gives the same
  number back as it takes the frame. Counted by length, a stuck link would hold about twice its
  8 MiB.
- **Full:** a sender's message is refused and answered `LINK_BUSY`; one of the router's own frames
  is skipped once the whole bound is taken, and no one is answered. A frame the queue took is never
  dropped to make room, and the node or peer is never disconnected for it: as its peer reads, the
  queue drains and takes messages again. Until 0.1.129 the queue had no bound, and a node that
  stopped reading without closing kept every frame for it in the router's memory.
- **Why this size:** a healthy link's queue is near empty, its writer handing each frame on as it
  comes. Before it grows at all, the node's SDK holds 256 messages of its own and the socket's
  kernel buffer is full. Against that, the bursts a link sees are small: an LSA per WAN peer every
  10 s (at most about 90 KiB at the LSA region's 1,024 nodes), one stream batch in flight per sink
  and stream (96 KiB), one frame per link for each broadcast. 8 MiB is 64 frames at the 128 KiB
  limit in buffers of their size, 32 in buffers twice that, and a stuck link costs the router no
  more than 8 MiB of heap, plus the frame its writer is on.
- **The line:** `router did not queue frames for a link whose queue is full (LINK_BUSY): senders
  answered, its own frames skipped; the link stays up`, with `link` (`node`, `wan`, `irp`),
  `peer` (the node's name, the hive, the sibling's UUID), `queued_frames`, `queued_bytes` (the
  heap they hold), the bounds, `refused` and `skipped` since the last line and `refused_total` for
  the process, at most once per link every 30 s.
- **The stream** is a node of its router: a batch for a full link, SY.storage's or a WAN peer's, is
  answered `LINK_BUSY` like any node's message, and the stream retries it as after a lost ack
  (every 0.5 to 8 s), its records kept on disk meanwhile.
- **A WAN link whose writer is stuck** (its peer or the network takes nothing) delivers nothing,
  LSAs included: the peer cuts the session after its dead interval, as for a silent peer (A-207),
  and meanwhile, once the queue is at the senders' bound, senders hear `LINK_BUSY` at once instead
  of queueing into a link that will end.

### System authority

32 actions are protected (`PROTECTED_SYSTEM_ACTIONS` in `src/router/system_policy.rs`: node
lifecycle, `CONFIG_GET`/`CONFIG_SET`/`CONFIG_CHANGED`, `SYSTEM_UPDATE`, edge commands, ...).
A message's `meta.msg` names one whatever its `msg_type`, in any case and with spaces around it
(`protected_system_action`, FINDINGS A-100): `config_get` sent as `admin` is CONFIG_GET. At local
delivery the router asks the baked `policy/system.wasm` whether the sender may perform the action,
by its canonical name (`authorize_system`); a denial drops the message with
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
  - `cognition.turns`: messages that local `AI.*` agents send or receive with a thread id; one
    an agent receives only once its link has taken it (A-221, since 0.1.129);
  - `STREAM_APPEND` from nodes: only `storage.usage` (LLM usage records), with SY.cognition
    accepted only on motherbee and worker hives and SY.frontdesk.gov, SY.architect and SY.admin
    only on the motherbee (`subject_allowed`). A refused append is counted. SY.cognition's
    `storage.cognition.*`, taken until 0.1.103, is refused since: its state lives in its own file
    ([cognition-hands-v1](../cognition-hands-v1.md) §11).
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
  - **A record too big for a batch** (for a turn, a copy over about 95 KiB) never reaches its
    sink: it is skipped and counted as `oversize`, and named in the journal, `router stream
    dropped a record too big for a batch; its sink never gets it`, with the sink, the subject, the
    producer, the trace_id and thread_id it carries, and its size against the most it could have
    had (A-253, since 0.1.132). At most 10 are named between two alarm lines
    (`STREAM_OVERSIZE_NAMED_PER_INTERVAL`), each line of a few hundred bytes, about 3 KiB at most:
    the producer and the ids come quoted and escaped, each cut at 128 bytes with its full length
    after the quotes. The others are only counted, and the alarm says how many (below). Until
    0.1.132 they were only counted.
  - **Who sends one:** a frame may be up to 128 KiB, so such a turn reaches its node but neither
    `turns` nor cognition. The Rust SDK normally sends a text over about 62 KiB as a blob, and so
    do the IO nodes, so their turns stay under it. A WF's text can reach it, since the Go SDK has
    no blob offload, and so can a WF's large `meta.context` (its resolved `vars`) with a short
    text, a payload that is not text, and a raw client. SY.timer cannot: its messages are system
    ones, never copied. The later fix is the Go SDK's text offload (FINDINGS A-264).
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

Each record lost as `oversize` is also named in a line of its own, `router stream dropped a record
too big for a batch; its sink never gets it` (above), at most 10 between two alarm lines; when
there were more, the alarm adds `router stream did not name N records too big for a batch in the
last 30 s: at most 10 are named every 30 s, and every one is in the oversize count` (A-253, since
0.1.132). `lab/ops.py health` sums neither: their records are in the `oversize` count already.

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
  invalid, when another process holds its instance lock, when the baked system policy cannot
  load, when its own SHM region or a socket cannot be created. The orchestrator's 5 s watchdog also restarts it when the unit is not active.
- Missing shared regions (config, LSA, OPA, identity, memory) do not stop it: it reopens them
  when they appear or when their heartbeat is older than 30 s. Without the config region a hive
  routes with no static routes, no taps and everything in VPN 0. The identity region is opened
  afresh on every read (`read_identity_snapshot`), so the router follows a region SY.identity
  created again (its limits changed in `hive.yaml`) at once; that region is empty until
  SY.identity's start-up rebuild fills it.
- A node frame over 128 KiB disconnects that node (`FRAME_TOO_LARGE`).
- A node whose link no longer writes (a write error stopped its writer) is disconnected like one
  that closed (`router: the node's link no longer writes; disconnecting`, A-203): it leaves the
  node table and the SHM, and a message for it is answered `NODE_NOT_FOUND`.
- A node that stops reading without closing (hung, or stuck behind its own full queue) stays
  connected: its link's queue fills to its bound, and each message for it is answered `LINK_BUSY`
  until it reads again (A-222; the link queue above).
- A peer with a bad certificate fails its handshake (`wan connection error` /
  `wan uplink error`); the router keeps running. A silent peer's session ends after the dead
  interval, 40 s, and its LSA turns stale (A-207).
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
