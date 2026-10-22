# Message protocol: reference

**Status:** the code on 2026-10-17 (0.1.79 and the cleanup that followed it).
**Audience:** developers of a node: in Rust with `crates/fluxbee_sdk`, in Go with
`go/fluxbee-go-sdk`, or in any language that speaks the router's socket.

This page describes the protocol between nodes and routers as the code implements it. Every
statement names the code that backs it. `docs/history/02-protocolo.md` (Spanish) is older and partly out of
date; where it disagrees with this page, the code and this page win.

Main sources: `crates/fluxbee_sdk/src/protocol.rs` (envelope and constants),
`crates/fluxbee_sdk/src/socket/connection.rs` (framing), `crates/fluxbee_sdk/src/node_client.rs`
and `split.rs` (the connection), `crates/fluxbee_sdk/src/rpc.rs` (request and response),
`src/router/mod.rs` (what the router does) and `go/fluxbee-go-sdk/` (the Go mirror). A function
named without a path in §2–§5 lives in `src/router/mod.rs`.

---

## 1. Transport

### 1.1 The socket

- Each router listens on a Unix stream socket in `/var/run/fluxbee/routers` (`ROUTER_SOCKET_DIR`,
  `src/paths.rs`) named `<router uuid without hyphens>.sock` (`node_socket_path`,
  `src/config/mod.rs`), mode `0666` (set in `src/router/mod.rs`).
- The same directory holds `irp-<uuid>.sock` (mode `0600`): sibling routers of a hive talk over it.
  Nodes never use it.
- `NodeConfig.router_socket` names one socket or the directory. Given the directory, both SDKs list
  the `*.sock` files, skip `irp-*`, sort them and try them in turn, starting at
  `fnv1a64(<node name without @hive>) % count` (`router_socket_candidates` in `node_client.rs`;
  `routerSocketCandidates` in `client.go`). The router a node landed on is named in its ANNOUNCE.

### 1.2 Framing

One message is one frame: a 4-byte big-endian unsigned length, then that many bytes of UTF-8 JSON
holding one `Message`. The length does not count its own 4 bytes (`read_frame`, `write_frame` in
`socket/connection.rs`; `ReadFrame`, `WriteFrame` in `go/fluxbee-go-sdk/socket.go`). Every link
uses the same framing: node to router, router to sibling router, hive to hive.

### 1.3 Size limit: 128 KiB

`MAX_FRAME_SIZE = 128 * 1024` bytes of JSON (`socket/connection.rs`; `maxFrameSize` in
`socket.go`). Both sides check the length before reading or writing the body.

| Who has the big frame | What happens | Code |
| --- | --- | --- |
| A Rust node sending | `NodeSender::send` refuses it with `NodeError::MessageTooLarge` and logs `FRAME_TOO_LARGE`; nothing is queued. A `text` payload is offloaded to a blob before this check (§8). | `split.rs` |
| A Go node sending | `NodeSender.Send` refuses it with `*MessageTooLargeError` and logs `FRAME_TOO_LARGE`; nothing is queued and the connection stays up. | `client.go` |
| The router, reading from a node | The read fails; the router logs `FRAME_TOO_LARGE — node sent oversized frame; disconnecting`, ends the connection and unregisters the node. | `handle_node` |
| The router, writing a frame it enlarged on the way (stamps, memory package) | The frame is skipped and counted, with at most one warning per link every 30 s; the link stays up. Neither side is told. | `write_link_frames` |

---

## 2. Joining the router

### 2.1 Name and UUID

`NodeConfig` (`node_client.rs`; the Go `NodeConfig` in `client.go` has the same fields):

| Field | Meaning |
| --- | --- |
| `name` | The node's L2 name, `KIND.name` (`AI.sales`). The SDK appends `@<hive_id>` unless the name has an `@`. A node launched by the orchestrator gets it from `FLUXBEE_NODE_NAME` (`managed_node_name`, `crates/fluxbee_sdk/src/managed_node.rs`). |
| `config_dir` | Where `hive.yaml` is. The SDK reads `hive_id` from it and fails without it. |
| `uuid_mode` | `Persistent`: the UUID is read from, or created in, `<uuid_persistence_dir>/<name without @hive>.uuid`. `Ephemeral`: a new UUID per process, no file. |
| `uuid_persistence_dir` | The SY nodes pass `/var/lib/fluxbee/state/nodes` (for example `src/bin/sy_admin.rs`). |
| `router_socket` | A socket or the socket directory (§1.1). |
| `version` | Sent in HELLO. The router does not check it. |

### 2.2 HELLO and ANNOUNCE

The first frame on a connection must be a HELLO; the router answers with an ANNOUNCE.

```json
{"routing": {"src": "<node uuid>", "dst": null, "ttl": 1, "trace_id": "<new uuid>"},
 "meta": {"type": "system", "msg": "HELLO"},
 "payload": {"uuid": "<node uuid>", "name": "AI.sales@worker1", "version": "0.1.0"}}

{"routing": {"src": "<router uuid>", "dst": "<node uuid>", "ttl": 1, "trace_id": "<the HELLO's trace_id>"},
 "meta": {"type": "system", "msg": "ANNOUNCE"},
 "payload": {"uuid": "<node uuid>", "name": "AI.sales@worker1", "status": "registered",
             "vpn_id": 0, "router_name": "RT.gateway@worker1"}}
```

The payloads are `NodeHelloPayload {uuid, name, version}` and
`NodeAnnouncePayload {uuid, name, status, vpn_id, router_name}` (`protocol.rs`, built by
`build_hello` and `build_announce`; the same types in `protocol.go`).

`handle_node` checks, in this order. A failure closes the connection without an ANNOUNCE.

1. The frame is a system message with `msg` `HELLO` (error `expected HELLO`).
2. `routing.src` and `payload.uuid` parse as UUIDs and are equal (`uuid mismatch`).
3. A name without `@` gets `@<router hive>` (`normalize_name`). A name starting with `RT.` is
   accepted only from the router's own process, checked by the socket's peer PID
   (`rt_name_allowed`).
4. One live connection per UUID (`claim_node_uuid`). If another connection holds the UUID, the
   router waits up to 1 s (`DUPLICATE_HELLO_GRACE`) for it to go, then refuses the new one and logs
   `HELLO rejected: this node UUID already has a live connection`. The old connection keeps its
   route.

Then the router assigns the VPN (`assign_vpn`: `SY.*` and `RT.*` names get 0; any other name takes
the first matching VPN rule of the hive config by priority, else 0), registers the node in its
shared memory and sends the ANNOUNCE. `name` is the name it registered, `router_name` the router's
own L2 name. Both SDKs take the name, VPN and router from it (`update_from_announce` in `split.rs`;
`SetAnnounce` in `client.go`).

### 2.3 A node speaks only as itself

After the handshake every frame must carry the node's UUID in `routing.src`. A frame with any
other `src` is dropped with the warning `router dropped a message whose routing.src is not the
sending node` (`frame_is_from_connection`); the sender is not told. A frame that does not parse as
a `Message` is logged (`received invalid message frame`) and skipped; the connection stays up.

### 2.4 WITHDRAW and the end of a connection

`NodeSender::close` (Rust), `NodeSender.Close` and `RouterDispatcher.Close` (Go) send WITHDRAW
(`build_withdraw`: `dst` null, `ttl` 1, payload `WithdrawPayload {uuid}`). The router handles
WITHDRAW, end of file and a read error alike: it unregisters the node and closes its side.

Send WITHDRAW right before the process exits. Neither SDK stops reconnecting because of it: the Rust
connection manager stops only when every `NodeSender` clone is dropped (`tx_queue_closed`, and the
dispatcher holds one), the Go one never stops.

### 2.5 Reconnection

Both SDKs reconnect by themselves and redo HELLO and ANNOUNCE with the same UUID. They differ:

| | Rust (`connection_manager_loop`, `node_client.rs`) | Go (`connectionManager`, `client.go`) |
| --- | --- | --- |
| Delay | After a connection that worked, the first attempt is immediate; failed attempts wait 100 ms, doubling up to 30 s | 500 ms before every attempt |
| Frames sent while the link is down | **Dropped**: the send queue (256) is emptied before every connection attempt (`drain_tx_queue`), yet `send` returned `Ok` | Kept in the 256-slot queue and written after the reconnect; `Send` fails with `sender queue full` when the queue is full |
| Link state | `NodeSender::is_connected()`, `wait_connected()` | `RouterDispatcher.IsConnected()` |
| Requests in flight | Failed with `RpcError::Disconnected` (`drain_pending_waiters`) | Failed with `RpcErrDisconnected` (`failAllPending`) |
| First connection | `RouterDispatcher::connect_with_retry` retries every `delay`, forever; no handshake timeout. A refused HELLO later in life is logged as `router handshake failed; retrying` | `ConnectWithRetry` retries every `delay` (returns the error when `delay <= 0`); 5 s handshake timeout. That first connection is closed and the manager opens another: a Go node sends two HELLOs at start |

So: a request is refused at once while the link is down (`send_with_matcher` checks the link
first), but a plain `send` during an outage is lost in Rust. That is why
`fluxbee_sdk::stream::append` waits for the link before sending (§6.6).

---

## 3. The envelope

### 3.1 Message

```json
{"routing": {...}, "meta": {...}, "payload": <any JSON>}
```

`Message {routing: Routing, meta: Meta, payload: Value}` (`protocol.rs`). The router's parser
requires `routing.src`, `routing.dst` (present, possibly `null`), `routing.ttl`,
`routing.trace_id` and `meta.type`; a frame without one is skipped (§2.3). `payload` becomes `null`
when absent.

Unknown keys are ignored when parsing (`protocol.rs` has no `deny_unknown_fields`, and Go's
`encoding/json` ignores them). The router parses every frame into `Message` and serializes it again
to forward and to deliver, so **a key that is not a field of `Routing` or `Meta` never reaches the
destination**. Put your own data in `payload` or `meta.context`.

### 3.2 routing

| Field | Type | Meaning |
| --- | --- | --- |
| `src` | string | The sender's UUID; must be the UUID of the connection (§2.3). Unchanged end to end, so a reply goes to it. |
| `src_l2_name` | string, optional | The sender's L2 name, **set by the router** on delivery (§5.1). Senders leave it out; what they put is overwritten. Read it with `Message::source_l2_name()` (Go `SourceL2Name()`). Absent on messages the router itself generates: ANNOUNCE, UNREACHABLE, TTL_EXCEEDED. |
| `dst` | string or null | Where to (§3.3). |
| `ttl` | integer 0–255 (`u8`) | Router-to-router hops left (§3.4). |
| `trace_id` | string | Correlation id; a reply copies the request's. A broadcast needs a UUID here. |

### 3.3 dst

| `dst` | Resolution |
| --- | --- |
| `"<uuid>"` | A node of this router, then of a sibling router, then of another hive the router knows (its LSA, then the hub's reachability advertisement); else UNREACHABLE `NODE_NOT_FOUND` (`handle_message`). |
| `"KIND.name@hive"` | Exact match on the full names the router knows (its nodes, its siblings' nodes, other hives' nodes), plus the hive's static routes, which forward a pattern to a hive or drop it silently (`resolve_by_name`; node entries are `MATCH_EXACT` in `rebuild_fib`). Always write the `@hive`. |
| `"KIND.name@*"` | The sender's own hive first, then the first match on any hive (`resolve_by_name`). |
| `"broadcast"` | Every node of this router except the sender, filtered by VPN and by `meta.target` when set (a pattern where `*` matches anything, else an exact name; `pattern_match`). With `ttl` ≥ 2 it also goes to the sibling routers, with `ttl` ≥ 3 to other hives through the gateway. A `trace_id` that is not a UUID drops it; a router takes a given broadcast `trace_id` once per 30 s (`BroadcastCache`). |
| `null` | The policy chooses (below). |

Only the lowercase string `broadcast` means broadcast; any other string is a unicast label
(`deserialize_dst`; Go `Destination.UnmarshalJSON`).

**`dst: null`** (`resolve_target_with_identity`):

1. No `meta.src_ilk` (absent or blank): the system policy (`frontdesk_route`,
   `policy/system.rego`) sends the message to the identity frontdesk, `SY.frontdesk.gov@<hive>`
   unless `hive.yaml` `government.identity_frontdesk` names another (`src/config/mod.rs`).
2. A `src_ilk` whose registration status in the identity shared memory (`/jsr-identity-<hive>`) is
   `temporary`: the frontdesk too.
3. Otherwise the operator's OPA policy picks the target. Its input is
   `{meta, routing: {src, src_l2_name}, action}`, where `action` is `meta.msg` (`build_opa_input`,
   `src/opa.rs`), with identity data from the shared memory. No policy loaded or an evaluation
   error gives UNREACHABLE `OPA_ERROR`; no target gives `OPA_NO_TARGET`.

The chosen name is then routed like a named unicast.

### 3.4 ttl

- A message that arrives with `ttl` 0 is answered with TTL_EXCEEDED.
- Every hop to another router (a sibling or another hive) subtracts 1, and a router forwards only
  when `ttl` ≥ 2, else it answers TTL_EXCEEDED. Delivery to a node of the same router costs nothing.
- The SDK uses 16 for requests and replies (`send_system_rpc`, `send_admin_rpc`, the status reply)
  and 1 for HELLO, ANNOUNCE, WITHDRAW and STREAM_APPEND, which never leave the router. Go's
  `SendSystemRPC` uses 16 too unless `Options.TTL` sets another; `BuildSystemRequest` alone turns a
  0 `ttl` into 1.

### 3.5 meta

`Meta` (`protocol.rs`; Go `Meta` in `protocol.go`). Every field but `type` is optional and left
out when empty.

| Field | Set by | Meaning |
| --- | --- | --- |
| `type` | sender | The message kind (§4). |
| `msg` | sender | The verb or message name: `HELLO`, `CONFIG_SET`, `ADMIN_COMMAND`... On system messages the router acts on it (§5, §7). |
| `src_ilk` | sender | ILK (`ilk:<uuid>`) of the interlocutor who speaks (§9.3). The router reads it only for `dst: null`. |
| `dst_ilk` | sender | ILK of the interlocutor spoken to. |
| `ich` | sender | The channel of the communication (§9.2). |
| `thread_id` | sender | The communication, `thread:sha256:<hex>` (§9.1). The router keys memory packages and cognition copies on it. |
| `memory_package` | **router only** | Memory of the communication for an `AI.*` destination (§5.3). Removed from anything a sender sets. |
| `scope` | sender | `vpn` or `global` (`SCOPE_VPN`, `SCOPE_GLOBAL`). `global` on a system message lifts the VPN check (`vpn_allows_between`). |
| `target` | sender | The broadcast filter (§3.3), also visible to the OPA policy. By convention the destination's name on vault and admin requests and `node_config_control` on CONFIG_* messages. Never used to route a unicast. |
| `action` | sender | A verb beside `msg`, by convention: the action of an ADMIN_COMMAND, the verb of `command` and `query` kinds, `bootstrap` on the vault's startup broadcasts (`VAULT_BOOTSTRAP_ACTION`). The router does not read it. |
| `action_class` | SDK helpers; the router when absent | `send_message`, `read`, `write`, `system_config`, `topology_change`, `external_action`, `identity_change`, `workflow_step` or `node_lifecycle` (`ActionClass`, `crates/fluxbee_sdk/src/policy.rs`). On delivery the router sets `send_message` on every kind except `system`, `admin` and `query_response` (`classify_routed_message`). |
| `action_result` | responder | `applied`, `accepted`, `blocked` or `failed` (`ActionResult`). SY.admin and SY.orchestrator set it on their responses (`derive_action_outcome`). |
| `result_origin` | responder | `node` when the action was blocked (`derive_action_outcome`). |
| `result_detail_code` | — | Defined in both SDKs; nothing sets it. |
| `priority` | sender | A free hint, visible to the OPA policy; the router does not act on it. |
| `context` | sender | Free JSON for the receiver and the OPA policy. IO nodes put the channel facts in `context.io`; the AI SDK reads `response_envelope` and `final_response_contract` from it (`crates/fluxbee_ai_sdk/src/message.rs`). |
| `via_tap` | **router only** | `true` on a tap copy, the copy a router sends to a monitoring target; such a copy is never tapped again. Serialized only when true. Missing from the Go `Meta`. |

### 3.6 Removed fields

- `meta.thread_seq` left the protocol in 0.1.78 (commit `104fa65`, Rust and Go): the order of a
  communication is the turn number SY.cognition assigns.
- `meta.ctx`, `meta.ctx_seq`, `meta.ctx_window` and their `CtxTurn` type left both SDKs in the
  latest cleanup (commit `41decbd`).

Do not send them. No code reads them, and by §3.1 the router drops them on the way.

### 3.7 payload

Any JSON, agreed between sender and receiver. The router reads it only on the system messages it
consumes itself (HELLO, STREAM_APPEND, OPA_RELOAD). Conversational text uses `text/v1` (§8).

---

## 4. Message kinds (`meta.type`)

| Kind | Used for | Router treatment |
| --- | --- | --- |
| `system` | The control plane: handshake, errors, config, status, vault, identity, SY services. `meta.msg` is the verb. | Compared case-insensitively (`is_system_kind`). Protected verbs are gated (§7). Not copied to the stream, no memory package. |
| `admin` | `ADMIN_COMMAND` and `ADMIN_COMMAND_RESPONSE` with SY.admin (§6.5). | Not copied to the stream, no memory package, no `action_class` stamp. |
| `user` | Conversation turns: what IO nodes deliver (`build_user_message`, `nodes/io/common/src/router_message.rs`) and what agents answer. | A turn (§5.2). |
| `command`, `command_response`, `query`, `query_response` | A verb in `meta.action`. SY.admin uses them toward SY.wf-rules (`send_l2_action_request`, `src/bin/sy_admin.rs`); Go has `BuildCommandResponse` and `BuildQueryResponse`. | `query_response` is treated like `admin`; the other three are turns. |

Any other string is accepted and treated as a turn. Send `system` in lowercase: the router
compares it case-insensitively, but the SDK reply matchers (`RouteMatch`, §10) compare it exactly.

---

## 5. What the router does on the way

### 5.1 Stamps

- `routing.src_l2_name`: from a local node, the name its connection registered
  (`handle_message`); from a sibling router, the name in that router's node table
  (`handle_peer_message`); from another hive, `<role>@<the peer hive authenticated by mTLS>`,
  whatever name the frame carried (`canonical_wan_src_name`). This is the field to authorize on.
- `meta.action_class` when absent (§3.5).
- `meta.memory_package` (§5.3).

### 5.2 Copies to the router stream

Each router keeps a short, best-effort log, its stream (`docs/router-stream-v1.md`). The router a
message enters by copies it there when its kind is not `system`, `admin` or `query_response`
(`should_publish_turn`; subject `storage.turns`, delivered to `SY.storage@motherbee`). When an
`AI.*` node sends or receives a turn that has a `thread_id` and is neither a broadcast nor a tap
copy, the agent's router also copies it for the hive's SY.cognition (`is_communication_turn`,
subject `cognition.turns`). Nodes never see these copies.

### 5.3 Memory package

When the router delivers to a node whose name starts with `AI.` (any case) a message whose kind is
a turn and that has a `thread_id`, it attaches the package it finds for `(thread_id, that node)` in
`/jsr-memory-<hive>`, which SY.cognition writes (`deliver_locally`, `package_for`). A package the
sender put is removed for every destination (`prepare_local_delivery`). The fields are
`MemoryPackage {package_version, thread_id, dominant_context, dominant_reason, contexts, reasons,
memories, episodes, truncated}` (`protocol.rs`); see [the cognition reference](cognition.md).

### 5.4 VPN and edge gates

`vpn_allows_between` decides every delivery:

- When either side is `SY.edge`: EDGE_* commands only from `SY.admin@motherbee` and their
  responses only back to it; between SY.edge and an `IO.*` node only non-system kinds; between
  SY.edge and SY.vault only `VAULT_GET` and `VAULT_GET_RESPONSE`. Anything else is `VPN_BLOCKED`.
- A system message with `scope: global` passes.
- A message to or from a `SY.*` or `RT.*` node passes.
- Otherwise both nodes need the same `vpn_id`, else UNREACHABLE `VPN_BLOCKED`.

### 5.5 Messages the router keeps

Never delivered to a node: the handshake HELLO, WITHDRAW, STREAM_APPEND (§6.6) and OPA_RELOAD,
with which SY.opa.rules tells the routers to load a new operator policy (the router passes it to
its siblings when `ttl` ≥ 2).

---

## 6. System messages a node meets

### 6.1 UNREACHABLE and TTL_EXCEEDED

A router sends them back to the sender of a message it cannot deliver; a router of another hive
sends them back over the same links. Envelope: `src` the router's UUID, `dst` the sender's UUID,
`ttl` 16, the failed message's `trace_id`, kind `system` (`build_unreachable`,
`build_ttl_exceeded`, `protocol.rs`).

| `msg` | Payload | Values |
| --- | --- | --- |
| `UNREACHABLE` | `UnreachablePayload {original_dst, reason}` | `original_dst`: the failed `dst` as a string (`broadcast`, or `null` for `dst: null`). `reason`: `NODE_NOT_FOUND`, `VPN_BLOCKED`, `PEER_UNAVAILABLE` (the sibling router is gone), `GATEWAY_UNAVAILABLE`, `WAN_UNAVAILABLE`, `OPA_ERROR`, `OPA_NO_TARGET`. |
| `TTL_EXCEEDED` | `TtlExceededPayload {original_dst, last_hop}` | `last_hop`: `router:<router uuid>`. |

The RPC helpers turn them into `RpcError::Unreachable` and `RpcError::TtlExceeded` (§10.4). Some
drops answer nothing, and a request then times out: a foreign `src` (§2.3), a protected verb from
an unauthorized sender (§7), a broadcast with a non-UUID `trace_id`, a drop route, a frame the
router enlarged past the limit (§1.3).

### 6.2 NODE_STATUS_GET

When SY.orchestrator reports the status of a RUNNING node (`get_node_status_flow`,
`src/bin/sy_orchestrator.rs`), it asks the node with `NODE_STATUS_GET` and waits 2 s
(`NODE_STATUS_FORWARD_TIMEOUT_SECS`); it reads `health_state`, `last_error` and `extensions` from
the payload of the answer. It is a protected verb (§7).

- Rust default handler: `try_handle_default_node_status(sender, incoming)`
  (`crates/fluxbee_sdk/src/status.rs`) answers `NODE_STATUS_GET_RESPONSE` to `routing.src` with the
  same `trace_id`, `ttl` 16 and payload `{"status": "ok", "health_state": "HEALTHY"}`.
  `NODE_STATUS_DEFAULT_HEALTH_STATE` may say `DEGRADED`, `ERROR` or `UNKNOWN`;
  `NODE_STATUS_DEFAULT_HANDLER_ENABLED=false` turns the handler off. It returns `true` when it
  answered.
- Go: `BuildDefaultNodeStatusResponse`, and `BuildManagedNodeStatusResponse`, which takes the
  health from the config state (`CONFIGURED` is `HEALTHY`, anything else `DEGRADED`) and adds
  `last_error` (`go/fluxbee-go-sdk/status.go`).

### 6.3 CONFIG_GET, CONFIG_SET, CONFIG_RESPONSE

A node's live config: the AI, IO and WF nodes the orchestrator manages, and SY nodes such as
SY.storage, SY.cognition and SY.identity. Only `SY.admin@motherbee` can send CONFIG_GET and
CONFIG_SET (§7); its actions `node_control_config_get` and `node_control_config_set` send them and
return the node's CONFIG_RESPONSE.

Request (`build_node_config_get_message`, `build_node_config_set_message`,
`crates/fluxbee_sdk/src/node_config.rs`): kind `system`, `msg` `CONFIG_GET` or `CONFIG_SET`,
`meta.target` `node_config_control`, `meta.action` the same verb, `dst` the node's name. The
builders take `ttl` from `NodeConfigEnvelopeOptions` and raise 0 to 1.

| Payload | Fields |
| --- | --- |
| `NodeConfigGetPayload` | `node_name`; optional `request_id`, `contract_version`, `requested_by`; extra keys are kept |
| `NodeConfigSetPayload` | `node_name`, `schema_version`, `config_version`, `apply_mode`, `config` (an object); optional `request_id`, `contract_version`, `requested_by`; extra keys are kept |

The answer (`build_node_config_response_message`) goes to the request's `src` with its
`trace_id`: `msg` and `meta.action` `CONFIG_RESPONSE`, `meta.target` `node_config_control`. A
managed node's payload (builders in `crates/fluxbee_sdk/src/managed_control_plane.rs`): `ok`,
`node_name`, `state` (`UNCONFIGURED`, `CONFIGURED` or `FAILED_CONFIG`), `schema_version`,
`config_version`, `effective_config` (secrets redacted; null unless configured), `error`
(`{code, message}` or null) and, on CONFIG_GET, `contract`: `required_fields`, `optional_fields`,
`secrets`, `notes`.

- Every node in the repo that handles CONFIG_SET accepts only `apply_mode` `replace`.
- A node writes an accepted config to its `config.json` and keeps the orchestrator's `_system`
  block (`persist_effective_config_with_root`; Go `PersistManagedConfig`).
- The version rule still differs by family, with code `stale_config_version`: the IO and WF nodes
  require a `config_version` greater than the current one (`ensure_config_version_advances`,
  `nodes/io/common/src/io_control_plane.rs`; `go/nodes/wf/wf-generic/node/config_control.go`);
  ai-generic refuses a lower one and answers an equal one already in effect with `ok`
  (`apply_config_set`, `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`).

See also `docs/node-config-control-plane-spec.md`.

### 6.4 PING and STATUS

A convention, not an SDK feature: ai-generic, io-slack, io-wapp, io-linkedhelper, SY.identity,
SY.storage and SY.cognition answer `PING` with `PONG`; ai-generic and io-slack answer `STATUS` with
`STATUS_RESPONSE`. Neither verb is protected.

### 6.5 ADMIN_COMMAND and ADMIN_COMMAND_RESPONSE

How a node asks SY.admin to act; SY.admin is the node that acts on other nodes for a user
(`policy/system.rego`, rule (4)). There is one, on the motherbee: `SY.admin@motherbee`
(`PRIMARY_HIVE_ID`, `protocol.rs`; Go `PrimaryAdminNode`).

- Request (`send_admin_rpc`, `rpc.rs`; Go `SendAdminRPC`): kind `admin`, `msg` `ADMIN_COMMAND`,
  `dst` and `meta.target` the admin, `meta.action` the action, `ttl` 16. Payload
  `{"action", "params", "request_id", "target"}`: `params` an object or null (Rust refuses anything
  else, Go sends `{}` for nil), `request_id` generated when not given, `target` (optional) the hive
  or node the action is about.
- Response (`send_admin_command_response`, `src/bin/sy_admin.rs`): kind `admin`, `msg`
  `ADMIN_COMMAND_RESPONSE`, the same `trace_id`, with `meta.action`, `action_class`,
  `action_result` and `result_origin`. Payload
  `{"status", "action", "payload", "error_code", "error_detail", "request_id"}`.
- Rust `send_admin_rpc` returns it as `AdminCommandResult` and fails only when `status` is missing
  (`parse_admin_response`): check `status == "ok"` yourself. Go returns the `Message`.
- SY.admin authorizes on the router-stamped `src_l2_name`, never on the payload
  (`handle_internal_admin_command`). Its actions: [SY.admin](../nodes/sy-admin.md).

### 6.6 STREAM_APPEND

A node appends a record to its router's stream with `fluxbee_sdk::stream::append(sender, subject,
payload)` (`crates/fluxbee_sdk/src/stream.rs`): kind `system`, `msg` `STREAM_APPEND`, `dst` null,
`ttl` 1, payload `StreamAppend {subject, payload}`. Fire and forget: there is no reply, and the
router takes it before routing whatever the `dst`. `append` waits for the link first, since a frame
sent during an outage is dropped (§2.5). The Go SDK has no helper.

The router appends it only when this producer, the name the node registered, may append this
subject (`subject_allowed`, `src/router/stream.rs`):

| Subject | Who may append |
| --- | --- |
| `storage.turns`, `cognition.turns` | Nobody: the router makes these copies itself |
| `storage.cognition.*` | `SY.cognition@<this hive>` |
| `storage.usage` | Any node, except that SY.cognition may only on a `motherbee` or `worker` hive, and SY.frontdesk.gov, SY.architect and SY.admin only on the motherbee |
| Anything else | Nobody |

A refused append is counted and logged at most every 30 s (`router refused a STREAM_APPEND: not a
subject this node may append`). The stream is best effort: a record that waits more than 120 s is
skipped (`STREAM_MAX_AGE_SECS`), and node appends get 25% of the 32 MiB on disk
(`STREAM_NODE_APPENDS_SHARE_PERCENT`, `STREAM_MAX_BYTES`).

### 6.7 Vault

SY.vault runs only on the motherbee (`VaultClient::for_primary`,
`crates/fluxbee_sdk/src/vault.rs`). A request is a system message to `SY.vault@motherbee` with `msg`
`VAULT_PUT`, `VAULT_GET`, `VAULT_GET_METADATA`, `VAULT_LIST`, `VAULT_DELETE`, `VAULT_ROTATE`,
`VAULT_ROLLBACK` or `VAULT_ASSIGN`, the caller's ILK in `meta.src_ilk` and the vault's name in
`meta.target`; the answer is `<msg>_RESPONSE`.

After each change SY.vault broadcasts `VAULT_SECRET_CHANGED` (`scope: global`, `ttl` 16) with
metadata only, never the value: `VaultSecretChangedPayload {op, resource_type, tenant_id, ilk,
assigned_ilks, general, version, key, hive_id, at_ms}` (`protocol.rs`). A consumer filters it with
`matches_interest` and reads the secret itself. When the secret's owner is an `SY.edge`, SY.vault
also sends the notice to that edge as a unicast: the router lets only that unicast into the DMZ
edge, never the broadcast (FINDINGS A-78). When SY.vault starts it broadcasts one for every
secret with `meta.action` `bootstrap`: nothing changed, so a consumer whose reaction is costly (a
restart) skips those. The Go SDK has no vault client. Details: [SY.vault](../nodes/sy-vault.md) and
`docs/history/sy-vault-spec.md`.

### 6.8 Identity

`SY.identity@<hive>` keeps ILKs (interlocutors), tenants and ICHs (channels). The verbs in
`crates/fluxbee_sdk/src/identity.rs`: `ILK_PROVISION` (an IO node creates a temporary ILK for an
unknown sender), `ILK_REGISTER`, `ILK_ADD_CHANNEL`, `ILK_UPDATE`, `ILK_SET_DEFINITION`,
`ICH_SET_ENABLED`, `TNT_CREATE`, `TNT_UPDATE`, `TNT_SET_SPONSOR`, `TNT_APPROVE`,
`IDENTITY_METRICS`; `src/bin/sy_identity.rs` also answers list, get, delete, restore and purge verbs.
These verbs are not in the router's protected list: SY.identity checks each one against its own
allowlist of senders by the router-stamped `src_l2_name` (`is_authorized`; `ILK_PROVISION` is open
to `IO.*`). Reading identity needs no message: a node reads `/jsr-identity-<hive>`
(`resolve_ilk_from_shm_name`). Details: [SY.identity](../nodes/sy-identity.md) and
[the identity reference](identity.md).

### 6.9 Defined, but not for nodes

- `LSA`, `WAN_REACHABILITY`, `WAN_ACCEPT`, `WAN_REJECT` and the WAN HELLO: between routers.
- `CONFIG_CHANGED`: from `SY.admin@motherbee` to SY.opa.rules, carrying the operator's OPA policy
  (`docs/opa-distribution.md`); protected.
- `ECHO`, `ECHO_REPLY`, `TIME_SYNC`: builders exist in `protocol.rs`, but no router or node in the
  repo emits or answers them (WF nodes ignore ECHO).

---

## 7. Protected system actions

Some system verbs act on nodes or on the hive. The router checks their sender when it delivers
them (`prepare_local_delivery`): the list is `PROTECTED_SYSTEM_ACTIONS`
(`src/router/system_policy.rs`), the decision is the baked policy `policy/system.rego`, compiled to
`policy/system.wasm`; a router whose policy does not load refuses to start
(`ensure_system_policy_loaded`). The input is the verb (`meta.msg`), the router-stamped
`src_l2_name` and the hive of the delivering router.

| Verbs | Allowed senders | Rule |
| --- | --- | --- |
| `EDGE_OPEN_URL`, `EDGE_CLOSE_URL`, `EDGE_LIST_URLS`, `EDGE_PUBLISH_BLOB`, `EDGE_UNPUBLISH_BLOB`, `EDGE_TLS_STATUS`, `EDGE_TLS_RELOAD`, `EDGE_ACME_CHALLENGE` | `SY.admin@motherbee` | (1) |
| `CONFIG_GET`, `CONFIG_SET`, `CONFIG_CHANGED`, `SYSTEM_UPDATE`, `SYSTEM_SYNC_HINT` | `SY.admin@motherbee` | (2) |
| `SPAWN_NODE`, `KILL_NODE`, `START_NODE`, `RESTART_NODE`, `REMOVE_NODE_INSTANCE`, `NODE_CONFIG_GET`, `NODE_CONFIG_SET`, `NODE_STATE_GET`, `NODE_STATUS_GET`, `LIST_NODES`, `GET_VERSIONS`, `GET_RUNTIMES`, `GET_RUNTIME`, `SYSTEM_CORE_ROLLBACK`, `ADD_HIVE_FINALIZE`, `REMOVE_HIVE_CLEANUP` | `SY.orchestrator@<any hive>`; `SY.admin@<the same hive>` | (3), (4) |
| `INVENTORY_REQUEST` | `SY.admin@<the same hive>` | (4) |
| `NODE_STATUS_GET` (in addition) | `SY.vault@<the same hive>` | (5) |

- A sender this router knows only from the hub's reachability advertisement (a spoke reached
  through the hub) never passes, whatever its name.
- An unauthorized one is dropped with the warning `router dropped protected SYSTEM action from
  unauthorized origin`. The sender gets nothing back; its request times out.
- Any other system verb passes this gate. A node that must restrict one checks `src_l2_name`
  itself, as SY.identity and SY.admin do.

For a node author: a CONFIG_SET or a NODE_STATUS_GET that reaches your node has passed this check.

---

## 8. Payloads: text/v1 and blobs

Conversational text is `TextV1Payload` (`crates/fluxbee_sdk/src/payload.rs`):

```json
{"type": "text", "content": "Hello", "attachments": []}

{"type": "text",
 "content_ref": {"type": "blob_ref", "blob_name": "content_0123456789abcdef.txt", "size": 150000,
                 "mime": "text/plain", "filename_original": "content.txt", "spool_day": "2026-10-17"},
 "attachments": []}
```

- `type` is `text`; exactly one of `content` and `content_ref`; `attachments` is a list of
  `BlobRef`, empty by default (`TextV1Payload::validate`).
- `BlobRef {type: "blob_ref", blob_name, size, mime, filename_original, spool_day}`
  (`crates/fluxbee_sdk/src/blob/mod.rs`): `blob_name` is `<name>_<hash>.<ext>`, where `name` is at
  most 128 characters from `[A-Za-z0-9._-]`, `hash` the first 16 hex digits of the SHA-256 of the
  bytes and `ext` 1 to 10 lowercase letters or digits (`validate_blob_name`); `spool_day` is
  `YYYY-MM-DD`. Under the default blob root the file is
  `/var/lib/fluxbee/blob/active/<first 2 hash digits>/<blob_name>`.
- The bytes never travel in a frame, only the ref. Between hives the blob directory is synchronized
  as files (`blob.sync` in `config/hive.yaml`, tool `syncthing`), so the file may arrive after the
  message: read it with `BlobToolkit::read_blob_with_retry`, which waits for a missing blob (5 s
  with the default `ResolveRetryConfig`) and checks the size and the hash in the name.
- **Automatic offload, Rust only.** `NodeSender::send` normalizes every payload whose `type` is
  `text` (`crates/fluxbee_sdk/src/send_normalization.rs`): when the inline payload, serialized, plus
  2,048 bytes (`TEXT_V1_DEFAULT_OVERHEAD_BYTES`) exceeds 64 KiB
  (`TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES`), the text becomes a blob (`content.txt`, `text/plain`)
  referenced by `content_ref`. If that fails, the payload goes unchanged, with a warning. The Go
  SDK does not do this.
- Limits: 128 KiB per frame (§1.3); with the Rust SDK, 64 KiB for an inline text payload. Other
  payloads are not offloaded: over the frame limit, `send` fails.

---

## 9. Threads and interlocutors

### 9.1 thread_id

`compute_thread_id(ThreadIdInput)` (`crates/fluxbee_sdk/src/thread.rs`) returns
`thread:sha256:<hex>`, the SHA-256 of one of these texts:

```text
NativeThread       v2|kind=native_thread|channel_type=C|entrypoint_id=E|conversation_id=V|native_thread_id=N
PersistentChannel  v2|kind=persistent_channel|channel_type=C|entrypoint_id=E|conversation_id=V
DirectPair         v2|kind=direct_pair|channel_type=C|entrypoint_id=E|party_a=LOW|party_b=HIGH
```

Every value is trimmed and `channel_type` lowercased; a missing `entrypoint_id` is `-`; the two
parties of `DirectPair` are sorted, so their order does not matter; an empty required value is
`ThreadIdError::MissingField`. The node that knows the medium computes it: io-slack uses
`NativeThread` inside a Slack thread and `PersistentChannel` outside one, io-wapp
`PersistentChannel` (`nodes/io/common/src/io_context.rs`). The Go SDK has no equivalent.

### 9.2 ich

`meta.ich` names the channel. IO nodes set `<channel>://<entrypoint id>` from `context.io`
(`extract_ich_from_context_obj`, `nodes/io/common/src/router_message.rs`); SY.edge stamps the
ICH of its public URL on every request it forwards (`src/bin/sy_edge.rs`). The identity layer's
ICH ids are `ich:<uuid v5>` of `(tenant, channel_type, address)` (`stable_ich_id`). The router does
not read `ich`.

### 9.3 src_ilk and dst_ilk, and replies

- `src_ilk` is who speaks; `dst_ilk` who is spoken to.
- A node launched by the orchestrator finds its own ILK in `FLUXBEE_NODE_ILK_ID` and its tenant in
  `FLUXBEE_NODE_TENANT_ID` (`read_self_ilk_from_env`, `read_self_tenant_from_env`,
  `crates/fluxbee_sdk/src/identity.rs`).
- An agent's reply speaks as the agent, to whoever spoke. `build_reply_meta`
  (`crates/fluxbee_ai_sdk/src/message.rs`) copies the incoming `meta`, sets `dst_ilk` to the
  incoming `src_ilk`, clears `src_ilk` and `memory_package`, and drops `thread_id`, `src_ilk`,
  `response_envelope` and, unless told to keep it, `final_response_contract` from `context`. The
  runtime then stamps the agent's ILK (`RuntimeConfig.self_ilk_id`,
  `crates/fluxbee_ai_sdk/src/runtime.rs`). A reply never carries the human's ILK as its own;
  `thread_id` and `ich` stay.
- Reply routing (`build_reply_routing`): `dst` the incoming `src`, the same `trace_id`, `ttl` the
  incoming one (at least 1). A reply whose payload has `type: "error"` gets `.error` appended to
  `meta.msg`, when there is one (`apply_error_reply_msg_suffix`).

---

## 10. Request and response: RouterDispatcher

`RouterDispatcher` (`crates/fluxbee_sdk/src/rpc.rs`; Go `go/fluxbee-go-sdk/dispatcher.go`) is the
supported way to use the connection: the raw `connect` is crate-private, and a CI guard
(`scripts/router_dispatcher_guards/no_direct_connect.sh`) keeps nodes off it. It owns the receiver,
matches replies to requests by `trace_id`, and sends everything else to channels the node
declares. A minimal node is in `crates/fluxbee_sdk/README.md`; `RouterDispatcherTestHarness` runs a
dispatcher over in-process channels for tests.

### 10.1 The profile

`OperationalRouteProfile::builder()` declares `command_channel(name)` (one consumer:
`take_command_receiver`), `broadcast_channel(name)` (any number: `subscribe`), and two ordered
rule lists over `(meta.type, meta.msg)`: `pre_pending_rule` and `post_pending_rule`, each a
`RouteMatch` and a `RouteTarget` (`Command`, `Broadcast` or `Drop`). `RouteMatch` is
`Exact {msg_type, msg}`, `OneOf {msg_type, msgs}`, `AnyMsgOfType(msg_type)` or `Any`; the first
match wins. `build()` refuses empty, duplicate or undeclared channels and a broad rule that hides a
later one.

### 10.2 Where an incoming message goes

`classify`, in this order:

1. A `pre_pending_rule` matches: its target. For commands that must never be taken for a reply.
2. A request is pending on this `trace_id` and its `PendingMatcher` matches: the request completes.
3. A request on this `trace_id` ended in the last 30 s and the message matches its matcher:
   dropped as stale (`RECENT_STALE_TTL`, at most 1,024 kept).
4. The message has the exact shape of a reply this node has awaited before
   (`register_response_only`): dropped as orphaned. A shape that a `post_pending_rule` sends to a
   broadcast channel is exempt (a catch-all `Any` rule does not count), and `AnyMsgOfType` and `Any`
   success matchers are never registered.
5. A `post_pending_rule` matches: its target.
6. Otherwise dropped and counted (`metric_route_unmatched`).

### 10.3 Sending a request

A `PendingMatcher` holds three `RouteMatch` lists, tried in order: `success` delivers the message,
`terminal_error` fails with the transport error, `invalid_response` fails with
`InvalidResponse`. Anything else leaves the request pending.

| Call | success | terminal_error | invalid_response |
| --- | --- | --- | --- |
| `send_system_rpc(SystemRpcRequest {target, request_msg, response_msg, payload, timeout})` | `system` + `response_msg` | `system` + `UNREACHABLE` or `TTL_EXCEEDED` | any `system` |
| `send_admin_rpc(AdminCommandRequest {admin_target, action, target, params, request_id, timeout})` | `admin` + `ADMIN_COMMAND_RESPONSE` | the same | any `admin` |
| `send_with_matcher(message, matcher, labels, timeout)` | yours | yours | yours |

`send_with_matcher` fails at once with `RpcError::Disconnected` when the link is down, fills an
empty `trace_id` (a new UUID) and `src`, and refuses a `trace_id` already pending
(`InvalidRequest`). A request over the frame limit fails at once with
`RpcError::Node(NodeError::MessageTooLarge)` (Go `RpcErrMessageTooLarge`) and leaves nothing
pending. `send_system_rpc` sends `ttl` 16 to the unicast `target`, with `action_class` from
`classify_system_message`.

### 10.4 Timeouts and errors

A zero timeout means 5 s (`default_rpc_timeout`; Go `SendWithMatcher`).
`RpcError::Timeout {trace_id, target, request_msg, response_msg, timeout_ms}` means the outcome is
unknown, not failed: the peer may have done the work (`RpcError::is_timeout`). Transport errors are
`RpcError::Unreachable {reason, original_dst}` and `RpcError::TtlExceeded {original_dst,
last_hop}`. Go returns `*RpcError` with `Kind` `RpcErrTimeout`, `RpcErrTerminalTransport`,
`RpcErrInvalidResponse`, `RpcErrDisconnected`, `RpcErrInvalidRequest` or `RpcErrMessageTooLarge`.

### 10.5 Answering a request

Answer to the request's `routing.src` (a UUID) with its `trace_id`, from your own UUID: the
requester's dispatcher matches on that. Builders: `try_handle_default_node_status`,
`build_node_config_response_message`, `fluxbee_ai_sdk::message::build_reply_message`; in Go
`BuildSystemResponse`, `BuildCommandResponse`, `BuildQueryResponse`. Send through
`RouterDispatcher::sender_snapshot()` (Go `SenderSnapshot()`). Do not reuse a response name as a
command name: once a node has awaited `X_RESPONSE` with an exact matcher, an unsolicited
`X_RESPONSE` is dropped (step 4 of §10.2). Answer requests only, never a reply or a notice. A
message without `routing.src_l2_name` was made by the router (UNREACHABLE, TTL_EXCEEDED); its
`routing.src` is the router's UUID, which no node has, so an answer comes back as another
UNREACHABLE, and a node that answers every message bounces it forever.

---

## 11. Rust and Go SDKs side by side

| Topic | Rust `crates/fluxbee_sdk` | Go `go/fluxbee-go-sdk` |
| --- | --- | --- |
| Reconnection | §2.5 | §2.5 |
| A frame over 128 KiB | Refused before sending (`NodeError::MessageTooLarge`) | Refused before sending (`*MessageTooLargeError`); the connection stays up |
| Inbound queue full (256) | Backpressure on the socket | The message is dropped; the dispatcher fails every pending request on any receive error (`dispatchLoop`) |
| Command channels | Unbounded, warning at depth 1,000 (`RPC_COMMAND_DEPTH_WARN_THRESHOLD`) | 64 messages; when full, dropped and counted (`CommandChannelDrops`) |
| `AnyMsgOfType` success matcher | Never registered as a reply shape | The same (`registerResponseOnly`) |
| System request `ttl` | `send_system_rpc`: 16 | `SendSystemRPC`: 16 unless `Options.TTL` sets another; `BuildSystemRequest` alone turns 0 into 1. It also sets `meta.action` to the verb |
| Admin request result | `AdminCommandResult` | The raw `Message` |
| text/v1 blob offload | Automatic (§8) | None |
| `compute_thread_id`, `stream::append`, vault client | Yes | No |
| `meta.via_tap` | In `Meta` | Not in `Meta` |

---

## 12. Related pages

- [Architecture reference](architecture.md): hives, routers and nodes before the wire details.
- [Router stream](../router-stream-v1.md): the log, the cognition copies, usage records.
- [Cognition reference](cognition.md) and [identity reference](identity.md).
- Node pages: [the router](../nodes/rt-gateway.md), [SY.admin](../nodes/sy-admin.md),
  [SY.orchestrator](../nodes/sy-orchestrator.md), [SY.vault](../nodes/sy-vault.md),
  [SY.identity](../nodes/sy-identity.md), [SY.edge](../nodes/sy-edge.md).
- [Operator OPA policy distribution](../opa-distribution.md) and
  [managed node config](../node-config-control-plane-spec.md).
- [Rust SDK](../../crates/fluxbee_sdk/README.md), [Go SDK](../../go/fluxbee-go-sdk/README.md),
  [AI SDK](../../crates/fluxbee_ai_sdk/README.md).
