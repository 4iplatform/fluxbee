# Router stream — v1

Status: design, reviewed once (2026-10-16). Finding A-70 (`lab/logbook/FINDINGS.md`).

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
    Messages page reads it, and the AI replies `turns` dropped exist only there (Q7).

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
  - Its UUID is a UUIDv5 of the router's UUID, kept under `state/<router L2 name>/`, so no node
    file under `state/nodes` can collide with it.
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
| `storage.turns` | `SY.storage@motherbee`, `SY.cognition@motherbee`, `SY.policy@motherbee` | `SY.storage@motherbee`, `SY.cognition@<hive>`, `SY.policy@<hive>` | `SY.storage@motherbee` |
| `storage.cognition.*` | `SY.storage@motherbee` | `SY.storage@motherbee` | — |
| `storage.usage` | `SY.storage@motherbee` | `SY.storage@motherbee` | `SY.storage@motherbee` |

Each sink has its own cursor. A slow or absent sink falls behind on its own and holds back no
other sink.

### 2.4 The log on disk

- **Where:** `/var/lib/fluxbee/stream/<router L2 name>/`, owned by the router.
- **The thread:** all of it runs on the stream's own OS thread (D13).
- **Segments:** append-only files named by their first seq, up to `STREAM_SEGMENT_BYTES` each.
- **A record:** `[u32 len][u32 crc32][u64 seq][u64 ts_ms][u16 subject_len][subject][u16
  producer_len][producer][payload]`.
- **`next_seq`** is persisted next to the cursors, so seqs never restart under live cursors.
- **Cursors:** `cursors.json` (sink name → last acked seq), rewritten by temp file and rename at
  most once a second, and only up to what has been fsynced.
- **Durability:**
  - every write goes to the file at once, so a router crash loses nothing written;
  - an fsync runs every `STREAM_FSYNC_INTERVAL_MS`, so a power cut loses at most that long.
- **Start:**
  - the newest segment is cut at its first bad record (length or crc);
  - each cursor is capped at the last seq on disk;
  - cursors of sinks no longer in the table are dropped.
- **Trim:**
  - a segment is deleted when every sink's cursor is past it, or when its newest record is older
    than the window;
  - the newest segment is rolled first and never deleted, so an idle router keeps its seqs;
  - records a sink had not acked when their segment went count as expired for that sink.
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
    payload embedded as raw JSON (`RawValue`), byte for byte.
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
/// At most one alarm line per interval, with the counts since the last one (D5, "log reducido").
pub const STREAM_ALARM_INTERVAL_SECS: u64 = 30;
```

**The alarm:** when anything is dropped (cap, full queue, expired, oversize, refused), the stream
logs one WARN line per `STREAM_ALARM_INTERVAL_SECS` with the counts by reason and subject. The
counters are also in its status, and `lab/ops.py health` counts them per hive.

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
    as today, then acks up to the last seq. It processes the rows after the ack.
  - **Checks on the producer:**
    - `storage.cognition.*` must come from `SY.cognition@<the stream's hive>`;
    - usage records are checked as §2.14 says.
  - **Its INFO log line per system message** drops to debug for `STREAM_BATCH`.
- **SY.cognition and SY.policy (local, `storage.turns`, best effort):**
  - **They ack on receipt.** They process from a bounded queue in their own task, and a full queue
    drops and counts.
  - **They skip any seq at or below the last one they applied for that stream,** so a repeat costs
    nothing. Today a repeat is processed again, and every pass pays the LLM again.
  - **Cognition sends its outputs with `STREAM_APPEND`** (`storage.cognition.*`). From a worker
    they reach the database for the first time (Q1).

### 2.8 The SDK

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
  - appended, by subject;
  - delivered and expired, by sink;
  - dropped, by reason;
  - the backlog in records and bytes, and the age of the oldest;
  - the last ack per sink.
- **SY.admin:** `GET /hives/{hive}/streams` asks every `RT.*.stream@<hive>` of that hive.
- **`ops.py health`:** it adds the stream drops since the given time.

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

  Processed rows are not purged (Q7).
- **The `turns` migration** runs once and does not touch the table again. Rolling back to 0.1.74
  or earlier needs the VM snapshot: the old code inserts with `ON CONFLICT (ctx, seq)`, which no
  longer has a unique index.
- **`ts` from the record:** comes with the stream (1b); on NATS a turn arrives within
  milliseconds.

### 2.14 Stage 3: LLM usage records

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
| 2 | Cognition and policy as sinks; cognition's outputs by `STREAM_APPEND`; `STORAGE_METRICS_GET` over the mesh; NATS removed everywhere (§2.11) | cognition on the motherbee and worker1 tags turns; worker1's cognition outputs reach the database; nothing listens on 4222; a new worker join renders a `hive.yaml` without `nats`; health clean |
| 3 | The AI function and the usage records (§2.14). The SDK part may be built during stages 1 and 2. | one call from each kind of consumer leaves a record with the right tenant, node, model and tokens; a forged record is refused; the admin answers by tenant and period; Cloud reads it through io.cloud |

Each stage: code and unit tests, one adversarial review, release, validation on 8.x, then FINDINGS,
the ledger and the logbook.

## 4. Out of scope and known limits

- **A sink away longer than the window:** what it misses is lost and counted (D10).
- **The ingress:** it ships its turns like any hive, and keeps their content on its disk only until
  SY.storage acks. A special mode for the DMZ is for later (D9).
- **SY.cognition's rebuild on a worker:** it reads postgres directly, and a worker has no
  database. The stream carries data to the database, not back.
- **A local process can still HELLO with any other node's name** (P-1, postponed with A-22). The
  streams are out of it: an `RT.*` name needs the router's own process (§2.1).
- **An Anthropic retry after a transport error can be billed twice:** one record, two charges.
- **Cognition loses its queue on a restart:** up to 256 turns acked but not processed yet, and,
  with a backlog, the newest past the queue. Best effort (D4).
- **Hub-vouched senders and non-gateway routers** (§2.12).

## 5. Open questions (for the operator)

- **Q1 (stage 2): two cognitions write the same rows.**
  - Cognition's entity ids do not include the hive. Each hive's cognition sees its own half of a
    cross-hive thread, numbered by its own router.
  - The upserts are last-writer-wins, and the rebuild reads every writer's rows.
  - The options:
    - key the rows by writer and entity, and rebuild only one's own;
    - or give each thread a single cognition.
  - Operator's direction (2026-10-16), to design after stage 1: *"la idea es que los mensajes y los
    cognitions trabajen juntos, que sean un solo sistema, que el generador del mensaje, evaluador,
    sea el dueño y no se duplique"*.
- **Q2 (stage 3): who is charged.**
  - A message carries no tenant. Its `src_ilk` is what the sender declared, and the frontdesk
    works exactly on messages without one.
  - Proposal:
    - a tenant's node charges its own tenant ("the node is the one that spends", ILK/TNT model
      §2.3);
    - a core node working on a conversation charges the tenant of the sender the router
      authenticated for that turn, else its destination's, else the root tenant;
    - `tenant_source` says which applied.
  - This needs an indexed ILK → tenant lookup that also resolves aliases.
- **Q3 (stage 3): a caller's timeout.** Cognition wraps each call in an 8 s timeout, and the
  provider bills a call that is dropped midway. Should the SDK detach the HTTP call so its
  response is still recorded, or count those calls as abandoned, without tokens?
- **Q4 (stage 3): Archi's spending.** Root tenant, or the tenant whose solution it builds? If the
  latter, the plan request must carry that tenant.
- **Q5 (stage 3): retention.** Keep usage forever, with a purge by period only when Cloud asks?
  Does io.cloud show the root tenant's spending (Archi, SY.admin)?
- **Q6 (stage 3): `ai_local_probe`.** It builds an OpenAI client directly and has no router. Delete
  it (repo cleanup)?
- **Q7 (after stage 1): where the message log lives.**
  - Archi's Messages page reads `storage_inbox` as the system-wide message log, so the inbox
    cannot be purged.
  - The AI replies `turns` dropped before 0.1.75 exist only there.
  - Options:
    - Archi reads `turns`, complete from 0.1.75 on, plus a one-time backfill of the old replies
      from the inbox; then the inbox keeps only what dedupe and retries need;
    - or the inbox stays the log, with no purge.
