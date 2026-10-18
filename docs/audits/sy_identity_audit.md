# SY.identity - Detailed technical audit

Review date: 2026-05-07  
Main scope: `src/bin/sy_identity.rs`  
Related context: `README.md`, `docs/history/onworking-coa/node_secret_tasks.md`, `scripts/install.sh`, `scripts/fluxbee_db_bootstrap.sh`

## 1. Executive summary

`SY.identity` is the identity node of Fluxbee. It keeps the registry of tenants (`TNT`), interlocutors (`ILK`), channels (`ICH`), temporary aliases and the cognitive definitions of agents. On `motherbee` it acts as the primary, with PostgreSQL; on workers it acts as a read replica synced from the primary.

The current architecture is reasonably aligned with the Fluxbee model:

- `SY.identity` is the owner of the identity domain.
- Primary persistence lives in PostgreSQL (`fluxbee_identity`).
- Fast reads for other components live in SHM (`/jsr-identity-<hive>`).
- DB secrets do not live in `hive.yaml`; they are loaded via `CONFIG_SET` and stored in `secrets.json`.
- The main mutations require the primary.
- Workers do not write to the DB and sync from `motherbee`.

Main risks detected:

- The sync TCP channel (`identity.sync.port`, default `9100`) has no authentication of its own in `SY.identity`; it depends on the deployment/network/firewall.
- The schema migration mechanism is partial. For alpha/testing, recreating the DB is accepted, but future production would lack a formal migration.
- `FLUXBEE_DATABASE_URL` / `JSR_DATABASE_URL` are still a compatibility fallback and, if used, can bypass the per-node secrets flow.
- Some error responses still expose raw Postgres or parsing messages. This is useful for alpha, but they should be better classified/redacted for shared environments.
- The authorization model is by L2 name/prefix. It is enough for the current stage, but not cryptographically strong if a malicious node manages to register with an allowed name.

## 2. Actual responsibilities

`SY.identity` implements these responsibilities:

- Create and manage tenants (`TNT_CREATE`, `TNT_UPDATE`, `TNT_SET_SPONSOR`, `TNT_APPROVE`, `TNT_LIST`, `TNT_GET`).
- Create, register and update interlocutors (`ILK_PROVISION`, `ILK_REGISTER`, `ILK_UPDATE`, `ILK_SET_DEFINITION`, `ILK_LIST`, `ILK_GET`).
- Associate channels with interlocutors (`ILK_ADD_CHANNEL`, `ICH_SET_ENABLED`).
- Keep temporary aliases from ILK merges.
- Persist the primary state in PostgreSQL.
- Publish snapshots and deltas to SHM.
- Sync replicas with an initial full sync and a delta stream.
- Expose `CONFIG_GET` / `CONFIG_SET` for the primary's DB secret.
- Report `PING`, `STATUS` and `IDENTITY_METRICS`.

These should not be the responsibility of `SY.identity`:

- Creating PostgreSQL roles.
- Installing PostgreSQL.
- Resetting testing DBs.
- Managing global secrets.
- Replacing `SY.admin` as the operations plane.
- Implementing business routing.

This is aligned with the recent change: DB creation/reset belongs to `scripts/fluxbee_db_bootstrap.sh` and to the future Linux package, not to the node runtime.

## 3. Topology and modes

### Primary

The primary is `SY.identity@motherbee`.

Conditions enforced by the code:

- `role=motherbee` requires `hive_id=motherbee`.
- If the hive is the primary, it tries to load `postgres_url`.
- If the DB is ready, it ensures the `fluxbee_identity` DB, ensures the schema and loads the store from the DB.
- If the DB is not ready, it starts degraded and rejects mutating actions with `DB_NOT_READY`.

### Replica

On workers:

- It uses no local DB.
- It starts with a minimal local in-memory state.
- If `identity.sync.upstream` is configured, it requests a full sync from the primary.
- Then it keeps a delta subscription.
- It exposes a local SHM with the synced data.
- It rejects mutating actions with `NOT_PRIMARY`.

### SHM

Name per hive:

```text
/jsr-identity-<hive_id>
```

Expected use:

- OPA/routing/IO can read identity, tenants, ILKs, ICHs, aliases and cognitive hashes without calling PostgreSQL.
- `SY.identity` is the writer of the segment.

## 4. Data model

### Tenant

Main fields:

- `tenant_id`: `tnt:<uuid>`
- `name`
- `domain`
- `status`: `pending`, `active`, `suspended`
- `settings`
- `sponsor_tenant_id`: optional, a reference to another tenant
- `created_at`, `updated_at`

Semantics:

- A tenant without a sponsor is a root.
- A tenant can be a sponsor if it has children.
- `TNT_SET_SPONSOR` accepts setting or clearing the sponsor.
- There is validation against self-sponsorship and cycles.

### ILK

Main fields:

- `ilk_id`: `ilk:<uuid>`
- `ilk_type`: `human`, `agent`, `system`
- `registration_status`: `temporary`, `partial`, `complete`
- `tenant_id`
- `identification`: free-form JSON, normalized by the registration paths
- `association`: derived for DB persistence
- `definition`: cognitive definition JSON for agents
- `channels`: list of ICHs, embedded in memory
- `deleted_at_ms`: soft delete for temporaries/aliases

Notes:

- `ILK_PROVISION` only allows `human` or `agent`; `system` is reserved for SY paths.
- `ILK_SET_DEFINITION` only applies to `ilk_type=agent`.
- The cognitive definition only accepts:
  - `role_hash`
  - `skill_hashes`
  - `handbook_hashes`

### ICH

Main fields:

- `ich_id`: `ich:<uuid>`
- `ilk_id`
- `tenant_id`
- `channel_type`
- `address`
- `owner_l2_name`
- `is_primary`
- `enabled`

Semantics:

- Canonical lookup: `(channel_type_lower, address_lower, tenant_id_lower)`.
- The DB constraint prevents duplicating the same channel within the tenant.
- `enabled` makes it possible to control the channel's availability without deleting the identity.

### Alias

Fields:

- `old_ilk_id`
- `canonical_ilk_id`
- `expires_at_ms`

Use:

- It allows temporary identity merges.
- GC runs every `30s`.
- When it expires, it deletes the alias and can soft-delete temporary ILKs.

### Vocabulary

A table and space in SHM exist, but in the current code it appears as a prepared capability, not as a main functional flow.

## 5. Supported actions

### Read

| Action | Response | Authorized source | Notes |
|---|---|---|---|
| `ILK_LIST` | `ILK_LIST_RESPONSE` | `SY.admin@*` | Lists known ILKs. |
| `ILK_GET` | `ILK_GET_RESPONSE` | `SY.admin@*` | Resolves aliases and embeds the tenant. |
| `TNT_LIST` | `TNT_LIST_RESPONSE` | `SY.admin@*` | Lists tenants with a summary. |
| `TNT_GET` | `TNT_GET_RESPONSE` | `SY.admin@*` | Returns the tenant, sponsor, children and counts. |
| `IDENTITY_METRICS` | `IDENTITY_METRICS_RESPONSE` | open | Returns internal metrics. |
| `PING` | `PONG` | open | Basic health. |
| `STATUS` | `STATUS_RESPONSE` | open | Status and DB readiness. |
| `CONFIG_GET` | `CONFIG_RESPONSE` | `SY.admin@*` | Discovers the DB secret contract. |

### Mutation

| Action | Response | Authorized source | Requires primary |
|---|---|---|---|
| `ILK_PROVISION` | `ILK_PROVISION_RESPONSE` | `IO.*` | Yes |
| `ILK_REGISTER` | `ILK_REGISTER_RESPONSE` | `SY.frontdesk.gov@*`, `SY.orchestrator@*`, exact bootstrap | Yes |
| `ILK_ADD_CHANNEL` | `ILK_ADD_CHANNEL_RESPONSE` | `SY.frontdesk.gov@*`, configured frontdesk | Yes |
| `ILK_UPDATE` | `ILK_UPDATE_RESPONSE` | `SY.orchestrator@*`, exact bootstrap | Yes |
| `ILK_SET_DEFINITION` | `ILK_SET_DEFINITION_RESPONSE` | `SY.admin@*`, `SY.architect@*`, exact bootstrap | Yes |
| `ICH_SET_ENABLED` | `ICH_SET_ENABLED_RESPONSE` | `IO.*`, `SY.admin@*`, `SY.architect@*`, `SY.frontdesk.gov@*` | Yes |
| `TNT_CREATE` | `TNT_CREATE_RESPONSE` | `SY.admin@*`, `SY.architect@*`, `SY.frontdesk.gov@*`, configured frontdesk | Yes |
| `TNT_UPDATE` | `TNT_UPDATE_RESPONSE` | `SY.admin@*`, `SY.architect@*`, `SY.frontdesk.gov@*` | Yes |
| `TNT_SET_SPONSOR` | `TNT_SET_SPONSOR_RESPONSE` | `SY.admin@*`, `SY.architect@*`, `SY.frontdesk.gov@*` | Yes |
| `TNT_APPROVE` | `TNT_APPROVE_RESPONSE` | `SY.admin@*` | Yes |
| `CONFIG_SET` | `CONFIG_RESPONSE` | `SY.admin@*` | Primary only |

## 6. Authorization

Authorization control is based on `src_l2_name`.

Mechanisms:

- A list of prefixes per action.
- A list of exact names for bootstrap.
- Some actions (`PING`, `STATUS`, `IDENTITY_METRICS`) are open.

Assessment:

- Correct for the current stage and for control inside a trusted router.
- Not enough as a strong security boundary if a process can register with a privileged L2 name.
- Real security depends on:
  - control over who can run nodes
  - control of the router socket
  - host permissions
  - the integrity of `SY.orchestrator`

Future recommendation:

- Tie authorization to a verified ILK/system identity, not only to the L2 name.
- Sign or validate the identity of privileged SY nodes.
- Reduce open actions, or answer only known SY sources when possible.

## 7. PostgreSQL persistence

Fixed DB:

```text
fluxbee_identity
```

Creation:

- `SY.identity` parses the base `postgres_url`.
- It switches the DB to `postgres` to create `fluxbee_identity` if it is missing.
- Then it switches the DB to `fluxbee_identity`.
- It ensures its own schema.

Tables:

- `identity_tenants`
- `identity_ilks`
- `identity_ilk_aliases`
- `identity_ichs`
- `identity_vocabulary`

Main indexes/constraints:

- `identity_tenants(tenant_id)` PK
- `identity_tenants.sponsor_tenant_id` FK self-reference
- `identity_ilks(ilk_id)` PK
- partial unique on `(email, tenant_id)` when email exists and is not deleted
- partial unique on `node_name` when it exists and is not deleted
- unique on ICH by `(channel_type, address, tenant_id)`

Assessment:

- The model is clear and has important constraints.
- Using a transaction for ILK persistence is correct.
- `upsert_tenant_in_db` is simple and adequate.
- There is in-memory rollback handling in several mutations if persistence fails.

Friction points:

- There is no formal system of versioned migrations.
- `CREATE TABLE IF NOT EXISTS` + some `ALTER TABLE` works for alpha, but it does not guarantee complete upgrades.
- DB error messages are not always classified; some failures end up as `DB_WRITE_FAILED`.
- `ensure_database_exists` requires the role to have permission to create a DB if the DB does not exist. The new `fluxbee_db_bootstrap.sh` reduces this friction on a clean install.

Recommended operational decision for alpha:

- For clean testing, use `FLUXBEE_DB_RESET=1 scripts/install.sh`.
- Do not put old-DB compatibility inside `SY.identity`, except for necessary and deliberate migrations.
- Keep `SY.identity` as the owner of the schema, not of the PostgreSQL role/cluster.

## 8. Secrets and CONFIG_SET

Canonical field:

```text
config.database.postgres_url
```

Persistence:

```text
/var/lib/fluxbee/nodes/SY/SY.identity@motherbee/secrets.json
```

Read precedence:

1. The node's local `secrets.json`.
2. `FLUXBEE_DATABASE_URL`.
3. `JSR_DATABASE_URL`.
4. missing.

Contract:

- `CONFIG_GET` returns metadata and redaction.
- `CONFIG_SET` requires:
  - `node_name`
  - `schema_version`
  - `config_version`
  - `apply_mode=replace`
  - `config.database.postgres_url`
- The real value does not come back in responses.
- Applying a new secret requires a restart of `sy-identity`.

Assessment:

- Aligned with `node_secret_tasks.md`.
- Good use of the SDK helper for `secrets.json`.
- Audit metadata uses `updated_by_ilk`, `updated_by_label` and `trace_id`.

Risks:

- The env fallback allows operating without a per-node secret. It is useful for bootstrap/testing, but it can confuse diagnosis.
- If `CONFIG_SET` is sent by a client that logs the full payload, the secret can end up outside `SY.identity`; this must be controlled in `SY.admin`/Archi/SCMD.

## 9. SHM

`SY.identity` dumps into SHM:

- tenants
- ILKs
- ICHs
- aliases
- ICH -> ILK lookup mappings
- definition hashes for agents

Default capacities:

- `max_ilks`: `8192`
- `max_tenants`: `1024`
- `max_vocabulary`: `4096`
- `max_ilk_aliases`: default equal to `max_ilks`

Configuration:

```yaml
identity:
  max_ilks: ...
  max_tenants: ...
  max_vocabulary: ...
  max_ilk_aliases: ...
```

Behavior:

- Initial full sync when the writer is created.
- Fast path for `ILK_PROVISION`.
- Incremental deltas for mutations.
- If an incremental apply fails, it rebuilds the full snapshot.
- Heartbeat every 5 seconds.

Assessment:

- Correct design for low-latency reads.
- Good full-snapshot fallback.
- Including `sponsor_tenant_id`, `enabled`, `owner_l2_name` and the cognitive hashes enables OPA/routing without DB calls.

Risks:

- If SHM limits are exceeded, writes/reads can degrade. There must be monitoring.
- The source of truth is still the DB on the primary; SHM must be treated as an operational cache/snapshot.

## 10. Synchronization between hives

Primary:

- Listens on `0.0.0.0:<identity.sync.port>`.
- Default: `9100`.
- Handles:
  - `IDENTITY_FULL_SYNC_REQUEST`
  - `IDENTITY_DELTA_SUBSCRIBE`

Replica:

- Reads `identity.sync.upstream`.
- Does an initial full sync.
- Subscribes to deltas.
- Validates an increasing sequence.
- Acknowledges each delta.
- Reconnects every 1 second if the connection drops.

Assessment:

- The protocol is simple and sufficient for alpha.
- A chunked full sync avoids a single giant payload.
- Delta ACK with retries reduces simple loss.

Risks:

- There is no authentication or authorization on the sync TCP socket.
- There is no TLS.
- There is no compression or advanced backpressure.
- If there is a sequence gap, the replica cuts the stream and reconnects, but it is not clear whether it always does a full resync before continuing.

Recommendations:

- In testing, restrict it by firewall/network.
- For production, add sync authentication or wrap it in a secure channel.
- On a delta gap, force a full sync before reopening the delta stream.
- Expose sync metrics: last full sync, last delta seq, reconnections, lag.

## 11. Cognitive definition of agents

The current change consolidates the concept of the agents' "soul" into `definition`.

Only agents (`ilk_type=agent`) can have:

```json
{
  "role_hash": "<sha256>",
  "skill_hashes": ["<sha256>"],
  "handbook_hashes": ["<sha256>"]
}
```

Validations:

- The JSON must be an object.
- Only those three keys are accepted.
- Hashes must be 64-character SHA-256 hex.
- Arrays respect the SHM limits (`IDENTITY_DEFINITION_MAX_SKILLS`, `IDENTITY_DEFINITION_MAX_HANDBOOKS`).
- An empty definition clears the cognitive configuration.

Flow:

- `ILK_SET_DEFINITION` modifies the ILK.
- It is persisted in the DB as `identity_ilks.definition`.
- It is published to SHM as hash bytes.
- AI nodes can read the hashes from SHM and then resolve blobs/capabilities elsewhere.

Assessment:

- The model is correct because identity stores immutable references by hash, not prompt text or large blobs.
- It lets OPA/routing use capabilities by hash.
- It keeps separation of concerns: identity knows the identity and the declarative definition; storage/blob/cognition resolve the content.

Risk:

- There is no visible operational contract that links a "definition" to its real blob and its lifecycle.
- If a hash points to a nonexistent blob, identity accepts it anyway; existence validation is left to another layer.

## 12. Relevant configuration in hive.yaml

Fields used:

```yaml
hive_id: motherbee
role: motherbee|worker

government:
  identity_frontdesk: SY.frontdesk.gov@motherbee

identity:
  merge_alias_ttl_secs: 3600
  max_ilks: 8192
  max_tenants: 1024
  max_vocabulary: 4096
  max_ilk_aliases: 8192
  sync:
    port: 9100
    upstream: 127.0.0.1:9100
```

Legacy field, present but not used for the effective DB:

```yaml
database:
  url: ...
```

`SY.identity` defines `DatabaseSection`, but the effective flow uses the local secret or env. The operational doc must keep stating that DB secrets do not go in `hive.yaml`.

## 13. Observability

Relevant events/logs:

- Startup without DB: `sy.identity started without active DB backend`.
- DB schema ensured.
- Store loaded from DB.
- Full sync bootstrap applied/failed.
- SHM snapshot/delta apply.
- Alias GC local/DB.
- Sync request failed.
- Action response with elapsed.

Diagnostic endpoints/actions:

- `STATUS`
- `PING`
- `IDENTITY_METRICS`
- `CONFIG_GET`
- `TNT_LIST`
- `ILK_LIST`

Gap:

- There is no HTTP health endpoint of its own; it is operated via L2/admin.
- There are no persistent replication lag metrics.
- `STATUS` could expose more sync information on replicas.

## 14. Findings

### H1 - Sync TCP without authentication

Severity: high for production, acceptable for a controlled alpha.

The sync listener accepts TCP connections and processes a JSON line protocol. There is no authentication or TLS.

Impact:

- An actor with network access to the port could request a full sync.
- It could open subscriptions and consume resources.

Current mitigation:

- Firewall/orchestrator.
- Controlled host exposure.

Recommendation:

- Authentication by hive token/key.
- Origin restriction by config.
- TLS or a private channel.

### H2 - Authorization based on the L2 name

Severity: medium/high.

Sensitive actions are authorized by `src_l2_name` and prefixes. If a malicious node manages to register with a privileged name, it could mutate identity.

Recommendation:

- Tie permissions to a validated ILK/system identity.
- Harden the router to prevent collision/spoofing of SY names.
- Audit, in `SY.admin`/orchestrator, the creation of nodes with SY names.

### H3 - Unversioned DB migrations

Severity: medium.

`ensure_primary_schema` uses `CREATE TABLE IF NOT EXISTS` and some `ALTER TABLE`, but there is no formal versioning.

Impact:

- In alpha, deleting/recreating the DB is acceptable.
- In production, schema changes can be left half-done.

Recommendation:

- Create an `identity_schema_migrations` table.
- Version the SQL migrations.
- Keep `install/reset` as a testing flow, not as an upgrade.

### H4 - The env fallback can confuse the secrets model

Severity: medium.

`FLUXBEE_DATABASE_URL` and `JSR_DATABASE_URL` are still active as a fallback.

Impact:

- A node can start configured without `CONFIG_GET` showing `local_file`.
- Operators may believe the secret is persisted when it comes from env.

Recommendation:

- Keep it for compatibility in alpha.
- Clearly document `source=env_compat`.
- In a future version, downgrade the env fallback to bootstrap-only or remove it.

### H5 - Partially raw DB errors

Severity: low/medium.

Some errors include a raw Postgres/parsing string.

Impact:

- It helps a lot in development.
- It can expose internal details in shared environments.

Recommendation:

- Keep it in alpha.
- Before production, map to codes and move the detail to internal logs.

### H6 - A delta stream gap does not explicitly force a full resync

Severity: medium.

The replica detects a gap/out-of-order and reconnects. The delta loop can subscribe again, but no mandatory full sync is observed before continuing.

Impact:

- A replica could be left with an incomplete state if it loses deltas and does not do a full refresh.

Recommendation:

- On a gap, run a full sync and only then resubscribe.

### H7 - Vocabulary prepared but not integrated

Severity: low.

The table/SHM for vocabulary exists, but there is no visible mutation/read flow equivalent to ILK/TNT.

Recommendation:

- If it is not used in the current sprint, document it as reserved.
- If it is used, add actions and tests.

## 15. Existing tests observed

In `sy_identity.rs` there are unit tests for:

- Bootstrap/frontdesk authorization.
- Rejection of unauthorized sources.
- ILK resolution with an alias and an embedded tenant.
- `TNT_GET` with sponsor/counts.
- `TNT_LIST` sorted and with summaries.
- Idempotency of tenant create by name/domain.
- Default tenant root.
- Set/clear sponsor.
- Rejection of cycles/self-sponsor.
- Cognitive definition validation.
- Rejection of a definition for a non-agent.
- ICH lookup by tenant.

Suggested missing tests:

- E2E of `CONFIG_SET` + restart + DB ready.
- E2E of `TNT_CREATE` + `TNT_SET_SPONSOR` + `TNT_LIST` via admin.
- E2E of `ILK_SET_DEFINITION` + SHM readback.
- E2E of a replica worker with full sync and delta.
- A delta gap test that verifies the full resync.
- A test of `fluxbee_db_bootstrap.sh` on a clean local Postgres.

## 16. Minimal runbook

### Clean server with PostgreSQL installed

```bash
sudo PATH="$PATH" FLUXBEE_DB_USER=sa FLUXBEE_DB_PASSWORD='***' ./scripts/install.sh
```

### Testing from scratch

```bash
./scripts/fluxbee_stop.sh
sudo PATH="$PATH" FLUXBEE_DB_RESET=1 FLUXBEE_DB_USER=sa FLUXBEE_DB_PASSWORD='***' ./scripts/install.sh
```

### Load the DB secret into SY.identity

Use `CONFIG_SET` via `SY.admin`/Archi with:

```json
{
  "node_name": "SY.identity@motherbee",
  "schema_version": 1,
  "config_version": 1,
  "apply_mode": "replace",
  "config": {
    "database": {
      "postgres_url": "postgresql://sa:***@127.0.0.1:5432/fluxbee"
    }
  }
}
```

Afterwards:

```bash
sudo systemctl restart sy-identity
```

### Verify status

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes/SY.identity@motherbee/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"operator"}' | jq .
```

Expected:

- `ok=true`
- `state=configured`
- `config.database.postgres_url="***REDACTED***"`
- `config.database.source="local_file"`
- `config.database.db_name="fluxbee_identity"`

## 17. Prioritized recommendations

### Short term

- Keep the DB bootstrap out of `SY.identity`.
- Use a full DB reset in testing, not ad hoc migrations.
- Add E2E for `TNT_LIST`/`TNT_GET`/`TNT_SET_SPONSOR`.
- Add E2E for `ILK_SET_DEFINITION` and the SHM read by hashes.
- Make sure the Archi docs/handbook explain `TNT_LIST`, `TNT_GET`, `ILK_SET_DEFINITION` and the runtime vs instance difference.

### Medium term

- Add a full resync on a delta stream gap.
- Expose replica sync metrics.
- Classify DB errors into stable codes.
- Formalize DB migrations.

### Future production

- Authentication/TLS for identity sync.
- Authorization by cryptographic identity, not only by L2 name.
- Harden the registration of `SY.*` names.
- Evaluate at-rest encryption for `secrets.json` as hardening, not as a boundary against root.

## 18. Expected health criteria

A healthy `SY.identity@motherbee` should meet these criteria:

- `STATUS` reports `state=configured`.
- `CONFIG_GET` reports `ok=true`.
- `TNT_LIST` returns at least the root tenant `fluxbee`.
- PostgreSQL has the `fluxbee_identity` DB.
- `identity_tenants`, `identity_ilks`, `identity_ichs`, `identity_ilk_aliases` exist.
- SHM `/jsr-identity-motherbee` exists and has an active heartbeat.
- There are no repeated `DB_NOT_READY` warnings.
- There are no delta gaps on workers.

## 19. Conclusion

`SY.identity` is well placed as the identity authority of Fluxbee, and it already brings together important pieces: tenants, sponsors, ILK/ICH, aliases, the cognitive definition and publishing to SHM. The biggest risk is not in the data model but in the operational boundaries: the PostgreSQL bootstrap, unauthenticated sync TCP and authorization by L2 name.

For the alpha stage, the right decision is to keep `SY.identity` simple and to clean DBs from the install infrastructure. To move to a more stable environment, the priorities should be versioned DB migrations, sync hardening and stronger authorization for identity mutations.
