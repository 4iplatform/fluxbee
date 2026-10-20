# fluxbee-sdk

The base SDK for writing a Fluxbee node in Rust. It covers:

- joining a router;
- the message protocol;
- requests to system nodes and to SY.admin;
- blobs;
- identity lookups;
- the contract between a node and the orchestrator that launches it.

AI nodes add [`fluxbee-ai-sdk`](../fluxbee_ai_sdk/README.md) on top. For the domain nodes that live in
this repo (`.gov`), see [`nodes/gov/README.md`](../../nodes/gov/README.md).

## A minimal node

`Cargo.toml`, with this crate by path:

```toml
[package]
name = "my-node"
version = "0.1.0"
edition = "2021"

[dependencies]
fluxbee-sdk = { path = "../fluxbee/crates/fluxbee_sdk" }
tokio = { version = "1.37", features = ["rt-multi-thread", "macros"] }
serde_json = "1.0"
```

`src/main.rs`: join the router, answer status queries, reply to user messages.

```rust
use std::time::Duration;

use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use fluxbee_sdk::{
    managed_node_name, try_handle_default_node_status, NodeConfig, NodeUuidMode,
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    fluxbee_sdk::logging::fmt().init();

    let config = NodeConfig {
        // FLUXBEE_NODE_NAME when the orchestrator launched the node; the default otherwise.
        name: managed_node_name("WF.hello", &[]),
        router_socket: "/var/run/fluxbee/routers".into(),
        uuid_persistence_dir: "/var/lib/fluxbee/state/nodes".into(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: "/etc/fluxbee".into(),
        version: "0.1.0".to_string(),
    };

    // Whatever is not an answer to this node's own requests goes to the "incoming" channel.
    let profile = OperationalRouteProfile::builder()
        .command_channel("incoming")
        .post_pending_rule(RouteMatch::Any, RouteTarget::Command("incoming"))
        .build()?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(config, Duration::from_secs(1), profile).await?;
    let sender = dispatcher.sender_snapshot();
    let mut incoming = dispatcher.take_command_receiver("incoming").await?;

    while let Some(msg) = incoming.recv().await {
        if try_handle_default_node_status(&sender, &msg).await? {
            continue;
        }
        if msg.meta.msg_type != "user" {
            continue;
        }
        let reply = Message {
            routing: Routing {
                src: sender.uuid().to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(msg.routing.src.clone()),
                ttl: 16,
                trace_id: msg.routing.trace_id.clone(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                thread_id: msg.meta.thread_id.clone(),
                ..Meta::default()
            },
            payload: json!({"type": "text", "content": "hello"}),
        };
        sender.send(reply).await?;
    }
    Ok(())
}
```

### `NodeConfig`

- **`name`:** the node's L2 name. Without `@<hive>`, the SDK appends the `hive_id` of
  `<config_dir>/hive.yaml`.
- **`router_socket`:** the router socket directory, or one socket file. With the directory, the SDK
  picks a router by a hash of the node's name, skipping the `irp-*` sockets.
- **`uuid_persistence_dir` and `uuid_mode`:** the node's L1 UUID, its connection identity
  (`HELLO.uuid`, `routing.src`).
  - `NodeUuidMode::Persistent`, the default, reuses `<dir>/<name>.uuid` and creates it the first
    time.
  - `NodeUuidMode::Ephemeral` draws a new UUID per process and writes no file: for short-lived
    diagnostic sessions.
- **`config_dir`:** where `hive.yaml` lives.
- **`version`:** sent in `HELLO`.

### Connecting: `RouterDispatcher`

- **The only way to connect** is `RouterDispatcher::connect_with_retry(config, delay, profile)`.
  `connect` is private to the SDK, and `scripts/router_dispatcher_guards/no_direct_connect.sh` keeps it
  that way.
- **The link recovers by itself.** `connect_with_retry` retries every `delay` until the router answers
  `HELLO` with `ANNOUNCE`. After that the SDK reconnects on its own whenever the link drops, with a
  backoff of up to 30 s.
- **The `OperationalRouteProfile` routes what arrives:**
  - an answer to one of the node's own requests (`send_system_rpc`, `send_admin_rpc`,
    `send_with_matcher`) goes to its waiter, matched by `trace_id`;
  - everything else follows ordered rules over `(meta.type, meta.msg)`. A rule leads to a command
    channel (`take_command_receiver`, one consumer) or a broadcast channel (`subscribe`);
  - `pre_pending_rule`s are checked before the waiters and `post_pending_rule`s after them. Put a
    catch-all (`RouteMatch::Any`) in `post_pending_rule`, or it takes the answers to the node's own
    requests;
  - a message no rule matches is dropped and counted (`metric_route_unmatched`).
- **`sender_snapshot()`** gives the `NodeSender`: `send`, `uuid`, `full_name`, `is_connected`,
  `wait_connected`.

### Sending

- **`Destination::Unicast`** takes a node's L2 name (`SY.admin@motherbee`) or a UUID, such as the
  `routing.src` of the message being answered. A reply keeps the request's `trace_id`.
- **`Destination::Resolve`** lets the router's policy (OPA) choose the destination. When it names
  none, the sender gets `UNREACHABLE` (`OPA_NO_TARGET`). A message with no `meta.src_ilk`, or a
  temporary one, goes to the identity frontdesk instead.
- **`Destination::Broadcast`** reaches the router's own nodes (only those whose name matches
  `meta.target`, when it is set), its sibling routers when `ttl` ≥ 2, and other hives when `ttl` ≥ 3.
- **A frame is at most 128 KB.** `send` refuses a larger one with `NodeError::MessageTooLarge`.
- **Long text moves to a blob by itself.** When a `{"type": "text"}` payload would pass 64 KB, `send`
  moves its `content` to a blob and sends a `content_ref` instead.

### Status

`try_handle_default_node_status(&sender, &msg)` answers `NODE_STATUS_GET` and returns `true` when the
message was one.

- **Call it in every node's receive loop**, unless the node answers status itself with the same
  contract.
- **What it reports:** `health_state` is `HEALTHY` unless `NODE_STATUS_DEFAULT_HEALTH_STATE` says
  otherwise. `NODE_STATUS_DEFAULT_HANDLER_ENABLED=false` turns the handler off.
- **What the operator sees:** with it, `GET /hives/{hive}/nodes/{name}/status` reports
  `health_source=NODE_REPORTED`.
- **A managed node** answers instead with `build_node_status_response_payload(state, last_error)`
  (`managed_control_plane`): `HEALTHY` when CONFIGURED, `DEGRADED` (never `ERROR`) otherwise, with
  `lifecycle_state` and the reason in `last_error`. ai.generic and SY.frontdesk.gov use it.

## A node the orchestrator launches

`run_node` (through SY.admin) launches a managed node as a transient systemd unit and gives it:

- **its name:** `FLUXBEE_NODE_NAME=<name>@<hive>`, read by `managed_node_name(default, legacy_env_keys)`;
- **its identity:** `FLUXBEE_NODE_ILK_ID` and `FLUXBEE_NODE_TENANT_ID`, read by
  `read_self_ilk_from_env()` and `read_self_tenant_from_env()`. A node needs its ILK to call SY.vault;
- **its config:** `/var/lib/fluxbee/nodes/<KIND>/<name>@<hive>/config.json`
  (`managed_node_config_path(name)`). The orchestrator writes it on `run_node` and `set_node_config`,
  with its own `_system` block.

`fluxbee_sdk::managed_control_plane` implements the single-config model on top of that file:

- the node reads `config.json` at start, and a `CONFIG_SET` writes it back;
- the states are UNCONFIGURED, CONFIGURED and FAILED_CONFIG;
- a node plugs in only its contract, a `ManagedNodeConfigContract` that validates and materializes
  its config;
- `nodes/io/common` (for the IO nodes) and `nodes/ai/ai-generic` use it; the contract is in
  [docs/node-config-control-plane-spec.md](../../docs/node-config-control-plane-spec.md).

## Requests to system nodes and to SY.admin

**A system request** is `dispatcher.send_system_rpc(SystemRpcRequest { target, request_msg,
response_msg, payload, timeout })`. It sends one `system` message and waits for its named answer.

- **Transport errors are errors:** `UNREACHABLE` and `TTL_EXCEEDED` become `RpcError::Unreachable`
  and `RpcError::TtlExceeded`.
- **No answer in time** is `RpcError::Timeout`. A zero timeout means 5 s.
- **A link that is down** fails at once with `RpcError::Disconnected`.

**The admin gateway** is `dispatcher.send_admin_rpc(AdminCommandRequest)`. It sends `ADMIN_COMMAND`
to SY.admin over the mesh, from any hive, for the same actions as its HTTP API:

```rust
use std::time::Duration;

use fluxbee_sdk::AdminCommandRequest;
use serde_json::json;

let out = dispatcher
    .send_admin_rpc(AdminCommandRequest {
        admin_target: "SY.admin@motherbee",
        action: "list_admin_actions",
        target: None, // Some("worker1") for an action on one hive, as /hives/{hive} in HTTP
        params: json!({}),
        request_id: None,
        timeout: Duration::from_secs(10),
    })
    .await?;
assert_eq!(out.status, "ok");
```

- **`params`** must be a JSON object or null. The answer is an `AdminCommandResult`: `status`,
  `action`, `payload`, `error_code`, `error_detail`, `request_id`, `trace_id`.
- **SY.admin checks the caller of some actions** by its router-stamped name. For example,
  `externalize` is only for the IO node that owns the channel, and IO.cloud may relay only its Cloud
  actions.
- **Actions on other nodes go through SY.admin.** A node does not command another node's control
  plane: `CONFIG_SET`, `SPAWN_NODE` and `SYSTEM_SYNC_HINT` are protected system actions, and the router
  delivers them only from an authorized origin.

## Blobs

Files travel as blobs under `/var/lib/fluxbee/blob/` (`BlobConfig::default()`); a message carries
only their `BlobRef`. A blob is written to `staging/` and becomes visible in `active/` when promoted.
Writing needs root, which is how managed nodes run: `staging/` is root's.

The producer writes the file, promotes it, then attaches it:

```rust
use fluxbee_sdk::blob::{BlobConfig, BlobToolkit};
use fluxbee_sdk::payload::TextV1Payload;

let blob = BlobToolkit::new(BlobConfig::default())?;
let blob_ref = blob.put_bytes(b"hello", "note.txt", "text/plain")?; // into staging/
blob.promote(&blob_ref)?; // into active/
let payload = TextV1Payload::new("See the attached note.", vec![blob_ref]).to_value()?;
```

The consumer reads it, waiting for it if needed:

```rust
use fluxbee_sdk::blob::{BlobConfig, BlobToolkit, ResolveRetryConfig};
use fluxbee_sdk::payload::TextV1Payload;

let blob = BlobToolkit::new(BlobConfig::default())?;
let text = TextV1Payload::from_value(&msg.payload)?;
for attachment in &text.attachments {
    let bytes = blob
        .read_blob_with_retry(attachment, ResolveRetryConfig::default())
        .await?;
    // ...
}
```

- **Promote before sending the `BlobRef`.** A consumer only reads `active/`.
- **The message can arrive before the blob.** Syncthing replicates `active/` to the other hives (the
  ingress gets only `public/`).
  - `read_blob_with_retry` waits with backoff, 5 s by default (`ResolveRetryConfig`), and retries only
    a missing blob.
  - There is no "confirmed publish". `SYSTEM_SYNC_HINT` is SY.admin's alone, so the consumer waits
    instead.
- **Read blobs only through the toolkit:** `read_blob`, `read_blob_async`, `read_blob_with_retry`, or
  `open_blob` to stream.
  - They open the path without following links and check the size. `read_blob*` also checks the
    SHA-256 in the blob's name; `open_blob` leaves that to the caller.
  - Never read the path `resolve` returns: `active/` belongs to the Syncthing user, and a name there
    can be a link (FINDINGS A-56).
- **Long text:** `build_text_v1_payload(content, attachments)` moves a long `content` to a blob.
- **Housekeeping:** `run_gc(BlobGcOptions)` cleans up, and `BlobToolkit::metrics_snapshot()` gives
  the counters.

More in [docs/blob-annex-spec.md](../../docs/blob-annex-spec.md) (its §6.6 predates the removal of the
confirmed publish).

## Cognition: what a node receives

Cognition belongs to the communication: the thread between an interlocutor and the agent (`AI.*`
node) that serves it, `(thread_id, agent ILK)` ([docs/router-stream-v1.md §6](../../docs/router-stream-v1.md)).

On the message:

- **`meta.thread_id`:** the conversation's thread. The node that starts a conversation sets it; IO
  nodes compute it with `compute_thread_id` from a `ThreadIdInput` (`thread:sha256:<hex>`).
- **`meta.memory_package`:** only on a message the router delivers to an `AI.*` node, with a
  `thread_id`, once SY.cognition has built the memory of that communication.
  - The router takes it from its hive's memory region, for each destination.
  - A package the sender put in is dropped.
- **No sequence number:** SY.cognition orders a communication by its own turn count.

What happens without any node's help:

- the router where a message enters copies it to its stream as `storage.turns`, and SY.storage keeps
  it in the table `turns`, the message log;
- the router of an agent copies both directions of each turn with a `thread_id` as `cognition.turns`,
  for the SY.cognition of its hive;
- SY.cognition derives threads, contexts, reasons, memories and episodes. It writes them back through
  the stream (`storage.cognition.batch`) to SY.storage's `cognition_*` tables, and refreshes the
  memory region.

What a node must not do:

- read `turns` or the `cognition_*` tables. Only SY.cognition reads a communication's state back, with
  `COGNITION_STATE_GET` over the mesh;
- read the memory region (`jsr-memory-<hive>`), which belongs to the router and SY.cognition;
- use PostgreSQL as its integration surface.

## The router stream

`fluxbee_sdk::stream::append(&sender, subject, payload)` sends `STREAM_APPEND` to the node's own
router. The router keeps a short, best-effort log and delivers it in batches to fixed sinks
([docs/router-stream-v1.md §2](../../docs/router-stream-v1.md)).

- **Fire and forget:** the router sends no reply. The call waits while the node's link is down,
  because the SDK drops what was queued for a link it is reconnecting.
- **Only some subjects are allowed per producer.** The router refuses the rest, and counts and logs
  them; the caller is not told.

  | Subject | Who may append it |
  | --- | --- |
  | `storage.usage` | any node; SY.storage checks each record. SY.cognition only on the motherbee or a worker; SY.frontdesk.gov, SY.architect and SY.admin only on the motherbee |
  | `storage.cognition.*` | only `SY.cognition@<this hive>` |
  | `storage.turns`, `cognition.turns` | no node: only the router makes them |

- **In practice a node does not call it.** The AI SDK's usage recorder appends the `storage.usage`
  records ([`fluxbee-ai-sdk`](../fluxbee_ai_sdk/README.md#usage-records-who-pays-for-each-call)).

## Other tools

| Tool | Path | Purpose |
| --- | --- | --- |
| Protocol | `fluxbee_sdk::protocol` | `Message`, `Routing`, `Destination`, `Meta`, `MemoryPackage`, the message-name constants |
| Text payload | `fluxbee_sdk::payload::TextV1Payload` | `text/v1`: `content` or `content_ref`, plus `attachments` |
| Identity lookup | `fluxbee_sdk::{resolve_ilk_from_hive_id, resolve_ilk_from_shm_name, resolve_ilk_from_hive_config}` | `(channel_type, address, tenant_id)` to `ilk:<uuid>`, from the hive's identity SHM (`/jsr-identity-<hive>`) |
| Thread id | `fluxbee_sdk::{compute_thread_id, ThreadIdInput}` | the canonical `thread_id` of a conversation |
| Vault | `fluxbee_sdk::VaultClient` | `VaultClient::for_primary(dispatcher, VaultCallerOwned::new(ilk, node_name))`; `resolve_resource(resource_type, tenant, timeout)` returns the node's own key, else one assigned to it, else its tenant's general key |
| Timer | `fluxbee_sdk::TimerClient` | SY.timer: `TimerClient::new_with_dispatcher(dispatcher, TimerClientConfig::default())`; see `examples/timer_client.rs` |
| Logging | `fluxbee_sdk::logging::fmt()` | the `tracing` subscriber every binary uses: colour only on a terminal |
| Prelude | `fluxbee_sdk::prelude::*` | common symbols. `RouteMatch`, `RouteTarget` and `RpcCommandReceiver` come from the crate root |

IO nodes provision the ILK of whoever writes to them (`ILK_PROVISION` to SY.identity) through
`nodes/io/common` (`provision_ilk_in_scope`); the SDK holds only the message names and types. The IO
node declares the `ilk_type`, `human` or `agent`, because only it knows; SY.identity refuses `system`.
