# SY.storage

SY.storage is the mesh's durable store. It receives the router streams of every hive and writes
them into the PostgreSQL database `fluxbee_storage`: the message log (`turns`), cognition's
durable state (`cognition_*`) and LLM usage records (`llm_usage`). It also answers the queries
other system nodes make about that data.

Source: `src/bin/sy_storage.rs`. Data model and table details: [docs/reference/storage.md](../reference/storage.md).

| | |
| --- | --- |
| Binary | `/usr/bin/sy-storage` (`packaging/build-deb.sh`, `RUST_BINS` entry `sy-storage:sy_storage`) |
| Node name | `SY.storage@motherbee` (`FLUXBEE_NODE_NAME` or `SY_STORAGE_NODE_NAME` override the base name) |
| Role | motherbee only |
| systemd unit | `sy-storage.service`, `ExecCondition` on `role: motherbee` (`unit_role_gate`) |
| Listens on | the local router socket only; no TCP port |
| State | PostgreSQL database `fluxbee_storage` |

## Where it runs and how it starts

- On a role other than motherbee the binary logs a warning and exits with status 0; a motherbee
  role with a `hive_id` other than `motherbee` is an error (`main`).
- It connects to the router first, then resolves its database through SY.vault: resource type
  `postgres` in the root tenant, with `resolve_resource_awaiting_vault` (5 s per attempt, up to
  `VAULT_BOOT_WAIT` = 60 s). The secret is looked up by resource type, not by key name;
  `fluxbee-firstboot` stores it as `storage_postgres_url` with value `{"postgres_url": ...}`.
  The URL carries no database name: SY.storage adds `fluxbee_storage` and creates that database
  if it is missing (`STORAGE_DB_NAME`, `ensure_database_exists`).
- Without a usable secret it starts without a database (`sy.storage started without active DB
  backend`): stream batches are not acknowledged, so the streams keep them and resend.
- It creates its missing tables and indexes at every start (`ensure_schema`): `turns`,
  `storage_inbox`, the `cognition_*` tables (threads, contexts, reasons, cooccurrences, scopes,
  scope_instances, memories, episodes) and `llm_usage`, each statement waiting at most 5 s for its
  lock. It runs no migrations: an existing table is left as it is, except that `turns.ctx` and
  `seq`, no longer written, lose their `NOT NULL` ([storage](../reference/storage.md) §6.1).
- The motherbee's orchestrator waits for it at boot: the unit active within 30 s, then a
  successful `STORAGE_METRICS_GET` over the mesh within 30 s (`finish_bootstrap`,
  `wait_for_storage_db_ready` in `src/bin/sy_orchestrator.rs`). Until then every admin action
  answers `ORCHESTRATOR_BOOTING`, and if the wait fails the orchestrator exits and is restarted.

## Messages

| Message | Who may send | What it does |
| --- | --- | --- |
| `STREAM_BATCH` → `STREAM_ACK {up_to_seq}` | a router stream (`RT.<router>.stream@<hive>`, `is_stream_node_name`) | writes the batch into `storage_inbox` in one transaction, then acknowledges it (`ingest_stream_batch`); other senders are dropped with a warning |
| `COGNITION_STATE_GET` → `COGNITION_STATE_RESPONSE` | `SY.cognition@*` | a page of one communication's durable state |
| `STORAGE_METRICS_GET` → `STORAGE_METRICS_RESPONSE` | any node | inbox metrics: pending, pending with error, oldest pending age, processed, max attempts |
| `USAGE_QUERY` → `USAGE_QUERY_RESPONSE` | `SY.admin@motherbee` only (`answer_usage_query`) | LLM usage records |
| `CONFIG_GET`, `CONFIG_SET`, `PING`, `STATUS`, `NODE_STATUS_GET` | `CONFIG_*` only from `SY.admin@motherbee` (router system policy) | control; `CONFIG_SET` refuses `config.database.postgres_url` (Model D') |
| `VAULT_SECRET_CHANGED` | `SY.vault@motherbee` | for a `postgres` change: exit with status 0, systemd restarts it with the new value |

Checks on stream records (`stream_rows`): a record's producer must be on the stream's hive;
`storage.cognition.*` must come from that hive's `SY.cognition`; a usage record must pass
`check_usage_record`, and one whose tenant cannot be verified is stored on its last attempt
with the claimed tenant, marked unverified.

The ingest queue holds 64 batches (`STREAM_INGEST_QUEUE`); a batch that does not fit is not
acknowledged and its stream sends it again.

## The inbox

Every record goes through `storage_inbox` first (dedupe and retry). Every 60 s
(`INBOX_MAINTENANCE_INTERVAL_SECS`) rows at least 60 s old that failed are retried, up to 10
attempts (`INBOX_MAX_ATTEMPTS`); processed rows are purged after 24 h
(`STORAGE_INBOX_RETENTION_HOURS`), at most 10 000 per pass.

## Failure behavior

- A usable URL whose database cannot be created or reached, or whose schema cannot be created:
  the process exits with status 1 and systemd restarts it.
- One of its PostgreSQL connections closes later: it exits and is restarted with new
  connections (`connect_or_exit_on_close`).
- Not reachable: the routers keep each record for 120 s (32 MiB) in their streams; past that it
  is counted as lost (see [rt-gateway](rt-gateway.md)).

## Health

- `systemctl status sy-storage`; `journalctl -u sy-storage`: `sy.storage started`, and a warning
  every 30 s while the inbox lags (`storage inbox lag is above threshold`).
- `curl -s 127.0.0.1:8080/config/storage/metrics` on the motherbee returns the inbox metrics, or
  `STORAGE_METRICS_UNAVAILABLE`. (`GET /config/storage` is the hive's storage path, not this
  node.)
- `GET /hives/motherbee/usage` returns usage records through `USAGE_QUERY`.
- `lab/ops.py health` counts the line `sy.storage usage records refused, mismatched or
  unverified in the last 30 s` as `usagerej`.
- `NODE_STATUS_GET` always answers `HEALTHY`; it does not reflect the database.

## Links

- [docs/reference/storage.md](../reference/storage.md): tables and data model.
- [rt-gateway](rt-gateway.md): the router stream that feeds it.
- [SY.cognition](sy-cognition.md), [SY.vault](sy-vault.md).
- [Operations](../reference/operations.md): first boot loads the postgres secret.
