# Fluxbee — SY.vault Specification

**Status:** v1.1 draft — implementation-ready after closed design decisions
**Date:** 2026-05-10
**Audience:** SY.vault developer, SDK maintainers, Archi designers
**Related:** `10-identity-v2.md`, `01-arquitectura.md`, `executor_manifest_pilot_spec.md`

---

## 1. Purpose

`SY.vault` is a system node that stores and serves secrets to other nodes in the Fluxbee system. Secrets are arbitrary JSON values (API keys, OAuth tokens, webhook URLs, credentials, etc.) that nodes need to call external services or to access protected resources.

The vault provides:

- A simple key-value store with metadata.
- Encryption at rest of all secret values.
- Authorization based on the requester's ILK and tenant.
- Full audit log of all operations.
- L2 message protocol consistent with other SY nodes.

The design prioritizes simplicity and operability over cryptographic completeness. Trust boundaries align with Fluxbee's existing model: the router canonicalizes message sources, and the vault trusts that canonicalization for authorization.

---

## 2. Design principles

**2.1 Trust the router for source authentication.** When a message arrives at the vault, `routing.src_l2_name` and `meta.src_ilk` are already validated/canonicalized by the router. The vault does not re-authenticate clients. It resolves tenant and ILK type from identity SHM using the canonical `meta.src_ilk`.

**2.2 Encrypt at rest, not in transit.** Secret values are encrypted in the database. Messages in transit travel through the standard Fluxbee routing infrastructure without additional encryption. Cross-hive encryption is the router's responsibility (see §14).

**2.3 Single L2 protocol.** Vault speaks the same L2 protocol as any other node. No HTTP API, no certificates, no TLS endpoints. Simplicity over complexity.

**2.4 Authorization in vault.** Until OPA integration is ready, the vault implements its own authorization rules based on the requester's identity. These rules are hardcoded in v1 and migrate to OPA later.

**2.5 Self-contained secret storage.** Vault has its own SQLite database, its own master key, and its own audit log. Secret storage does not depend on `SY.storage`, Postgres, NATS persistence, or any node-local `secrets.json`. Vault does read identity SHM for authorization metadata (`tenant_id`, `ilk_type`, status) because the current L2 protocol does not carry `src_tenant_id`.

**2.6 Append-only audit.** Every operation is logged with full context. Audit log is tamper-evident in design (append-only table) but not cryptographically signed in v1.

**2.7 Canonical secret backend.** This is a monolithic replacement for node-local secret persistence in the alpha line. New secret writes go through `SY.admin -> SY.vault`; nodes consume secrets through SDK helpers. The previous `secrets.json` model is deprecated for new writes once vault is enabled.

---

## 3. Threat model

### 3.1 Threats addressed

**T1: A node legitimately registered in Fluxbee tries to read secrets it should not access.**

Mitigation: vault checks canonical `meta.src_ilk` against secret metadata (`owner_ilk`, `tenant_id`) and identity SHM metadata. Returns error if not authorized.

**T2: A node tries to impersonate another node when requesting a secret.**

Mitigation: handled by the router via canonicalization, before the message reaches the vault. The vault trusts `routing.src_*`.

**T3: An attacker steals the SQLite file from the host (backup leak, disk theft, etc.).**

Mitigation: secrets are encrypted at rest with AES-256-GCM. Without the master key (separate file with restricted permissions), the database is useless.

**T4: An attacker reads the master key from the filesystem.**

Mitigation: `/etc/fluxbee/vault.master.key` has 0600 permissions (only the user running the vault can read). If the attacker has root, they can read everything anyway — no software-only mitigation against root compromise. Mitigation here is OS-level (LUKS, careful access control on the host).

**T5: An audit trail is needed to investigate an incident.**

Mitigation: every vault operation logs source identity, timestamp, operation, key, and result. Logs never include secret values.

### 3.2 Threats explicitly NOT addressed in v1

**T6: Network sniffing between hives.** Cross-hive TCP traffic is not encrypted at the vault level. This is the router's responsibility (see §14).

**T7: Compromise of the running vault process.** If an attacker executes code in the vault process, they have access to the master key in memory and to all secrets. No software defense against this — the vault process is a trusted boundary.

**T8: Repudiation.** The audit log is append-only but not cryptographically signed. An attacker with database write access could in principle modify the log. Mitigation: filesystem permissions on the database file.

**T9: Hardware attacks (cold boot attacks, RAM scraping, side channels).** Not in scope.

---

## 4. Storage architecture

### 4.1 SQLite database

Vault uses an embedded SQLite database located at `/var/lib/fluxbee/vault.db`.

Why SQLite:

- The vault is a pure key-value store. Queries are trivial (lookup by key).
- SQLite is battle-tested and ubiquitous.
- Single file is easy to backup (`cp`) and inspect (`sqlite3` CLI).
- The same engine is already used by WF, so the dev team is familiar with it.
- Transactional semantics ensure consistency (PUT + audit log entry are atomic).

### 4.2 Database schema

```sql
CREATE TABLE secrets (
    key VARCHAR(256) PRIMARY KEY,
    value_ciphertext BLOB NOT NULL,
    value_nonce BLOB NOT NULL,
    metadata TEXT NOT NULL,          -- JSON
    version INTEGER NOT NULL DEFAULT 1,
    previous_value_ciphertext BLOB,
    previous_value_nonce BLOB,
    previous_version INTEGER,
    created_at TEXT NOT NULL,        -- ISO-8601
    updated_at TEXT NOT NULL,
    last_accessed_at TEXT,
    access_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_secrets_tenant
    ON secrets(json_extract(metadata, '$.tenant_id'));

CREATE INDEX idx_secrets_owner
    ON secrets(json_extract(metadata, '$.owner_ilk'));

CREATE TABLE audit_log (
    audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,         -- ISO-8601
    operation VARCHAR(32) NOT NULL,
    key VARCHAR(256),
    caller_l2_name VARCHAR(128),
    caller_ilk VARCHAR(64),
    caller_tenant_id VARCHAR(64),
    result VARCHAR(16) NOT NULL,     -- 'success' | 'noop' | 'denied' | 'error'
    error_code VARCHAR(32)
);

CREATE INDEX idx_audit_timestamp ON audit_log(timestamp);
CREATE INDEX idx_audit_key ON audit_log(key);
CREATE INDEX idx_audit_caller ON audit_log(caller_ilk);
CREATE INDEX idx_audit_operation ON audit_log(operation);
```

### 4.3 Encryption at rest

**Algorithm:** AES-256-GCM (authenticated encryption with associated data).

**Master key:** stored in `/etc/fluxbee/vault.master.key` with permissions `0600`. Owned by the user running the vault process. The file contains 32 raw bytes (256 bits).

**Master key generation:** at first boot of the vault, if the file doesn't exist, generate 32 random bytes from the OS CSPRNG and write the file. Subsequent boots load the existing key.

**Per-secret nonce:** each PUT/ROTATE generates a fresh 12-byte nonce. The nonce is stored alongside the ciphertext in the secrets table.

**No key rotation in v1.** If master key rotation is needed in the future, it requires a re-encrypt-all migration. Not in scope for v1.

**No HSM / KMS in v1.** The master key lives in plaintext on disk. This is an explicit tradeoff for simplicity. The host is the trust boundary.

### 4.4 SHM usage

The vault does not write vault data to SHM and never places secret values in shared memory.

The vault does read identity SHM for authorization because the current message shape has `meta.src_ilk` but no `src_tenant_id` field. The SHM lookup is used only to resolve caller `tenant_id`, `ilk_type`, and active/deleted status from the canonical ILK.

---

## 5. Secret model

### 5.1 Secret structure

```json
{
  "key": "tenant:techline:slack-webhook-support",
  "value": { "webhook_url": "https://hooks.slack.com/services/...", "channel": "#support" },
  "metadata": {
    "tenant_id": "tnt:85e6eefe-6034-47ee-969d-a05a4189873b",
    "owner_ilk": "ilk:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    "description": "Slack webhook for Techline support mirror channel",
    "created_by": "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
    "created_at": "2026-05-10T10:00:00Z",
    "tags": ["solution:techline-support", "channel:slack"]
  }
}
```

The `value` can be any JSON: string, object, array, number. The vault does not interpret it. Common shapes:

- Single token: `"sk-abc123..."`
- OAuth bundle: `{ "client_id": "...", "client_secret": "...", "refresh_token": "..." }`
- API config: `{ "api_key": "...", "endpoint": "...", "region": "us-east-1" }`

### 5.2 Key naming convention

Recommended convention (not enforced):

```
<scope>:<purpose>[:<qualifier>]
```

Where `scope` is one of:

- `sys:` — system-wide secrets (master credentials, infrastructure)
- `tenant:<tenant-slug>:` — human-readable tenant-scoped keys
- `node:<node-name>:` — secrets specific to a single node
- `solution:<solution-name>:` — secrets for a specific solution

Examples:

```
sys:openai-api-key
sys:postgres-master-password
tenant:techline:slack-webhook-support
tenant:techline:quickbooks-credentials
node:io-slack-support:bot-token
solution:techline-billing:stripe-api-key
```

The vault accepts any key matching `^[a-z0-9][a-z0-9:_-]{0,255}$`.

Key text is not authoritative for authorization. Authorization uses `metadata.tenant_id` and `metadata.owner_ilk`, not a tenant slug parsed from the key.

### 5.3 Metadata fields

| Field | Type | Required | Description |
|---|---|---|---|
| `tenant_id` | string | Yes | Canonical tenant the secret belongs to. Must be `tnt:<uuid>`. Infrastructure-wide secrets use the fixed Fluxbee root tenant `tnt:00000000-0000-0000-0000-000000000001`. Used for authorization. |
| `owner_ilk` | string | Yes | ILK that "owns" the secret (typically the consumer). |
| `description` | string | No | Free-text description for humans. |
| `created_by` | string | Auto-filled | ILK that created the secret (from canonical `meta.src_ilk`). |
| `created_at` | timestamp | Auto-filled | When the secret was created. |
| `updated_at` | timestamp | Auto-filled | Last update. |
| `tags` | string[] | No | Free-form tags for filtering/organization. |

The vault auto-fills `created_by` from the message's canonicalized source. Other fields come from the request. Any request-provided `created_by`, `created_at`, or `updated_at` is ignored or rejected.

### 5.4 Versioning

The vault keeps **current + previous version** for rollback purposes. Previous versions older than the immediately previous one are discarded.

When a secret is rotated:

1. Current `value_ciphertext`, `value_nonce`, and `version` are moved to `previous_*` columns.
2. New `value_ciphertext`, `value_nonce` are stored.
3. `version` is incremented.

A `VAULT_ROLLBACK` operation swaps current and previous if the operator needs to revert.

Full version history is not in v1. If audit trail of all changes is needed, it's reconstructed from the audit log.

---

## 6. Operations protocol

### 6.1 Node identity

- L2 name: `SY.vault@<hive>` (e.g., `SY.vault@motherbee`)
- Binary: `sy-vault`
- Lock path: `/var/run/fluxbee/sy-vault.lock`
- Database: `/var/lib/fluxbee/vault.db`
- Master key: `/etc/fluxbee/vault.master.key`

### 6.2 Verbs

| Verb | Purpose | Authorization |
|---|---|---|
| `VAULT_PUT` | Create or update a secret | Admin, Architect |
| `VAULT_GET` | Retrieve a secret | Based on metadata (see §7) |
| `VAULT_LIST` | List secrets (no values) | Based on filter and identity |
| `VAULT_DELETE` | Remove a secret permanently | Admin, Architect |
| `VAULT_ROTATE` | Replace value of existing secret | Admin, Architect |
| `VAULT_ROLLBACK` | Revert to previous version | Admin, Architect |
| `VAULT_GET_METADATA` | Get metadata only (no value) | More permissive than GET |

### 6.3 VAULT_PUT

Create or update a secret.

**Request:**
```json
{
  "key": "tenant:techline:slack-webhook-support",
  "value": { "webhook_url": "https://hooks.slack.com/...", "channel": "#support" },
  "metadata": {
    "tenant_id": "tnt:85e6eefe-6034-47ee-969d-a05a4189873b",
    "owner_ilk": "ilk:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    "description": "Slack webhook for Techline support",
    "tags": ["solution:techline-support"]
  }
}
```

`tenant_id` and `owner_ilk` are required in metadata. `tenant_id` must be `tnt:<uuid>`; infrastructure-wide secrets use the fixed Fluxbee root tenant `tnt:00000000-0000-0000-0000-000000000001`. `created_by` is auto-filled from canonical `meta.src_ilk` and rejects any value provided in the request.

**Response (success):**
```json
{
  "status": "ok",
  "key": "tenant:techline:slack-webhook-support",
  "version": 1,
  "created_at": "2026-05-10T10:00:00Z",
  "changed": true
}
```

**Response (error):**
```json
{
  "status": "error",
  "error_code": "UNAUTHORIZED" 
}
```

### 6.4 VAULT_GET

Retrieve the value and metadata of a secret.

**Request:**
```json
{ "key": "tenant:techline:slack-webhook-support" }
```

**Response (success):**
```json
{
  "status": "ok",
  "key": "tenant:techline:slack-webhook-support",
  "value": { "webhook_url": "https://hooks.slack.com/...", "channel": "#support" },
  "metadata": {
    "tenant_id": "tnt:85e6eefe-6034-47ee-969d-a05a4189873b",
    "owner_ilk": "ilk:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    "description": "Slack webhook for Techline support",
    "created_by": "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
    "created_at": "2026-05-10T10:00:00Z",
    "updated_at": "2026-05-10T10:00:00Z",
    "tags": ["solution:techline-support"]
  },
  "version": 1
}
```

**Response (denied):**
```json
{ "status": "error", "error_code": "UNAUTHORIZED" }
```

The denied response is identical regardless of whether the key exists or not. This prevents enumeration of keys.

The vault updates `last_accessed_at` and increments `access_count` on successful GETs.

### 6.5 VAULT_LIST

List secrets, optionally filtered. Values are NEVER returned in this operation.

**Request:**
```json
{
  "filter": {
    "prefix": "tenant:techline:",
    "tags": ["solution:techline-support"],
    "tenant_id": "tnt:85e6eefe-6034-47ee-969d-a05a4189873b"
  }
}
```

All filter fields are optional. If no filter, lists all secrets the caller is authorized to see.

**Response:**
```json
{
  "status": "ok",
  "count": 3,
  "secrets": [
    {
      "key": "tenant:techline:slack-webhook-support",
      "metadata": {
        "tenant_id": "tnt:85e6eefe-6034-47ee-969d-a05a4189873b",
        "owner_ilk": "ilk:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
        "description": "Slack webhook for Techline support",
        "created_at": "2026-05-10T10:00:00Z",
        "tags": ["solution:techline-support"]
      },
      "version": 1
    }
  ]
}
```

### 6.6 VAULT_DELETE

Remove a secret permanently. Includes both current and previous versions.

**Request:**
```json
{ "key": "tenant:techline:slack-webhook-support" }
```

**Response:**
```json
{
  "status": "ok",
  "key": "tenant:techline:slack-webhook-support",
  "deleted": true
}
```

### 6.7 VAULT_ROTATE

Replace the value of an existing secret. Preserves metadata, increments version, moves old value to previous.

**Request:**
```json
{
  "key": "sys:openai-api-key",
  "value": "sk-newkey-abc123..."
}
```

**Response:**
```json
{
  "status": "ok",
  "key": "sys:openai-api-key",
  "rotated_at": "2026-04-17T16:30:00Z",
  "current_version": 4,
  "previous_version": 3
}
```

Only the value changes. Metadata stays. Use VAULT_PUT to update metadata.

### 6.8 VAULT_ROLLBACK

Restore the previous version of a secret. Swaps current and previous.

**Request:**
```json
{ "key": "sys:openai-api-key" }
```

**Response:**
```json
{
  "status": "ok",
  "key": "sys:openai-api-key",
  "current_version": 3,
  "previous_version": 4
}
```

If there is no previous version, returns error.

### 6.9 VAULT_GET_METADATA

Get only metadata, no value. Useful for inspection without granting access to the secret value.

**Request:**
```json
{ "key": "tenant:techline:slack-webhook-support" }
```

**Response:**
```json
{
  "status": "ok",
  "key": "tenant:techline:slack-webhook-support",
  "metadata": { ... },
  "version": 1,
  "last_accessed_at": "2026-04-17T15:00:00Z",
  "access_count": 47
}
```

This operation has more permissive authorization than GET — it can be authorized for anyone in the same tenant, useful for tooling and inventory.

---

## 7. Authorization

### 7.1 Authorization rules in v1

Hardcoded in vault. Migrate to OPA when OPA integration is mature.

The vault extracts caller identity from the current L2 message shape:

- `routing.src_l2_name` for node-level admin override.
- `meta.src_ilk` for ILK-level ownership checks.
- identity SHM lookup by `meta.src_ilk` for caller `tenant_id`, `ilk_type`, and status.

If `meta.src_ilk` is missing for a non-admin caller, or if identity SHM cannot resolve an active ILK, the request is denied.

**VAULT_PUT, VAULT_DELETE, VAULT_ROTATE, VAULT_ROLLBACK:**

Allowed only if `routing.src_l2_name` matches one of:

- `SY.admin@*`
- `SY.architect@*`

**VAULT_GET:**

Allowed if:

- `routing.src_l2_name` matches `SY.admin@*` or `SY.architect@*` (admin override), OR
- `meta.src_ilk == metadata.owner_ilk` (owner reads their own secret), OR
- caller tenant resolved from identity SHM equals `metadata.tenant_id` AND caller `ilk_type == "system"` (system nodes within the same tenant can read tenant secrets).

For infrastructure-wide system secrets, use the fixed Fluxbee root tenant; read access is still enforced by vault policy.

**VAULT_LIST:**

Returns only secrets the caller is authorized to see (filtered server-side).

- Admin and architect: all secrets.
- Other nodes: only secrets where they would pass VAULT_GET authorization.

**VAULT_GET_METADATA:**

Same as VAULT_GET but also allows any node in the same tenant to read metadata (without value).

### 7.2 Authorization audit

Every authorization decision is logged. Denials are logged with `result: "denied"` and `error_code: "UNAUTHORIZED"`. Successes are logged with `result: "success"`. Idempotent no-change writes are logged with `result: "noop"` and do not increment secret version.

The audit log includes the full caller identity even on denials, which is critical for incident investigation.

### 7.3 Future: OPA integration

When OPA reads vault data and is integrated with the router, the hardcoded rules above migrate to Rego policies. The vault then sends authorization queries to OPA via the router (or OPA evaluates inline) instead of hardcoding logic.

The transition is non-breaking: as long as OPA's decisions match the current hardcoded rules, no behavior change is observed.

---

## 8. Audit log

### 8.1 Logged operations

Every vault operation, regardless of result, generates an audit log entry in the local SQLite database at `/var/lib/fluxbee/vault.db`. Audit is not sent through NATS, `SY.storage`, or a separate external database.

```sql
INSERT INTO audit_log (timestamp, operation, key, caller_l2_name, caller_ilk, caller_tenant_id, result, error_code)
VALUES (?, ?, ?, ?, ?, ?, ?, ?);
```

### 8.2 What is NOT logged

- **Secret values.** Never. Even on errors.
- **Metadata fields containing PII or sensitive data.** The metadata is logged only by reference (key), not full content.
- **Stack traces.** Errors are logged by code only.

### 8.3 Audit log retention

In v1, the audit log grows indefinitely. The vault provides no automatic rotation or archival. If the log grows too large, the operator manually archives old entries (e.g., copy to file and delete from table).

A future improvement is automatic rotation by date or by row count, with archived entries moved to compressed log files. This remains local to the vault; it is not a NATS stream.

### 8.4 Audit log query

There is no L2 verb to query the audit log in v1. The operator queries directly via `sqlite3 /var/lib/fluxbee/vault.db`. A future audit read verb/admin action may be added if needed by tooling.

There is also no admin action for audit reads in v1. Audit read UX is a later cross-system audit topic; v1 only guarantees local audit writes.

---

## 9. Trust model and message canonicalization

### 9.1 Trusting canonical source fields

The vault does NOT verify the identity of the message sender beyond what the router provides. Specifically:

- `routing.src_l2_name` is trusted as the L2 name of the sending node.
- `meta.src_ilk` is trusted as the canonical ILK of the sending node/user flow.
- caller tenant and ILK type are resolved from identity SHM using `meta.src_ilk`.

These fields are populated by the router during message canonicalization (the router validates that the actual sender matches the declared identity, based on socket peer information, ICH registration, and identity SHM).

If a node attempts to forge canonical source identity, the router rejects or canonicalizes the message before it reaches the vault.

### 9.2 What this means for security

The vault's security depends on the integrity of the router. If the router is compromised, an attacker can forge identities and the vault's authorization is bypassed.

This is consistent with how other SY nodes (identity, opa-rules, wf-rules) operate. The router is a trusted component of the system.

For the threat model where this matters (attacker wants to escalate by forging a different identity), the protection is at the router level, not at the vault level.

### 9.3 No additional authentication in vault

The vault does not maintain its own list of authorized nodes. It does not issue session tokens, API keys, or any form of node-level credential. The L2 message protocol IS the authentication mechanism.

---

## 10. Boot sequence

```
1. Acquire lock at /var/run/fluxbee/sy-vault.lock
   (single instance per host)

2. Load master key from /etc/fluxbee/vault.master.key
   - If file does not exist: generate 32 random bytes, write file with 0600
   - If file exists: read 32 bytes
   - If file is wrong size or permissions are too open: error and exit

3. Open SQLite at /var/lib/fluxbee/vault.db
   - If file does not exist: create with schema
   - If file exists: validate schema version, migrate if needed

4. Connect to router via L2 SDK
   - Vault registers as SY.vault@<hive>
   - Vault is a system-type ILK

5. Begin processing L2 messages
```

The vault is operational immediately after step 5. Orchestrator manages it as a normal SY service. Other nodes do not block on vault readiness: they should start in their normal degraded/unconfigured state and begin functioning when their required secrets become available in vault and their reload/retry path succeeds.

`sy-vault` uses the same service user and ownership model as the existing SY services. v1 does not introduce a dedicated `sy-vault` Unix user.

---

## 11. Configuration

The vault has minimal configuration. Defaults are sane.

```json
{
  "database_path": "/var/lib/fluxbee/vault.db",
  "master_key_path": "/etc/fluxbee/vault.master.key",
  "lock_path": "/var/run/fluxbee/sy-vault.lock",
  "audit_log_max_size_mb": 1024,
  "audit_log_warn_size_mb": 512
}
```

`audit_log_warn_size_mb`: when audit log file exceeds this, vault logs a warning. No automatic action.

`audit_log_max_size_mb`: when exceeded, vault logs a critical warning. Operator should archive.

---

## 12. SDK helpers

### 12.1 Rust SDK

```rust
/// Get a secret value.
pub async fn vault_get(
    sender: &NodeSender,
    key: &str,
) -> Result<VaultGetResponse, SdkError>;

/// Set or update a secret.
pub async fn vault_put(
    sender: &NodeSender,
    key: &str,
    value: serde_json::Value,
    metadata: VaultMetadata,
) -> Result<VaultPutResponse, SdkError>;

/// List secrets matching a filter.
pub async fn vault_list(
    sender: &NodeSender,
    filter: VaultFilter,
) -> Result<VaultListResponse, SdkError>;

/// Delete a secret.
pub async fn vault_delete(
    sender: &NodeSender,
    key: &str,
) -> Result<VaultDeleteResponse, SdkError>;

/// Rotate a secret value.
pub async fn vault_rotate(
    sender: &NodeSender,
    key: &str,
    value: serde_json::Value,
) -> Result<VaultRotateResponse, SdkError>;

/// Rollback to previous version.
pub async fn vault_rollback(
    sender: &NodeSender,
    key: &str,
) -> Result<VaultRollbackResponse, SdkError>;
```

### 12.2 Go SDK

```go
func VaultGet(sender *Sender, key string) (*VaultGetResponse, error)
func VaultPut(sender *Sender, key string, value interface{}, metadata VaultMetadata) (*VaultPutResponse, error)
func VaultList(sender *Sender, filter VaultFilter) (*VaultListResponse, error)
func VaultDelete(sender *Sender, key string) (*VaultDeleteResponse, error)
func VaultRotate(sender *Sender, key string, value interface{}) (*VaultRotateResponse, error)
func VaultRollback(sender *Sender, key string) (*VaultRollbackResponse, error)
```

Both SDKs send the corresponding L2 message to `SY.vault@<hive>` and parse the response.

### 12.3 Vault references

Secret-bearing node config should not carry plaintext once vault is enabled. It should carry vault references:

```json
{
  "secrets": {
    "openai": {
      "api_key_ref": "vault://sys:openai-api-key"
    }
  }
}
```

Reference format is intentionally simple in v1:

```text
vault://<key>
```

The string after `vault://` is the vault key exactly as stored in `secrets.key`.

### 12.4 Retry helpers

SDK consumers need a standard way to wait for a secret that is not loaded yet. The SDK should provide a bounded retry helper:

```rust
pub async fn vault_get_with_retry(
    sender: &NodeSender,
    key: &str,
    policy: VaultRetryPolicy,
) -> Result<VaultGetResponse, SdkError>;
```

Initial retry policy:

- max elapsed: `60s`;
- initial delay: `250ms`;
- max delay: `5s`;
- jitter: `20%`;
- retry on `VAULT_UNAVAILABLE`, timeout, and `KEY_NOT_FOUND`;
- do not retry on `UNAUTHORIZED`, `INVALID_KEY_FORMAT`, or `INVALID_VAULT_REF`.

The first runtime to validate this behavior should be `SY.architect`: it should start without its OpenAI secret, retry/read from vault when configured, and move from missing-secret to configured without requiring a special orchestrator sequence.

### 12.5 Caching considerations

The SDK helpers do NOT cache responses by default. Each call is a fresh L2 round-trip.

If a node needs to cache a secret (e.g., AI node loads OpenAI key once at boot), it does so explicitly in its own logic. The vault does not provide caching directives.

If a secret is rotated, cached copies become stale. The node responsible for the secret's lifecycle is responsible for refreshing the cache (e.g., re-querying periodically, or being notified via L2 broadcast — not in v1).

---

## 13. Secret write flow and node migration

Vault replaces node-local secret writes. Once vault is enabled, new secret-bearing configuration should use this flow:

1. The node exposes required secret fields through its existing `CONFIG_GET` contract.
2. `SY.admin` receives a secret-bearing config update from Archi/operator.
3. `SY.admin` writes plaintext secret values to `SY.vault` via `VAULT_PUT`.
4. `SY.admin` stores or forwards only non-secret config plus `vault://...` references.
5. The node resolves the reference using SDK `vault_get` / `vault_get_with_retry`.
6. If vault or the secret is not available, the node remains degraded/unconfigured and retries according to its runtime policy.

Nodes should not persist new plaintext secrets to local `secrets.json`. Existing `secrets.json` support is legacy state and can be removed/migrated during this alpha iteration.

The first end-to-end consumer for retry/read behavior is `SY.architect`: it should be able to start without its OpenAI secret, accept vault-backed configuration through admin, and become configured once `vault_get_with_retry` succeeds.

---

## 14. Future: cross-hive TCP encryption (not part of vault)

### 14.1 The problem

Today, routers in different hives communicate via plain TCP. Messages between hives travel without encryption. An attacker with network access between hives can capture traffic, including messages addressed to the vault from cross-hive nodes.

This is NOT a vault concern. The vault operates at the L2 message layer, which is already abstracted from transport. The encryption (or lack thereof) of the underlying transport is the router's responsibility.

### 14.2 Planned solution (not in v1)

A future enhancement to the router will add TLS to inter-hive TCP connections:

- Each router gets a TLS certificate identifying its hive.
- Routers in the same Fluxbee installation share a CA (or trust each other's certs explicitly).
- Cross-hive connections use mTLS (both router and peer authenticate).
- Local Unix sockets remain unencrypted (host boundary is the trust boundary).

This work is independent of the vault and benefits all cross-hive communication, not just secret-related messages.

### 14.3 Scope of this work for the vault

When the router gains cross-hive TLS, the vault automatically benefits without any changes. The vault still receives L2 messages from the router; the router handles transport security transparently.

The vault does not need to be aware of which transport its clients used.

### 14.4 Estimated timeline

Cross-hive TLS for routers is a follow-up project after the vault is operational. Tentative:

- Phase 1 (current): vault implementation, no cross-hive TLS, single-hive deployments.
- Phase 2 (future): router gains cross-hive TLS, multi-hive deployments become production-ready.

There is no hard deadline. The work is triggered when multi-hive production deployments become a real need.

---

## 15. Error codes

| Code | Description |
|---|---|
| `UNAUTHORIZED` | Caller is not authorized for this operation on this key |
| `KEY_NOT_FOUND` | Key does not exist (for operations where this is differentiated from UNAUTHORIZED) |
| `KEY_EXISTS` | Key already exists (for operations that require new) |
| `INVALID_KEY_FORMAT` | Key does not match `^[a-z0-9][a-z0-9:_-]{0,255}$` |
| `INVALID_VALUE` | Value is not valid JSON or is too large (>1 MB) |
| `INVALID_METADATA` | Metadata is missing required fields (`tenant_id`, `owner_ilk`) |
| `INVALID_VAULT_REF` | Vault reference string is malformed |
| `VAULT_UNAVAILABLE` | Vault service is not reachable or timed out |
| `NO_PREVIOUS_VERSION` | VAULT_ROLLBACK called on a secret with no previous version |
| `STORAGE_ERROR` | SQLite error |
| `ENCRYPTION_ERROR` | AES-GCM error (should not happen in normal operation) |
| `MASTER_KEY_NOT_AVAILABLE` | Vault could not load the master key (boot failure) |

For VAULT_GET and VAULT_LIST, denied access returns `UNAUTHORIZED` regardless of whether the key exists. This prevents enumeration attacks.

---

## 16. Implementation checklist

```
[ ] Generate master key on first boot if not present
[ ] Load master key with permission validation (must be 0600)
[ ] Open SQLite, create schema if needed
[ ] Implement AES-256-GCM encrypt/decrypt for values
[ ] Implement nonce generation (12 bytes random per encryption)
[ ] Connect to router via L2 SDK as SY.vault@<hive>
[ ] Implement VAULT_PUT handler
[ ] Implement VAULT_GET handler
[ ] Implement VAULT_LIST handler with filter
[ ] Implement VAULT_DELETE handler
[ ] Implement VAULT_ROTATE handler
[ ] Implement VAULT_ROLLBACK handler
[ ] Implement VAULT_GET_METADATA handler
[ ] Authorization logic per §7
[ ] Audit log writes for every operation (success, noop, denial, error)
[ ] Update last_accessed_at and access_count on successful GETs
[ ] Idempotency for PUT (same value -> no version increment, audit as noop)
[ ] Lock file to prevent multiple instances
[ ] Graceful shutdown (close SQLite cleanly)
[ ] Boot validation: master key file permissions, db file permissions

[ ] Rust SDK: vault_get, vault_put, vault_list, vault_delete, vault_rotate, vault_rollback
[ ] Rust SDK: vault_get_with_retry and vault_ref parsing helpers
[ ] Go SDK: same surface
[ ] SDK error handling with typed errors
[ ] SY.architect: use vault-backed secret read/retry for OpenAI key as first consumer

[ ] Audit log size monitoring
[ ] Documentation of operator procedures (init, backup, restore)
[ ] Initial reference plans / examples for common secrets
```

---

## 17. Operator procedures

### 17.1 Initialization

On first deploy, no special steps. Vault generates its master key and database on first boot.

```bash
# Vault binary installed at /usr/bin/sy-vault
# Configuration default at /etc/fluxbee/sy-vault.conf

systemctl start sy-vault
# or
/usr/bin/sy-vault
```

### 17.2 Backup

To backup the vault, copy two files:

```bash
sudo cp /var/lib/fluxbee/vault.db /backup/vault.db.YYYY-MM-DD
sudo cp /etc/fluxbee/vault.master.key /backup/vault.master.key.YYYY-MM-DD
```

**Both files are required.** The database is useless without the master key.

Backup locations should have similar or stricter permissions than the originals.

### 17.3 Restore

```bash
systemctl stop sy-vault
sudo cp /backup/vault.db.YYYY-MM-DD /var/lib/fluxbee/vault.db
sudo cp /backup/vault.master.key.YYYY-MM-DD /etc/fluxbee/vault.master.key
# Ownership must match the same service user/group used by the other SY services.
sudo chown <sy-service-user>:<sy-service-group> /var/lib/fluxbee/vault.db
sudo chmod 0600 /etc/fluxbee/vault.master.key
systemctl start sy-vault
```

### 17.4 Audit log inspection

```bash
sqlite3 /var/lib/fluxbee/vault.db
> SELECT * FROM audit_log WHERE caller_ilk = 'ilk:suspect-node' ORDER BY timestamp DESC LIMIT 100;
> SELECT * FROM audit_log WHERE result = 'denied' ORDER BY timestamp DESC LIMIT 50;
> SELECT operation, COUNT(*) FROM audit_log GROUP BY operation;
```

### 17.5 Audit log archival

When the audit log grows too large:

```bash
sqlite3 /var/lib/fluxbee/vault.db <<EOF
.headers on
.mode csv
.output /backup/audit-2026-04.csv
SELECT * FROM audit_log WHERE timestamp < '2026-05-01';
DELETE FROM audit_log WHERE timestamp < '2026-05-01';
VACUUM;
EOF
```

### 17.6 Alpha reset

In the alpha development cycle, `fluxbee_cleanall.sh` full clean/reset may delete both:

- `/var/lib/fluxbee/vault.db`
- `/etc/fluxbee/vault.master.key`

This intentionally destroys vault contents. The next vault boot generates a new database and master key.

---

## 18. Decisions

| Decision | Rationale |
|---|---|
| Embedded SQLite | Simple, transactional, single-file, dev team familiarity |
| AES-256-GCM | Industry-standard authenticated encryption |
| Master key in filesystem | Tradeoff for simplicity; relies on OS-level access control |
| L2 only, no HTTP | Consistent with all other Fluxbee nodes |
| Trust router canonicalization | Same trust model as identity and opa-rules |
| Read identity SHM for auth | Current L2 messages carry `meta.src_ilk`, not `src_tenant_id`; SHM resolves tenant/type locally |
| No mTLS / certificates in v1 | Vault doesn't have direct connections; canonicalization is sufficient |
| Hardcoded authorization in v1 | OPA integration deferred; logic is straightforward enough to hardcode |
| Audit log in same SQLite | Transactional with operations; single file to backup |
| No NATS audit persistence | Avoids dependency/circularity; vault remains locally auditable even if other services are degraded |
| Versioning: current + previous | Simple rollback; full history available via audit log |
| Encryption at rest only | Channel encryption is router's responsibility |
| Master key at first boot | No external setup; vault is self-bootstrapping |
| Admin-centralized writes | Secret writes are centralized in `SY.admin -> SY.vault`; nodes consume through SDK helpers |
| Deprecated node-local secret writes | `secrets.json` remains legacy/cleanup concern, not the new write path |
| Same SY service user model | Avoids a new permissions branch in v1; dedicated user can be revisited for production hardening |
| Alpha reset deletes DB/key | Current test workflow recreates the full platform from scratch |
| No audit read API in v1 | v1 only writes local audit; integrated system audit is a later cross-system topic |
| SDK retry defaults fixed | `60s` max elapsed, `250ms` initial delay, `5s` max delay, `20%` jitter |
| No HSM/KMS in v1 | Adds operational complexity; not justified for piloto |
| Cross-hive TLS deferred to router | Vault is transport-agnostic; router handles network security |

---

## 19. References

| Topic | Document |
|---|---|
| Identity model | `10-identity-v2.md` |
| System architecture | `01-arquitectura.md` |
| OPA rules | `go/sy-opa-rules/main.go` |
| L2 message protocol | core router specification |
| Executor pilot (uses vault) | `executor_manifest_pilot_spec.md` |

---

## 20. What is NOT in v1

- TLS / mTLS (handled at router level, future)
- Certificate-based authentication
- HSM / KMS integration
- Dynamic secrets (auto-generated credentials)
- Automatic secret rotation
- Lease / TTL on secrets
- Secret sharing or access delegation between ILKs
- Audit log signing for tamper-evidence
- Hierarchical key paths (Vault-style `/secret/data/...`)
- OPA integration (planned)
- L2/admin verb to query audit log
- Automatic audit log rotation
- ~~Hot-reload notification on secret change~~ — **IMPLEMENTED** (moved out of this list): `sy_vault.rs` broadcasts `VAULT_SECRET_CHANGED` on put/rotate/delete/rollback plus a bootstrap fan-out, so consumers are woken rather than only polling
- Multi-instance / replication
- Cross-hive vault federation
- Support for new node-local plaintext `secrets.json` writes once vault is enabled
