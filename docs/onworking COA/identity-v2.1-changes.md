# Fluxbee — Identity v2.1 Changes

**Status:** v2.1 draft — ready for implementation
**Date:** 2026-04-16
**Audience:** SY.identity developer, router/SHM developer, SDK maintainers
**Base:** `SY.identity v2.0` (current production code in `sy_identity.rs`)
**Scope:** three additive changes, no removals, no breaking protocol changes, SHM full rebuild (no migration)

---

## 1. Summary of Changes

| Change | Layer | Purpose |
|---|---|---|
| `sponsor_tenant_id` on Tenant | DB + in-memory + SHM + protocol | Model tenant hierarchy: who created/administers this tenant |
| `owner_l2_name` on ICH | DB + in-memory + SHM + protocol | Which IO node owns this channel (L2 full name) |
| `enabled` on ICH | DB + in-memory + SHM + protocol + SDK | Enable/disable a channel without destroying it |

All three changes follow the same propagation pattern: Rust struct → PostgreSQL column → SHM entry → sync delta (automatic via JSON serialization).

SHM layout changes require a full rebuild of the identity SHM region. No migration — delete and recreate. All consuming nodes (router, admin, OPA, etc.) must be recompiled against the new SHM structs.

---

## 2. Change 1: `sponsor_tenant_id` on Tenant

### 2.1 What it models

A tenant can be sponsored by another tenant. The sponsor is the tenant that created and administers this one. This models the hierarchy:

```
tnt:fluxbee-infra (root, sponsor=NULL — the infrastructure tenant)
  ├── sponsor of → tnt:admin-cesar (admin registered, sponsors his clients)
  │   ├── sponsor of → tnt:acme-corp (client of admin Cesar)
  │   └── sponsor of → tnt:beta-corp (another client)
  └── sponsor of → tnt:admin-maria (another admin)
```

The root tenant (sponsor=NULL) is the infrastructure tenant. In self-hosted, it's the default `fluxbee` tenant. In cloud, it's the platform operator's tenant.

### 2.2 Rust struct changes

**`TenantRecord`** (line 264):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TenantRecord {
    tenant_id: String,
    name: String,
    domain: Option<String>,
    status: String,
    settings: Value,
    sponsor_tenant_id: Option<String>,  // NEW
}
```

**`TntCreateRequest`** (line 246):

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TntCreateRequest {
    name: String,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    settings: Option<Value>,
    #[serde(default)]
    sponsor_tenant_id: Option<String>,  // NEW
}
```

### 2.3 PostgreSQL schema change

```sql
ALTER TABLE identity_tenants
    ADD COLUMN sponsor_tenant_id UUID REFERENCES identity_tenants(tenant_id);

CREATE INDEX IF NOT EXISTS idx_identity_tenants_sponsor
    ON identity_tenants(sponsor_tenant_id)
    WHERE sponsor_tenant_id IS NOT NULL;
```

Self-referencing FK. NULL for root tenants.

### 2.4 SHM change

**`TenantEntry`** — add 16 bytes for sponsor UUID:

```rust
#[repr(C)]
pub struct TenantEntry {
    pub tenant_id: [u8; 16],
    pub name: [u8; 128],
    pub domain: [u8; 128],
    pub status: u8,
    pub flags: u8,
    pub _pad0: [u8; 5],
    pub max_ilks: u32,
    pub sponsor_tenant_id: [u8; 16],  // NEW — all zeros if no sponsor
    pub created_at: u64,
    pub updated_at: u64,
}
```

When `sponsor_tenant_id` is NULL in DB, the SHM field is `[0u8; 16]`. Readers interpret all-zeros as "no sponsor" (same pattern as null UUIDs elsewhere in SHM).

### 2.5 `tenant_entry_from_record` change (line 2340)

```rust
fn tenant_entry_from_record(tenant: &TenantRecord) -> Result<TenantEntry, IdentityError> {
    let tenant_uuid = parse_prefixed_uuid(&tenant.tenant_id, "tnt")?;
    let now_ms = now_epoch_ms();

    // NEW: parse sponsor if present
    let sponsor_bytes: [u8; 16] = match &tenant.sponsor_tenant_id {
        Some(sponsor_id) => {
            let sponsor_uuid = parse_prefixed_uuid(sponsor_id, "tnt")?;
            *sponsor_uuid.as_bytes()
        }
        None => [0u8; 16],
    };

    let mut entry = TenantEntry {
        tenant_id: *tenant_uuid.as_bytes(),
        name: [0u8; 128],
        domain: [0u8; 128],
        status: parse_tenant_status_for_shm(&tenant.status),
        flags: FLAG_ACTIVE,
        _pad0: [0u8; 5],
        max_ilks: tenant.settings.get("max_ilks")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        sponsor_tenant_id: sponsor_bytes,  // NEW
        created_at: now_ms,
        updated_at: now_ms,
    };
    copy_bytes_with_len(&mut entry.name, tenant.name.trim());
    // ... rest unchanged
    Ok(entry)
}
```

### 2.6 `create_tenant` changes (line 779)

Add validation that sponsor exists if specified:

```rust
fn create_tenant(&mut self, req: TntCreateRequest) -> Result<Value, String> {
    validate_non_empty("name", &req.name)?;

    // NEW: validate sponsor exists
    if let Some(ref sponsor_id) = req.sponsor_tenant_id {
        let _ = parse_prefixed_uuid(sponsor_id, "tnt")?;
        if !self.tenants.contains_key(sponsor_id) {
            return Err("INVALID_SPONSOR_TENANT".to_string());
        }
    }

    // ... existing name/domain normalization ...

    let tenant_id = format!("tnt:{}", Uuid::new_v4());
    self.tenants.insert(
        tenant_id.clone(),
        TenantRecord {
            tenant_id: tenant_id.clone(),
            name: normalized_name,
            domain: normalized_domain,
            status,
            settings: req.settings.unwrap_or_else(|| json!({})),
            sponsor_tenant_id: req.sponsor_tenant_id,  // NEW
        },
    );

    Ok(json!({
        "status": "ok",
        "tenant_id": tenant_id,
        "created": true,
        "sponsor_tenant_id": req.sponsor_tenant_id,  // NEW in response
    }))
}
```

### 2.7 `upsert_tenant_in_db` changes (line 3698)

```sql
INSERT INTO identity_tenants (tenant_id, name, domain, status, settings, sponsor_tenant_id, updated_at)
VALUES ($1::text::uuid, $2, $3, $4, $5::jsonb, $6::text::uuid, NOW())
ON CONFLICT (tenant_id) DO UPDATE
SET
    name = EXCLUDED.name,
    domain = EXCLUDED.domain,
    status = EXCLUDED.status,
    settings = EXCLUDED.settings,
    sponsor_tenant_id = EXCLUDED.sponsor_tenant_id,
    updated_at = NOW()
```

Add `$6` parameter for `sponsor_tenant_id` (can be NULL).

### 2.8 `default_tenant_id()` fix (line 525)

Current implementation returns the first key from a HashMap — non-deterministic order.

Change to: return the tenant with `sponsor_tenant_id = None` (the root tenant). If multiple roots exist (shouldn't happen in normal operation), return the first alphabetically by tenant_id for determinism.

```rust
fn default_tenant_id(&self) -> Option<String> {
    // prefer root tenant (no sponsor)
    let mut root_tenants: Vec<&String> = self.tenants.iter()
        .filter(|(_, t)| t.sponsor_tenant_id.is_none())
        .map(|(id, _)| id)
        .collect();
    root_tenants.sort();
    if let Some(id) = root_tenants.first() {
        return Some((*id).clone());
    }
    // fallback: any tenant, sorted for determinism
    let mut all: Vec<&String> = self.tenants.keys().collect();
    all.sort();
    all.first().map(|id| (*id).clone())
}
```

### 2.9 Sync propagation

No changes needed. `TenantRecord` is serialized as JSON in sync deltas. The new `sponsor_tenant_id` field is included automatically via `#[derive(Serialize)]`. Workers receive it, deserialize it, update their in-memory store and SHM.

### 2.10 Protocol changes summary

| Verb | Change |
|---|---|
| `TNT_CREATE` | Accepts optional `sponsor_tenant_id` field |
| `TNT_CREATE_RESPONSE` | Includes `sponsor_tenant_id` if set |
| `ILK_GET_RESPONSE` | Tenant section includes `sponsor_tenant_id` |

No new verbs required. No breaking changes to existing requests (new field is optional with default NULL).

### 2.11 New error code

| Code | Description |
|---|---|
| `INVALID_SPONSOR_TENANT` | `sponsor_tenant_id` specified but the referenced tenant does not exist |

---

## 3. Change 2: `owner_l2_name` on ICH

### 3.1 What it models

Each ICH (channel) is owned by a specific IO node that operates it. The `owner_l2_name` is the full L2 name of that node (e.g. `IO.whatsapp@motherbee`).

The ICH does not exist without a node — the IO node creates the ICH via `ILK_ADD_CHANNEL`. The `owner_l2_name` is filled automatically from the L2 name of the node that sent the `ILK_ADD_CHANNEL` request (resolved from `routing.src` by identity).

### 3.2 Rust struct changes

**`ChannelRecord`** (line 273):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChannelRecord {
    ich_id: String,
    channel_type: String,
    address: String,
    owner_l2_name: Option<String>,  // NEW — L2 full name of the owning IO node
    enabled: bool,                   // NEW — see Change 3
}
```

### 3.3 PostgreSQL schema change

```sql
ALTER TABLE identity_ichs
    ADD COLUMN owner_l2_name VARCHAR(128);

CREATE INDEX IF NOT EXISTS idx_identity_ichs_owner
    ON identity_ichs(owner_l2_name)
    WHERE owner_l2_name IS NOT NULL;
```

### 3.4 SHM change

**`IchEntry`** — add 128 bytes for L2 name:

```rust
#[repr(C)]
pub struct IchEntry {
    pub ich_id: [u8; 16],
    pub ilk_id: [u8; 16],
    pub tenant_id: [u8; 16],
    pub channel_type: [u8; 32],
    pub address: [u8; 256],
    pub owner_l2_name: [u8; 128],  // NEW — all zeros if not assigned
    pub is_primary: u8,
    pub enabled: u8,               // NEW — see Change 3
    pub flags: u8,
    pub _pad: [u8; 5],
    pub added_at: u64,
}
```

### 3.5 Auto-fill on `ILK_ADD_CHANNEL`

When identity receives `ILK_ADD_CHANNEL`, it resolves the sender's L2 name from the message's `routing.src` UUID (using the router SHM peer lookup that identity already has access to, or from the ILK record of the sender if registered).

The `owner_l2_name` is set automatically — the caller does not need to specify it. If the resolution fails (sender is unknown), the field is left NULL and a warning is logged. This should be rare since the IO node should be registered before adding channels.

### 3.6 Protocol changes

| Verb | Change |
|---|---|
| `ILK_ADD_CHANNEL` | Response includes `owner_l2_name` (auto-filled) |
| `ILK_GET_RESPONSE` | Each channel in the channels list includes `owner_l2_name` |

No new verbs for setting the owner — it's auto-filled on channel creation. If a channel moves to a different node (rare), it's done by removing and re-adding the channel from the new node.

---

## 4. Change 3: `enabled` on ICH

### 4.1 What it models

A channel can be enabled or disabled independently of its existence. A disabled channel exists, is associated to its ILK, and is registered in the system — but it should not transmit or receive messages.

Use cases:

- Operational pause (maintenance, client requested silence).
- Safety default (channel born disabled, explicitly enabled when ready).
- Administrative control (admin disables a channel without destroying the association).

### 4.2 Default: disabled

ICHs are born `enabled: false` when created via `ILK_ADD_CHANNEL`. They must be explicitly enabled via `ICH_SET_ENABLED`. This is a "safe by default" design — nothing transmits until someone says it should.

### 4.3 Rust struct changes

See `ChannelRecord` in Change 2 above — the `enabled: bool` field is included there.

For `ILK_ADD_CHANNEL` processing: when a new channel is created, `enabled` is set to `false` by default. The caller cannot override this in the create request — enabling is always a separate explicit action.

### 4.4 PostgreSQL schema change

```sql
ALTER TABLE identity_ichs
    ADD COLUMN enabled BOOLEAN NOT NULL DEFAULT FALSE;
```

### 4.5 SHM change

See `IchEntry` in Change 2 above — the `enabled: u8` field is included there. Value `0` = disabled, `1` = enabled.

### 4.6 New protocol verb: `ICH_SET_ENABLED`

```
ICH_SET_ENABLED
  Request:
  {
    "ich_id": "ich:a1b2c3d4-...",
    "enabled": true
  }

  Response (success):
  {
    "status": "ok",
    "ich_id": "ich:a1b2c3d4-...",
    "enabled": true,
    "owner_l2_name": "IO.whatsapp@motherbee"
  }

  Response (error):
  {
    "status": "error",
    "error_code": "ICH_NOT_FOUND"
  }
```

This verb handles both enable (`enabled: true`) and disable (`enabled: false`). Same verb, boolean parameter. Can be called at any time — at creation, after the channel has been running, during maintenance.

**Authorization:** any node can call this. OPA rules on the router determine which nodes are allowed to change channel state. Typical allowlist: the owning IO node, SY.admin, Archi.

### 4.7 SDK helpers

**Rust SDK** (`fluxbee_sdk`):

```rust
/// Enable or disable an ICH. Returns the new state.
pub async fn set_ich_enabled(
    sender: &NodeSender,
    ich_id: &str,
    enabled: bool,
) -> Result<IchSetEnabledResponse, SdkError>;
```

**Go SDK** (`fluxbee-go-sdk`):

```go
// SetIchEnabled enables or disables an ICH by ID.
func SetIchEnabled(sender *Sender, ichID string, enabled bool) (*IchSetEnabledResponse, error)
```

Both SDKs send the `ICH_SET_ENABLED` message to `SY.identity@<hive>` and wait for the response.

### 4.8 Propagation to consumers

When `enabled` changes, identity:

1. Updates the in-memory `ChannelRecord`.
2. Updates the PostgreSQL row.
3. Updates the `IchEntry` in SHM (sets the `enabled` byte to 0 or 1).
4. Emits a sync delta to workers (the delta includes the full channel record with the new `enabled` value).

The SHM change is visible immediately to any node reading the identity SHM region (router, OPA, etc.). No polling needed — the SHM sequence counter (`seq`) increments, and readers detect the change on their next read.

### 4.9 Who enforces `enabled`

Identity stores and propagates the `enabled` state. **Identity does not enforce it** — identity is a repository, not an enforcer.

Enforcement options (to be decided separately, not part of this identity change):

**Option A — OPA enforcement.** The router reads `enabled` from the ICH data in SHM. When building the OPA input for a message, the router includes the channel's `enabled` state. An OPA base rule blocks messages from/to disabled channels. This is centralized and every IO node inherits it without code changes.

**Option B — IO node enforcement.** Each IO node reads the `enabled` state of its channels from SHM before processing a message. If disabled, it drops the message. This requires each IO to have the check, but gives the IO node control over how to handle disabled state (e.g. queue messages instead of dropping, return a specific error to the external caller).

**For this document:** we only put the data in place (DB, in-memory, SHM, sync). The enforcement mechanism is a separate decision that depends on how the router builds OPA input and whether the ICH data is already in OPA's bundle. This is flagged as follow-up work.

### 4.10 New error codes

| Code | Description |
|---|---|
| `ICH_NOT_FOUND` | `ich_id` does not exist |
| `INVALID_ICH_STATE` | `enabled` value is not a boolean |

---

## 5. Complete SHM Layout Changes

### 5.1 `TenantEntry` (before and after)

**Before:**
```rust
pub struct TenantEntry {
    pub tenant_id: [u8; 16],
    pub name: [u8; 128],
    pub domain: [u8; 128],
    pub status: u8,
    pub flags: u8,
    pub _pad0: [u8; 5],
    pub max_ilks: u32,
    pub created_at: u64,
    pub updated_at: u64,
    pub _reserved: [u8; 8],
}
```

**After:**
```rust
pub struct TenantEntry {
    pub tenant_id: [u8; 16],
    pub name: [u8; 128],
    pub domain: [u8; 128],
    pub status: u8,
    pub flags: u8,
    pub _pad0: [u8; 5],
    pub max_ilks: u32,
    pub sponsor_tenant_id: [u8; 16],  // NEW
    pub created_at: u64,
    pub updated_at: u64,
}
```

Size change: `_reserved` (8 bytes) removed, `sponsor_tenant_id` (16 bytes) added. Net +8 bytes per entry.

### 5.2 `IchEntry` (before and after)

**Before:**
```rust
pub struct IchEntry {
    pub ich_id: [u8; 16],
    pub ilk_id: [u8; 16],
    pub tenant_id: [u8; 16],
    pub channel_type: [u8; 32],
    pub address: [u8; 256],
    pub is_primary: u8,
    pub flags: u8,
    pub _pad: [u8; 6],
    pub added_at: u64,
}
```

**After:**
```rust
pub struct IchEntry {
    pub ich_id: [u8; 16],
    pub ilk_id: [u8; 16],
    pub tenant_id: [u8; 16],
    pub channel_type: [u8; 32],
    pub address: [u8; 256],
    pub owner_l2_name: [u8; 128],  // NEW
    pub is_primary: u8,
    pub enabled: u8,               // NEW — 0=disabled, 1=enabled, default 0
    pub flags: u8,
    pub _pad: [u8; 5],
    pub added_at: u64,
}
```

Size change: `owner_l2_name` (128 bytes) + `enabled` (1 byte) added, `_pad` reduced by 1 byte. Net +128 bytes per entry.

### 5.3 Rebuild process

SHM changes require a clean rebuild:

1. Stop all nodes that read identity SHM (router, admin, any consumer).
2. Delete the identity SHM region file.
3. Recompile all binaries that import `json_router::shm` with the new struct definitions.
4. Restart SY.identity — it recreates the SHM region with the new layout and repopulates from DB.
5. Restart all consumers.

No data migration in SHM. The SHM is a projection of DB state — it's fully reconstructed from DB on every identity boot.

---

## 6. Complete Protocol Changes

### 6.1 Modified verbs

| Verb | Change |
|---|---|
| `TNT_CREATE` | Accepts optional `sponsor_tenant_id` |
| `TNT_CREATE_RESPONSE` | Includes `sponsor_tenant_id` |
| `ILK_ADD_CHANNEL` | Response includes `owner_l2_name` (auto-filled) and `enabled: false` |
| `ILK_GET_RESPONSE` | Tenant includes `sponsor_tenant_id`. Channels include `owner_l2_name` and `enabled`. |

### 6.2 New verbs

| Verb | Purpose |
|---|---|
| `ICH_SET_ENABLED` | Enable or disable a channel. Request: `{ich_id, enabled}`. Response: `{status, ich_id, enabled, owner_l2_name}` |

### 6.3 New error codes

| Code | Description |
|---|---|
| `INVALID_SPONSOR_TENANT` | Sponsor tenant referenced in TNT_CREATE does not exist |
| `ICH_NOT_FOUND` | ICH referenced in ICH_SET_ENABLED does not exist |
| `INVALID_ICH_STATE` | Invalid value for `enabled` field |

---

## 7. Complete DB Migration SQL

```sql
-- Change 1: sponsor on tenants
ALTER TABLE identity_tenants
    ADD COLUMN IF NOT EXISTS sponsor_tenant_id UUID REFERENCES identity_tenants(tenant_id);

CREATE INDEX IF NOT EXISTS idx_identity_tenants_sponsor
    ON identity_tenants(sponsor_tenant_id)
    WHERE sponsor_tenant_id IS NOT NULL;

-- Change 2: owner L2 name on ICHs
ALTER TABLE identity_ichs
    ADD COLUMN IF NOT EXISTS owner_l2_name VARCHAR(128);

CREATE INDEX IF NOT EXISTS idx_identity_ichs_owner
    ON identity_ichs(owner_l2_name)
    WHERE owner_l2_name IS NOT NULL;

-- Change 3: enabled state on ICHs
ALTER TABLE identity_ichs
    ADD COLUMN IF NOT EXISTS enabled BOOLEAN NOT NULL DEFAULT FALSE;
```

Three columns, two indexes. All additive, no data loss.

---

## 8. SDK Changes Summary

### 8.1 Rust SDK (`fluxbee_sdk`)

New function:

```rust
pub async fn set_ich_enabled(
    sender: &NodeSender,
    ich_id: &str,
    enabled: bool,
) -> Result<IchSetEnabledResponse, SdkError>;
```

### 8.2 Go SDK (`fluxbee-go-sdk`)

New function:

```go
func SetIchEnabled(sender *Sender, ichID string, enabled bool) (*IchSetEnabledResponse, error)
```

Both SDKs send `ICH_SET_ENABLED` to `SY.identity@<hive>` via L2 unicast and return the structured response.

---

## 9. Sync Protocol Impact

No changes to the sync protocol itself. The existing delta mechanism serializes `TenantRecord` and `IlkRecord` (which contains `channels: Vec<ChannelRecord>`) as JSON. The new fields (`sponsor_tenant_id`, `owner_l2_name`, `enabled`) are included automatically via Serde.

Workers receiving deltas will:

1. Deserialize the updated records with the new fields.
2. Update their in-memory store.
3. Update their local SHM with the new TenantEntry / IchEntry layouts.

Workers must be compiled with the new struct definitions before receiving deltas that contain the new fields. In practice, all binaries are rebuilt together as part of this change.

---

## 10. Follow-up Work (not in this document)

| Item | Description | Blocked by |
|---|---|---|
| OPA reads ICH `enabled` from SHM | Router includes `enabled` in OPA input; base rule blocks disabled channels | Requires router change to read ICH region |
| OPA reads `sponsor_tenant_id` | Router includes sponsor in OPA input for cross-tenant policies | Requires router change to read tenant sponsor |
| IO node checks `enabled` before processing | IO nodes read SHM before accepting/sending on a channel | Requires IO SDK helper |
| Cloud admin creation flow | External signup → TNT_CREATE + ILK_REGISTER in one flow | Requires external auth integration |
| Execution plan steps for ICH enable | Add `ich_set_enabled` as admin action for use in execution plans | Requires admin action registry update |

---

## 11. Implementation Checklist

```
[ ] TenantRecord: add sponsor_tenant_id field
[ ] TntCreateRequest: add sponsor_tenant_id field
[ ] create_tenant: validate sponsor exists
[ ] create_tenant: propagate sponsor_tenant_id to TenantRecord
[ ] upsert_tenant_in_db: add $6 parameter for sponsor
[ ] default_tenant_id: return root tenant (sponsor=NULL) deterministically
[ ] TenantEntry SHM: add sponsor_tenant_id [u8; 16]
[ ] tenant_entry_from_record: populate sponsor_tenant_id bytes
[ ] TNT_CREATE_RESPONSE: include sponsor_tenant_id

[ ] ChannelRecord: add owner_l2_name field
[ ] ChannelRecord: add enabled field (default false)
[ ] ILK_ADD_CHANNEL handler: auto-fill owner_l2_name from routing.src
[ ] ILK_ADD_CHANNEL handler: set enabled=false on new channels
[ ] IchEntry SHM: add owner_l2_name [u8; 128] and enabled u8
[ ] ich_entry_from_record: populate new fields

[ ] New handler: ICH_SET_ENABLED
[ ] ICH_SET_ENABLED: validate ich_id exists
[ ] ICH_SET_ENABLED: update in-memory, DB, SHM
[ ] ICH_SET_ENABLED: emit sync delta
[ ] ICH_SET_ENABLED: return response with new state

[ ] Rust SDK: add set_ich_enabled function
[ ] Go SDK: add SetIchEnabled function

[ ] SHM struct rebuild: update TenantEntry + IchEntry in shm crate
[ ] SHM region: delete and recreate on first boot after update
[ ] DB migration: run ALTER TABLE statements
[ ] Full binary rebuild of all SHM consumers

[ ] ILK_GET_RESPONSE: include sponsor_tenant_id in tenant, owner_l2_name and enabled in channels
[ ] ILK_LIST_RESPONSE: same additions
```

---

## 12. What is NOT changed in this version

- ILK struct: no changes to IlkRecord or IlkEntry.
- Identity vocabulary: no changes.
- Aliases: no changes.
- Merge flow: no changes.
- Registration flow: no changes (other than responses including new fields).
- Cognition integration: not affected.
- Claims model: deferred to separate discussion (identity v3 / agent skills).
- Billing: separate system, not part of identity.
