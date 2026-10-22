# Storage: the router stream and SY.storage

**Status:** v2.0, as it is in 0.1.79
**Date:** 2026-10-17
**Audience:** developers of the router, of SY.storage, and of the nodes that write or read records

This doc says what the code does today, and names the code that backs it. The main code is in
`src/bin/sy_storage.rs` (SY.storage) and `src/router/stream.rs` (the router stream); a function or
constant named without a path lives in the first one. The design and its decisions are in
`docs/router-stream-v1.md` (A-70).

> **Short history.** The old transport (NATS) was deleted in 0.1.78: there is no port 4222, and no
> `nats:` section in `hive.yaml`. Since 0.1.76 the turns arrive through the stream of each router;
> since 0.1.77, what cognition derives; since 0.1.79, the LLM usage records (`storage.usage`, table
> `llm_usage`). The 0.1.79 cleanup removed the `events`, `memory_items` and
> `storage_reactivation_applied` tables, the one-time reset of the cognition tables and the one-time
> backfill of `turns`. The cleanup after 0.1.80 folded the migrations of `turns` and
> `storage_inbox` into their base schema and removed the `storage_migrations` table. A database
> created before 0.1.81 needs, once, `ALTER TABLE turns ALTER COLUMN message SET NOT NULL;` and,
> after the upgrade, `DROP TABLE storage_migrations;` (`CREATE TABLE IF NOT EXISTS` does not change
> an existing table). Since 0.1.83 nothing writes `turns.ctx` and `seq`; the release after it drops
> them (§6.1).

---

## 1. Summary

Each router carries a stream: a short log on disk that delivers its records in batches with an ack.
The router never waits, neither for the log nor for the database. It is best effort: it smooths out
timing, it is not a store. What a sink does not take within the window (120 s, or 32 MB per router)
is lost and counted.

```text
┌─────────────────────────────────────────────────────────────────┐
│                      KEY PRINCIPLE                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  No process waits for PostgreSQL.                               │
│  The stream of each router is the buffer. SY.storage is the     │
│  only one that writes to the fluxbee_storage database.          │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

SY.identity has its own database (`fluxbee_identity`, see `docs/reference/identity.md`).
SY.architect reads `turns` directly from `fluxbee_storage`, with `SELECT` only
(`src/bin/sy_architect/messages_db.rs`).

| Problem | Solution |
| --- | --- |
| Router blocked waiting for the database | The copy goes to a bounded queue of the stream; the router never waits |
| The WAN goes down | The stream keeps the records and retries within the window; what goes past it is lost and counted |
| PostgreSQL slow or down | Without a database, SY.storage does not ack and the stream resends; if the connection drops, SY.storage exits and systemd restarts it |
| Duplicates from retries | Each record lands on the same inbox row (`dedupe_key`) and each table absorbs the repeat |
| Arrival order | It does not matter: each row has its key; the cognition rows also have their turn number |

---

## 2. Architecture

### 2.1 Overview

```text
 Any hive (motherbee, worker, ingress, egress)
 ─────────────────────────────────────────────
  node ──► router ──► normal routing (does not wait for the log)
             │
             │ copies each message (storage.turns) and the turns of its
             │ AI.* agents (cognition.turns); takes the allowed
             │ STREAM_APPEND (storage.cognition.batch, storage.usage)
             ▼
  RT.<router>.stream@<hive>      log on disk, window 120 s / 32 MB
             │
             │ STREAM_BATCH batches to each sink, which answers STREAM_ACK
             │ (through normal routing: local, IRP or WAN)
     ┌───────┴──────────────────────────┐
     ▼                                  ▼
  SY.cognition@<same hive>           SY.storage@motherbee
  (cognition.turns; motherbee        (storage.turns, storage.cognition.*,
   and workers only)                   storage.usage)
     │ jsr-memory (local SHM)           │
     │ STREAM_APPEND                    ▼
     │ storage.cognition.batch       storage_inbox ──► turns
     │ storage.usage                               ──► cognition_*
     └──► stream of its router                     ──► llm_usage
          ──► SY.storage@motherbee                     (PostgreSQL)
```

### 2.2 Components

| Component | Where | What it does |
| --- | --- | --- |
| Router | all hives | Copies the turns to its stream without waiting; takes the `STREAM_APPEND` of its nodes |
| Router stream (`RT.<router>.stream@<hive>`) | one per router | Log on disk with a window; delivers batches with an ack to its sinks |
| SY.storage | the motherbee only | Takes the batches into `storage_inbox` and from there to PostgreSQL; answers reads over the mesh |
| SY.cognition | motherbee and workers | Takes `cognition.turns` from the streams of its hive; sends what it derives and its LLM usage through the stream |
| Nodes that call a model | wherever they run | Send one usage record per call (`storage.usage`) |

---

## 3. The router stream

### 3.1 Where it lives

It is not configured. Each router runs its stream, which is also a node of that router
(`stream_node_name`: `RT.gateway.stream@worker1`; the `RT.` prefix belongs to the router process).
The limits are constants in `src/router/stream.rs` (§12).

The log lives in `/var/lib/fluxbee/state/<router L2>/stream/` (`stream::open`): append-only
segments of 4 MB and a `state.json` with the next seq, the epoch and the cursor of each sink. If the
directory is not usable, the router carries on and the stream keeps records in memory only.

### 3.2 How records come in

1. **Turn copy** (`turn_copy`, `src/router/mod.rs`): the router where a message comes in copies it
   as `storage.turns`, with the real name of the sender in `routing.src_l2_name`. It does not copy
   the `system`, `admin` or `query_response` types (`should_publish_turn`). Each message is copied
   only once, on the router of its sender.
2. **Copy for cognition** (`cognition_copy`): the router where an agent (`AI.*`) is connected
   copies as `cognition.turns` each turn with a `thread_id` that the agent sends (when it comes in)
   or receives (when it is delivered). No tap copies and no broadcasts. Details in
   `docs/reference/cognition.md` §2.
3. **`STREAM_APPEND {subject, payload}`**: a node sends it to its own router
   (`fluxbee_sdk::stream::append`, with no reply). The router consumes it and does not route it to
   any node (`take_stream_append`). The producer is the name of the connection. It only accepts
   (`subject_allowed`):
   - `storage.cognition.*` from `SY.cognition@<this hive>`;
   - `storage.usage` from any node, except SY.cognition outside the motherbee and the workers, and
     SY.frontdesk.gov, SY.architect or SY.admin outside the motherbee (`usage_producer_allowed`);
   - `storage.turns` and `cognition.turns` never: only the router copies them.

   A rejected append is counted, with one log line every 30 s at most.

The router never waits. It leaves the record in a bounded queue, and the disk work runs on a thread
of its own. If the queue is full, or more than 16 MB are waiting for the disk, the record is dropped
and counted. The turns and the appends have separate queues and caps; `storage.usage` has a reserve
in both (1 MB and 128 slots), so the cognition packages do not crowd it out.

### 3.3 Delivery

- **One sender per sink** (`run_sink`): each sink has its cursor and a single batch in flight
  (`STREAM_BATCH`, up to 256 records and 96 KB), so it receives in order.
- **Ack:** the sink answers `STREAM_ACK {up_to_seq}` and the stream moves its cursor forward. Only
  the ack from the sink of that batch counts (`accept_ack`).
- **Retries:** with no ack in 5 s, the same batch is resent, with a backoff from 0.5 to 8 s. It is
  "at least once": every sink absorbs repeats.
- **Expired:** a record older than the window is skipped for that sink and counted; so is one that
  does not fit in a frame.
- **Epoch:** each batch carries the epoch of the stream, which changes when the seq may restart
  (`state.json` was lost, or the stream runs in memory).

### 3.4 Losses and status

What is lost (cap, full queue, expired, too large, rejected) is counted once and shows up in an
alarm line every 30 s. `lab/ops.py health` shows it as `streamlost`, and what is lost from
`storage.usage` as `usagelost`. The stream answers `STREAM_STATUS`, and the SY.admin
`GET /hives/{hive}/streams` lists the streams of a hive (`handle_list_streams`).

---

## 4. Between hives (no bridge)

There is no separate bridge: a batch is just another message. The stream is a node of its router
and the gateway announces it in the LSA, so its `STREAM_BATCH` messages go out through normal
routing (local, IRP to another router of the hive, WAN), and the `STREAM_ACK` messages come back by
name. If the WAN goes down, the stream retries within the window; what goes past it is lost and
counted.

All hives send to `SY.storage@motherbee` (`sinks_for`). The ingress and the egress do not run
SY.cognition: there, `cognition.turns` has no sink.

---

## 5. SY.storage

### 5.1 Where it runs, and its database

- **Only on the motherbee** (`main`). With another role it exits without doing anything; the
  motherbee must have `hive_id: motherbee`.
- **The database URL** comes from SY.vault: the `postgres` resource of the root tenant's pool
  (`resolve_database_url`). At boot it waits up to 60 s for the vault (`VAULT_BOOT_WAIT`).
- **The secret carries the host and the credentials, with no database.** If it carries a database
  name, SY.storage rejects it and boots without a database (`database_config_from_url`). The
  database is `fluxbee_storage` (`STORAGE_DB_NAME`); if it does not exist, SY.storage creates it
  (`ensure_database_exists`).
- **The schema** is ensured at every boot (`ensure_schema`): it creates the missing tables and
  indexes (`TURNS_SCHEMA_SQL`, `INBOX_SCHEMA_SQL`, `COGNITION_SCHEMA_SQL`, `USAGE_SCHEMA_SQL`).
  Each statement waits at most 5 s for its lock (`lock_timeout`, `SCHEMA_STEPS`); past that the
  start fails and systemd retries, so a DDL never queues the table's readers and writers behind a
  long transaction. There are no migrations: an existing table is left as it is, except that
  `turns.ctx` and `seq` lose their `NOT NULL` (§6.1).
- **Three connections** (`Storage`):
  - a shared one, for processing and reads;
  - one for ingest, one transaction per batch;
  - one for `USAGE_QUERY`, which opens with the first query.
- `CONFIG_SET` carries no secret fields: it rejects `config.database.postgres_url` and
  `postgres_url_ref` (`apply_storage_config_set`). `CONFIG_GET` and `STATUS` say whether the
  database is configured (`configured` or `missing_secret`).

### 5.2 Interfaces

Everything arrives over the mesh, as `system` messages (`build_storage_rpc_profile`).

| Message | Direction | Use |
| --- | --- | --- |
| `STREAM_BATCH` | IN | Batches from the streams (`RT.*.stream@*`): `storage.turns`, `storage.cognition.batch`, `storage.usage` |
| `STREAM_ACK` | OUT | Ack to the stream, after the batch is written into `storage_inbox` |
| `COGNITION_STATE_GET` | IN/OUT | The rows of a communication, paged; only for `SY.cognition@*` (§7.1) |
| `STORAGE_METRICS_GET` | IN/OUT | Inbox metrics, for any node (§7.2) |
| `USAGE_QUERY` | IN/OUT | The LLM usage records; only for `SY.admin@motherbee` (§7.3) |
| `CONFIG_GET`, `CONFIG_SET`, `PING`, `STATUS` | IN/OUT | Node control (§5.1) |
| `VAULT_SECRET_CHANGED` | IN | A new `postgres` secret restarts the process (§5.6) |

### 5.3 Ingest

`run_stream_ingest` takes the batches one at a time, in arrival order (`ingest_stream_batch`):

1. **The sender:** it only accepts a batch from a stream (`is_stream_node_name` on the
   `src_l2_name` that the router stamps). Any other sender: dropped without an ack.
2. **The records** (`stream_rows`). These are dropped, and acked anyway:
   - a record whose producer is not a node of the stream's hive;
   - a `storage.cognition.*` that does not come from `SY.cognition@<the stream's hive>`;
   - a turn whose `routing.src_l2_name` is not its producer;
   - a usage record that `check_usage_record` rejects (§6.3);
   - a subject with no handler.
3. **`storage.cognition.batch`** is expanded: each envelope of the package gets a row of its own,
   under `storage.cognition.<entity>`, as if it had come alone.
4. **The inbox:** in one transaction, an upsert of each row into `storage_inbox` by its
   `dedupe_key` (`upsert_stream_rows`). The key is
   `<subject>:<FNV-1a hash of subject and payload>:<length>` (`message_dedupe_key`), so a resent
   batch lands on the same rows.
5. **The ack:** `STREAM_ACK {up_to_seq}` with the last seq of the batch, only after the commit.
6. **Processing** runs in a separate task (a queue of 64 batches, `STREAM_PROCESS_QUEUE`), so the
   ack never waits. If that queue is full, the rows wait for the inbox retry.

If there is no database, if the transaction fails, or if the ingest queue (64,
`STREAM_INGEST_QUEUE`) is full, the batch is not acked and the stream resends it.

### 5.4 The inbox

`storage_inbox`: `dedupe_key` (key), `subject`, `payload` (bytes), `received_at`, `source_ts` (the
moment the router appended the record), `attempts`, `processed_at`, `last_error`.

- **Processing a row** (`process_inbox_message`): the handler of its subject is applied (§5.5). If
  it works, `processed_at` is set; if it fails, `last_error` is set.
- **A repeated row** adds one attempt. If it was already processed, it is not processed again.
- **Retries** (`replay_pending_messages`), of the pending rows with fewer than 10 attempts:
  - at boot, of any age;
  - after that, every 60 s (`run_inbox_maintenance_loop`), the ones older than 60 s
    (`INBOX_RETRY_MIN_AGE_SECS`), so as not to step on live processing;
  - in arrival order, 200 at a time and up to 20 rounds per pass.
- **Ten attempts** at most (`INBOX_MAX_ATTEMPTS`). A row that reaches 10 stays in the inbox, with
  its last error, and shows up in the metrics. On its last attempt, a usage record whose tenant
  could not be verified is stored anyway, unverified (§6.3).
- **Purge:** on each pass, the processed rows older than 24 h are deleted
  (`STORAGE_INBOX_RETENTION_HOURS`), up to 10,000 per pass (`INBOX_PURGE_BATCH`). The inbox only
  absorbs resends and restarts; the data lives in the tables.
- **Alarm every 30 s** (`run_storage_metrics_loop`): a WARN if there are 100 or more pending rows,
  if the oldest one is 120 s old or more, or if any pending row has an error.

### 5.5 Handlers

| Subject | Table | Rule |
| --- | --- | --- |
| `storage.turns` | `turns` | `ON CONFLICT (dedupe_key) DO NOTHING` (`persist_turn`) |
| `storage.cognition.threads` | `cognition_threads` | Key `communication_id`; replaced only by an equal or later turn |
| `storage.cognition.contexts`, `.reasons`, `.cooccurrences`, `.scope_instances`, `.memories`, `.episodes` | `cognition_*` | Key: the id of the entity; same turn rule |
| `storage.cognition.scopes` | `cognition_scopes` | Key `scope_id`; same turn rule |
| `storage.usage` | `llm_usage` | `ON CONFLICT DO NOTHING` on `record_id` and `provider_response_id` (`persist_usage`) |

### 5.6 Reconnection and degradation

- **The connection to PostgreSQL drops:** SY.storage exits with code 1 and systemd starts it again
  with new connections (`connect_or_exit_on_close`; the unit has `Restart=always` and
  `RestartSec=5`, in `packaging/build-deb.sh`). The client does not
  reconnect by itself.
- **There is a URL but the database does not answer at boot:** it exits with code 1 and systemd
  retries (`exit_without_database`).
- **There is no secret in the vault:** it boots without a database and stays that way. It does not
  ack batches, and the reads over the mesh answer that it has no database. The streams keep records
  within their window.
- **A new secret arrives:** a `VAULT_SECRET_CHANGED` for `postgres` from the `SY.vault` of its own
  hive, which matches its interest, makes it `exit(0)`, and systemd restarts it with the new value
  (`handle_vault_secret_changed`). From any other origin, it is ignored.

---

## 6. Tables

### 6.1 `turns`: the message log

One row per copied message. Columns: `dedupe_key`, `ts`, `stored_at`, `from_ilk`, `to_ilk`, `ich`,
`msg_type`, `content`, `tags`, `thread_id`, `trace_id`, `message`, and `ctx` and `seq`, which
nothing writes (below).

- **The key** is `dedupe_key`, the same as the inbox's (`idx_turns_dedupe_key`, unique). A response
  shares the `trace_id` of its request and still has its own row.
- **Where each column comes from** (`parse_turn`):
  - `trace_id`, from `routing.trace_id`, and it is required;
  - `from_ilk`, from `meta.src_ilk` or, if it is missing, from `routing.src`;
  - `to_ilk`, from `meta.dst_ilk`; `ich`, from `meta.ich` (`unknown` if it is missing);
  - `msg_type`, from `meta.type`; `thread_id`, from `meta.thread_id`;
  - `content`, the payload; `tags`, those of `payload.tags` or `meta.tags`, sorted and without
    repeats;
  - `message`, the whole message, as the router copied it.
- **`ctx` and `seq`** are not written since 0.1.83; nothing reads them, and the protocol has no
  such fields (`Meta` in `crates/fluxbee_sdk/src/protocol.rs`). They stay one release as nullable
  columns so that a rollback to 0.1.82 still starts: its start creates an index on `(ctx, seq)`,
  and its INSERT writes them. While a table still has their `NOT NULL`, `ensure_schema` drops it;
  it reads `information_schema.columns` first, so once that is done a start takes no lock on
  `turns`. The next release drops both columns and that index.
- **`message`** is required (`NOT NULL`), like `content`.
- **`ts`** is when the router copied the message (the time of the stream record). **`stored_at`**
  is when SY.storage wrote it: it is the order that a live reader follows, because a batch can
  arrive late.
- **Indexes:** `(ts DESC, dedupe_key DESC)`, `(stored_at, dedupe_key)`, `(thread_id)`. A database
  that ran 0.1.82 or earlier also keeps `idx_turns_ctx_seq` on `(ctx, seq)` until the next release
  drops it.
- **Who reads it:** the Messages page of Archi (`src/bin/sy_architect/messages_db.rs`).
- Nothing purges `turns`.

### 6.2 `cognition_*`

`cognition_threads`, `cognition_contexts`, `cognition_reasons`, `cognition_cooccurrences`,
`cognition_scopes`, `cognition_scope_instances`, `cognition_memories`, `cognition_episodes`
(`COGNITION_SCHEMA_SQL`).

- **Per communication:** every row, except a scope's, carries `thread_id` and `agent_ilk` (the
  communication) and `source_turn`, the turn in which it was written. The thread row is keyed by the
  communication (`communication_id`). A scope is reached through its instances.
- **Each row** stores columns for fast reads and the whole envelope in `payload` (JSONB), plus
  `entity_version`, `writer`, `source_ts` and `updated_at`.
- **The guard:** an upsert replaces a row only if its turn is equal or later
  (`WHERE ... source_turn <= EXCLUDED.source_turn`). A late retry, or a cognition that booted
  empty, never moves it back.
- A `close` is stored with status `closed` (`normalized_status`). An envelope with a
  `schema_version` other than 2 fails and stays in the inbox (`parse_cognition_envelope`).
- **Indexes:** `(thread_id, agent_ilk)` on each per-communication table, and `scope_id`,
  `context_id` and `reason_id` where they apply.

What each entity is: `docs/reference/cognition.md` §4.

### 6.3 `llm_usage`: LLM usage (0.1.79)

One row per call to a model (`USAGE_SCHEMA_SQL`). The AI SDK produces them
(`crates/fluxbee_ai_sdk/src/usage.rs`, one for each 2xx response, and `abandoned` if the call was
cut off before), and the nodes send them through `storage.usage`.

**Columns:**

| Column | What it is |
| --- | --- |
| `record_id` | Key. A UUID that the producer sets |
| `provider_response_id` | The provider's response id; unique where present |
| `ts` | When the response arrived, by the producer's clock |
| `stored_at` | When SY.storage wrote it |
| `hive`, `node_name`, `node_ilk` | Who spent |
| `tenant_id` | The tenant that SY.storage charged |
| `tenant_source` | Why: `node`, `agent`, `case`, `root` or `fallback_root` |
| `claimed_tenant_id` | The tenant that the record claimed |
| `verified` | `false` when identity never answered and the tenant that the record claimed was kept |
| `counterpart_ilk` | The counterpart (for SY.cognition, the agent's ILK) |
| `provider`, `model`, `served_model`, `service_tier` | The model requested and the one that answered |
| `kind`, `outcome` | `generate` or `function_turn`; `ok`, `truncated`, `refused`, `unreadable` or `abandoned` |
| `uncached_input`, `cache_read`, `cache_write`, `output`, `reasoning` | The token counts |
| `raw_usage` | The provider's usage object, up to 2 KB |
| `trace_id`, `thread_id`, `work` | Which work it belongs to |

- **Indexes:** `provider_response_id` unique where not null; `(tenant_id, ts)`, `(node_name, ts)`,
  `(ts)`, `(stored_at, record_id)` and `(tenant_id, stored_at, record_id)`.
- **A response goes in only once:** `ON CONFLICT DO NOTHING` on both keys.
- **No foreign key** to the tenants: purging a tenant does not touch it. Nothing is purged from
  this table.
- **`stored_at`** is the `now()` of an autocommit insert on the shared connection, so it follows
  commit order. A `since` cursor never skips a row that commits later.

**Before it enters the inbox** (`check_usage_record`), a record is rejected, and counted, if:

- it comes from a `SY.*` that may not record usage (`foreign_sy`). The ones that may are
  SY.cognition, SY.frontdesk.gov, SY.architect and SY.admin;
- it cannot be read, or it has the time or a count out of range (`malformed`);
- it lacks a required field, or a field is too long (`defective`, `UsageRecord::defect`);
- its `node_name` is not its producer (`not_producer`);
- its `tenant_source` does not fit that producer (`wrong_source`), or it is not the root tenant
  when it must be (`not_root`).

**When it processes the record, SY.storage decides the tenant** (`decide_usage_tenant`):

| Producer | `tenant_source` | Tenant charged |
| --- | --- | --- |
| A node outside `SY.*` | `node` | Its own, according to identity, looked up by its name (`handler_node`) |
| SY.cognition | `agent` | The one of the agent's ILK (`counterpart_ilk`), according to identity |
| SY.frontdesk.gov | `case` | The case's, as it comes |
| SY.architect, SY.admin | `root` | The root tenant |
| Any | `fallback_root` | The root tenant |

- Identity is read from the motherbee SHM for each record, never from a cache
  (`identity_tenant`).
- If identity does not answer, or does not have that ILK yet, the row fails and the inbox retries
  it. On the last attempt it is stored with the tenant that the record claimed and
  `verified = false`.
- A record charged to a tenant other than the one it claimed counts as `mismatched`.
- Every 30 s, a WARN line gives the rejected records by reason, the `mismatched` ones and the
  unverified ones (`report_usage_counts`). `ops.py health` shows them as `usagerej`.

---

## 7. Reads over the mesh

### 7.1 `COGNITION_STATE_GET`

The state of a communication, so that SY.cognition can rebuild it (`answer_cognition_state`).

- It only answers a `SY.cognition@*` sender, by the name that the router stamps.
- Request: `{thread_id, agent_ilk, after}`. Response: the envelopes, in the form cognition wrote
  them, ordered by entity and then by id (`COGNITION_STATE_ORDER`).
- Pages of up to 64 KB (`COGNITION_STATE_PAGE_BYTES`); `next` points to the last
  `(entity, id)` of the page.
- It runs in a task of its own, so the main loop keeps taking batches.

When cognition asks for it: `docs/reference/cognition.md` §6.

### 7.2 `STORAGE_METRICS_GET`

The inbox metrics (`answer_storage_metrics`, `StorageInboxMetrics` in
`crates/fluxbee_sdk/src/storage.rs`): `pending`, `pending_with_error`, `oldest_pending_age_s`,
`processed_total` and `max_attempts`.

- Any node can ask: they are counts.
- Each answer scans the inbox, so only one runs at a time. If another one is in progress, it
  answers `ok: false` and the asker retries.
- Without a database, it answers `ok: false` with the error.

Who uses it:

- SY.admin: `GET /config/storage/metrics` (`handle_storage_metrics_http`, timeout 8 s). If there is
  no answer, 503 `STORAGE_METRICS_UNAVAILABLE`.
- SY.orchestrator, when it boots on the motherbee (`wait_for_storage_db_ready`): it waits for
  `sy-storage` to be active and for this query to answer `ok`. It tries every 250 ms, for up to
  30 s. Meanwhile, the admin commands get `ORCHESTRATOR_BOOTING`.

### 7.3 `USAGE_QUERY`

The `llm_usage` records (`answer_usage_query`, `plan_usage_query`, `usage_query_page`).

**Who and how:**

- Only `SY.admin@motherbee`, according to the router; any other sender gets `FORBIDDEN`.
- One query at a time, on its own connection, with a 15 s limit per statement. If another one is
  in progress, or the query fails, it answers `USAGE_UNAVAILABLE`.

**The query** (`UsageQuery` in `crates/fluxbee_sdk/src/usage.rs`):

- exactly one of `tenant_id` and `all_tenants`;
- either `since`: the records stored after a cursor, by `(stored_at, record_id)`, to sync little
  by little. It also accepts a bare RFC 3339 time, to start;
- or a period `[from, to)` by `ts`, of up to 92 days (`USAGE_QUERY_MAX_DAYS`): grouped by
  `group_by` (`day`, `model`, `node`, `tenant`), with the calls and the five sums, or record by
  record, with `after`;
- `limit` from 1 to 1000, 500 if not given. Each page stays under 62 KB, with at least one
  item.

**The answer:**

- Record cursor: `<RFC 3339 time with microseconds>|<record_id>`. Group cursor: the last key. A
  `day` is `YYYY-MM-DD` in UTC.
- `next` is missing when a period has no more pages, or in a `since` when nothing new arrived.
- Errors: `INVALID_REQUEST` (the query or a cursor), `FORBIDDEN` or `USAGE_UNAVAILABLE`.

**Who asks** (`src/bin/sy_admin.rs`):

- The operator: `GET /hives/motherbee/usage?tenant_id=&from=&to=&group_by=&since=&after=&limit=`
  (`llm_usage_query_from_http`). Without `tenant_id`, it asks for all tenants. Any hive other than
  `motherbee` answers 400.
- SY.admin checks the query first (`check_llm_usage_query`, 400 `INVALID_REQUEST`), and then
  translates the `error_code` of SY.storage: `INVALID_REQUEST` is 400, the rest 503.
- Over the mesh, the `get_llm_usage` action: all tenants at once only for Archi
  (`authorize_llm_usage_scope`); any other sender names a tenant.
- Cloud: the `get_usage` op of IO.cloud, always with the tenant it claims, never all of them and
  never the root (`enforce_cloud_relay_content`). Contract: `docs/io-cloud-api.md`.

---

## 8. SY.cognition and the stream

SY.cognition runs on the motherbee and on the workers, and does not read PostgreSQL. In short:

- **Input:** `STREAM_BATCH` from the streams of its own hive (`cognition.turns`). It acks on
  receipt into a bounded queue, and skips what it already took from that stream and epoch.
- **Communication:** `(thread_id, agent ILK)`. The producer of the record is the agent's node, and
  cognition maps it to its ILK through the identity SHM.
- **Outputs:** `STREAM_APPEND` under `storage.cognition.batch`, packages of up to 64 KB, each
  envelope with its communication and its turn number. And one usage record per call to a model
  (`storage.usage`).
- **Rebuild:** through `COGNITION_STATE_GET` (§7.1).

All the details: `docs/reference/cognition.md`.

---

## 9. Timing and guarantees

### 9.1 Timing

| Step | Blocks the router |
| --- | --- |
| Router → stream queue | No |
| Queue → disk (the stream's own thread; fsync every 250 ms) | No |
| Stream → sink (`STREAM_BATCH`, one batch in flight per sink; resend after 5 s without an ack) | No |
| SY.storage → `storage_inbox` (one transaction) → `STREAM_ACK` | No (another process) |
| `storage_inbox` → `turns` / `cognition_*` / `llm_usage` (a separate task, after the ack) | No (another process) |

The router never waits for the log: it only leaves the copy in a bounded queue. A record waits for
its sink for the window at most (120 s).

### 9.2 Degradation

| Failure | What happens |
| --- | --- |
| The WAN goes down, or a sink | The stream keeps the records and retries (backoff 0.5–8 s) within the window; what goes past it is lost and counted |
| PostgreSQL goes down | SY.storage exits and systemd restarts it; without a database it does not ack and the stream resends |
| Stream disk not accessible | The stream keeps records in memory only |
| 32 MB cap, or a full queue (16 MB waiting for the disk) | New records are dropped and counted; routing goes on |
| Identity does not answer when a usage is charged | The row is retried; on attempt 10 it is stored unverified |
| SY.cognition goes down | It loses its queue (what was acked and not processed); when it comes back, it reloads its communications from SY.storage |
| `jsr-memory` cannot be read | The message is delivered without `memory_package` |

### 9.3 Idempotency

```sql
-- Inbox: the key is dedupe_key (a hash of subject and payload); a resent batch lands on the
-- same rows, and a row already processed is not processed again.
INSERT INTO storage_inbox ... ON CONFLICT (dedupe_key) DO UPDATE SET attempts = ... RETURNING ...;

-- Turns: the same dedupe_key. (ctx, seq) was the key until 0.1.74; a response reuses the
-- trace_id of its request, and it got lost. Since 0.1.83 they are not written.
INSERT INTO turns ... ON CONFLICT (dedupe_key) DO NOTHING;

-- Cognition: each row carries its communication and the turn in which it was written
-- (source_turn); it is replaced only by an equal or later turn.
INSERT INTO cognition_contexts ... ON CONFLICT (context_id) DO UPDATE SET ...
    WHERE cognition_contexts.source_turn <= EXCLUDED.source_turn;

-- LLM usage: a response goes in only once (record_id, or provider_response_id where present).
INSERT INTO llm_usage ... ON CONFLICT DO NOTHING;
```

---

## 10. Several routers in one hive

One router per hive is the supported topology ([architecture.md](architecture.md) §4.2), so a hive
has one stream. The code still has a mode with several routers in a hive, which is not supported
([routing-and-policy.md](routing-and-policy.md) §1 and §10); in it the streams work as below.

| Scenario | Supported | Notes |
| --- | --- | --- |
| One router per hive | Yes | One stream per hive |
| N routers in one hive (gateway + switches) | No | One stream per router; each message is copied once, where it comes in |

```text
RT.gateway.stream@worker1 ───┐
RT.switch1.stream@worker1 ───┼──► SY.storage@motherbee   (one cursor per stream)
RT.gateway.stream@motherbee ─┘
```

Each stream stores on its own disk and sends its batches through normal routing (a switch's
batches, through IRP to the gateway), and the gateway announces all the streams of the hive in its
LSA. SY.storage receives from all of them; since each row has its key, the arrival order does not
matter.

---

## 11. Subjects and sinks

| Subject | Produced by | Sink |
| --- | --- | --- |
| `storage.turns` | The router where the message comes in (all roles) | `SY.storage@motherbee` |
| `cognition.turns` | The router where the `AI.*` agent is (motherbee, worker) | `SY.cognition@<same hive>` |
| `storage.cognition.batch` | `SY.cognition@<hive>`, with `STREAM_APPEND` | `SY.storage@motherbee` (expanded into `storage.cognition.<entity>`) |
| `storage.usage` | Each node that calls a model, with `STREAM_APPEND` | `SY.storage@motherbee` |

**Cursors:** each sink has its own (the last acked seq) in `state.json`; a slow or absent sink
falls behind on its own, without holding back the others. A resent batch can arrive twice:
SY.storage absorbs it by `dedupe_key`, and SY.cognition by stream, epoch and seq.

---

## 12. Constants

```rust
// Router stream (src/router/stream.rs)
pub const STREAM_MAX_AGE_SECS: u64 = 120;                  // window: older is skipped and counted
pub const STREAM_MAX_BYTES: u64 = 32 * 1024 * 1024;        // on-disk cap per router
pub const STREAM_NODE_APPENDS_SHARE_PERCENT: u64 = 25;     // share of the cap for STREAM_APPEND
pub const STREAM_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
pub const STREAM_BATCH_MAX_RECORDS: usize = 256;
pub const STREAM_BATCH_MAX_BYTES: usize = 96 * 1024;
pub const STREAM_ACK_TIMEOUT_SECS: u64 = 5;
pub const STREAM_RETRY_MIN_MS: u64 = 500;
pub const STREAM_RETRY_MAX_MS: u64 = 8_000;
pub const STREAM_FSYNC_INTERVAL_MS: u64 = 250;
pub const STREAM_TURN_QUEUE: usize = 4_096;                // turn copies waiting for the disk
pub const STREAM_NODE_APPEND_QUEUE: usize = 1_024;         // STREAM_APPEND from nodes
pub const STREAM_QUEUE_MAX_BYTES: u64 = 16 * 1024 * 1024;  // in memory, waiting for the disk
pub const STREAM_USAGE_RESERVE_BYTES: u64 = 1024 * 1024;   // storage.usage reserve on disk
pub const STREAM_USAGE_QUEUE_RESERVE: usize = 128;         // and in the queue
pub const STREAM_ALARM_INTERVAL_SECS: u64 = 30;            // one alarm line with what was lost

// SY.storage (src/bin/sy_storage.rs)
const STREAM_INGEST_QUEUE: usize = 64;                     // batches waiting for ingest
const STREAM_PROCESS_QUEUE: usize = 64;                    // acked batches waiting for processing
const INBOX_MAINTENANCE_INTERVAL_SECS: u64 = 60;           // retry of failed rows + purge
const INBOX_RETRY_MIN_AGE_SECS: i32 = 60;
const INBOX_MAX_ATTEMPTS: i32 = 10;
const INBOX_REPLAY_BATCH_SIZE: i64 = 200;
const INBOX_REPLAY_MAX_ROUNDS: u32 = 20;
const STORAGE_INBOX_RETENTION_HOURS: i32 = 24;             // processed rows
const INBOX_PURGE_BATCH: i64 = 10_000;
const COGNITION_STATE_PAGE_BYTES: usize = 64 * 1024;
const USAGE_QUERY_PAGE_BYTES: usize = 62 * 1024;
const STORAGE_METRICS_LOG_INTERVAL_SECS: u64 = 30;
const STORAGE_METRICS_WARN_PENDING: i64 = 100;
const STORAGE_METRICS_WARN_AGE_SECS: i64 = 120;

// Usage queries (crates/fluxbee_sdk/src/usage.rs)
pub const USAGE_QUERY_MAX_DAYS: i64 = 92;
pub const USAGE_QUERY_DEFAULT_LIMIT: u32 = 500;
pub const USAGE_QUERY_MAX_LIMIT: u32 = 1000;
pub const USAGE_FIELD_MAX_CHARS: usize = 256;
pub const USAGE_RAW_MAX_BYTES: usize = 2 * 1024;
```

---

## 13. References

| Topic | Document |
| --- | --- |
| Router stream (design, A-70) | `router-stream-v1.md` |
| Cognition | `reference/cognition.md` |
| Identity and tenants | `reference/identity.md` |
| Fluxbee Cloud contract | `io-cloud-api.md` |
| Routing | `04-routing.md` |
| Shared Memory | `03-shm.md` |
| WAN connectivity | `05-conectividad.md` |
