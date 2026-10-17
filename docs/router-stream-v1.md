# Router stream — v1

Status: design reviewed once (2026-10-16); stages 1a and 1b validated on 8.x (0.1.75, 0.1.76);
stage 2 designed with the operator and reviewed (2026-10-17, §6), validated on 8.x (2a in 0.1.77,
§6.8; 2b in 0.1.78, §6.9: NATS is gone); stage 3 designed with the operator and reviewed once
(2026-10-17, §7), built and its code reviewed once (§7.9). Finding A-70 (`lab/logbook/FINDINGS.md`).

The router gets a small stream of its own. It replaces the embedded NATS broker, and carries three
kinds of data to SY.storage:
- turns: a copy of every message;
- what SY.cognition derives from them;
- LLM usage records, which are stage 5 of `docs/ilk-tenant-model-v1.md`.

The fluxbee socket is the pipe; the stream only lets data wait for its consumer. On the way it
fixes the bugs the review found in today's router, SY.storage and SY.cognition (§2.12, §2.13).

## 0. Decisions (operator, 2026-10-16)

| # | Decision | Operator |
|---|---|---|
| D1 | NATS dies. | *"NATS quedó como algo inicial viejo.. ahora tiene que morir"* |
| D2 | The router gets a simple JetStream-like stream, for certain messages only. The pipe is the fluxbee socket. | *"solo para esperar flujo y transmitir el dato con otros tiempos"* |
| D3 | One stream per router, named `RT.<router>.stream@<hive>`. | *"nombre OK"* |
| D4 | Best effort for every consumer. The stream smooths timing; it is not a store. | *"siempre es mejor esfuerzo"* |
| D5 | A small window: 2 minutes and 32 MB per router, as visible constants. Accumulating is a serious problem: alarm and a reduced log. | *"la comunicación no puede quedar atrapada por minutos"*, *"ponelos visibles en constantes"* |
| D6 | The router never waits for the log. | |
| D7 | Order: (1) the stream with SY.storage as its sink, (2) cognition and policy as sinks and NATS removed, (3) tokens. The SDK's single AI function may go in parallel. | *"el orden es el correcto"* |
| D8 | Tokens: record only; one record per LLM response. Cloud owns and reads the data through SY.admin and io.cloud. Every model call goes through one SDK function that requires the tenant. Who is charged: Q2. | stage 5 talk |
| D9 | No special mode for the ingress (DMZ) now. | *"no me parece importante ahora"* |
| D10 | A record that waits longer than the window is lost and counted. | *"por ahora no queda otra que esperar"* |
| D11 | 128 KB is the frame limit: a frame over it is skipped and counted, and the link recovers for the next one. | *"debería saltar ese mensaje pero recuperarse en el próximo"* |
| D12 | The `RT.` prefix is reserved for the router's own nodes. | *"RT. OK, reservado"* |
| D13 | The stream's disk work runs on a thread of its own, in parallel: the router must stay ultra fast and the stream is slower by construction. | *"stream en paralelo"* |

## 1. What is there today (verified in code at 0.1.74; PROD measured 2026-10-16)

- **The "NATS" is not NATS.**
  - `src/nats/mod.rs` is a broker of our own that speaks a subset of the NATS text protocol.
  - It runs inside the router process (`prepare_nats_runtime`, `server_id: fluxbee-router`),
    one per hive, on `127.0.0.1:4222`, with a health check that restarts it.
  - It has no link between hives.
- **Messages do not travel on it.**
  - Messages go over the router's sockets and the WAN with mTLS.
  - The router publishes a copy of every non-system message as a turn (`maybe_publish_turn`,
    `storage.turns`). Each publish opens a new TCP connection.
- **Its durability is weak.**
  - Each subject is an in-memory queue capped at 50,000 messages. Past the cap it drops the
    oldest, read or not, and a consumer silently skips ahead.
  - Each durable consumer has one message in flight.
  - Durable consumers are never removed, and a subject is pruned only up to its slowest one.
  - On disk, the whole state is one JSON file (payloads as arrays of numbers), cloned under the
    broker lock and rewritten every 500 ms when anything changed.
- **Who uses it:**
  - the router → `storage.turns` → SY.storage (the DB), SY.cognition and SY.policy (which only
    counts turns by `action_class`);
  - SY.cognition → `storage.cognition.*` (8 subjects) → SY.storage;
  - one RPC, `storage.metrics.get` (SY.admin, and SY.orchestrator's database probe);
  - `storage.events`, `storage.items` and `storage.reactivation` have a consumer in SY.storage,
    but nothing in the product publishes to them; only `scripts/storage_subjects_e2e.sh` does.
- **Only the motherbee's traffic reaches the database.**
  - SY.storage runs only on the motherbee.
  - PROD on 2026-10-16:
    - the motherbee: 949 turns, all stored;
    - worker1: 67 turns, read by its cognition and policy, never stored;
    - the ingress: 597 turns of edge traffic, consumed by no one, kept on the DMZ's disk (1.35 MB
      of JSON), growing to 50,000.
  - This is the gap A-38 recorded ("que lo de los workers llegue al motherbee"), and it covers
    every hive but the motherbee.
- **The log plane lives inside the router's fast path.** The broker, its snapshot and a connection
  per published message all compete with routing.
- **Live bugs the review found** (fixed by this design, §2.12 and §2.13):
  - **The AI's replies are not stored.** `turns` is keyed by `(ctx, seq)`. Without `meta.ctx`,
    both come from the `trace_id`, and a reply reuses the request's `trace_id`. So
    `ON CONFLICT DO NOTHING` throws the reply away.
  - **SY.cognition processes a turn several times.** It acks only after processing, and the broker
    re-sends an unacked message every 2 s. A turn whose tagging takes more than 2 s is processed
    again. On 2026-10-16 one probe turn cost 3 or 4 tagger calls and as many summarizer calls.
  - **An oversize frame breaks a link.** The router's writers stop at a frame over 128 KB. A node
    or a sibling router then sees its link close and reconnects, losing the frames in between. The
    WAN link stays dead in one direction for good.
  - **A message from another hive to a node on a non-gateway router is dropped.**
  - **SY.storage never reconnects to postgres:** a closed connection is only logged, and a
    database it cannot use at start leaves it running without one until someone restarts it.
  - **`storage_inbox` retries failed rows only at startup.** It is also the message log: Archi's
    Messages page reads it, and the AI replies `turns` dropped exist only there (Q7, decided in
    1b: Archi reads `turns`).

What the stream reuses:
- **Router-internal messages:** the router already consumes OPA_RELOAD itself and routes it to no
  node.
- **The LSA:** the gateway announces every node of its hive, its own and those of its sibling
  routers (`build_local_lsa_payload`). Any hive routes to them by name.
- **Taps:** they already copy a message to another node, applied once at the source router.
- **SY.storage's inbox (`storage_inbox`):** its key is a hash of subject and payload, so a repeat
  is absorbed.
- **The config parses the hive's role** (`hive_role`); the router does not read it yet.

## 2. Design

### 2.1 One stream per router

Every router runs a stream, as a task of its own process. The stream is also a node:
- **It joins its router as a node,** through the router's own socket, like any other node: HELLO,
  a persistent UUID, the name stamped by the router.
  - It connects to `cfg.node_socket_path`, the router's own socket file, never to the socket
    directory: with the directory the SDK hashes the name over every router's socket and could
    land on a sibling router.
  - It is spawned after the socket is bound and never awaited inside `Router::run`, which accepts
    nodes last: waiting for its HELLO there would deadlock.
  - Its UUID is persisted under `state/<router L2 name>/` (SDK `NodeUuidMode::Persistent`), so
    no node file under `state/nodes` can collide with it.
- **Its name is `RT.<router>.stream@<hive>`,** derived from `router_l2_name`
  (`JSR_ROUTER_NAME` may lack the `RT.` prefix): `RT.gateway.stream@motherbee`,
  `RT.gateway.stream@worker1`, and `RT.<switch>.stream@<hive>` for another router of the hive.
- **`RT.` is reserved (D12).** Today any process may HELLO with any name, and the orchestrator
  refuses only `SY.*` and exactly `RT.gateway` for managed nodes. So:
  - the router accepts a HELLO with an `RT.*` name only when the socket's peer credentials name
    the router's own process (`peer_cred()` PID = `std::process::id()`);
  - the orchestrator refuses every `RT.*` name for a managed node.
- **Because it is a node:**
  - the gateway's LSA announces it, so acks and status queries reach it by name from any hive;
  - its batches leave through the normal routing paths: local delivery, IRP to a sibling router,
    the WAN;
  - inventories list it as a node of kind RT.

The routing code does not change. The router gains two hooks (§2.2).

The review checked the alternative, a node handle with in-process channels registered without a
socket. It would duplicate the UUID claim, SHM registration, FIB rebuild, LSA broadcast, ANNOUNCE
and the per-frame loop. The socket stays.

### 2.2 How data gets in

1. **The turn copy.** Where `maybe_publish_turn` publishes today, the router appends the copy to
   its stream.
   - It is the same copy as today: after `thread_seq` is assigned, with `action_class` (SY.policy
     reads it).
   - The rule stays: every message except `system`, `admin` and `query_response` kinds.
   - The copy is made once, by the router where the message enters.
   - The copy carries the sender's real name (`conn_node_name`). Today it keeps whatever
     `src_l2_name` the node wrote, because the router stamps it only later.
2. **`STREAM_APPEND` from a node.** A node sends the system message `STREAM_APPEND {subject,
   payload}`, addressed to its router's UUID (the `routing.src` of the router's ANNOUNCE), so a
   router without the hook delivers it to no one.
   - The router it is connected to intercepts it by name, as it does with OPA_RELOAD, and appends
     it to its own stream. The interception comes before `thread_seq` is assigned, so an append
     never uses up a sequence number of a conversation.
   - The router routes it to no node.
   - Each record keeps its producer: the node's connection name (`conn_node_name`), never the
     `src_l2_name` the node wrote.
   - The subject must be allowed for that producer:

     | Subject | May append it |
     |---|---|
     | `storage.turns` | only the router itself; refused from a node |
     | `storage.cognition.*` | only `SY.cognition@<this hive>` |
     | `storage.usage` | any node (SY.storage checks it, §2.14) |

   - A refused append is counted and logged.

The router never waits on either path (D6, D13):
- an append goes into a bounded in-memory queue with `try_send`. The router holds only a clone of
  the queue's sender and atomic counters, no lock;
- the turn copies and the nodes' appends have separate queues and separate shares of the cap, so
  a node appending in a loop cannot crowd out the turns;
- the stream's disk work runs on an OS thread of its own;
- a full queue drops the record and counts it;
- a stream that cannot open its directory logs it, drops and counts; the router runs anyway.

### 2.3 Subjects and sinks

Each subject has its sinks, which depend on the role of the stream's hive. The table is code
constants; the router starts reading `hive_role` for it.

| Subject | motherbee | worker | ingress, egress |
|---|---|---|---|
| `storage.turns` | `SY.storage@motherbee` | `SY.storage@motherbee` | `SY.storage@motherbee` |
| `cognition.turns` (stage 2a, §6.2) | `SY.cognition@motherbee` | `SY.cognition@<hive>` | — |
| `storage.cognition.*` | `SY.storage@motherbee` | `SY.storage@motherbee` | — |
| `storage.usage` | `SY.storage@motherbee` | `SY.storage@motherbee` | `SY.storage@motherbee` |

The table is as built in 2a: SY.cognition takes the copies its router makes of its agents'
communications, not every turn, and SY.policy is retired (§6).

Each sink has its own cursor. A slow or absent sink falls behind on its own and holds back no
other sink.

**Stage 1b:** every subject goes to `SY.storage@motherbee` only. SY.cognition and SY.policy keep
reading turns from NATS until stage 2 makes them sinks; the router still copies turns to NATS on
the motherbee and the workers, where they read them, and no longer on the ingress and the egress.

### 2.4 The log on disk

- **Where:** `<state dir>/<router L2 name>/stream/` (`/var/lib/fluxbee/state/RT.gateway@<hive>/stream/`
  on PROD), owned by the router.
- **The thread:** all of it runs on the stream's own OS thread (D13). It places a record in the
  in-memory log under a lock and writes it to disk after releasing it: the senders and the status
  take that lock on the router's runtime threads, which never wait on the disk.
- **Segments:** append-only files named by their first seq, up to `STREAM_SEGMENT_BYTES` each. A
  write that fails closes the segment, and the next record starts a new one.
- **A record:** `[u32 len][u32 checksum][u64 seq][u64 ts_ms][u8 source][u16 subject_len][subject]
  [u16 producer_len][producer][payload]`; the checksum is FNV-1a folded to 32 bits.
- **`state.json`:** `next_seq` and the cursors (sink name → last acked seq), rewritten by temp
  file and rename at most once a second when they changed, and only up to what has been fsynced.
  `next_seq` keeps the seqs from restarting under live cursors.
- **Durability:**
  - every write goes to the file at once, so a router crash loses nothing written;
  - an fsync runs every `STREAM_FSYNC_INTERVAL_MS`, so a power cut loses at most that long.
- **Start:**
  - the newest segment is cut at its first bad record (length or crc);
  - each cursor is capped at the last seq on disk;
  - cursors of sinks no longer in the table are dropped.
- **Trim:**
  - a record leaves the log when every sink took it, or when it is past the window and no batch
    carrying it waits for an ack;
  - a record that leaves before one of its sinks took it counts as expired for that sink, once;
  - a segment is deleted when every record in it left the log; when nothing is left, the newest
    goes too, once `state.json` is saved, so a record stays on disk only until its sinks took it
    (§4) and the next record starts a new segment;
  - at a restart, the records past the window leave the log at once, counted the same way.
- **Cap:** at `STREAM_MAX_BYTES`, new records are dropped and counted (D5). Nothing old is deleted
  to make room; age trimming frees space within the window.

### 2.5 Delivery

- **One sender per sink, over every subject of that sink.** Each record carries its subject, so a
  batch to SY.storage mixes turns and cognition records instead of breaking at every change.
- **The batch:**
  - at most `STREAM_BATCH_MAX_RECORDS` records and `STREAM_BATCH_MAX_BYTES` bytes, measured on the
    serialized `STREAM_BATCH`. Each router hop re-serializes a message and adds a few fields, so
    the margin under the 128 KB frame stays;
  - `STREAM_BATCH {stream, records: [{seq, ts_ms, subject, producer, payload}]}`, with every
    payload embedded as JSON. It round-trips through `serde_json::Value`, so its bytes can change
    (key order), but the same record always yields the same bytes: a resent record keeps its
    inbox key.
- **The ack:** the sink answers `STREAM_ACK {up_to_seq}` to the stream's name, and the stream
  advances that sink's cursor.
  - The stream accepts an ack only when its router-stamped sender is that sink, it moves the
    cursor forward, and `up_to_seq` is at most the last seq of the batch in flight. STREAM_* are
    not protected actions, so any node could otherwise send one and make a stream skip records.
- **Retries:**
  - no ack within `STREAM_ACK_TIMEOUT_SECS`, or an UNREACHABLE, means the same batch is sent again;
  - the backoff starts at `STREAM_RETRY_MIN_MS` and doubles up to `STREAM_RETRY_MAX_MS`.
- **At least once:** a batch can arrive twice, so every sink must take a repeat (§2.7).
- **One batch in flight per sink,** so each sink receives in order.
- **Expired:** a record older than `STREAM_MAX_AGE_SECS` when its turn comes is skipped for that
  sink and counted.
- **Oversize:** a record that alone does not fit in a frame is skipped and counted.
- **Counted once:** a skipped record is counted when the sink's cursor passes it, which happens
  right away ahead of a batch; one met after the first record of a batch ends that batch and is
  passed by the next plan. A batch sent again counts nothing more.
  - Only text over 64 KB goes to blobs, so turns between about 96 and 128 KB are a new loss.
  - NATS took up to 1 MB.

### 2.6 The window, the alarm and the constants

All in one place, at the top of `src/router/stream.rs`:

```rust
/// How long a record may wait for its sink. Older records are skipped and counted (D5).
pub const STREAM_MAX_AGE_SECS: u64 = 120;
/// The most a router's stream keeps on disk. Past it, new records are dropped and counted (D5).
pub const STREAM_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// The share of STREAM_MAX_BYTES the nodes' appends may use; the rest is for the turn copies.
pub const STREAM_NODE_APPENDS_SHARE_PERCENT: u64 = 25;
pub const STREAM_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
pub const STREAM_BATCH_MAX_RECORDS: usize = 256;
pub const STREAM_BATCH_MAX_BYTES: usize = 96 * 1024;
pub const STREAM_ACK_TIMEOUT_SECS: u64 = 5;
pub const STREAM_RETRY_MIN_MS: u64 = 500;
pub const STREAM_RETRY_MAX_MS: u64 = 8_000;
pub const STREAM_FSYNC_INTERVAL_MS: u64 = 250;
pub const STREAM_TURN_QUEUE: usize = 4_096;
pub const STREAM_NODE_APPEND_QUEUE: usize = 1_024;
/// Payload bytes waiting for the disk thread, both queues together.
pub const STREAM_QUEUE_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// At most one alarm line per interval, with the counts since the last one (D5, "log reducido").
pub const STREAM_ALARM_INTERVAL_SECS: u64 = 30;
```

**The alarm:** when anything is dropped (cap, full queue, expired, oversize, refused), the stream
logs one WARN line per `STREAM_ALARM_INTERVAL_SECS` with the counts by reason. The counters are
also in its status, and `lab/ops.py health` sums them per hive (`streamlost`).

A third bound covers the queue to the disk thread: `STREAM_QUEUE_MAX_BYTES` (16 MB) of payload
waiting, both queues together, so a stalled disk cannot grow the router's memory; past it a record
is dropped like on a full queue.

### 2.7 The sinks

- **SY.storage@motherbee:**
  - **Who it accepts from:** a `STREAM_BATCH` only from a stream, whose router-stamped name must be
    `RT.<router>.stream@<hive>`.
  - **Its own database connection.** Batch ingest gets a dedicated postgres connection. The
    shared client cannot run a transaction, and a manual `BEGIN` on it would pull the other
    loops' statements in.
  - **No database, no ack.** If the connection closes, SY.storage exits and systemd restarts it.
    Today it only logs, and with the stream the backlog would expire meanwhile.
  - **Ingest:** in one transaction it upserts every record into `storage_inbox`, with the same key
    as today, then acks up to the last seq. A task of its own processes the new rows after the
    ack, so an ack never waits for the previous batch's processing; past `STREAM_PROCESS_QUEUE`
    batches the rows wait in the inbox for its retry.
  - **Checks on the producer:**
    - every record's producer must be a node of the stream's hive: a router copies and takes only
      what its own nodes send;
    - a turn's message must name its producer as sender (`routing.src_l2_name`), the name Archi
      shows;
    - `storage.cognition.*` must come from `SY.cognition@<the stream's hive>`;
    - usage records are checked as §2.14 says.
  - **Its INFO log line per system message** drops to debug for `STREAM_BATCH`.
- **SY.cognition (local, `cognition.turns`, best effort; §6.2 as built in 2a):**
  - **It acks on receipt.** It processes from a bounded queue in its own task, and a full queue
    drops and counts.
  - **It skips any record at or below the last one it accepted from that stream and epoch,** so a
    repeat costs nothing and a stream whose seq restarted is not ignored.
  - **It sends its outputs with `STREAM_APPEND`** (`storage.cognition.batch`). From a worker they
    reach the database for the first time (Q1).
  - SY.policy is retired (§6.2).

### 2.8 The SDK

- **Built in 1b:** the wire contract, `fluxbee_sdk::stream` (the message names, the subjects, the
  batch and ack shapes, the stream node names). The helpers below come with their first users,
  cognition in stage 2 and the AI function in stage 3.
- **Rust:**
  - `fluxbee_sdk::stream::append(sender, subject, payload)`: a `STREAM_APPEND` to the node's own
    router, fire and forget, no reply;
  - `fluxbee_sdk::stream::Batch`: parses a `STREAM_BATCH`;
  - `ack(sender, &batch)`: answers it.
- **Stage 2:** `fluxbee_sdk::nats` (the client, its request/reply and envelopes) dies.
- **Go:** nothing changes. Nothing in Go produces or consumes these subjects.

### 2.9 Several routers in a hive

PROD runs one router per hive, but the code supports several: one gateway (WAN, LSA writer) and
others like switches, joined by IRP. Each node picks a router by a hash of its name.

- **Each router has its own stream:** the gateway's is `RT.gateway.stream@<hive>`, a switch's is
  `RT.<switch>.stream@<hive>`.
- **Appends:** a node appends to the router it is connected to, and the turn copy is made where
  the message enters. Every message is copied exactly once.
- **A switch's batches:** they go by IRP to the gateway and on by the WAN, like any message.
- **The sinks:** they receive from every stream of every hive.
- **The LSA:** the gateway announces every stream of the hive, because they are nodes of it.
- **The way back needs a fix (§2.12):** today a message from another hive to a node on a
  non-gateway router is dropped by that router, so a switch's stream would never get its acks.

### 2.10 Status

- **The stream answers `STREAM_STATUS`:**
  - appended, turns and node appends;
  - delivered and expired, by sink;
  - dropped, by reason;
  - the backlog in records and bytes, and the age of the oldest;
  - the last ack per sink.
- **SY.admin:** `GET /hives/{hive}/streams` (action `list_streams`) asks every
  `RT.*.stream@<hive>` the LSA lists for that hive. A stream that does not answer is listed with
  its error.
- **`ops.py health`:** `streamlost` sums the losses the stream's alarm lines report since the given
  time.

### 2.11 What dies with NATS (stage 2)

- **The router:**
  - the broker (`src/nats/mod.rs`, 1,709 lines);
  - its start, its readiness wait (up to 20 s; the router no longer needs it to start) and its
    recovery loop;
  - the `nats_*` config fields.
- **The SDK:** `crates/fluxbee_sdk/src/nats.rs` (2,782 lines) and its config bits.
- **The consumers:** the NATS code in SY.storage, SY.cognition, SY.policy and SY.admin.
- **The orchestrator:**
  - its wait for NATS at boot;
  - its database probe over `storage.metrics.get`, which moves to `STORAGE_METRICS_GET` over the
    mesh, like SY.admin's;
  - the `nats` directories it creates at `add_hive`.
- **The `nats:` section of `hive.yaml`:**
  - `packaging/hive.yaml.example` and `config/hive.yaml`;
  - the three hive.yaml renders in the orchestrator (worker, ingress, egress).

  An old `hive.yaml` that still has the section keeps parsing.
- **The installers:** the `nats` state directory in `scripts/install.sh` and
  `packaging/deb-postinst`.
- **`/var/lib/fluxbee/nats` on every hive:** the upgrade logs the broker's pending counts, then
  removes it. On the ingress, those are the 597 unconsumed turns.
- **Diagnostics and scripts:**
  - `src/bin/wf_nats_diag.rs` and `src/bin/jetstream_envelope_diag.rs` (628 lines);
  - `storage_smoke`, `storage_ingestion_suite`, `storage_subjects_e2e`, the `nats_*` scripts,
    `jetstream_envelope_e2e`;
  - the NATS steps of `sdk_gate`.
- **The three subjects without a producer** (`storage.events`, `storage.items`,
  `storage.reactivation`): their ingestion goes. Their tables stay.
- **Docs:** `04-routing`, `08-apendices`, `07-operaciones`, `12-cognition-v2`, `13-storage`,
  `admin-internal-gateway-spec`, and the rest that name NATS.

### 2.12 Router bugs fixed on the way

The stream depends on these, so they come first.

- **An oversize frame breaks a link (D11).**
  - When a frame is over 128 KB, the router's writer for that link stops.
  - A node or a sibling router sees its link close and reconnects, losing the frames in between.
    The WAN writer leaves its link dead in one direction for good.
  - A node cannot send one: the SDK refuses it before sending. A frame grows past the limit only
    when the router enlarges it on the way (`src_l2_name`, `action_class`, the memory package).
  - **Fix:** one writer for the four links. It skips a frame over the limit (`write_frame` would
    refuse it before writing a byte), counts it, logs at most once per interval with the total,
    and keeps the link.
- **A message from another hive to a node on a non-gateway router is dropped.**
  - The gateway hands it over IRP, and the receiving router's `handle_peer_message` drops any frame
    whose sender is not one of its sibling routers' nodes.
  - **Fix:** when the sender is not a sibling's node and the frame comes from the gateway, the
    sender must be a node of another hive in this router's LSA.
    - Its name is derived here, canonical to its hive as the gateway's WAN ingress does
      (EDGE-01), not taken from the frame.
    - A miss refreshes the LSA once before dropping, and a drop is logged.
    - With tests on an LSA snapshot.
  - **Known limit:** a node the gateway knows only through the hub's reachability plane (spoke to
    hub to spoke) still cannot reach a non-gateway router: that plane lives only on the gateway.
- **`RT.` names are not reserved:** the HELLO check and the orchestrator guard of §2.1.

### 2.13 Storage and cognition bugs fixed on the way

- **The AI's replies are not stored (`turns`).**
  - The unique key becomes the record's inbox key; `(ctx, seq)` stays as a plain index.
  - `ts` comes from the record's `ts_ms`, not the arrival time, so a delayed turn keeps its time.
- **Cognition processes a turn several times.** In stage 1, still on NATS, its consumer acks on
  receipt and processes from its own bounded queue, as §2.7 says. It stops paying 2 to 4 LLM calls
  per turn now, before the stream reaches it.
- **Cognition's worker:** if it ever stops, cognition exits instead of acking and dropping every
  turn.
- **SY.storage and its database:** with a database configured, SY.storage exits when it cannot
  create, connect or set up its schema at start, and when its connection closes. systemd starts
  it again until postgres answers. Without a database URL it still starts degraded.
- **`storage_inbox`'s retry** becomes periodic, every 60 s, for rows over a minute old:
  - in insert order, with a cursor, so failing rows do not hold back the rest;
  - each failed retry counts as an attempt, up to 10.

  Processed rows were kept until Q7 was decided; since 1b they are purged after 24 h, once the
  one-time backfill ran (Q7).
- **The `turns` migration** runs once and does not touch the table again. Rolling back to 0.1.74
  or earlier needs the VM snapshot: the old code inserts with `ON CONFLICT (ctx, seq)`, which no
  longer has a unique index.
- **`ts` from the record:** comes with the stream (1b); on NATS a turn arrives within
  milliseconds.

### 2.14 Stage 3: LLM usage records

Superseded in its details by §7 (designed with the operator and reviewed, 2026-10-17).


- **One AI function.**
  - Every model call goes through the AI SDK's two factories (`create_llm_client`,
    `create_function_calling_model`). That is 14 call sites in 6 files, each building its client
    per operation.
  - The provider clients become private to the SDK. The only outside direct use is
    `ai_local_probe`, a dev tool without a router (Q6).
  - Both factories take a required `AiAccount`:
    - `tenant_id`, which must not be empty;
    - the spending node and its ILK;
    - the counterpart ILK and its tenant, when there is one;
    - `trace_id` and `thread_id`, both optional (Archi's chat has a session, not a trace);
    - the node's `Arc<RouterDispatcher>`, so the append takes the current sender at send time.
- **What is recorded:**
  - every 2xx response, right after its body is parsed and before any check. Truncation, missing
    text or a failed schema still cost money;
  - an `outcome` field says how each call ended;
  - non-2xx responses carry no usage.
- **The counts, normalized**, because each provider counts differently:
  - OpenAI: `input_tokens` includes cached input, and `output_tokens` includes reasoning;
  - Anthropic: `input_tokens` excludes cache reads and writes, cache writes come split by TTL, and
    thinking is billed inside `output_tokens`.

  So every record has:
  - `uncached_input`, `cache_read`, `cache_write` and `output` (reasoning included);
  - `reasoning`, as an optional subset;
  - the provider's raw `usage` object.

  Archi's per-task budget keeps counting input plus output as the provider reports them, its
  behavior today.
- **The record also has:**
  - `record_id`, a UUID, so two identical calls never collapse into one key;
  - `ts`, `hive`, `node_name`, `ilk_id`, `tenant_id`, `tenant_source`;
  - `counterpart_ilk`, `counterpart_tenant`;
  - `provider`, `model`, `kind` (`generate`, `function_turn`), `outcome`;
  - `trace_id`, `thread_id`.

  It goes as one `storage.usage` record through `stream::append`.
- **Trust.** The router cannot tell an append made by the SDK from one built by hand, so
  SY.storage checks:
  - it resolves the producer's ILK and tenant from the motherbee's identity;
  - a tenant's node may charge only its own tenant;
  - only SY.cognition, SY.frontdesk.gov, SY.admin and SY.architect may charge another tenant;
  - the producer's hive must be the stream's hive.

  Records that fail are dropped, counted and logged.
- **SY.admin's AI executor** can be called by any mesh node (A-62, parked), so its records carry the
  caller's name as the counterpart.
- **Storage:**
  - an `llm_usage` table, keyed by `record_id` (`ON CONFLICT DO NOTHING`) and indexed by
    `(tenant_id, ts)` and `(node_name, ts)`;
  - no foreign key to the tenants, which would block a tenant purge.
- **Reads:**
  - SY.admin has no database client, so usage queries are a new storage request, paged to fit a
    frame;
  - SY.admin answers by tenant, period and grouping (day, model, node), plus a paged list;
  - io.cloud exposes it to Cloud, which owns the data (D8).
- **A suspended tenant is still recorded** when a core node works on its conversations. Fluxbee
  records; Cloud decides.
- **Who is charged:** Q2.

## 3. Stages and gates

| Stage | What | Gate on 8.x |
|---|---|---|
| 1a | The live bugs (§2.12, §2.13), on today's NATS: frames, IRP, `RT.` reserved, the `turns` key, cognition's duplicates, SY.storage's database, the inbox retry | a request and its reply (one trace_id) both land in `turns`; one turn costs one tagger call; SY.storage restarts when its database connection is cut; a process that is not the router cannot HELLO as `RT.*`; health clean. The oversize skip and the IRP hand-over are unit tests: PROD cannot produce them (one router per hive, the SDK refuses oversize sends) |
| 1b | The stream in every router; SY.storage as the sink of `storage.turns`; `STREAM_STATUS`, the admin endpoint and the health count. On the motherbee, SY.storage drops only its NATS turns loop; the router removes `durable.sy-storage.turns` once at start; ingress and egress stop copying turns to NATS | worker1's and the ingress's turns reach the database; SY.storage stopped 60 s under traffic: backlog visible, then all delivered, nothing twice; stopped 3 min: expired counted, the alarm logged, routing unaffected; a router restarted with a backlog resumes; a motherbee deploy loses nothing inside the window; health clean |
| 2a | Cognition per communication (§6): the agent's router feeds its local cognition both directions; cognition as a stream sink, its outputs by `STREAM_APPEND`; its rebuild over the mesh; replies carry the agent's ILK; SY.policy removed | see §6.4 |
| 2b | NATS deleted everywhere (§2.11, §6.3): `STORAGE_METRICS_GET` over the mesh, the three subjects without a producer, the `nats` directory and `hive.yaml` sections | see §6.4 |
| 3 | The AI function and the usage records (§2.14). The SDK part may be built during stages 1 and 2. | one call from each kind of consumer leaves a record with the right tenant, node, model and tokens; a forged record is refused; the admin answers by tenant and period; Cloud reads it through io.cloud |

Each stage: code and unit tests, one adversarial review, release, validation on 8.x, then FINDINGS,
the ledger and the logbook.

## 4. Out of scope and known limits

- **A sink away longer than the window:** what it misses is lost and counted (D10).
- **The ingress:** it ships its turns like any hive, and keeps their content on its disk only until
  SY.storage acks. A special mode for the DMZ is for later (D9).
- **A local process can still HELLO with any other node's name** (P-1, postponed with A-22). The
  streams are out of it: an `RT.*` name needs the router's own process (§2.1).
- **An Anthropic retry after a transport error can be billed twice:** one record, two charges.
- **Cognition loses its queue on a restart:** up to 256 turns acked but not processed yet, and,
  with a backlog, the newest past the queue. Best effort (D4).
- **Hub-vouched senders and non-gateway routers** (§2.12).
- **Usage records (stage 3, §7):** a node killed with model calls in flight is billed with no
  record; on one host any process may register a `SY.*` name (P-1), so a record from a charging
  `SY.*` producer is trusted within the hives §7.5 allows. A node whose router stays down keeps
  1,024 records waiting; past that it drops them, counted only in its own journal.

## 5. Open questions (for the operator)

- **Q1 (stage 2): two cognitions write the same rows.** *Decided (operator, 2026-10-17): §6.*
  - Cognition's entity ids do not include the hive. Each hive's cognition sees its own half of a
    cross-hive thread, numbered by its own router.
  - The upserts are last-writer-wins, and the rebuild reads every writer's rows.
  - The options:
    - key the rows by writer and entity, and rebuild only one's own;
    - or give each thread a single cognition.
  - Operator's direction (2026-10-16), to design after stage 1: *"la idea es que los mensajes y los
    cognitions trabajen juntos, que sean un solo sistema, que el generador del mensaje, evaluador,
    sea el dueño y no se duplique"*.
- **Q2 (stage 3): who is charged.** *Decided (operator, 2026-10-17): a tenant's node charges its
  own tenant; SY.cognition charges the tenant of the communication's agent; SY.frontdesk.gov
  charges the tenant of the case it registers ("OK todo").*
  - A message carries no tenant. Its `src_ilk` is what the sender declared, and the frontdesk
    works exactly on messages without one.
  - Proposal:
    - a tenant's node charges its own tenant ("the node is the one that spends", ILK/TNT model
      §2.3);
    - a core node working on a conversation charges the tenant of the sender the router
      authenticated for that turn, else its destination's, else the root tenant;
    - `tenant_source` says which applied.
  - This needs an indexed ILK → tenant lookup that also resolves aliases.
- **Q3 (stage 3): a caller's timeout.** *Decided (operator, 2026-10-17): recorded as `abandoned`,
  without tokens, for now ("después vemos qué cambiamos si no sirve así").* Cognition wraps each
  call in an 8 s timeout, and the
  provider bills a call that is dropped midway. Should the SDK detach the HTTP call so its
  response is still recorded, or count those calls as abandoned, without tokens?
- **Q4 (stage 3): Archi's spending.** *Decided (operator, 2026-10-17): the root tenant pays
  ("cobranza será a raíz"); each record keeps what Archi's work belongs to when Archi knows it
  (session, pipeline run, solution), the place for the client tenant later. Archi's solutions are a
  topic of their own, for later.* Root tenant, or the tenant whose solution it builds? If the
  latter, the plan request must carry that tenant.
- **Q5 (stage 3): retention.** *Decided (operator, 2026-10-17): nothing is deleted for now; a
  general database policy comes later, by date ("sin dudas necesita fechas").* Keep usage
  forever, with a purge by period only when Cloud asks?
  Does io.cloud show the root tenant's spending (Archi, SY.admin)?
- **Q6 (stage 3): `ai_local_probe`.** *Decided (operator, 2026-10-17): deleted in the repo
  cleanup.* It builds an OpenAI client directly and has no router. Delete
  it (repo cleanup)?
- **Q7 (after stage 1): where the message log lives.** *Decided (operator, 2026-10-16): Archi
  reads `turns` — "que lea turn es integrado mas que lo otro, si es asi hacerlo". Built in 1b:*
  - **`turns` keeps the whole message** (`message`), its `thread_id` and `trace_id`. `ts` is when
    the router copied it; `stored_at`, when SY.storage wrote it, which is the order Archi's live
    tail follows, since a worker's batch can arrive late.
  - **A one-time backfill** (`turns_backfill_from_inbox_v1`, recorded in `storage_migrations`)
    copies the messages the inbox still holds:
    - a 0.1.75 row gets its message;
    - a row of before 0.1.75 (`legacy:<ctx>:<seq>`) takes its inbox key and message;
    - an AI reply `turns` had dropped is inserted with its arrival time.
  - **Then the inbox is purged:** processed rows older than 24 h, never before the backfill ran.
    The inbox keeps only what dedupe and retries need.
  - Archi's Messages page shows time, from, to, kind, ICH and thread; the detail shows the whole
    message, or the payload alone on rows older than the backfill can reach.

## 6. Stage 2: cognition per communication, NATS deleted (designed 2026-10-17)

### 6.0 Facts (verified in code at 0.1.76 and on 8.x, 2026-10-17)

- **What cognition does:** per turn with a `thread_id`, up to two LLM calls (the tagger on that
  message alone; the narrative summarizer on most turns after the first) and a vault lookup of its
  AI key. It derives a thread, contexts, reasons, cooccurrences, a scope and its instance, a memory
  and episodes, with decaying weights. It writes up to ~100 entity envelopes per turn
  (`storage.cognition.*`) and rewrites `/dev/shm/jsr-memory-<hive>` (4 MB, one memory package per
  hot thread).
- **Who uses it:** only ai.generic. The router attaches a thread's package to a message it
  delivers to a local node, by exact `thread_id`, from its hive's region; ai.generic puts it in its
  prompt. The router attaches it to any destination, keeps one the sender set, and serves the last
  snapshot indefinitely when cognition stops.
- **What each cognition sees:** the router copies a turn where the message enters, so in a
  conversation between an IO node on one hive and an AI node on another, each hive's cognition sees
  one direction. The AI's memory is built from its own replies.
- **References:** `sha256(prefix|thread_id|label…)`, per thread, without hive. Two cognitions on
  the same thread overwrite each other (last writer wins, `writer` keeps only the last one) and
  each makes its own scopes. A worker's outputs never reached postgres (its NATS has no consumer),
  and its rebuild always fails: it reads postgres directly and a worker has no database.
- **Order:** `thread_seq` is an in-memory counter per router, reset to 1 when the router restarts.
  Only cognition reads it.
- **Replies:** the AI SDK copies the incoming meta, so an AI's reply carries the human's
  `src_ilk` (a contract test pins it). For cognition, the reply reads as the human speaking.
- **SY.policy:** reads turns and only counts missing `action_class`/`action_result`/
  `result_origin` in a 30 s log line. No LLM, no outputs.
- **8.x data:** test data only (19 threads, mostly probes).

### 6.1 Decisions (operator, 2026-10-17)

- **D14. Cognition belongs to the interlocutors.** *"la parte cognitiva es de los interlocutores
  no de que hive está o que nodo es... Cognitive necesita referencia y armado de los mensajes entre
  ilk, los nodos son mecanismos, igual que el router."* Memory and its references are per
  communication (the thread between ILKs). No hive and no node in the cognitive model: a
  hive-keyed memory would fork when an agent's node moves.
- **D15. The owner of a turn is the owner of the communication:** the agent that serves it, the
  one that generates and evaluates. One evaluation per communication.
- **D16. Cognition stays next to the agents** (motherbee and workers), so ai.generic reads its
  memory from the local shared memory at delivery, as today. It is not centralized.
- **D17. The agent's router feeds its local cognition both directions** of its agents'
  communications.
- **D18. The order carries the router's name.**
- **D19. Rebuild as needed; the test data can be wiped.**
- **D20. SY.policy is removed** (*"nació como una extensión que no ocurrió"*).
- **D21. NATS is deleted entirely, no legacy.** The ingress's 597 turns stuck in its broker go
  with it.
- **D22. The LLM cost stays as is for now** (two calls per turn, replies included).

### 6.2 Stage 2a: cognition per communication

- **A communication is the thread between a human (or another interlocutor) and the agent that
  serves it:** `(thread_id, agent ILK)`. That follows from D14 and D15: two agents in one IO
  conversation are two communications, each with its owner and its memory. Entity references
  become `sha256(prefix|thread_id|agent_ilk|label…)`, still without hive or node.
- **The cognition copy (router).** Every message an agent sends enters through its router, and
  every message it receives is delivered by that router. That router copies, into its stream under
  a new subject `cognition.turns`, one copy per local agent end of the message:
  - **out:** a local agent node (`AI.*`) sends a message with a `thread_id`, of a kind that counts
    as a turn (not system, admin or query_response): copied at entry, owner = that agent;
  - **in:** the router delivers such a message to a local agent node: copied at delivery, owner =
    that agent. The three delivery sites (local, after the WAN, after IRP) serialize one buffer for
    several destinations today; the copy is per destination. Never a tap copy (`meta.via_tap`) nor
    a broadcast;
  - a message between two agents is copied twice, once per owner: each agent's communication sees
    both directions;
  - the copy is taken before the memory package is attached, and carries the owner's node name in
    the record (`producer`). Cognition maps it to the agent's ILK through the identity shared
    memory (`handler_node`), so the node stays mechanism. An agent with no ILK there gets no memory,
    counted (`no_agent_ilk`);
  - `cognition.turns` goes only to `SY.cognition@<own hive>`, on the roles that run it (motherbee,
    worker), and is added to the stream's fixed sink list; no node may append it.
  - The `storage.turns` copy (the message log, at entry, to SY.storage) does not change.
- **The order (D18).** A communication's turns are all copied by its agent's router, so that
  router's stream orders them. Cognition numbers them with the communication's turn number
  (`turn_count`), which it already keeps, persists and restores: it replaces `thread_seq` in the
  hot-set ranking, the scope instances' start and end, and the choice of the open scope at
  rebuild. No clock is involved, and an agent that moves continues the numbering after its
  rebuild.
  - The router stops assigning `meta.thread_seq`. The field leaves the protocol in 2b, with its
    echoes (io-slack, io-linkedhelper, the test nodes, the AI SDK, the Go struct).
- **SY.cognition as a sink.** It takes `STREAM_BATCH` only from its own hive's streams
  (`RT.*.stream@<own hive>`) and acks on receipt into its bounded queue (§2.7).
  - **Repeats:** it skips any record at or below the last seq it accepted into the queue for that
    stream and epoch. The epoch is new in every batch: a random number the stream keeps in
    `state.json` and draws again when that file is missing or the stream runs in memory. Without
    it, a stream whose seq restarts at 1 would be ignored until it passed the old seq.
- **Its outputs.** `STREAM_APPEND` under `storage.cognition.batch`, packing a turn's envelopes
  into appends of at most 64 KB (a rich turn makes ~80 KB, and the stream skips a record over
  96 KB). SY.storage expands each into one inbox row per envelope (`storage.cognition.<entity>`),
  so the per-entity handlers and the producer check (`SY.cognition@<the stream's hive>`) stay.
  - Each envelope carries the communication's turn number; a row is overwritten only by an equal
    or higher one, so a late retry or a cognition that restarted empty cannot roll it back.
- **The memory package** keeps its mechanism (local shared memory, attached at delivery), with
  three fixes: attached only to agent destinations (`AI.*`), per destination; a package the sender
  set is dropped on all three delivery paths (nothing legitimate sets one); keyed by the
  communication (thread and the destination agent).
- **Shared memory survives restarts.** Cognition no longer rewrites the region from its in-memory
  set alone: it keeps the entries of communications it has not loaded yet, within the capacity
  ranking, and drops one only when SY.storage answers that the communication has no state.
- **The rebuild over the mesh (D19).** Cognition stops reading postgres.
  - `COGNITION_STATE_GET {thread_id, agent_ilk}` to `SY.storage@motherbee`, accepted only from
    `SY.cognition@*` as stamped by the router. It returns every row of the communication (closed
    ones too, as the live path keeps them), paged by key within a frame. Scopes have no `thread_id` column: through their
    instances.
  - When: at start, for the communications in the shared memory, before the first turn, with
    bounded concurrency and a timeout per call, on the worker's task (the main loop keeps acking
    the streams meanwhile); on the first turn of a communication it does not hold (an agent that
    moved here); and before the next turn of one held but idle for 5 minutes (its agent may have
    moved away and back), whose stored state wins when it is further along.
  - A communication SY.storage has no state for yet stays in the region as it was: its last
    envelopes may still be on their way.
  - Workers rebuild like the motherbee.
  - The `cognition_*` tables are emptied once (marker `cognition_reset_v1`).
- **Replies between ILKs.** The AI SDK's reply carries `src_ilk` = the agent's own ILK
  (`self_ilk_id`) and `dst_ilk` = the incoming `src_ilk`; an agent booted without an ILK sends no
  `src_ilk`, never the human's. SY.frontdesk.gov uses the same builders. Nothing routes on a
  reply's `src_ilk` (IO nodes reply by `meta.context.io.reply_target`; the identity pre-resolve
  only runs on Resolve). Archi labels replies by `src_ilk` and will show the agent.
  - Cognition's binding compares ILK weights: both participants weigh 1.0 whatever the direction,
    or every reply would look like a topic shift.
- **NATS loses its last turn readers.** 2a stops the router's NATS copy of turns everywhere and
  retires the `durable.sy-cognition.turns` and `durable.sy-policy.turns` consumers (§2.13's
  retired list), so nothing piles up until 2b.
- **SY.policy retired over two releases** (D20). Removing it in one release would block every
  worker's core update: a worker's orchestrator rejects a manifest that lacks a component its
  `hive.yaml` lists, and the core update does not rewrite that file.
  - **2a:** the binary, the manifest entry and the template's sections stay. Each orchestrator, at
    boot and before reading its node list, stops and disables SY.policy and drops it from its own
    role's `nodes` and `wait_for`, in one write, logged once.
  - **2b:** the binary, unit, manifest entry, build lists, stop scripts and every template section
    go.

### 6.3 Stage 2b: NATS deleted

Inventory verified at 0.1.76 (about 8,900 lines go):
- **Whole files:** the broker `src/nats/mod.rs` (1,823 lines), the SDK client `nats.rs` and
  `client_config.rs` (2,815, plus its re-exports in `comm/mod.rs` and `prelude.rs`), the diags
  `wf_nats_diag` and `jetstream_envelope_diag` (628), eight scripts (1,706).
- **Inside files:** SY.storage (~1,000: the subject loops, `storage.metrics.get` over NATS, the
  ingest of `storage.events`/`items`/`reactivation`, which have no producer), SY.admin (~300),
  the router (~190: start, readiness wait, recovery loop, publisher, retired-consumer purge, stop
  on signals in `main.rs`), the orchestrator (~150), cognition, config.
- **New work:**
  - `STORAGE_METRICS_GET` over the mesh in four places: the SDK constant, SY.storage's responder,
    SY.admin's `/config/storage/metrics`, and the orchestrator's boot wait for the database. That
    wait runs before the orchestrator joins the router today: it uses an ephemeral connection
    (`NodeUuidMode::Ephemeral`) or moves after the join;
  - renders without `nats:` (worker, ingress, egress joins) and no `nats` directory (postinst,
    install, joins);
  - on upgrade, once and logged: the pending counts of `embedded-js-*.json`, then the `nats`
    directory and the `hive.yaml` `nats:` section removed;
  - `meta.thread_seq` out of the protocol (§6.2);
  - SY.policy deleted (§6.2).
- Port 4222 has no firewall rule or unit to change.

### 6.4 Gates on 8.x

- **2a:**
  - a conversation with its two ends on different hives (a test sender on worker1, an AI node on
    the motherbee): the motherbee's cognition receives both directions, in order; worker1's
    cognition receives nothing of it;
  - one set of rows per communication in postgres, written by one cognition;
  - the AI's reply carries the agent's ILK; the next message reaches the AI with a package built
    from the human's side too;
  - a temporary ai.generic on worker1: worker1's cognition processes its communication and the
    outputs reach postgres;
  - a cognition restart rebuilds over the mesh, and the packages stay meanwhile;
  - SY.policy runs nowhere and no hive lists it; nothing reads NATS turns; health clean.
  - Unit tests for what PROD cannot produce (one router per hive): the copy across routers of a
    hive, the epoch, two agents in one conversation.
- **2b:** nothing listens on 4222; no `nats` directory or section anywhere; no SY.policy binary or
  unit; the admin's storage metrics and the orchestrator's boot wait work over the mesh; a new
  worker join renders a `hive.yaml` without `nats`; health clean.

### 6.5 Open, for later

- **Cognition throughput:** one turn at a time, two LLM calls each. Processing several
  communications in parallel (each in order) when traffic asks for it.

### 6.6 Review (one round, 2026-10-17)

One HIGH, seven MEDIUM and six LOW, all folded in above:
- **HIGH:** removing SY.policy in one release blocks every worker's core update → two releases.
- **MEDIUM:**
  - a restart blanked the shared memory before the reload → the region keeps what is not loaded;
  - `thread_seq` had no replacement and the overwrite guard leaned on wall clocks → the turn
    number;
  - one append per turn could pass the 96 KB record limit, and `storage.cognition` missed the
    producer checks → packed `storage.cognition.batch`;
  - attributing replies to the agent would split every scope → equal ILK weights;
  - two agents on one `thread_id` collided → the communication is `(thread_id, agent ILK)`;
  - a stream whose seq restarts left cognition deaf → the epoch;
  - the orchestrator's database wait over the mesh ran before its join → an ephemeral connection.
- **LOW:** NATS turns piling up between 2a and 2b; the state query's caller check and paging;
  per-destination copies and packages; replies without an ILK; the SDK re-exports; the gates.

### 6.7 Code review of 2a (one round, 2026-10-17)

No HIGH. One MEDIUM: a communication held while its agent moved away and back went stale →
the idle refetch above. LOW, all fixed: the agent node set right after a fetch; the reload keeps
what SY.storage has no state for yet and runs off the main loop; no memory region read for a
message without a thread; the hive.yaml edit written atomically; scopes guarded by the turn too;
SY.frontdesk.gov's replies carry its own ILK; stale comments and doc passages.

### 6.8 2a on 8.x (0.1.77, 2026-10-17)

Every 2a gate of §6.4 passed (details in `lab/DEPLOYMENTS.md`):
- a test sender on worker1 and an agent on the motherbee: the motherbee's cognition got both
  directions, worker1's nothing; one communication in postgres, with the agent's ILK;
- the replies carry the agent's ILK; the agent gets its communication's package, and a package
  the sender put in is dropped;
- a cognition restart reloads over the mesh and keeps the region; a held communication idle for
  5 minutes is fetched once before its next turn;
- an agent on worker1: worker1's cognition outputs reach postgres through the stream;
- SY.policy runs nowhere (the motherbee's `worker` section still names it until 2b); nothing
  reached NATS; health clean.

For 2b: SY.cognition's start line reports `ai_secret_source=missing`, because the key is now
resolved afterwards on the turn worker.

### 6.9 2b as built, and its code review (one round, 2026-10-17)

As §6.3 planned, about 9,000 lines go: the broker, the SDK client, the two diags, eight scripts,
SY.policy, `meta.thread_seq`, the ingest of the three subjects without a producer (their tables
stay). Where the build differs from the plan, or adds to it:
- **The orchestrator joins its router first,** then waits for SY.storage's database with
  `STORAGE_METRICS_GET` (no ephemeral connection). Its workers start with the join: system
  messages are taken as they come, as the periodic reconcile already allows, and admin commands get
  `ORCHESTRATOR_BOOTING` until the boot ends, so callers fail fast as they did while the
  orchestrator was not on its router yet (`fluxbee-firstboot` waits on exactly that).
- **`STORAGE_METRICS_GET`** is open to any node (read-only counts); one inbox scan runs at a time,
  and a request that finds one running is told so.
- **One-time transitions at boot, logged:** the broker's pending messages per subject, then
  `/var/lib/fluxbee/nats` and the `nats:` section of `hive.yaml` (after the router runs the new
  binary: the old one, stopping, flushes its broker to disk); SY.policy out of every role section
  of `hive.yaml` and its unit and binary removed where left. `system_nodes_for_role` leaves retired
  nodes out for every reader (the boot, the core components a spoke installs, a join's templates),
  even where the rewrite failed. These go once every hive booted 0.1.78.
- SY.cognition reports its AI key source once it looked it up (§6.8).

The review (two lenses: the upgrade path, what the removal left) found no HIGH. One MEDIUM: the
motherbee's old router, stopping during the boot, wrote the broker's state back after the clean-up
→ the clean-up runs after the router restarts. LOW, all fixed: retired nodes filtered for every
reader of a role section; admin commands queued during the boot → `ORCHESTRATOR_BOOTING`; metrics
scans could stack → one at a time; leftover unit and binary on a motherbee installed by
`scripts/install.sh`; stale comments; the admin catalog parity check had missed `list_streams`
(1b) and the WF deploy's new argument (A-68).

On 8.x (0.1.78, 2026-10-17): nothing of NATS or SY.policy is left on any hive; the ingress logged
its 597 unread turns before its broker went; storage metrics and the orchestrator's database wait
work over the mesh; with SY.storage held, admin commands got `ORCHESTRATOR_BOOTING` within 0.01 s
during the boot; a cognition conversation still works; health clean. Details in
`lab/DEPLOYMENTS.md`.

## 7. Stage 3: LLM usage records (designed and built 2026-10-17)

It builds §2.14 on what stages 1 and 2 left: every hive's stream already delivers `storage.usage`
to SY.storage, and a communication's agent is known at each turn. Reviewed once (§7.8); where §7
and §2.14 differ, §7 holds. §7.2–§7.6 describe the code as built; its review is §7.9.

### 7.0 Facts (verified in code at 0.1.78)

- **The calls:** 14 sites in 6 files build a client per operation through the AI SDK's two
  factories:
  - `create_llm_client`: the plain `generate` path, used by SY.cognition's tagger and summarizer,
    and as a fallback by ai.generic and SY.frontdesk.gov;
  - `create_function_calling_model`: the tool-loop `run_turn` path, used by ai.generic,
    SY.frontdesk.gov, SY.architect (all six sites) and SY.admin's executor.
  - Only `ai_local_probe`, a dev tool, builds a provider client directly.
- **Usage today:**
  - `generate` reads none;
  - OpenAI's `run_turn` reads `total_tokens`, Anthropic's `input + output`, with no cache fields;
  - the runner returns the sum only on success: a failed or overlong run loses it.
- **Timeouts:** the SDK sets none and spawns nothing, so a caller that drops the future aborts the
  HTTP request and nothing parses the answer. Callers:
  - SY.cognition wraps each call in 8 s;
  - the AI runtime wraps `on_message` in 60 s and runs it up to 3 times in all; for ai.generic and
    SY.frontdesk.gov only the timeout re-runs it (model errors become a reply), so one message can
    cost three full runs;
  - SY.architect and SY.admin's executor set none.
- **Who is at each call** (all one step away at most):
  - ai.generic: its tenant and ILK (env from the orchestrator), the message's `trace_id`,
    `thread_id`, `src_ilk`;
  - SY.frontdesk.gov: the case's tenant (`case_tenant_of`, computable before the call; a root case
    stops before any call);
  - SY.cognition: the agent ILK's tenant, in the same identity record it already reads for the
    agent ILK; an agent ILK never changes tenant;
  - SY.architect: its session; in the pipeline stages also the run (`PipelineRunRecord`:
    `pipeline_run_id`, `session_id`, `solution_id`, stage);
  - SY.admin's executor: its `execution_id` (not on `executor_validate_plan`), and the caller's
    router-stamped name (not forwarded to the executor today).
- **The transport is ready:** the router accepts `storage.usage` from any producer and sends it to
  SY.storage@motherbee. SY.storage acks it and drops it today (no handler, debug log only).
- **Trust inputs:**
  - the producer is the name the connection registered; only `RT.*` names are reserved (A-65, P-1);
  - SY.storage runs on the motherbee as root and can read `/jsr-identity-motherbee`:
    `find_ilk_by_handler_node_from_hive_id` gives a node's ILK and its tenant; the orchestrator
    registers a node's ILK there before it spawns the node, also for a worker's node;
  - the identity SHM drops deleted ILKs, and a motherbee whose SY.identity could not load its
    database holds only the system ILKs until a restart;
  - SY.admin has no database client; a mesh reply must fit 128 KB.
- **Cloud:** io.cloud claims a tenant per request, never the root one, and SY.admin re-checks the
  relayed content per action.

### 7.1 Decisions (operator, 2026-10-17, §5 Q2–Q6)

- A tenant's node pays its own tenant; SY.cognition pays with the communication's agent's tenant;
  SY.frontdesk.gov with the case's tenant.
- A call its caller abandoned is recorded as `abandoned`, without tokens.
- SY.architect and SY.admin's executor pay with the root tenant; each record keeps what Archi's work
  belongs to (session, pipeline run, solution), the place for the client tenant later.
- Nothing is deleted for now; a general database policy by date comes later.
- `ai_local_probe` goes. It must go now: the provider clients become private to the SDK.
- Earlier (2026-10-16): Fluxbee only records; Cloud owns and reads the data; delivery during a long
  motherbee outage is not addressed now.

### 7.2 The AI SDK: one way to call a model

- **`AiAccount`, required by both factories:**
  - `recorder`: the process's usage recorder (below);
  - `node_name`, `node_ilk`;
  - `tenant_id` and `tenant_source`: `node`, `agent`, `case`, `root`, or `fallback_root` when the
    tenant a rule names is unknown (then the tenant is the root one);
  - `counterpart_ilk`, `trace_id`, `thread_id`, all optional;
  - `work`: optional, fixed keys with short values (Archi: `session_id`, `pipeline_run_id`,
    `solution_id`, `stage`; SY.admin's executor: `execution_id`, `caller`).
- **The recorder:** one task per process, fed by a bounded channel (1,024 records) through a
  synchronous `try_send`. It appends each record to the node's router stream with the dispatcher's
  current sender. A full channel or a failed append is counted and logged in the node's journal,
  never returned to the caller. The AI runtime starts it and hands it to its node; SY.* nodes
  start their own.
- **An append waits for the node's link** (`stream::append`, for cognition's packs too): the SDK
  drops what was queued for a link it is reconnecting, so a record made while the router restarts
  waits in the recorder's channel instead.
- **One record per 2xx response,** on both paths, queued as soon as the body is parsed and before
  any check: truncation, missing text or a failed schema still cost money.
  - `run_turn` records each turn as it answers, so a run that fails later keeps its earlier turns.
  - Anthropic's retries record only the attempt that answered 2xx.
  - A 2xx whose body cannot be read is recorded `unreadable`, without tokens.
  - Non-2xx and transport errors carry no usage: no record.
- **`abandoned`:** a guard is armed while a request is outstanding and disarmed on every return;
  if the caller drops the call while it is armed, the guard's `Drop` queues `abandoned`, without
  tokens, with the same `try_send`.
- **`outcome`** comes from the response body: `ok`, `truncated` (OpenAI `status` incomplete /
  `incomplete_details`; Anthropic `stop_reason` `max_tokens`), `refused` (Anthropic `refusal`),
  plus `unreadable` and `abandoned`.
- **The counts, normalized** (a null or missing field counts 0):
  - OpenAI (Responses): `uncached_input` = `input_tokens` − `input_tokens_details.cached_tokens`;
    `cache_read` = the cached tokens; `cache_write` = 0; `output` = `output_tokens` (reasoning
    included); `reasoning` = `output_tokens_details.reasoning_tokens`;
  - Anthropic (Messages): `uncached_input` = `input_tokens`; `cache_read` =
    `cache_read_input_tokens`; `cache_write` = `cache_creation_input_tokens` (its 5-minute and
    1-hour split stays in the raw usage); `output` = `output_tokens` (thinking included);
    `reasoning` absent;
  - the provider's raw `usage` (capped at 2 KB), the provider's response `id`, the model it served
    and OpenAI's `service_tier` are kept. Pricing is Cloud's, per record.
- **What Archi's budget sees does not change:** the runner still sums what the provider reports.
- The provider clients become private; the factories are the only way in. The mock client stays
  for tests.

### 7.3 Who pays, per site

| Node | Sites | `tenant_id` | `tenant_source` | Also |
|---|---|---|---|---|
| ai.generic | 2 | its own (`FLUXBEE_NODE_TENANT_ID`) | `node` | counterpart = the message's `src_ilk`; trace, thread |
| SY.frontdesk.gov | 2 | the case's (`case_tenant_of`, before the call) | `case` | counterpart, trace, thread |
| SY.cognition | 2 | the agent ILK's (cached with it; the cache follows the identity SHM's `seq`) | `agent` | counterpart = the agent ILK; trace, thread |
| SY.architect | 6 | root | `root` | `work` = session, pipeline run, solution, stage, as known |
| SY.admin executor | 2 | root | `root` | `work` = `execution_id`, the caller's router-stamped name |

Where a rule's tenant is unknown (no case tenant yet, an agent without a tenant), the call is
still made and recorded against the root tenant, `fallback_root`. What SY.storage does with these
values is §7.5: for `node` and `agent` it takes the tenant from identity itself.

### 7.4 The record and the stream

- **One `storage.usage` record per call,** through `stream::append`, about 1–2 KB:
  `record_id` (a UUID), `provider_response_id`, `ts` (when the answer arrived, the producer's
  clock), `hive`, `node_name`, `node_ilk`, `tenant_id`, `tenant_source`, `counterpart_ilk`,
  `provider`, `model`, `served_model`, `service_tier`, `kind` (`generate`, `function_turn`),
  `outcome`, `uncached_input`, `cache_read`, `cache_write`, `output`, `reasoning`, `raw_usage`,
  `trace_id`, `thread_id`, `work`.
- **Volume:** a human↔agent exchange makes about 5–7 records (cognition's 2 calls per direction,
  ai.generic's tool turns and its `generate` fallback): about 6 KB, against about 160 KB of
  cognition's envelopes.
- **A reserve in the stream:** `storage.usage` keeps 1 MB of the nodes' 8 MB disk share and 128
  slots of the nodes' queue, so cognition's packs cannot crowd it out; its losses are counted apart
  and reported as `usagelost`. Its window stays 120 s, like every record.

### 7.5 SY.storage

- **Into the inbox like every subject,** checked when the row is processed. A check that cannot be
  answered (identity unreadable, the ILK not there yet) is a processing error, so the inbox retries
  it (10 × 60 s).
- **Checks,** on top of the stream's (the producer belongs to the stream's hive):
  - `node_name` must be the producer;
  - **a node outside `SY.*`** pays its own tenant: SY.storage takes the tenant from the motherbee's
    identity (the producer's ILK by `handler_node`), not from the record. A record that claims
    another tenant is stored with the identity's tenant and counted as a mismatch;
  - **SY.cognition** (motherbee and worker hives only): the tenant is taken from the
    `counterpart_ilk` (the agent ILK) in identity;
  - **SY.frontdesk.gov** (motherbee only): the case's tenant is taken as recorded;
  - **SY.architect and SY.admin** (motherbee only): the root tenant only;
  - `fallback_root` means the root tenant; any other `SY.*` producer is refused.

  The identity snapshot is read once per batch of rows, so a node name reused by another tenant is
  never charged from a stale cache.
- **When identity never answers** (still unresolved on the last retry: a deleted node, a degraded
  SY.identity): the record is stored with the tenant it claims, `verified = false`, and counted.
  Spend is never dropped for a lookup.
- **Refused records** (a wrong name, a foreign `SY.*`, a malformed or oversized record, a negative
  count) are acked and dropped, and counted. One WARN line every 30 s gives the counts (refused,
  mismatched, unverified); `ops.py health` shows them as `usagerej`.
- **Table `llm_usage`:** the record's fields plus `claimed_tenant_id`, `stored_at` and `verified`;
  key `record_id`, and `provider_response_id` unique where present (`ON CONFLICT DO NOTHING`: one
  response lands once); indexes `(tenant_id, ts)`, `(node_name, ts)`, `(ts)`, `(stored_at,
  record_id)` and `(tenant_id, stored_at, record_id)`; no foreign key to the tenants (a purge stays
  free). Nothing is purged.
  - `stored_at` is the `now()` of an autocommit insert on SY.storage's one shared connection, so it
    follows commit order and a `since` cursor never passes a row that commits later. Inserting from
    another connection or inside a transaction would break that.
- **`USAGE_QUERY` over the mesh,** from `SY.admin@motherbee` only, one at a time, on its own
  database connection with a 15 s statement timeout, every filter a bound parameter:
  - a tenant, or every tenant only with an explicit `all_tenants` that SY.admin sets for the
    operator and Archi, never for a relayed Cloud request;
  - a period by `ts`, at most 92 days;
  - either a grouping from a closed list (`day`, `model`, `node`, `tenant`) with calls and the five
    sums per group, or the records themselves; both paged under 64 KB with a cursor;
  - records also by `stored_at` with a `since` cursor, so a reader can sync incrementally: rows
    arrive late by design (the stream's window, the inbox retries), and a period is final only
    after them.

  As built:
  - a period is `[from, to)`; a page holds 1 to 1,000 rows (500 by default) and stays under 62 KiB
    with at least one row; the limits are `fluxbee_sdk::usage`'s;
  - a records cursor is `<instant, µs>|<record_id>` (`ts` in a period, `stored_at` in a sync); a
    groups cursor is the last key; `since` also takes a bare RFC 3339 instant to start;
  - `next` is absent when a period has no more pages, and in a sync when nothing new arrived (the
    reader keeps its cursor);
  - groups come in key order; a `day` is `YYYY-MM-DD` in UTC;
  - a refusal carries `error_code`: `INVALID_REQUEST` (the question, a cursor), `FORBIDDEN` (a
    caller other than SY.admin@motherbee), `USAGE_UNAVAILABLE` (no database, a failed query or
    another one running).

### 7.6 Reads

- **SY.admin:** `get_llm_usage` (`GET /hives/motherbee/usage?tenant_id=&from=&to=&group_by=&since=&after=&limit=`),
  for the operator and Archi.
  - Without a `tenant_id` the HTTP route asks for every tenant. Every tenant at once is for the
    operator (the HTTP route, the executor) and `SY.architect@motherbee`; over the mesh only Archi
    and IO.cloud may ask (the relay rule refuses the rest).
  - The admin checks the question first (400 `INVALID_REQUEST`), then maps SY.storage's
    `error_code`: `INVALID_REQUEST` → 400, the rest → 503.
  - Archi reads it with `fluxbee_system_get` on `/hives/motherbee/usage?…`, the same query string.
- **Cloud:** a Cloud op `get_usage` → `get_llm_usage` with io.cloud's claimed tenant, never the root
  one. It is a Cloud-only action (not shared with other nodes), and SY.admin's relay rule for it
  requires the claimed tenant and only the known fields. Cloud sees its tenant's spending; the root
  tenant's (Archi, SY.admin) stays with the operator. A tenant's records name another tenant's
  people only as ai.generic's counterpart, an ILK the agent already received.

### 7.7 Gates on 8.x

- An agent in the `pruebas` tenant, with a role, answers two turns: its records are charged to
  `pruebas` (`node`); cognition's for the same turns to `pruebas` (`agent`); counts above zero; the
  provider response ids unique.
- An Archi chat turn: a record charged to the root tenant, with its session in `work`.
- A turn through SY.frontdesk.gov charges the case's tenant.
- A probe node appending a record that claims another tenant: stored with its own tenant, counted
  as a mismatch; a probe under a foreign `SY.*` name: refused; both in the WARN line and `usagerej`.
- SY.cognition's tagger timeout set to 50 ms: its calls are recorded `abandoned`, without tokens;
  the timeout restored.
- `GET /hives/motherbee/usage` by tenant, by day, by model, the paged records and the `since`
  cursor; the Cloud op for `pruebas` answers, and a request for the root tenant through io.cloud is
  refused.
- Nothing lost in the streams (`usagelost` 0); health clean.

### 7.8 Review (one round, 2026-10-17)

Two lenses (accounting; trust and Cloud). One HIGH, seven MEDIUM, nine LOW; all folded in above:
- **HIGH:** refusing a producer that identity does not know dropped real spend (a degraded
  SY.identity holds only the system ILKs; deleted nodes; a name reused inside a cache) → checks at
  processing time with the inbox's retries, the tenant taken from identity, and `verified = false`
  instead of a drop.
- **MEDIUM:**
  - the record rode on the caller's future, and a `Drop` cannot send → a per-process recorder fed
    by `try_send`, the record queued before any check, the guard armed only while a request is
    outstanding;
  - cognition's packs could crowd usage records out of the stream → a reserve, `usagelost`;
  - reads by `ts` missed late rows → paging by `stored_at`, a `since` cursor;
  - `USAGE_QUERY` could stall SY.storage → its own connection, one at a time, a statement timeout,
    a bounded period, a closed grouping list;
  - cognition's agent cache and a reused node name → the tenant from identity, the cache on `seq`.
- **LOW:** `outcome` from the body (OpenAI's tool turns had no truncation check); `unreadable`;
  the provider's response id as a unique key; nulls as 0, the served model and service tier kept,
  pricing per record; grouped answers paged, `work` and `raw_usage` capped, negative counts
  refused; an explicit `all_tenants`; Cloud-only action with its own relay rule; charging `SY.*`
  producers accepted only from their hives; doc fixes.
- **Known limits (§4):** a node killed with calls in flight (an upgrade, an OOM) is billed with no
  record, not even `abandoned`; on one host a process can still register a `SY.*` name (P-1), so the
  hive narrowing above is the cheap part of that fix.

### 7.9 Code review (one round, 2026-10-17)

Two lenses (accounting on the producer side; trust, storage and reads), on the code before 0.1.79:
- **HIGH (trust):** the router narrowed the charging `SY.*` names with exact case and SY.storage read
  them without case, so a process registering `sy.frontdesk.gov@<ingress>` could charge any tenant
  as verified → the router reads them without case too.
- **MEDIUM:** a record appended while the node's router restarted was queued, then dropped by the
  SDK on reconnect, and counted nowhere → `stream::append` waits for the link (§7.2).
- **LOW, fixed:**
  - Archi's failure classifier recorded no session or run → it uses its pipeline's context;
  - a finished pipeline run was named in the work of later chat turns → only a running one;
  - Cloud's per-tenant sync had no matching index → `(tenant_id, stored_at, record_id)`.
- **LOW, left to A-62:** the executor runs any plan as the operator for any mesh node (A-62), so a
  plan can read every tenant's usage. The usage read changes nothing there.
- **Checked:**
  - one record per 2xx on every path, `abandoned` only while a request is outstanding;
  - the counts as §7.2 says, and every site as §7.3;
  - no other way to a provider;
  - the reserve;
  - the query's parameters, connection, timeout and one-at-a-time rule;
  - cursors round-trip at µs;
  - the contract the same in SY.storage, SY.admin, io.cloud, Archi and `docs/io-cloud-api.md`
    §5.5;
  - `ops.py health` reads the router's and SY.storage's lines.
