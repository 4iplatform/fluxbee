# SY.vault

SY.vault is the hive's secret store. It keeps every credential the mesh uses (the
PostgreSQL URL, AI provider keys, IO channel tokens, edge bearers and TLS material) encrypted on
the motherbee's disk, decides which node may read each one, and tells the mesh when one changes.
No node keeps a secret in its own config (Model D'): nodes ask SY.vault at run time.

Source: `src/bin/sy_vault.rs` (the node) and `crates/fluxbee_sdk/src/vault.rs` (wire types,
`VaultClient`, key rules).

| | |
| --- | --- |
| Binary | `/usr/bin/sy-vault` (`packaging/build-deb.sh`, `RUST_BINS` entry `sy-vault:sy_vault`) |
| Node name | `SY.vault@<hive_id>`, in practice `SY.vault@motherbee` |
| Role | motherbee only |
| systemd unit | `sy-vault.service` (`Restart=always`, `RestartSec=5`, from `gen_unit` in `packaging/build-deb.sh`) |
| Listens on | the local router socket only; no TCP port |
| State | `/var/lib/fluxbee/vault.db`, `/etc/fluxbee/vault.master.key`, lock `/var/run/fluxbee/sy-vault.lock` |

## Where it runs

- Only on the motherbee. `validate_system_nodes` in `src/bin/sy_orchestrator.rs` refuses a
  hive.yaml whose non-motherbee role lists `SY.vault` ("vault is motherbee-only") and refuses a
  motherbee list without it.
- Spokes never receive the binary: `core_component_names_from_section` ships a spoke only
  `rt-gateway`, the SY services of its role's `system_nodes` list and `sy-orchestrator`.
- Every client addresses the motherbee's vault. `VaultClient::for_primary` targets
  `SY.vault@<PRIMARY_HIVE_ID>`, and `PRIMARY_HIVE_ID` is `"motherbee"`
  (`crates/fluxbee_sdk/src/protocol.rs`).
- SY.admin sends vault actions to `SY.vault@<hive in the URL>` (`handle_vault_command` in
  `src/bin/sy_admin.rs`), so the HTTP routes below must name the motherbee:
  `/hives/motherbee/vault/...`.

## How it starts

`main` in `src/bin/sy_vault.rs`, in order:

1. Exits on non-Linux targets. Log level comes from `JSR_LOG_LEVEL` (default `info`).
2. Reads `hive_id` from `/etc/fluxbee` (`fluxbee_sdk::load_hive_id`).
3. Computes its own ILK with `deterministic_system_ilk_id("SY.vault@<hive>")`. It does not wait
   for the identity SHM.
4. Computes two ILK sets, also without the SHM:
   - the administrative set: the deterministic ILKs of `SY.admin@<hive>` and
     `SY.architect@<hive>`;
   - the system set (`compute_well_known_system_ilks`): its own ILK, the administrative set, and
     the deterministic ILK of every `SY.*` entry of `system_nodes.<role>.nodes` in
     `/etc/fluxbee/hive.yaml`.
5. Takes an exclusive `flock` on `/var/run/fluxbee/sy-vault.lock` (`acquire_lock`). A second
   instance fails with `lock unavailable`.
6. Loads `/etc/fluxbee/vault.master.key` (`load_or_create_master_key`). If the file does not
   exist it writes 32 random bytes with mode 0600. If it exists, it must be exactly 32 bytes and
   have no group/other permission bits, or the process exits.
7. Opens `/var/lib/fluxbee/vault.db` (`VaultStore::open`): SQLite in WAL mode, the DB and its
   `-wal`/`-shm` files set to 0600, tables created if missing.
8. Connects to the router (`RouterDispatcher::connect_with_retry`, retry every 1 s) with a
   persistent node UUID under `/var/lib/fluxbee/state/nodes`.
9. Sends the bootstrap broadcasts (next section), then serves requests.

### Why SY.vault starts last

The motherbee's `system_nodes.motherbee.nodes` list in hive.yaml is the start order, and
`validate_system_nodes` enforces two rules: the list starts with `SY.config.routes`, and every
vault consumer (`SY.admin`, `SY.architect`, `SY.storage`, `SY.identity`, `SY.cognition`,
`SY.frontdesk.gov`) comes before `SY.vault`.

The reason is the startup announcement. `emit_bootstrap_secret_broadcasts` sends one
`VAULT_SECRET_CHANGED` (`op=put`) per stored secret, marked with `meta.action = "bootstrap"`
(`VAULT_BOOTSTRAP_ACTION`). A router broadcast reaches only the nodes registered at that moment
(the comment in `validate_system_nodes`), so the consumers must already be on the router. A
consumer that boots without its secret reacts to that announcement.

Details that follow from the code:

- The announcement covers at most 200 secrets, the most recently written first: it calls
  `VaultStore::list` with a default filter, whose limit defaults to 200.
- Consumers do not depend only on the broadcast. `VaultClient::resolve_resource_awaiting_vault`
  retries transport failures (no route, timeout) every 750 ms for up to the caller's budget;
  `VAULT_BOOT_WAIT` is 60 s.
- On a cold boot SY.storage and SY.identity restart once more when the announcement lands: their
  handler exits on a matching event (comment in `main`, and `handle_vault_secret_changed` in
  `src/bin/sy_storage.rs`). This is expected.

## Data model

- **Key.** One namespace for the whole hive (`key` is the primary key of the `secrets` table).
  Charset `[a-z0-9:_-]`, first character `[a-z0-9]`, 1 to 256 bytes (`vault_key_is_valid` in the
  SDK, `validate_key` in the node).
- **Value.** Any JSON, at most 1 MiB once serialized (`MAX_SECRET_VALUE_BYTES`).
- **Metadata** (`VaultMetadata`): `tenant_id` (required, `tnt:<uuid>`), `resource_type`
  (required, e.g. `postgres`, `openai`, `slack`), `ilk` (the owner, optional), `assigned_ilks`,
  `description`, `tags`, and `created_by`/`created_at`/`updated_at` stamped by the vault. The
  legacy `owner_ilk` field is still read (`secret_owner_ilk`).
- **Three kinds of secret**, by metadata:
  - owned: `ilk` is set (a node, or a person whose secret it is);
  - assigned: `assigned_ilks` lists nodes that use it besides the owner;
  - general: no owner and no assigned node (`is_general_secret`). Every node of its tenant reads it.
- **Versions.** The current value and one previous value. Rotate and put move current to
  previous; rollback swaps them.
- **Encryption.** AES-256-GCM with the master key, a fresh random 12-byte nonce per write
  (`encrypt_value`).

### Every secret has a tenant

`validate_metadata` refuses a put without a `tnt:` tenant or without a `resource_type`. SY.admin
fills the tenant in when the caller leaves it out: `normalize_vault_put_payload` sets
`metadata.tenant_id` to the root tenant `tnt:00000000-0000-0000-0000-000000000001`
(`DEFAULT_ROOT_TENANT_ID`), and `default_vault_request_tenant` does the same for delete, rotate,
rollback and assign. Infrastructure secrets live in the root tenant; for example
`fluxbee-firstboot` stores `storage_postgres_url` there with `resource_type: postgres`.

## Who is the caller

Every request is tied to the node that sent it (`resolve_caller`, `check_caller_binding`;
FINDINGS A-65 in the code comment):

- The caller's name is the router-stamped `routing.src_l2_name`. The claimed ILK is
  `meta.src_ilk`.
- A request that claims an ILK without a stamped name is refused (`UNAUTHORIZED`).
- A `SY.*` name whose deterministic ILK equals the claimed ILK is accepted without the SHM.
- Any other claimed ILK must be in the identity SHM with `handler_node` equal to the stamped
  name. Another node's ILK is refused (`UNAUTHORIZED`); an ILK the SHM does not hold yet gets
  `IDENTITY_UNAVAILABLE` (retryable).
- A request with no ILK goes on unresolved: it can list, but reads no value.
- Refusals are answered and written to the audit log, never dropped.

The code comment states the limit of this check: across hives the stamped name is authenticated
by the WAN router; on one host it is the name the process declared when it connected.

## Who may read a secret

`authorize_read` decides `VAULT_GET` and `VAULT_GET_METADATA`, in this order:

1. The caller owns the secret (`metadata.ilk` equals the caller's ILK).
2. The caller is in `assigned_ilks`.
3. The secret is general and:
   - it is in the root tenant and the caller's ILK is in the system set computed from
     hive.yaml, or the identity SHM types the caller as `system`. A `SY.*` node the SHM does not
     hold yet (for example a worker's SY node while the motherbee's identity restarts) gets
     `IDENTITY_UNAVAILABLE`, not a refusal;
   - or the caller's tenant (from the SHM) is the secret's tenant.
4. Anything else is `UNAUTHORIZED`. There is no administrator bypass: SY.admin cannot read a
   secret owned by or assigned to another node, nor a non-root tenant's general secret.

For `VAULT_GET` the value goes out only while the secret's tenant is active
(`ensure_tenant_active`, `tenant_check`): the root tenant always counts as active; otherwise the
tenant's status is read from the identity SHM. `active` passes; any other status is
`UNAUTHORIZED` with the status in the message; a tenant absent from the SHM (deleted, or identity
not loaded) is `IDENTITY_UNAVAILABLE`. `VAULT_GET_METADATA` skips this check and never returns
the value. Each successful `VAULT_GET` bumps `access_count` and `last_accessed_at`.

Consumers use `VaultClient::resolve_resource(resource_type, my_tenant)`: an owned secret first,
then one assigned to it, then its own tenant's general secret, never another tenant's. Each step
lists with `limit: 1` (newest `updated_at` first) and then gets that key.

## Who may write

| Action | Allowed callers (`src/bin/sy_vault.rs`) |
| --- | --- |
| `VAULT_PUT`, `VAULT_ASSIGN` | SY.admin and SY.architect of this hive (`Caller::is_well_known_admin`) |
| `VAULT_DELETE`, `VAULT_ROTATE`, `VAULT_ROLLBACK` | SY.admin, SY.architect, or the secret's owner |
| `VAULT_LIST` | anyone; metadata only (comment "Model D': list is open" in `VaultStore::list`) |

Operators and Fluxbee Cloud write through SY.admin, which adds its own gates in
`src/bin/sy_admin.rs`: the IO.cloud relay may only put, list, assign and delete inside the
tenant it claims, never the root tenant, never a reserved key, and never set `metadata.ilk`,
`owner_ilk` or `owner_l2` (`enforce_cloud_relay_content`). Any hive's `SY.orchestrator` may call
`vault_list` and `vault_delete` to remove the secrets a node owns when the node is purged
(`ORCHESTRATOR_TEARDOWN_ACTIONS` in `authorize_cloud_relay`).

## Messages

All requests are `SYSTEM` messages sent to `SY.vault@motherbee`; each answer goes back to the
sender with the same `trace_id` (`send_system_response`). The node ignores every other action
except `NODE_STATUS_GET`.

| Request → response | Payload | Result |
| --- | --- | --- |
| `VAULT_PUT` → `VAULT_PUT_RESPONSE` | `{key, value, metadata}` | `{status, key, version, changed}`. Same value as stored: `changed: false`, no broadcast. Updating a key keeps its owner and assigned nodes unless the put names an owner or assigned nodes. |
| `VAULT_GET` → `VAULT_GET_RESPONSE` | `{key}` | `{status, key, value, metadata, version, last_accessed_at, access_count}` |
| `VAULT_GET_METADATA` → `VAULT_GET_METADATA_RESPONSE` | `{key}` | as `VAULT_GET` without `value` |
| `VAULT_LIST` → `VAULT_LIST_RESPONSE` | `{filter?}`: `prefix`, `tenant_id`, `resource_type`, `ilk` (`""` = general only), `assigned_ilk`, `tags`, `limit` (default 200, 1..1000) | `{status, count, secrets[]}`, newest `updated_at` first |
| `VAULT_DELETE` → `VAULT_DELETE_RESPONSE` | `{key, tenant_id}` | `{status, key, deleted: true}` |
| `VAULT_ROTATE` → `VAULT_ROTATE_RESPONSE` | `{key, tenant_id, value}` | `{status, key, rotated_at, current_version, previous_version}` |
| `VAULT_ROLLBACK` → `VAULT_ROLLBACK_RESPONSE` | `{key, tenant_id}` | `{status, key, current_version, previous_version}`; `NO_PREVIOUS_VERSION` if there is none |
| `VAULT_ASSIGN` → `VAULT_ASSIGN_RESPONSE` | `{key, tenant_id, assigned_ilks}` | `{status, key, assigned_ilks, general, changed}`; value and version unchanged |

### KEY_TAKEN and KEY_NOT_FOUND

- A put of a key that already belongs to another tenant fails with `KEY_TAKEN`. The check runs
  before the same-value shortcut, so it never answers `ok` (`VaultStore::put`).
- Delete, rotate, rollback and assign of a key that belongs to another tenant fail with
  `KEY_NOT_FOUND` and the same message as a missing key (`ensure_in_tenant`,
  `VaultError::KeyNotFoundInTenant`), so naming another tenant's key reveals nothing. Only the
  audit row differs: `denied` instead of `error` (`audit_result_for_error`).
- Get and get-metadata take no tenant: the key is looked up hive-wide and the read rules above
  decide.

### Error codes

`VaultError::code`: `INVALID_REQUEST`, `UNAUTHORIZED`, `KEY_NOT_FOUND`, `KEY_TAKEN`,
`NO_PREVIOUS_VERSION`, `IDENTITY_UNAVAILABLE` (the only one worth retrying), `STORAGE_ERROR`,
`ENCRYPTION_ERROR`, `MASTER_KEY_NOT_AVAILABLE`, `IO_ERROR`, `INVALID_VALUE`, `NODE_ERROR`. An
error answer is `{status: "error", error_code, message}`.

### Reserved keys

`CLOUD_RESERVED_VAULT_KEY_PREFIXES` in `crates/fluxbee_sdk/src/vault.rs`: `edge_channel_secret:`,
`edge_tls`, `ssh:`, `edge_fanout_verify:` (prefix match, case-insensitive,
`is_cloud_reserved_vault_key`). This is not a vault rule: SY.vault does not check it. SY.admin
refuses these keys only when the IO.cloud relay asks for a put, assign or delete
(`enforce_cloud_relay_content`). SY.admin itself stores `edge_channel_secret:<ich>` and the
fanout verify token, owned by the edge node.

## VAULT_SECRET_CHANGED

After a mutation that changed something (a put with `changed: true`, rotate, delete, rollback,
an assign with `changed: true`), SY.vault broadcasts `VAULT_SECRET_CHANGED`
(`send_broadcast_secret_changed`): `Destination::Broadcast`, scope `global`, a new trace id. The
payload (`VaultSecretChangedPayload` in `crates/fluxbee_sdk/src/protocol.rs`) carries metadata
only, never the value: `op` (`put|rotate|delete|rollback|assign`), `resource_type`, `tenant_id`,
`ilk`, `assigned_ilks` (current and previous, plus a previous owner the change removed),
`general` (now or before the change), `version` (`0` on delete), `key`, `hive_id`, `at_ms`
(`build_secret_changed_payload`). A secret without `resource_type` is never announced.

Consumers filter with `VaultSecretChangedPayload::matches_interest`, which mirrors the read
rules. What they do:

- SY.storage and SY.identity exit with status 0 so systemd restarts them with the new value,
  and only when the event comes from `SY.vault@<own hive>` (`handle_vault_secret_changed` in
  `src/bin/sy_storage.rs`).
- SY.edge re-resolves its channel secrets and treats the bootstrap re-announcement differently
  from a real TLS change (`VAULT_BOOTSTRAP_ACTION`, `src/bin/sy_edge.rs`).
- SY.admin and SY.cognition re-resolve their AI key (`handle_vault_secret_changed_admin` in
  `src/bin/sy_admin.rs`; the handler in `src/bin/sy_cognition.rs`).

## Model D': no secrets in node config

Nodes name a resource type, not a secret. SY nodes refuse secret-bearing `CONFIG_SET` fields,
for example `config.database.postgres_url` on SY.storage and
`config.secrets.openai.api_key` on SY.cognition (`reject_cognition_secret_fields`), and resolve
the secret from SY.vault by `resource_type`. SY.admin normalizes `resource_type` on a put
(`normalize_resource_type`: lowercase, runs of other characters become `_`, at most 64
characters, at least one letter) and resolves `metadata.owner_node` to the owner ILK from the
identity SHM, or deterministically for a `SY.*` name. It rejects the legacy `metadata.owner_ilk`.

## Config

SY.vault has no `CONFIG_SET`. Its inputs are `/etc/fluxbee/hive.yaml` (`hive_id`, `role`,
`system_nodes`) and `JSR_LOG_LEVEL`. Paths are constants: `DEFAULT_DB_PATH`,
`DEFAULT_MASTER_KEY_PATH`, `DEFAULT_LOCK_PATH`.

## State on disk

- `/var/lib/fluxbee/vault.db` (0600): table `secrets` (key, metadata JSON, version, current and
  previous nonce and ciphertext, previous version, created/updated/last-accessed times, access
  count), table `audit_log` (timestamp, operation, key, caller name, caller ILK, caller tenant,
  result `success|noop|denied|error`, error code), table `schema_version`.
- `/etc/fluxbee/vault.master.key` (32 bytes, 0600). If the file is missing at start, a new
  random key is written, and every secret stored under the old key then fails to decrypt
  (`ENCRYPTION_ERROR`). `fluxbee-factory-reset` keeps the master key and the infrastructure keys
  `storage_postgres_url`, `edge_channel_secret:*` and `edge_tls*` (`INFRA_KEYS`,
  `INFRA_PREFIXES` in `packaging/fluxbee-factory-reset`).

## Failure behavior

- Startup errors (lock held, bad master key file, DB open failure) end the process; systemd
  restarts it every 5 s.
- If the router channel closes, the main loop returns and the process exits; systemd restarts it.
- With the identity SHM unavailable, non-SY callers get `IDENTITY_UNAVAILABLE` and SY callers of
  this hive still read the root tenant's general secrets (the boot path).
- A failed broadcast is logged (`failed to emit VAULT_SECRET_CHANGED broadcast`); the answer to
  the caller was already sent.

## Health

- `systemctl status sy-vault` and `journalctl -u sy-vault`. Startup logs `sy.vault started` and
  then `sy.vault bootstrap broadcast complete` (or `vault.db is empty, nothing to announce`).
- The node answers `NODE_STATUS_GET` with the SDK default handler
  (`try_handle_default_node_status` in `crates/fluxbee_sdk/src/status.rs`).
- `curl -s 127.0.0.1:8080/hives/motherbee/vault/secrets` on the motherbee lists metadata; an
  answer proves the admin → vault round trip.

## SY.admin HTTP routes

Served by SY.admin on the motherbee (`127.0.0.1:8080`, route table in `src/bin/sy_admin.rs`):

| Route | Admin action |
| --- | --- |
| `GET /hives/{hive}/vault/secrets?prefix=&tenant_id=&resource_type=&ilk=&tags=&limit=` | `vault_list` |
| `POST /hives/{hive}/vault/secrets` body `{key, value, metadata}` | `vault_put` |
| `GET /hives/{hive}/vault/secrets/{key}/metadata` | `vault_get_metadata` |
| `GET /hives/{hive}/vault/secrets/{key}` | `vault_get` |
| `DELETE /hives/{hive}/vault/secrets/{key}?tenant_id=` | `vault_delete` |
| `PUT /hives/{hive}/vault/secrets/{key}/assigned` body `{tenant_id?, assigned_nodes?, assigned_ilks?}` | `vault_assign` |
| `POST /hives/{hive}/vault/secrets/{key}/rotate?tenant_id=` body `{value}` | `vault_rotate` |
| `POST /hives/{hive}/vault/secrets/{key}/rollback?tenant_id=` | `vault_rollback` |

The put metadata accepts `owner_node` (resolved to `ilk`) and `assigned_nodes` (resolved to
`assigned_ilks`).

Known gap (seen 2026-10-17): the metadata route, like the value route, forwards only `key`, with
no `tenant_id`. SY.vault applies the read rules to `VAULT_GET_METADATA`, and SY.admin is
neither the owner nor an assigned node of such secrets and belongs to the root tenant (doc
comment of `DEFAULT_ROOT_TENANT_ID`), so for an owned, assigned or non-root-tenant secret this
route answers `UNAUTHORIZED` (the comment above `edge_channel_secret` in `handle_externalize`
says the same for edge tokens). Use the list route with `tenant_id` and `prefix` to see that
metadata.

## Links

- [SY.identity](sy-identity.md): the SHM SY.vault reads for callers and tenants.
- [SY.storage](sy-storage.md): the main consumer of `storage_postgres_url`.
- [Operations](../reference/operations.md): first boot, where the postgres secret is loaded.
- `docs/ilk-tenant-model-v1.md` §2.2: the tenant and secret model the code cites.
