# SY.vault — Model D' (resource-oriented secrets)

**Status:** design closed and implemented as the active model. Replaces the old "vault_put + CONFIG_SET with ref" approach from Phase J.

**Parent spec:** `docs/sy-vault-spec.md` (vault backbone — message envelope, encryption, audit, etc. remains valid).

---

## 1. Why this design exists

Phase J as implemented forced the operator to do two coordinated operations to set a secret on a node:

1. `vault_put` (curl, with the operator picking key name and `owner_ilk`).
2. `CONFIG_SET` to the node with the vault ref string.

This had three real problems:

- The operator had to know vault key naming conventions and node ILK UUIDs to load a secret.
- Sharing a single OpenAI/Postgres secret across N system nodes forced N near-duplicate puts (one per `owner_ilk`), each holding the same plaintext under a different key.
- The `owner_ilk` field assumed every secret belongs to one identity, which is the wrong shape for workspace-level shared resources (the dominant case in alpha).

Model D' treats secrets as **typed resources** that may be either dedicated to one identity or shared in a per-tenant pool. The reader matches by resource type; the writer doesn't need to know the consumer's ILK.

---

## 2. Secret schema

```text
struct VaultSecret {
    // identifier
    key: String,                    // operator-chosen label, unique per (tenant_id)
    value: Value,                   // plaintext payload, AES-256-GCM at rest

    metadata: VaultMetadata {
        resource_type: String,      // REQUIRED. canonical normalized form (see §5)
        tenant_id: String,          // REQUIRED. always "tnt:<uuid>". Infra-wide secrets live in the fixed root tenant `tnt:00000000-0000-0000-0000-000000000001` (alias "fluxbee").
        ilk: Option<String>,        // null = pool, "ilk:<uuid>" = dedicated
        description: Option<String>,
        tags: Vec<String>,
        created_by: Option<String>, // operator label (informational only)
        created_at: String,         // vault-managed
        updated_at: String,         // vault-managed
        version: i64,               // vault-managed, monotonically increments on rotate
    }
}
```

### Field rules

- **`key`**: free-form label chosen by the operator (e.g. `"openai-prod-shared"`, `"cognition-openai-dedicated"`). Unique per `(tenant_id, key)`. Conflict → `KEY_ALREADY_EXISTS`. Used as the addressable identifier for `vault_delete`, `vault_rotate`, `vault_rollback`.

- **`resource_type`**: REQUIRED on PUT. Canonical normalized form: lowercase, words joined by `_`. Examples: `openai`, `anthropic`, `postgres`, `google_calendar`, `slack`, `hubspot`. Admin normalizes operator-provided strings before forwarding to vault. See §5 for the fixed SDK enum.

- **`tenant_id`**: REQUIRED. Always `tnt:<uuid>` — there is no `sys` sentinel. Infrastructure-wide / hive-level resources live in the fixed root tenant `tnt:00000000-0000-0000-0000-000000000001` (alias `fluxbee`, seeded by SY.identity at bootstrap). Per-tenant resources use that tenant's own `tnt:<uuid>`. Operator may omit `tenant_id` on PUT and admin defaults to the root tenant. Tenant association is part of the cost model — every secret belongs to a tenant.

- **`ilk`**: OPTIONAL. If set, the secret is **dedicated** to that ILK; only that ILK can read its plaintext. If `null`, the secret is in the **pool** of the tenant and any caller of the same tenant can read it.

- **`version`**: vault-managed. Starts at 1 on first put. Increments on `vault_rotate` (which preserves history for one previous version). PUT with same value is a noop and does not increment.

- **`(resource_type, tenant_id, ilk)` is NOT unique.** Multiple secrets with the same triple are allowed: same resource, same tenant, same dedication. Disambiguation on read is by `created_at DESC` (most recent wins). This supports pre-loaded rotation: operator adds a new key with the same triple, and consumers pick it up on boot or after the matching `VAULT_SECRET_CHANGED` broadcast. The old key stays until the operator deletes it.

---

## 3. Operator workflow (write path)

Single endpoint, single payload. Operator never touches consumer node config for secrets.

```bash
curl -X POST http://127.0.0.1:8080/hives/motherbee/vault/secrets \
  -H 'Content-Type: application/json' \
  -d '{
    "key": "openai-prod-shared",
    "value": {"api_key": "sk-..."},
    "metadata": {
      "resource_type": "openai",
      "tenant_id": "tnt:00000000-0000-0000-0000-000000000001"
    }
  }'
```

That single put publishes an OpenAI key in the root-tenant pool. Every system node in the hive that needs OpenAI will pick it up on boot or after the `VAULT_SECRET_CHANGED` broadcast. No CONFIG_SET to architect, cognition, or admin is needed. (If `tenant_id` is omitted, admin defaults to the fixed root tenant.)

If the operator wants to dedicate it to one node, they add `"owner_node": "SY.cognition"` and admin resolves it to the deterministic ILK before forwarding to vault. Example:

```json
"metadata": {
    "resource_type": "openai",
    "tenant_id": "tnt:00000000-0000-0000-0000-000000000001",
    "owner_node": "SY.cognition"
}
```

Admin computes `deterministic_system_ilk_id("SY.cognition@<hive>")` and sets `metadata.ilk = "ilk:<uuid>"` before forwarding. `owner_node` is admin-side syntactic sugar; vault only sees `metadata.ilk`.

### What the operator never has to provide

- ILK UUIDs (resolved by admin when `owner_node` is given, or omitted for pool)
- Vault key naming conventions for the consumer (operator picks any label)
- Which nodes will use this secret (consumers discover; not in metadata)
- A separate CONFIG_SET to the consumer node

### `vault_put` authorization

Only `SY.admin` and `SY.architect` (resolved by deterministic ILK match, see §6) can call `VAULT_PUT`. Operator humans always reach vault through admin's HTTP path or through archi. Direct vault PUT from a generic node is rejected.

---

## 4. Consumer workflow (read path)

The consumer node knows in its own code which resource types it needs. At boot and whenever it receives a matching `VAULT_SECRET_CHANGED` broadcast, it queries vault for each resource type.

### Required resources declaration (in node code)

Each node hardcodes a constant listing its required resources and the local config field they feed:

```rust
const REQUIRED_RESOURCES: &[(ResourceType, &str)] = &[
    (ResourceType::OpenAi, "ai_providers.openai.api_key"),
    (ResourceType::Postgres, "storage.postgres_url"),
];
```

This is part of the node's source code, not a config or hive.yaml entry. The programmer who codes the node knows what providers it talks to.

### Match algorithm (per resource type)

```text
fn resolve(resource_type, my_ilk, my_tenant):
    // (1) Dedicated to me?
    candidates = vault_list(
        resource_type = resource_type,
        ilk = my_ilk,
        tenant_id = my_tenant,
    ).sort_by(created_at DESC)
    if !candidates.empty():
        return candidates[0]

    // (2) In the pool of my tenant?
    candidates = vault_list(
        resource_type = resource_type,
        ilk = null,
        tenant_id = my_tenant,
    ).sort_by(created_at DESC)
    if !candidates.empty():
        return candidates[0]

    // (3) Degraded
    return None
```

Two separate `vault_list` calls (clearer code), one round-trip each. For alpha this is fine; no need for a single composite query.

### Refresh policy

- At boot: resolve once for every entry in `REQUIRED_RESOURCES`. If any returns None, the node logs and continues in degraded mode for that capability.
- Refresh/change notification: `SY.vault` emits `VAULT_SECRET_CHANGED` after a mutating operation. Consumers filter the broadcast by `resource_type`, tenant, and optional `ilk`, then re-resolve the resource.
- On change: cached secret is replaced atomically when possible. Nodes with connection pools that are not hot-swappable exit cleanly and let systemd restart them.

### Reporting (CONFIG_GET)

The node's CONFIG_GET response includes:

```json
{
  "resources": {
    "openai": {
      "resolved": true,
      "source": "pool",
      "vault_key": "openai-prod-shared",
      "version": 3,
      "resolved_at": "2026-05-12T20:00:00Z"
    },
    "postgres": {
      "resolved": false,
      "source": null,
      "error": "no secret with resource_type=postgres and (ilk=ilk:<self> or pool) in tenant tnt:00000000-0000-0000-0000-000000000001"
    }
  }
}
```

This is the operator-facing diagnostic. If the node is degraded, the operator inspects CONFIG_GET to see which resource is missing. The format is simple in alpha; if archi (an AI) needs richer multi-check correlation later, we add fields.

---

## 5. ResourceType enum (SDK)

Defined in `fluxbee_sdk::vault::ResourceType`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    OpenAi,         // -> "openai"   (special-cased: drop separator)
    Anthropic,      // -> "anthropic"
    Gemini,         // -> "gemini"
    Mistral,        // -> "mistral"
    Cohere,         // -> "cohere"
    Perplexity,     // -> "perplexity"
    Postgres,       // -> "postgres"
    Mysql,          // -> "mysql"
    Redis,          // -> "redis"
    Mongodb,        // -> "mongodb"
    GoogleCalendar, // -> "google_calendar"
    GoogleDrive,    // -> "google_drive"
    GoogleSheets,   // -> "google_sheets"
    GoogleDocs,     // -> "google_docs"
    GoogleSlides,   // -> "google_slides"
    GoogleCloud,    // -> "google_cloud"
    Gmail,          // -> "gmail"
    MicrosoftGraph, // -> "microsoft_graph"
    OutlookEmail,   // -> "outlook_email"
    OutlookCalendar,// -> "outlook_calendar"
    Teams,          // -> "teams"
    Sharepoint,     // -> "sharepoint"
    Slack,          // -> "slack"
    Discord,        // -> "discord"
    Hubspot,        // -> "hubspot"
    Salesforce,     // -> "salesforce"
    LinkedHelper,   // -> "linked_helper"
    Github,         // -> "github"
    Gitlab,         // -> "gitlab"
    Jira,           // -> "jira"
    Linear,         // -> "linear"
    Notion,         // -> "notion"
    Stripe,         // -> "stripe"
    Twilio,         // -> "twilio"
    Sendgrid,       // -> "sendgrid"
    Smtp,           // -> "smtp"
    Imap,           // -> "imap"
    Aws,            // -> "aws"
    Azure,          // -> "azure"
    S3,             // -> "s3"
    Webhook,        // -> "webhook"
    BearerToken,    // -> "bearer_token"
    ApiKey,         // -> "api_key"
    OAuthBundle,    // -> "oauth_bundle"
    Custom(String), // -> stored as the inner string (must already be normalized)
}
```

- The SDK enum lets known types be type-checked.
- Custom variant is an escape hatch for a provider that has not been promoted to the fixed enum yet. Common providers/resources should use one of the variants above, not arbitrary strings.
- Normalization rule (`normalize_resource_type(s) -> String`): trim, lowercase, replace runs of non-alphanumeric chars with single `_`, drop leading/trailing `_`. Examples: `"OpenAI"` → `"openai"`, `"Google Calendar"` → `"google_calendar"`, `"linked-helper"` → `"linked_helper"`. Admin normalizes before forwarding to vault. Vault stores the normalized string and rejects empty / pure-digit / over-N-chars.

### Per-resource value contract

Each `resource_type` has a documented `value` shape so consumers know what to expect. Critical: the secret carries **only what is actually secret** — implementation knobs that any consumer can derive (database name, target table, retry policy) live in the consumer's code, not in the secret.

| `resource_type` | `value` shape | Notes |
| --------------- | ------------- | ----- |
| `postgres` | `"postgresql://USER:PASS@HOST:PORT"` (string, no dbname) or `{"host", "port", "user", "password"}` (object form, future) | **No dbname**. Each consumer hardcodes its own (`fluxbee_storage`, `fluxbee_identity`, etc.) and applies `with_dbname` before connecting. The operator loads ONE postgres secret per cluster — storage, identity, architect, cognition share it. If the operator includes a dbname out of habit, consumers log a warning and overwrite. |
| `openai` | `{"api_key": "sk-..."}` or bare string `"sk-..."` | Provider-level shared secret. |
| `anthropic` | same shape as openai | |
| `google_calendar`, `gmail`, `google_drive`, `google_sheets`, `google_docs`, `google_slides`, `google_cloud` | `{"access_token", "refresh_token", "client_id", "client_secret"}` or provider-specific service-account bundle | per-account if `ilk` is set; per-tenant if pool |
| `microsoft_graph`, `outlook_email`, `outlook_calendar`, `teams`, `sharepoint` | OAuth bundle | per-account if `ilk` is set; per-tenant if pool |
| `slack` | `{"bot_token", "app_token", "signing_secret"}` | workspace-level (pool) or per-user (ilk) |
| `discord` | `{"bot_token": "...", "webhook_url": "..."}` depending on integration | |
| `hubspot`, `salesforce`, `linked_helper` | per-provider object | |
| `github`, `gitlab`, `jira`, `linear`, `notion` | OAuth bundle or provider token object | |
| `stripe`, `twilio`, `sendgrid` | provider API key/token object | |
| `smtp`, `imap` | `{"host", "port", "user", "password"}` | |
| `aws`, `azure`, `google_cloud`, `s3` | cloud credential bundle | |
| `webhook`, `bearer_token`, `api_key`, `oauth_bundle` | generic integration shapes for cases that are intentionally not provider-specific | |

Rule of thumb when introducing a new `resource_type`: ask "would two consumers in the same tenant reasonably share this value?" If yes → that field belongs in the secret. If no (it's implementation-specific) → the consumer hardcodes or configures it via CONFIG_SET.

---

## 6. Authorization rules

`SY.vault` does **not** trust `routing.src_l2_name` strings. It computes a fixed set of well-known administrative ILKs at boot and authorizes by ILK equality.

### Well-known administrative ILKs

Vault, at boot, computes once and caches:

```rust
let admin_ilk     = deterministic_system_ilk_id(format!("SY.admin@{hive}"));
let architect_ilk = deterministic_system_ilk_id(format!("SY.architect@{hive}"));
// identity is NOT in the admin set — it does not need privileged operations
let well_known_admin_ilks = HashSet::from([admin_ilk, architect_ilk]);
```

`deterministic_system_ilk_id` is exposed in `fluxbee_sdk::identity` so both vault and sy_identity use the same source (no code duplication).

### Per-operation rules

| Operation | Authorization |
| --------- | ------------- |
| `vault_get` (plaintext) | `caller.src_ilk == secret.ilk` (dedicated match), **or** `secret.ilk == null` and caller-resolved `tenant_id == secret.tenant_id` (pool match). **No admin bypass — not even archi/admin sees plaintext that is not theirs.** If they wanted a secret back, they shouldn't have lost it; they rotate it. |
| `vault_list` | Open to any caller in the hive. Returns secrets visible to the caller's tenant (caller may not be resolvable yet — see §6.3). No values, only metadata. |
| `vault_get_metadata` | Same as `vault_list`. |
| `vault_put` | `caller.src_ilk in well_known_admin_ilks`. No other caller can write. |
| `vault_delete`, `vault_rotate`, `vault_rollback` | `caller.src_ilk in well_known_admin_ilks` (administrative path), **or** `caller.src_ilk == secret.ilk` (a node may rotate/delete its own dedicated secret). Pool secrets are administered only by admin/architect. |

### Identity at boot (chicken/egg) — solved without override

Identity needs to read its `postgres_url` secret before identity SHM is written (it's the writer). It cannot be resolved by `tenant_id`/`ilk_type` lookup because the lookup table is identity SHM and it's empty.

Resolution under Model D':

1. Operator loads the identity postgres URL with `owner_node: "SY.identity"`. Admin computes `deterministic_system_ilk_id("SY.identity@<hive>")` and sets `metadata.ilk = "ilk:<uuid>"`.
2. Identity at boot computes its own ILK with the same function (no SHM read needed). Sends `vault_get` with `meta.src_ilk = identity_ilk_deterministic`.
3. Vault matches `caller.src_ilk == secret.ilk` by direct string comparison. **No SHM resolution required.**

If vault cannot resolve a caller via SHM (caller's ILK not yet seeded), the rule is: **deny pool reads and deny tenant-based filtering**. Only allow operations where the auth can be resolved by direct ILK equality. This is the general rule; identity at boot is its first beneficiary.

For `vault_list`/`vault_get_metadata` from unresolved callers: allow listing within the declared tenant of the query, but mark `caller_resolved: false` in the audit. No information leak because list/metadata is open by design.

---

## 7. Audit

Audit rows are written for every operation (success, denied, noop, error). Per existing vault spec — unchanged from Phase A-I.

In Model D' the `caller_ilk` field of the audit row matters more than before: for operations done via admin on behalf of a human operator, the `caller_ilk` is admin's ILK; the human's identity is captured separately in `metadata.created_by` (operator-supplied label) and in the future may also be captured in a `human_actor` column. For now, `metadata.created_by` is the operator-facing trace of "who loaded this".

---

## 7bis. Self ILK acquisition (boot-time)

Every node that talks to vault needs to know its own ILK at boot to put into `meta.src_ilk`. The mechanism depends on family:

| Family | Self ILK source |
| ------ | --------------- |
| `SY.identity` | computed in-process via `deterministic_system_ilk_id` (it's the writer of identity SHM) |
| Other `SY.*` system nodes listed in `hive.yaml system_nodes` | deterministic system ILK derived from `node_name`; SHM lookup is no longer required for boot ownership |
| `AI.*`, `IO.*`, `WF.*` (dynamically spawned via orchestrator) | `FLUXBEE_NODE_ILK_ID` env var injected by orchestrator's `systemd-run` after a successful `ILK_REGISTER` to identity. SDK helper `read_self_ilk_from_env()` reads it at boot |
| `sy.orchestrator` | does not have a self ILK; doesn't interact with vault for its own behalf |

The orchestrator already calls `ILK_REGISTER` on every `run_node` for non-system instances and persists `node_name → ilk_id` locally. The env-var injection is the missing step that lets the spawned node read its own ILK without an extra round-trip to identity. Implementation lives in Phase J'-0a (`docs/onworking COA/sy_vault_tasks.md`).

`sy.frontdesk.gov` is a special case: it's listed as a system node in `hive.yaml` but the binary is derived from the AI runner. It now receives/caches the deterministic system ILK through the SY alignment shim tracked in Phase J'-0b.

---

## 8. Open/deferred items

These remain deferred after the Model D' implementation:

- **Load balancing across pool secrets**. Multiple secrets with the same `(resource_type, tenant, ilk=null)` exist by design (rotation pre-loading, multi-key) but the read picks the most recent. True load-distribution across multiple usable keys is a future addition, not alpha.
- **Push-based change notification is implemented.** `VAULT_SECRET_CHANGED` is the current mechanism; remaining work is unit/E2E coverage, not design.
- **Per-user / per-agent secrets (`usr:<ilk>` namespace)**. When humans/agents acquire their own tokens. This is exactly the use case where `metadata.ilk` set per identity is the right model. Out of scope for alpha.
- **Cross-hive secret federation**. Vault is per-hive in alpha.

---

## 9. What changed vs the removed Phase J

Phase J (J1-J5) had nodes accepting CONFIG_SET with `*_ref` strings and persisting them in local `secrets.json`. Under Model D' the nodes:

- Do **not** receive ref strings via CONFIG_SET.
- Do **not** persist any vault ref locally. `secrets.json` for secrets goes away.
- At boot and on `VAULT_SECRET_CHANGED`, query vault directly using `REQUIRED_RESOURCES` declared in code.

CONFIG_SET still exists for **non-secret configuration** (default models, timeouts, thresholds, catalog mode, etc.). The split is: vault is the only home for secret values; CONFIG_SET handles everything else.

What's preserved from current code:

- `VaultCaller`, `VaultRetryPolicy`, `ResourceType`, and `resolve_resource` in the SDK.
- `wait_for_self_system_ilk_id` boot-time helper.
- `deterministic_system_ilk_id` — moved from `sy_identity.rs` into `fluxbee_sdk::identity` so vault can use it.
- The vault L2 message envelope (`VAULT_PUT`, `VAULT_GET`, etc.) — payload shapes change slightly (no more `owner_ilk` required, new `resource_type`, etc.).

What was removed/rewritten:

- All node-side persistence of `*_ref` keys in `secrets.json` (J1-J5 added these — they go).
- `extract_*_api_key_ref` / `extract_*_postgres_url_ref` helpers (refs no longer in CONFIG_SET).
- `reject_*_plaintext` helpers in CONFIG_SET (CONFIG_SET no longer has secret-bearing fields at all, so there's nothing to reject).
- The `is_admin()` L2-name check in vault — replaced by well-known ILK set comparison.
- Phase K (IO nodes) tasks need to be rewritten to use Model D'.

The active closeout plan is in `sy_vault_tasks.md`.
