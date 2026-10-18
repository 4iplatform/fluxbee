# SY.identity

SY.identity is the mesh's register of who exists. It keeps:

- **ILKs**, the identity keys of people (`human`), AI and other workload nodes (`agent`) and
  system nodes (`system`);
- **tenants**, the cost centers every ILK is charged to;
- **ICHs**, identity channels: an address on a channel type (a phone number, a Slack user, a
  Cloud account) bound to one ILK;
- **aliases**, left behind when two ILKs are merged.

The primary runs on the motherbee and stores everything in PostgreSQL. Each worker runs a
read-only replica that mirrors it over TCP port 9100. Every instance writes its hive's identity
shared-memory region, which the router, SY.vault and the nodes read.

Source: `src/bin/sy_identity.rs`. SHM layout: `IdentityRegionWriter` in `src/shm/mod.rs`. Sync
authentication: `src/mesh_hmac.rs`. Shared helpers and constants:
`crates/fluxbee_sdk/src/identity.rs`.

| | |
| --- | --- |
| Binary | `/usr/bin/sy-identity` (`packaging/build-deb.sh`, `RUST_BINS` entry `sy-identity:sy_identity`) |
| Node name | `SY.identity@<hive_id>` |
| Role | motherbee (primary), worker (replica) |
| systemd unit | `sy-identity.service`, `ExecCondition` on `role: motherbee\|worker` (`unit_role_gate`) |
| Listens on | the router socket; the primary also on TCP `0.0.0.0:9100` |
| Writes | POSIX shared memory `/jsr-identity-<hive_id>` |
| State | PostgreSQL database `fluxbee_identity` (primary only); `/var/lib/fluxbee/identity/keys/` |

## Where it runs

`main` in `src/bin/sy_identity.rs` decides from `/etc/fluxbee/hive.yaml`:

- `role: motherbee` is the primary. Its `hive_id` must be `motherbee`.
- `role: worker` is a replica. Its `hive_id` must not be `motherbee`.
- Any other role ends the process with `sy.identity supports only role=motherbee|worker`. The
  packaged unit's `ExecCondition` skips the start on other roles, and the ingress and egress
  `system_nodes` lists do not include SY.identity. Those hives have no identity SHM.

## How it starts

1. Exits on non-Linux targets; log level from `JSR_LOG_LEVEL` (default `info`).
2. Reads `hive.yaml`: `hive_id`, `role`, `government.identity_frontdesk`, `identity.*`
   (`merge_alias_ttl_secs`, `max_ilks`, `max_tenants`, `max_vocabulary`, `max_ilk_aliases`,
   `sync.port`, `sync.upstream`, `sync.auth`) and `system_nodes.<role>.nodes` (`HiveFile`).
3. Connects to the router first, so it is registered before SY.vault sends its startup
   announcements (see [SY.vault](sy-vault.md)).
4. **Primary only: the database.** `resolve_database_url` asks SY.vault for the resource
   `postgres` in the root tenant with `resolve_resource_awaiting_vault` (5 s per attempt, up to
   `VAULT_BOOT_WAIT` = 60 s, retrying only when the vault does not answer). The value is a URL
   string or `{"postgres_url": ...}` without a database name; SY.identity creates the database
   `fluxbee_identity` if it is missing (`IDENTITY_DB_NAME`, `ensure_database_exists`) and applies
   its schema under a PostgreSQL advisory lock (`ensure_primary_schema`, migrations v1 to v4 in
   `identity_schema_migrations`). Without the secret, or if the database cannot be set up, it
   starts without a database (`sy.identity started without active DB backend`): reads work from
   memory and writes answer `DB_NOT_READY`.
5. Opens or creates the SHM region (next section). If it cannot, it logs
   `identity shm unavailable` and goes on.
6. **Replica only:** loads its HMAC key (below) and pulls a full copy from the primary
   (`fetch_full_sync_from_primary`). If that fails it logs `identity full sync bootstrap FAILED;
   replica is DEGRADED` and starts with what it has.
7. Makes sure the root tenant `tnt:00000000-0000-0000-0000-000000000001` (name `fluxbee`)
   exists and is active (`ensure_default_root_tenant`), and seeds one `system` ILK for every
   `SY.*` entry of `system_nodes.<role>.nodes` (`ensure_system_ilks_from_hive`). These ILKs are
   deterministic: `deterministic_system_ilk_id` hashes `fluxbee:identity:system-ilk:v1:` plus the
   node name, so SY.vault and other nodes compute the same id without asking.
8. Runs its loops: the router message loop, the sync listener or the replica's sync tasks, the
   merge-alias GC every 30 s (`ALIAS_GC_INTERVAL_SECS`), and the SHM heartbeat every 5 s.

## Data model

- **ILK types:** `human`, `agent`, `system`. New ILKs are `temporary` until registered, then
  `complete` (`SHM_REG_STATUS_*`).
- **Tenants** are `pending`, `active` or `suspended`; one may sponsor others. The root tenant is
  protected: it cannot be deleted (`TENANT_ROOT_PROTECTED`) and holds the system.
- **ICH:** `(ich_id, channel_type, address, scope, owner, enabled)`. The lookup key is
  `(channel_type, address, scope)`, lowercased (`canonical_ich_key`). The scope is normally the
  tenant; only `IO.cloud@<hive>` may use the installation-wide scope, and only for the `cloud`
  channel type (`resolve_channel_scope`).
- **Aliases:** when a registration merges a temporary ILK into an existing one, an alias points
  from the old id to the kept one for `merge_alias_ttl_secs` (default 3600 s). When it expires,
  the GC marks the temporary ILK deleted (`gc_expired_aliases`).

### Provision, then register

Identity is created in two steps:

1. An IO node that sees a new address sends `ILK_PROVISION {ich_id, channel_type, address,
   tenant_id?, ilk_type?, scope?}` (`provision_temporary_ilk`). SY.identity returns the ILK that
   already holds that channel, or creates a `temporary` one (charged to the root tenant when no
   tenant is given) with the channel disabled.
2. The router sends any message without an ILK, or with a `temporary` one, to the identity
   frontdesk (`frontdesk_route` in `policy/system.rego`, `apply_identity_pre_resolve` in
   `src/router/mod.rs`). The frontdesk, or the orchestrator for the nodes it launches, sends
   `ILK_REGISTER {ilk_id, ilk_type, tenant_id, identification}`, which makes the ILK `complete`
   (`register_ilk`). A registration whose email another ILK of the same tenant already holds
   merges into that ILK.

## The identity SHM region

- Name `/jsr-identity-<hive_id>` (`identity_shm_name`); each instance writes its own hive's
  region, with an `is_primary` flag in the header (`IdentityRegionWriter::open_or_create`).
- Writes are seqlock-guarded; the heartbeat is refreshed every 5 s.
- Contents: tenants, ILKs (with `handler_node`, the node an ILK belongs to), ICHs and their
  lookup mappings, and unexpired aliases (`sync_identity_shm_mappings`). Marked (deleted)
  tenants and ILKs are left out.
- Default limits: 8192 ILKs, 1024 tenants, 4096 vocabulary entries, aliases = the ILK limit
  (`DEFAULT_IDENTITY_SHM_MAX_*`, `identity_region_limits`); raise them with `identity.max_*`.
  A write over a limit fails and is logged, and the region keeps its previous content.
- Readers include the router (frontdesk pre-resolve and policy data), SY.vault (caller binding
  and tenant status, `src/bin/sy_vault.rs`), SY.timer and SY.wf-rules at startup (their own
  ILK, `go/fluxbee-go-sdk/identity_shm.go`) and IO nodes resolving a channel to an ILK
  (`fluxbee_sdk::resolve_ilk_from_hive_id`, called from `nodes/io/common/src/identity.rs`).

## Sync between hives (TCP 9100)

- Only the primary listens, on `0.0.0.0:<identity.sync.port>` (default 9100,
  `DEFAULT_IDENTITY_SYNC_PORT`). A replica connects to `identity.sync.upstream`, which `add_hive`
  writes into the worker's hive.yaml together with `auth: required` (`src/bin/sy_orchestrator.rs`).
- Framing: one JSON object per line over plain TCP, at most 16 MiB per line
  (`MAX_SYNC_LINE_BYTES`). There is no TLS and no HTTP.
- **Authentication** (`identity.sync.auth`): missing, empty or unknown means required
  (`auth_mode_required`); only `disabled`, `off` or `none` turn it off, with a warning. Each
  connection starts with a mutual challenge-response: `IDENTITY_AUTH_HELLO {hive, nonce}`,
  `IDENTITY_AUTH_CHALLENGE`, `IDENTITY_AUTH_RESPONSE`, `IDENTITY_AUTH_OK`
  (`server_auth_handshake`, `client_auth_handshake`). Each side proves it holds the hive's key by
  an HMAC-SHA256 of the other side's nonce (`src/mesh_hmac.rs`). Keys are
  `/var/lib/fluxbee/identity/keys/<hive>.key` (`KEYS_DIR`): the motherbee holds one per worker,
  each worker its own. `add_hive` creates the key and copies it over SSH
  (`ensure_local_identity_key`, `distribute_hive_identity_key_inner`). A replica with auth
  required and no key file does not start.
- **Primary to replica:** `IDENTITY_FULL_SYNC_REQUEST` returns the whole store in chunks of 256
  items per kind. `IDENTITY_DELTA_SUBSCRIBE` starts a stream of `IDENTITY_DELTA` frames
  (tenant, ILK and alias upserts and deletes), each acknowledged with `IDENTITY_DELTA_ACK`
  (2 s, 3 tries); the primary sends `IDENTITY_SYNC_HEARTBEAT` every 20 s and a replica that hears
  nothing for 60 s subscribes again from a full snapshot.
- **Replica to primary:** with `IDENTITY_DELTA_PUBLISH` a replica sends its own hive's ILKs
  (`node_name` ending in `@<its hive>`), as a full snapshot every 30 s
  (`IDENTITY_PUBLISH_RECONCILE_SECS`, `push_local_deltas_upstream`), and only once it holds a
  baseline from the primary. The primary accepts a publish only for the authenticated hive and
  only ILKs that hive owns (`delta_authorized_for_hive`), stores them and passes them on.
- Limits: 64 concurrent sync connections, 256 delta subscribers, 1024 queued frames per
  subscriber (a slow one is dropped and resyncs).

## Messages and who may send them

Requests are `SYSTEM` messages; answers are `<MSG>_RESPONSE` (`response_name`). The check is
`IdentityRuntime::is_authorized` on the router-stamped `routing.src_l2_name`. Prefixes
`SY.admin@`, `SY.architect@` and `SY.frontdesk.gov@` count only from the same hive
(`prefix_allowed_same_hive_scoped`); `IO.` and `SY.orchestrator@` count from any hive. A refused
request gets `UNAUTHORIZED_REGISTRAR`.

| Message | Allowed senders | Primary only |
| --- | --- | --- |
| `ILK_PROVISION` | `IO.*` | yes |
| `ILK_REGISTER` | `SY.frontdesk.gov@`, `SY.orchestrator@`, the configured `government.identity_frontdesk`, `SY.identity@<hive>` | yes |
| `ILK_ADD_CHANNEL` | `IO.*`, `SY.frontdesk.gov@`, the configured frontdesk | yes |
| `ILK_UPDATE` | `SY.orchestrator@`, `SY.identity@<hive>` | yes |
| `ILK_SET_DEFINITION` | `SY.admin@`, `SY.architect@`, `SY.identity@<hive>` | yes |
| `ILK_SET_TENANT` | `SY.admin@`, `SY.architect@` | yes |
| `ILK_DELETE` | `SY.admin@`, `SY.orchestrator@`, `SY.identity@<hive>` | yes |
| `ILK_RESTORE` | `SY.admin@` | yes |
| `ILK_PURGE` | `SY.admin@`, `SY.orchestrator@` | yes |
| `ICH_SET_ENABLED` | `IO.*` (only for an ICH it owns), `SY.admin@`, `SY.architect@`, `SY.frontdesk.gov@`, the configured frontdesk | yes |
| `TNT_CREATE`, `TNT_UPDATE`, `TNT_SET_SPONSOR` | `SY.admin@`, `SY.architect@` | yes |
| `TNT_APPROVE`, `TNT_DELETE`, `TNT_RESTORE`, `TNT_PURGE` | `SY.admin@` | yes |
| `ILK_LIST`, `ILK_GET`, `TNT_LIST`, `TNT_GET` | `SY.admin@` | no |
| `CONFIG_GET`, `CONFIG_SET` | `SY.admin@` (the router also allows only `SY.admin@motherbee`) | no |
| `IDENTITY_METRICS`, `PING`, `STATUS` | anyone | no |

- A replica answers every primary-only message with `NOT_PRIMARY`; it does not forward it
  (`action_requires_primary`). A primary without a database answers `DB_NOT_READY`.
- Because `SY.admin@` counts only from the same hive, a worker's replica refuses SY.admin's
  queries: the admin's identity routes are useful with `hive = motherbee` only.
- `VAULT_SECRET_CHANGED` is handled before the table (see Failure behavior). `NODE_STATUS_GET` is
  answered by the SDK default handler.

The callers in the code: IO nodes provision against `SY.identity@motherbee`
(`nodes/io/common/src/provision.rs`); the orchestrator registers the ILK of each node it
launches as type `agent` and deletes and purges it on teardown (`src/bin/sy_orchestrator.rs`);
SY.admin sends the operator's and Fluxbee Cloud's tenant and ILK operations
(`handle_identity_command` in `src/bin/sy_admin.rs`).

## Delete marks, purge removes

Deleting is reversible; purging is physical and final (`delete_ilk`, `restore_ilk`, `purge_ilk`,
`delete_tenant`, `restore_tenant`, `purge_tenant`):

- `ILK_DELETE` marks the ILK (`deleted_at_ms`) and removes it from lookups and from the SHM.
  Its rows stay, so its channel keys, node name and email stay reserved (`ILK_DELETED` for anyone
  who tries to take them). A second delete answers `already_deleted: true`. System ILKs are
  refused (`SYSTEM_ILK_PROTECTED`).
- `ILK_RESTORE` un-marks it (`ILK_NOT_DELETED` if it was not marked). A person comes back even
  while its tenant is marked; any other ILK waits for its tenant (`TENANT_DELETED`).
- `ILK_PURGE` removes a marked ILK and its aliases (`ILK_NOT_DELETED` if it is not marked).
- `TNT_DELETE` marks the tenant only and never marks an ILK. It is refused while the tenant
  sponsors active tenants (`TENANT_HAS_CHILDREN`) or still has node identities
  (`TENANT_HAS_NODES`: kill and purge them first); the answer reports `people_charged`. Before
  0.1.67 a tenant delete also marked its ILKs; schema migration v4 un-marks those
  (`tenant_marks_no_ilk`).
- `TNT_RESTORE` un-marks the tenant, unless its sponsor is marked.
- `TNT_PURGE` needs a marked tenant and refuses while it sponsors any tenant, has node identities
  or has people charged to it (`TENANT_HAS_PEOPLE`: move them with `ILK_SET_TENANT` first). It
  removes the tenant, its ILKs and the channels in its scope. SY.admin first checks that the
  tenant has no vault secrets (`purge_tenant_vault_block`: `TENANT_HAS_SECRETS`, HTTP 409).

Every write runs on a snapshot of the store, commits in one database transaction and only then
sends its deltas; any error rolls the store back (`run_lifecycle_op`).

## Config

- `hive.yaml` keys listed under "How it starts".
- `CONFIG_GET`/`CONFIG_SET` describe and version the node's control state; `CONFIG_SET` takes no
  secret-bearing field (Model D'). The state is persisted in
  `/var/lib/fluxbee/nodes/SY/SY.identity@<hive>/config.json`.

## State on disk

- PostgreSQL `fluxbee_identity` (primary): `identity_tenants`, `identity_ilks`,
  `identity_ilk_aliases`, `identity_ichs`, `identity_ilk_tenant_history`,
  `identity_vocabulary` (created, unused), `identity_schema_migrations`.
- A replica has no database: its state is in memory and in its SHM region, rebuilt from the
  primary at every start.
- `/var/lib/fluxbee/identity/keys/<hive>.key`: sync HMAC keys (0600, directory 0700).

## Failure behavior

- **Postgres secret changes:** on the primary, a `VAULT_SECRET_CHANGED` for `postgres` that
  comes from `SY.vault@<own hive>` makes the process exit with status 0; systemd restarts it with
  the new value (`handle_vault_secret_changed`). Any other origin is refused and logged. A replica
  ignores it.
- **Postgres down later:** a write fails with `DB_WRITE_FAILED` (or a `DUPLICATE_*` code for a
  constraint) and the in-memory store is rolled back; reads keep working.
- **Replica cut off:** the sync tasks retry every second; a replica without a baseline does not
  publish its ILKs (publishing an empty view would delete them on the primary).
- **HMAC failure:** the primary logs `identity sync request failed`; the replica logs the failure
  and reconnects.
- Startup errors (wrong role, bad `hive.yaml`, missing HMAC key on a replica, a non-`SY.*` entry
  in `system_nodes`) end the process; systemd restarts it every 5 s.

## Health

- `systemctl status sy-identity`; log lines: `identity sync listener ready`,
  `identity sync auth: required (per-hive HMAC)`, `seeded deterministic system ILKs from
  hive.yaml`, `identity full sync bootstrap applied`, `identity delta subscriber registered`.
- `PING` → `PONG {state, database{source, configured, ready}}` and `STATUS` →
  `{role: primary|replica, state, schema_version, config_version, database{..., last_error}}`.
  `state` is `configured`, `missing_secret`, `db_not_ready` or `replica_non_primary`
  (`identity_state_label`).
- `IDENTITY_METRICS` → counts of tenants, ILKs, ICHs, aliases and deleted ILKs.
- `NODE_STATUS_GET` always answers `HEALTHY`; it does not reflect the database.

## SY.admin HTTP routes

On the motherbee (`127.0.0.1:8080`), sent to `SY.identity@<hive in the URL>`:

| Route | Message |
| --- | --- |
| `GET /hives/{hive}/identity/ilks?include_deleted=&pending_signup=` | `ILK_LIST` |
| `GET /hives/{hive}/identity/ilks/{ilk_id}` | `ILK_GET` |
| `POST /hives/{hive}/identity/ilks/{ilk_id}/definition` | `ILK_SET_DEFINITION` |
| `DELETE /hives/{hive}/identity/ilks/{ilk_id}` | `ILK_DELETE` |
| `POST /hives/{hive}/identity/ilks/{ilk_id}/restore` | `ILK_RESTORE` |
| `POST /hives/{hive}/identity/ilks/{ilk_id}/purge` | `ILK_PURGE` |
| `PUT /hives/{hive}/identity/ilks/{ilk_id}/tenant` | `ILK_SET_TENANT` |
| `GET /hives/{hive}/identity/tenants?include_deleted=` | `TNT_LIST` |
| `POST /hives/{hive}/identity/tenants` | `TNT_CREATE` |
| `GET` / `PUT /hives/{hive}/identity/tenants/{tenant_id}` | `TNT_GET` / `TNT_UPDATE` |
| `POST /hives/{hive}/identity/tenants/{tenant_id}/sponsor` | `TNT_SET_SPONSOR` |
| `DELETE /hives/{hive}/identity/tenants/{tenant_id}` | `TNT_DELETE` |
| `POST /hives/{hive}/identity/tenants/{tenant_id}/restore` | `TNT_RESTORE` |
| `POST /hives/{hive}/identity/tenants/{tenant_id}/purge` | `TNT_PURGE` |
| `GET /hives/{hive}/identity/tenants/{tenant_id}/nodes` | admin query `list_tenant_nodes` |

`fluxbee-factory-reset` uses the delete and purge routes for every non-system ILK and every
tenant but the root (`purge_identity` in `packaging/fluxbee-factory-reset`).

## Links

- [SY.vault](sy-vault.md): reads the SHM to bind callers and check tenants.
- [rt-gateway](rt-gateway.md): the frontdesk pre-resolve.
- [Operations](../reference/operations.md): `add_hive`, which provisions the sync key.
- `docs/ilk-tenant-model-v1.md`: the tenant model the code cites.
