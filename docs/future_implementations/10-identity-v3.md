# Fluxbee — 10 Identity (SY.identity) v3

**Status:** v3.0 draft
**Date:** 2026-04-13
**Audience:** All developers (IO, AI, WF, SY, router, OPA)
**Replaces:** `10-identity-v2.md` (archived as historical reference)

---

## 1. Purpose

SY.identity is the central registry of all entities that participate in the system. In v3, the conceptual center shifts from a classic software user model (roles, permissions, capabilities) to a **descriptive, claim-based model** aligned with the nature of Fluxbee participants.

Its responsibilities are:

1. **Generate identities** — assign unique ILKs to every entity (human, agent, system).
2. **Associate identities** — link ILKs to tenants, channels (ICHs), and nodes.
3. **Describe identities** — maintain claims about what each ILK knows, does, and is responsible for, and maintain recognition of those claims by others.
4. **Serve identity data** — make the full identity dataset available in SHM for real-time routing (L3 / OPA).

SY.identity does **not** handle:

- Authentication (edge/IO responsibility).
- Routing permissions themselves — SY.identity provides data, OPA enforces policy.
- The normative layer (Constitution, Charters) — that is external to identity.
- Cognitive data (SY.cognition responsibility).

---

## 2. Conceptual Model: Three Layers

Fluxbee separates identity-related concerns into three distinct layers with clean responsibilities:

### 2.1 Identity Layer (SY.identity)

Describes entities:

- who they are
- what they declare to know or do
- what others recognize about them
- what office they occupy, if institutional

Identity is descriptive. It answers "who is this entity and what does the system say about them", not "what is this entity allowed to do right now".

### 2.2 Normative Layer (external to identity)

Defines what the system considers correct:

- Constitution: global principles applying system-wide
- Charters: office/domain-specific working rules

Identity may reference normative documents via `charter_ref`, but does not store or interpret their full content. The normative layer is documentary first, not executable first.

In v3, the normative layer is scaffolded conceptually but not fully implemented. Charters are referenced as URIs; their content lives outside SY.identity.

### 2.3 Enforcement Layer (OPA, router, runtime)

Enforces what actually happens:

- routing constraints
- OPA policies consuming identity data
- prompt restrictions
- runtime operational controls

Enforcement consumes identity metadata and normative references but does not replace them. OPA reads `data.identity` populated by the router from SHM and evaluates rules against it.

**The principle:**

- Identity describes.
- Normative orients.
- Enforcement acts.

---

## 3. Identifier Format Convention

Unchanged from v2.

| Entity | Protocol format | SHM format | DB format |
|---|---|---|---|
| Tenant | `tnt:<uuid-v4>` | `[u8; 16]` raw | `UUID` |
| ILK | `ilk:<uuid-v4>` | `[u8; 16]` raw | `UUID` |
| ICH | `ich:<uuid-v4>` | `[u8; 16]` raw | `UUID` |
| Claim | `clm:<uuid-v4>` | `[u8; 16]` raw | `UUID` |
| Recognition | `rcg:<uuid-v4>` | `[u8; 16]` raw | `UUID` |

Rules:
- In JSON, API, OPA: always prefixed form.
- In SHM: raw 16-byte UUID.
- In PostgreSQL: native `UUID`.
- SY.identity converts at the SHM boundary.

---

## 4. Core Entities

### 4.1 TNT (Tenant)

Unchanged from v2. A tenant is an organization, company, or account operating within Fluxbee. Every ILK belongs to exactly one tenant.

```json
{
  "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
  "name": "Acme Fluxbee Operations",
  "domain": "acme.com",
  "status": "active",
  "created_at": "2026-04-13T10:00:00Z",
  "approved_by": "ilk:7c9e6679-7425-40de-944b-e07fc1f90ae7"
}
```

### 4.2 ILK (Interlocutor Key)

An ILK is the unique identity of any entity that participates in messaging: humans, AI agents, system processes, and workflows.

Format: `ilk:<uuid-v4>`. Immutable once created.

Types:

| Type | Description | Registered by |
|---|---|---|
| `human` | Person (operator, customer, admin) | AI.frontdesk |
| `agent` | AI node that processes messages | SY.orchestrator |
| `system` | Workflow, bot, integration, sensor | SY.orchestrator |

Rules:
- ILK is immutable once created.
- Every ILK belongs to exactly one tenant.
- ILK persists across node restarts — same `node_name` maps to the same ILK.
- Temporary ILKs are real ILKs with valid UUIDs, created by `ILK_PROVISION` from IO nodes for unknown channels.

### 4.3 ICH (Interlocutor Channel)

Unchanged from v2. A communication channel owned by a single ILK (e.g. a phone number, a Slack handle, an email address).

```json
{
  "ich_id": "ich:a1b2c3d4-5678-90ab-cdef-1234567890ab",
  "ilk_id": "ilk:550e8400-...",
  "channel_type": "whatsapp",
  "address": "+5411...",
  "added_at": "2026-04-13T10:00:00Z",
  "is_primary": true
}
```

### 4.4 Claim (new in v3)

A **claim** is the core unit of identity metadata in v3. It is a statement about what an ILK knows, does, or is responsible for. Claims replace the v2 concepts of `roles`, `capabilities`, and controlled vocabulary.

Format: `clm:<uuid-v4>`.

Structure:

```json
{
  "claim_id": "clm:11111111-1111-1111-1111-111111111111",
  "ilk_id": "ilk:550e8400-...",
  "facet": "function",
  "key": "can_invoice",
  "statement": "Authorized to initiate invoice issuance workflows.",
  "canonical": true,
  "declared_by": "ilk:system",
  "declared_at": "2026-04-13T10:00:00Z",
  "revoked_at": null
}
```

Fields:

| Field | Description |
|---|---|
| `claim_id` | Unique identifier for this claim instance. |
| `ilk_id` | ILK this claim belongs to (the "holder"). |
| `facet` | One of `domain`, `function`, `responsibility`. See 4.4.1. |
| `key` | Stable snake_case identifier. Canonical if in the embedded catalog, emergent otherwise. |
| `statement` | Human-readable description of the claim. |
| `canonical` | `true` if the key matches an entry in the canonical catalog embedded in SY.identity. |
| `declared_by` | ILK that declared this claim (the holder itself, an authority, or `ilk:system` for defaults). |
| `declared_at` | Timestamp of declaration. |
| `revoked_at` | Null if active; timestamp if revoked. Revoked claims are soft-deleted, kept for audit. |

#### 4.4.1 Facets

A claim has exactly one facet. The three facets are fixed in v3:

**`domain`** — describes topics or areas the ILK can legitimately speak about.

Examples: `billing`, `support_l1`, `sales`, `onboarding`.

**`function`** — describes what the ILK knows how to do operationally. Functions are typically actionable, often consumed by OPA for authorization.

Examples: `can_invoice`, `can_refund`, `can_register_human`.

**`responsibility`** — describes what the ILK is assigned to handle as an ongoing duty.

Examples: `identity_frontdesk`, `billing_intake`, `tenant_admission`.

In v3, no fourth facet is allowed. If a new dimension emerges, it is deferred to v3.1 with explicit design discussion.

#### 4.4.2 Key validation

Keys are validated at declaration time:

- Regex: `^[a-z][a-z0-9_]{0,63}$`
- Lowercase ASCII only.
- Starts with a letter.
- Max 64 characters.
- Snake_case by convention.

Invalid keys are rejected with error `INVALID_CLAIM_KEY`.

#### 4.4.3 Canonical vs emergent

Claims with a key matching an entry in the embedded canonical catalog (see section 8) are marked `canonical: true`. Their semantics are stable and can be referenced directly in OPA rules with confidence.

Claims with any other key are marked `canonical: false` (emergent). Emergent claims are valid and persisted normally, but OPA rules should not rely on them for hard authorization decisions — they are intended for soft uses like search, filtering, and introspection.

The distinction is a flag at declaration time. There is no separate lifecycle for canonical vs emergent claims; they coexist in the same tables and SHM region.

### 4.5 Recognition (new in v3)

A claim gains meaning when other ILKs **recognize** it. Recognition is explicit and tracked.

Format: `rcg:<uuid-v4>`.

Structure:

```json
{
  "recognition_id": "rcg:22222222-2222-2222-2222-222222222222",
  "claim_id": "clm:11111111-...",
  "recognizer_ilk_id": "ilk:gov.identity.frontdesk",
  "recognized_at": "2026-04-13T11:00:00Z",
  "revoked_at": null
}
```

Rules:

- Any ILK can recognize any claim. No authorization required in v3.
- An ILK cannot recognize its own claims.
- A recognizer can only recognize a given claim once (unique on `claim_id + recognizer_ilk_id`).
- Recognition can be revoked. Revoked recognitions are soft-deleted.
- Recognition never upgrades an emergent claim to canonical — `canonical` is determined solely by the key matching the catalog, not by recognition count.

### 4.6 Institutional Role (new in v3, for `.gov` and system nodes)

Nodes that occupy institutional offices carry additional metadata describing their office and charter. This is minimal and intentionally simple.

```json
{
  "institutional_role": {
    "office": "government.identity.frontdesk",
    "jurisdiction": ["identity", "interlocutor_onboarding"],
    "charter_ref": "charter://identity/frontdesk-v1"
  }
}
```

Fields:

| Field | Description |
|---|---|
| `office` | Canonical office identifier (string, stable, snake_case / dotted form). |
| `jurisdiction` | Array of domain strings indicating scope of authority. |
| `charter_ref` | URI to the charter document that governs this office. SY.identity does not resolve or validate the URI. |

Only `agent` and `system` ILKs can have an `institutional_role`. Humans cannot. Attempting to set an institutional_role on a human ILK returns `INVALID_INSTITUTIONAL_ROLE_TARGET`.

Institutional roles are set by `SY.orchestrator` during `ILK_REGISTER` or updated via `ILK_SET_INSTITUTIONAL_ROLE`, which is restricted to orchestrator.

### 4.7 Complete ILK Document (v3)

```json
{
  "ilk_id": "ilk:550e8400-e29b-41d4-a716-446655440000",
  "ilk_type": "human",
  "registration_status": "complete",
  "created_at": "2026-04-13T10:00:00Z",
  "registered_at": "2026-04-13T10:05:00Z",
  "registered_by": "ilk:7c9e6679-...",

  "identification": {
    "display_name": "Juan Pérez",
    "email": "juan@acme.com",
    "phone": "+5411...",
    "document_type": "DNI",
    "document_number": "12345678",
    "company": "Acme Argentina S.A."
  },

  "association": {
    "tenant_id": "tnt:550e8400-...",
    "channels": [
      {
        "ich_id": "ich:a1b2c3d4-...",
        "channel_type": "whatsapp",
        "address": "+5411...",
        "is_primary": true
      }
    ]
  },

  "claims": [
    {
      "claim_id": "clm:11111111-...",
      "facet": "domain",
      "key": "billing",
      "statement": "Has knowledge of billing and invoicing.",
      "canonical": true,
      "declared_by": "ilk:gov.identity.frontdesk",
      "recognized_by": [
        {
          "recognition_id": "rcg:22222222-...",
          "recognizer_ilk_id": "ilk:gov.identity.frontdesk"
        }
      ]
    }
  ],

  "institutional_role": null
}
```

For an institutional node (for example `SY.frontdesk.gov`):

```json
{
  "ilk_id": "ilk:...",
  "ilk_type": "system",
  "registration_status": "complete",
  "identification": {
    "display_name": "SY.frontdesk.gov",
    "node_name": "SY.frontdesk.gov@motherbee"
  },
  "association": {
    "tenant_id": "tnt:fluxbee-default",
    "channels": []
  },
  "claims": [
    {
      "facet": "responsibility",
      "key": "identity_frontdesk",
      "canonical": true,
      "declared_by": "ilk:system"
    },
    {
      "facet": "function",
      "key": "can_register_human",
      "canonical": true,
      "declared_by": "ilk:system"
    }
  ],
  "institutional_role": {
    "office": "government.identity.frontdesk",
    "jurisdiction": ["identity", "interlocutor_onboarding"],
    "charter_ref": "charter://identity/frontdesk-v1"
  }
}
```

---

## 5. Registration Status

Unchanged from v2.

| Status | Meaning |
|---|---|
| `temporary` | First contact — ILK created with ICH only, no identification data, pending frontdesk registration. |
| `partial` | Some identification data present, registration in progress. |
| `complete` | Minimum required data present, fully operational. |

Transitions follow the same rules as v2: reassignment of tenant is only allowed from `temporary → complete`.

---

## 6. Protocol

### 6.1 Verbs (v3)

Existing from v2 (unchanged):

- `ILK_PROVISION` / `_RESPONSE`
- `ILK_REGISTER` / `_RESPONSE`
- `ILK_ADD_CHANNEL` / `_RESPONSE`
- `ILK_UPDATE` / `_RESPONSE`
- `TNT_CREATE` / `_RESPONSE`
- `TNT_APPROVE` / `_RESPONSE`

New in v3:

- `ILK_DECLARE_CLAIM` / `_RESPONSE`
- `ILK_REVOKE_CLAIM` / `_RESPONSE`
- `ILK_RECOGNIZE_CLAIM` / `_RESPONSE`
- `ILK_UNRECOGNIZE_CLAIM` / `_RESPONSE`
- `ILK_SET_INSTITUTIONAL_ROLE` / `_RESPONSE`
- `IDENTITY_LIST_CANONICAL_CLAIMS` / `_RESPONSE`

All use `meta.type = "system"` and follow the standard Fluxbee envelope.

### 6.2 `ILK_DECLARE_CLAIM`

Declares a claim for a target ILK. The target can be the requester itself (self-declaration) or another ILK (declaration on behalf of).

Request:
```json
{
  "meta": { "type": "system", "msg": "ILK_DECLARE_CLAIM" },
  "payload": {
    "target_ilk": "ilk:550e8400-...",
    "facet": "function",
    "key": "can_invoice",
    "statement": "Authorized to initiate invoice issuance workflows."
  }
}
```

Response:
```json
{
  "meta": { "type": "system", "msg": "ILK_DECLARE_CLAIM_RESPONSE" },
  "payload": {
    "ok": true,
    "claim_id": "clm:11111111-...",
    "canonical": true
  }
}
```

Validation:
- `target_ilk` must exist and not be deleted.
- `facet` must be one of `domain`, `function`, `responsibility`.
- `key` must match the regex in section 4.4.2.
- If a claim with the same `(ilk_id, facet, key)` already exists and is not revoked, returns `CLAIM_ALREADY_EXISTS` with the existing `claim_id`.
- If the holder already has 32 active claims, returns `TOO_MANY_CLAIMS`.

Allowlist:
- Self-declaration always allowed.
- Declaration on behalf of another ILK allowed from `SY.frontdesk.gov@*` and `SY.orchestrator@*`. Other sources get `UNAUTHORIZED_DECLARER`.

### 6.3 `ILK_REVOKE_CLAIM`

Revokes a claim by `claim_id`.

Request:
```json
{
  "meta": { "type": "system", "msg": "ILK_REVOKE_CLAIM" },
  "payload": {
    "claim_id": "clm:11111111-..."
  }
}
```

Validation:
- Only the original `declared_by` ILK can revoke. Exception: `SY.orchestrator` can revoke any claim.
- Already-revoked claims return `CLAIM_ALREADY_REVOKED`.

### 6.4 `ILK_RECOGNIZE_CLAIM`

Records that the requester recognizes a claim.

Request:
```json
{
  "meta": { "type": "system", "msg": "ILK_RECOGNIZE_CLAIM" },
  "payload": {
    "claim_id": "clm:11111111-..."
  }
}
```

The recognizer is resolved from `routing.src`.

Validation:
- Cannot recognize own claim (holder ILK cannot recognize their own claim).
- Cannot recognize twice (`RECOGNITION_ALREADY_EXISTS`).
- If claim has 16 active recognitions, returns `TOO_MANY_RECOGNITIONS`.

No authorization required — any ILK can recognize any claim.

### 6.5 `ILK_UNRECOGNIZE_CLAIM`

Revokes a recognition. Only the original recognizer can do this.

### 6.6 `ILK_SET_INSTITUTIONAL_ROLE`

Sets or updates the `institutional_role` on an ILK. Restricted to `SY.orchestrator@*`.

Request:
```json
{
  "meta": { "type": "system", "msg": "ILK_SET_INSTITUTIONAL_ROLE" },
  "payload": {
    "target_ilk": "ilk:...",
    "office": "government.identity.frontdesk",
    "jurisdiction": ["identity", "interlocutor_onboarding"],
    "charter_ref": "charter://identity/frontdesk-v1"
  }
}
```

To clear: omit all fields except `target_ilk` and pass `"clear": true`.

### 6.7 `IDENTITY_LIST_CANONICAL_CLAIMS`

Returns the full list of canonical claims embedded in the current SY.identity binary. Consumed by Archi, SY.admin, and any tool that needs to know the stable vocabulary.

Request:
```json
{
  "meta": { "type": "system", "msg": "IDENTITY_LIST_CANONICAL_CLAIMS" },
  "payload": {}
}
```

Response:
```json
{
  "meta": { "type": "system", "msg": "IDENTITY_LIST_CANONICAL_CLAIMS_RESPONSE" },
  "payload": {
    "ok": true,
    "binary_version": "sy-identity 1.0.0-abc123",
    "claims": [
      {
        "facet": "function",
        "key": "can_invoice",
        "statement": "Authorized to initiate invoice issuance workflows.",
        "description": "This claim allows the holder to trigger WF.invoice and related billing workflows.",
        "typical_holders": ["agent", "human"],
        "typical_recognizers": ["SY.orchestrator", "AI.billing"]
      }
    ]
  }
}
```

No authorization required — this is pure introspection.

---

## 7. Default Claims by Node Name Pattern

When SY.orchestrator registers a new node via `ILK_REGISTER`, SY.identity checks embedded default rules and automatically injects claims matching the node's L2 name pattern. This ensures institutional and system nodes are born with the claims they need to function immediately.

Defaults are declared in code (see section 8.2). Patterns can be:

- **Exact match** — `SY.frontdesk.gov` matches only that name.
- **Prefix match** — `SY.` matches any `SY.*` node.
- **Suffix match** — `.gov` matches any node whose L2 name ends with `.gov`.

Multiple patterns can match the same node; all their defaults are combined (deduplicated by `(facet, key)`).

Default claims are declared with `declared_by = "ilk:system"` and `canonical = true` (since they reference canonical keys).

Defaults are applied only at `ILK_REGISTER` time. Subsequent restarts that re-register the same `node_name` re-evaluate defaults against the current binary, so updates to defaults propagate to existing nodes on their next registration.

---

## 8. Canonical Catalog (Embedded in Code)

### 8.1 Design principle

The canonical claims catalog **is not a configuration file**. It is Rust source code embedded in the SY.identity binary. Changing it is a code change: PR, review, merge, build, release, redeploy.

This decision is deliberate and eliminates:

- A separate catalog file to maintain.
- Independent versioning of the catalog vs the code.
- Runtime validation of a catalog file format.
- The question "which catalog version is live right now?" — the answer is always "the one in the current binary".
- The coordination problem between a catalog file and its consumers.

### 8.2 Code layout

The catalog lives in `sy_identity/src/canonical_claims.rs`:

```rust
pub struct CanonicalClaim {
    pub facet: Facet,
    pub key: &'static str,
    pub statement: &'static str,
    pub description: &'static str,
    pub typical_holders: &'static [IlkType],
    pub typical_recognizers: &'static [&'static str],
}

pub const CANONICAL_CLAIMS: &[CanonicalClaim] = &[
    CanonicalClaim {
        facet: Facet::Function,
        key: "can_invoice",
        statement: "Authorized to initiate invoice issuance workflows.",
        description: "Holders of this claim can trigger WF.invoice and related billing workflows.",
        typical_holders: &[IlkType::Agent, IlkType::Human],
        typical_recognizers: &["SY.orchestrator", "AI.billing"],
    },
    // ... more claims
];

pub struct DefaultClaim {
    pub facet: Facet,
    pub key: &'static str,
}

pub struct NodeDefaultRule {
    pub pattern: NodePattern,
    pub claims: &'static [DefaultClaim],
    pub institutional_role: Option<InstitutionalRoleTemplate>,
}

pub const NODE_DEFAULTS: &[NodeDefaultRule] = &[
    NodeDefaultRule {
        pattern: NodePattern::Exact("SY.frontdesk.gov"),
        claims: &[
            DefaultClaim { facet: Facet::Responsibility, key: "identity_frontdesk" },
            DefaultClaim { facet: Facet::Function, key: "can_register_human" },
            DefaultClaim { facet: Facet::Function, key: "can_merge_ilk_channels" },
        ],
        institutional_role: Some(InstitutionalRoleTemplate {
            office: "government.identity.frontdesk",
            jurisdiction: &["identity", "interlocutor_onboarding"],
            charter_ref: "charter://identity/frontdesk-v1",
        }),
    },
    NodeDefaultRule {
        pattern: NodePattern::PrefixDot("SY"),
        claims: &[
            DefaultClaim { facet: Facet::Function, key: "can_read_system_diagnostics" },
        ],
        institutional_role: None,
    },
    // ... more defaults
];
```

### 8.3 Validation at compile time

Tests in SY.identity enforce invariants at compile/test time:

- Every `DefaultClaim.key` must exist in `CANONICAL_CLAIMS`.
- No duplicate `(facet, key)` pairs in `CANONICAL_CLAIMS`.
- All `key` values must match the regex `^[a-z][a-z0-9_]{0,63}$`.
- No two `NodeDefaultRule` with the same `pattern`.

If any invariant is violated, tests fail and the build is blocked. No runtime validation needed.

### 8.4 Initial canonical catalog (v1.0)

This is the starting vocabulary of the system. It is intentionally small. When a new canonical claim is needed, it is added via code change and release.

**`facet: domain`** — knowledge areas

| Key | Statement |
|---|---|
| `billing` | Has knowledge of billing, invoicing, and accounts receivable. |
| `support` | Has knowledge of customer support workflows and common resolutions. |
| `sales` | Has knowledge of sales processes, lead qualification, and opportunity closing. |
| `onboarding` | Has knowledge of user and tenant onboarding processes. |
| `identity_lifecycle` | Has knowledge of the ILK/ICH/tenant lifecycle in Fluxbee. |
| `system_operations` | Has knowledge of Fluxbee internal operations and diagnostics. |

**`facet: function`** — operational capabilities

| Key | Statement |
|---|---|
| `can_invoice` | Authorized to initiate invoice issuance workflows. |
| `can_refund` | Authorized to initiate refund workflows. |
| `can_escalate_to_human` | Authorized to transfer a conversation to a human operator. |
| `can_create_tenant` | Authorized to create new tenants. |
| `can_approve_tenant` | Authorized to approve pending tenants. |
| `can_register_human` | Authorized to complete registration of human ILKs. |
| `can_merge_ilk_channels` | Authorized to associate new channels with an existing ILK. |
| `can_read_system_diagnostics` | Authorized to read system diagnostic and health information. |

**`facet: responsibility`** — ongoing duties

| Key | Statement |
|---|---|
| `identity_frontdesk` | Responsible for first-contact registration of new identities. |
| `billing_intake` | Responsible for receiving and processing billing requests. |
| `support_intake` | Responsible for receiving and classifying support requests. |
| `tenant_admission` | Responsible for handling new tenant admission. |
| `system_monitoring` | Responsible for monitoring system health. |

Total: 19 canonical claims. This is the v1.0 catalog.

### 8.5 Initial node defaults (v1.0)

| Pattern | Default claims | Institutional role |
|---|---|---|
| Exact: `SY.frontdesk.gov` | `resp/identity_frontdesk`, `func/can_register_human`, `func/can_merge_ilk_channels` | `government.identity.frontdesk` |
| Prefix: `SY.` | `func/can_read_system_diagnostics` | none |
| Prefix: `AI.billing` | `domain/billing`, `func/can_invoice` | none |
| Prefix: `AI.support` | `domain/support`, `func/can_escalate_to_human` | none |
| Prefix: `AI.sales` | `domain/sales`, `func/can_escalate_to_human` | none |
| Prefix: `WF.invoice` | `resp/billing_intake`, `func/can_invoice` | none |

These defaults exist to make the system functional on day one. They can be edited via code change.

Nodes not matching any pattern receive no default claims. Their ILK is created with an empty claims list, which is valid.

---

## 9. PostgreSQL Schema (v3)

### 9.1 Tables

Unchanged from v2:

- `identity_tenants`
- `identity_ilks` (with new `company` column and `institutional_role` JSON column)
- `identity_ichs`
- `identity_ilk_aliases`

New in v3:

- `identity_claims`
- `identity_claim_recognitions`

Removed from v2 (no migration, clean slate):

- All `roles`, `capabilities`, `degrees` fields from `identity_ilks`.
- `identity_vocabulary` table.

### 9.2 `identity_ilks` changes

```sql
ALTER TABLE identity_ilks
  ADD COLUMN company TEXT,
  ADD COLUMN institutional_role JSONB,
  DROP COLUMN roles,
  DROP COLUMN capabilities,
  DROP COLUMN degrees;
```

`company` is optional free-text. `institutional_role` is NULL for non-institutional ILKs, otherwise a JSON object matching section 4.6.

### 9.3 `identity_claims`

```sql
CREATE TABLE identity_claims (
    claim_id UUID PRIMARY KEY,
    ilk_id UUID NOT NULL REFERENCES identity_ilks(ilk_id),
    facet TEXT NOT NULL CHECK (facet IN ('domain', 'function', 'responsibility')),
    key TEXT NOT NULL,
    statement TEXT,
    canonical BOOLEAN NOT NULL,
    declared_by UUID NOT NULL,
    declared_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX idx_claims_unique_active
    ON identity_claims (ilk_id, facet, key)
    WHERE revoked_at IS NULL;

CREATE INDEX idx_claims_ilk
    ON identity_claims (ilk_id)
    WHERE revoked_at IS NULL;

CREATE INDEX idx_claims_canonical
    ON identity_claims (canonical, facet, key)
    WHERE revoked_at IS NULL;
```

### 9.4 `identity_claim_recognitions`

```sql
CREATE TABLE identity_claim_recognitions (
    recognition_id UUID PRIMARY KEY,
    claim_id UUID NOT NULL REFERENCES identity_claims(claim_id),
    recognizer_ilk_id UUID NOT NULL REFERENCES identity_ilks(ilk_id),
    recognized_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX idx_recognitions_unique_active
    ON identity_claim_recognitions (claim_id, recognizer_ilk_id)
    WHERE revoked_at IS NULL;

CREATE INDEX idx_recognitions_claim
    ON identity_claim_recognitions (claim_id)
    WHERE revoked_at IS NULL;
```

---

## 10. SHM Layout (v3)

### 10.1 Region

`jsr-identity-<hive>` — extended from v2.

### 10.2 New structures

```rust
#[repr(C)]
pub struct ClaimEntry {
    pub claim_id: [u8; 16],
    pub facet: u8,           // 0=domain, 1=function, 2=responsibility
    pub canonical: u8,       // 0 or 1
    pub _pad: [u8; 2],
    pub key: [u8; 64],       // null-padded ASCII
    pub declared_by: [u8; 16],
    pub declared_at_ms: u64,
    pub flags: u16,          // active, revoked, stale
    pub recognition_count: u16,
    pub recognizers: [[u8; 16]; MAX_RECOGNITIONS_PER_CLAIM],  // 16 slots
}

pub const MAX_CLAIMS_PER_ILK: usize = 32;
pub const MAX_RECOGNITIONS_PER_CLAIM: usize = 16;
```

Each `IlkEntry` gains:

```rust
pub struct IlkEntry {
    // ... existing v2 fields (without roles/capabilities/degrees)
    pub company: [u8; 128],        // new
    pub institutional_role_ptr: u32,  // offset to InstitutionalRoleEntry, 0 if none
    pub claim_count: u16,           // new
    pub claims: [ClaimEntry; MAX_CLAIMS_PER_ILK],  // new, fixed array
}

pub struct InstitutionalRoleEntry {
    pub office: [u8; 64],
    pub jurisdiction_count: u8,
    pub jurisdiction: [[u8; 32]; 8],  // up to 8 jurisdictions
    pub charter_ref: [u8; 128],
}
```

### 10.3 Writer / readers

Writer: `SY.identity` primary. Replica workers receive updates via the existing identity sync channel (same mechanism as v2).

Readers: `router` (for pre-OPA canonicalization and OPA data bundle construction), `SY.admin` (for introspection queries).

All reads use the existing seqlock protocol.

### 10.4 OPA data bundle projection

When the router builds the OPA data bundle, it projects claims into an indexed form for O(1) lookup from Rego. This is the critical optimization for authorization performance.

Raw SHM structure:
```
IlkEntry.claims = [ClaimEntry, ClaimEntry, ...]  // linear array
```

Router projection into `data.identity[ilk_id]`:
```json
{
  "ilk:550e8400-...": {
    "ilk_type": "agent",
    "registration_status": "complete",
    "tenant_id": "tnt:...",
    "institutional_role": { "office": "..." },
    "claims_index": {
      "function/can_invoice": {
        "canonical": true,
        "statement": "...",
        "declared_by": "ilk:system",
        "recognizer_count": 2
      },
      "domain/billing": {
        "canonical": true,
        "statement": "...",
        "declared_by": "ilk:gov.identity.frontdesk",
        "recognizer_count": 0
      }
    }
  }
}
```

The projection key format is `<facet>/<key>`. This makes Rego queries O(1):

```rego
has_claim(ilk, facet, key) {
    data.identity[ilk].claims_index[sprintf("%s/%s", [facet, key])]
}

has_canonical_claim(ilk, facet, key) {
    data.identity[ilk].claims_index[sprintf("%s/%s", [facet, key])].canonical
}
```

The router rebuilds this projection whenever:

- The OPA bundle is reloaded (on `OPA_RELOAD`).
- The identity SHM `seq` advances and the bundle is cached (periodic rebuild, bounded frequency).

---

## 11. OPA Integration

### 11.1 Data consumed

From `data.identity[ilk]`:

- `ilk_type`, `registration_status`, `tenant_id` (existing)
- `company` (new)
- `institutional_role` (new)
- `claims_index` (new, as projected in 10.4)

From `data.identity_aliases` (existing): merge alias resolution.

### 11.2 Standard helper rules

Helper Rego functions provided as part of the default policy scaffold:

```rego
has_claim(ilk, facet, key) {
    data.identity[ilk].claims_index[sprintf("%s/%s", [facet, key])]
}

has_canonical_claim(ilk, facet, key) {
    c := data.identity[ilk].claims_index[sprintf("%s/%s", [facet, key])]
    c.canonical == true
}

has_recognized_claim(ilk, facet, key) {
    c := data.identity[ilk].claims_index[sprintf("%s/%s", [facet, key])]
    c.recognizer_count > 0
}

has_institutional_office(ilk, office) {
    data.identity[ilk].institutional_role.office == office
}
```

### 11.3 Example policies

Authorize disparo of WF.invoice:

```rego
allow_trigger_wf_invoice {
    input.meta.action == "trigger_wf"
    input.meta.wf_target_type == "WF.invoice"
    has_canonical_claim(input.meta.src_ilk, "function", "can_invoice")
}
```

Route temporary ILKs to frontdesk:

```rego
target := "SY.frontdesk.gov@" + hive {
    data.identity[input.meta.src_ilk].registration_status == "temporary"
    hive := data.hive_id
}
```

---

## 12. SDK Helpers (Rust `fluxbee_sdk`)

New helpers in `fluxbee_sdk::identity`:

```rust
pub async fn declare_claim(
    sender: &Sender,
    receiver: &mut Receiver,
    target_ilk: &str,
    facet: Facet,
    key: &str,
    statement: &str,
) -> Result<DeclareClaimResult>;

pub async fn revoke_claim(
    sender: &Sender,
    receiver: &mut Receiver,
    claim_id: &str,
) -> Result<RevokeClaimResult>;

pub async fn recognize_claim(
    sender: &Sender,
    receiver: &mut Receiver,
    claim_id: &str,
) -> Result<RecognizeClaimResult>;

pub async fn unrecognize_claim(
    sender: &Sender,
    receiver: &mut Receiver,
    claim_id: &str,
) -> Result<UnrecognizeClaimResult>;

pub async fn list_canonical_claims(
    sender: &Sender,
    receiver: &mut Receiver,
) -> Result<Vec<CanonicalClaimInfo>>;

pub fn has_claim_in_shm(
    shm: &IdentitySHMReader,
    ilk_id: &str,
    facet: Facet,
    key: &str,
) -> Result<bool>;
```

`has_claim_in_shm` is a local SHM read for nodes that need to check claims without doing a round-trip to SY.identity. It reads the projected claims directly from the SHM region.

---

## 13. SY.admin Surface (v3 additions)

New read-only endpoints for claims inspection:

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/hives/{hive}/identity/ilks/{ilk_id}/claims` | List all active claims of an ILK, with recognition counts |
| `GET` | `/hives/{hive}/identity/claims/{claim_id}` | Full detail of one claim, including list of recognizers |
| `GET` | `/identity/canonical-claims` | Full canonical catalog from the current binary (proxied from `IDENTITY_LIST_CANONICAL_CLAIMS`) |
| `GET` | `/hives/{hive}/identity/canonical-claims` | Same, scoped to a specific hive |

No mutation endpoints in v3 — claim declaration and recognition always go through the SDK directly to SY.identity via system messages.

---

## 14. Error Codes (v3 additions)

| Code | Description |
|---|---|
| `INVALID_CLAIM_KEY` | Key does not match `^[a-z][a-z0-9_]{0,63}$`. |
| `INVALID_FACET` | Facet is not one of `domain`, `function`, `responsibility`. |
| `CLAIM_ALREADY_EXISTS` | Active claim with same `(ilk, facet, key)` already exists. |
| `CLAIM_NOT_FOUND` | Claim ID does not exist. |
| `CLAIM_ALREADY_REVOKED` | Claim is already revoked. |
| `TOO_MANY_CLAIMS` | ILK would exceed `MAX_CLAIMS_PER_ILK` (32). |
| `TOO_MANY_RECOGNITIONS` | Claim would exceed `MAX_RECOGNITIONS_PER_CLAIM` (16). |
| `RECOGNITION_ALREADY_EXISTS` | Recognizer already recognizes this claim. |
| `RECOGNITION_NOT_FOUND` | Recognition ID does not exist. |
| `CANNOT_RECOGNIZE_OWN_CLAIM` | Holder ILK attempted to recognize its own claim. |
| `UNAUTHORIZED_DECLARER` | Requester cannot declare claims on behalf of the target ILK. |
| `UNAUTHORIZED_REVOKER` | Requester cannot revoke this claim (not original declarer and not orchestrator). |
| `INVALID_INSTITUTIONAL_ROLE_TARGET` | Attempted to set institutional_role on a human ILK. |
| `INVALID_CHARTER_REF` | Charter ref does not follow URI format. |

---

## 15. What v3 Does Not Include

These items are explicitly out of scope for v3 and deferred to later iterations:

- **Normative enforcement** — Constitution and charters are referenced but not interpreted or enforced.
- **Trust or reputation engine** — recognition is tracked but not aggregated into trust scores.
- **Degree / graduation lifecycle** — completely removed from v3. If needed in the future, handled by a separate SY service.
- **Claim templates or inheritance** — no "role" abstraction on top of claims.
- **Time-bound or conditional claims** — claims are either active or revoked, nothing in between.
- **Claim types per-tenant** — single global vocabulary for the canonical catalog.
- **Multi-catalog support** — single embedded catalog per SY.identity binary.
- **Claims migration from v2** — no migration. v2 data is considered historical.

---

## 16. Decisiones de diseño

| Decisión | Razón |
|---|---|
| Claims reemplazan roles/capabilities | Modelo descriptivo más alineado con entidades de Fluxbee |
| Tres facets fijos (`domain`, `function`, `responsibility`) | Suficiente para v3, evita complejidad taxonómica |
| Canonical catalog embedded as code | Elimina archivo separado, versionado paralelo, validación runtime |
| Defaults por pattern de nombre L2 | Nodos institucionales nacen con claims sin que orchestrator los transporte |
| Recognition sin autorización | Cualquier ILK puede reconocer; si aparece abuso, se agregan reglas OPA |
| Sin migración desde v2 | Sistema está en alpha/piloto, no hay datos productivos que preservar |
| `company` como texto libre | Sin tabla separada hasta que haya necesidad concreta |
| OPA data bundle projection O(1) | Performance en hot path de routing |
| Límites 32 claims/ILK, 16 recogn/claim | Dimensionamiento SHM razonable, no genera dolor práctico |
| Normative layer conceptual, no implementada | Alcance v3 acotado, charters son referencias opacas |
| `IDENTITY_LIST_CANONICAL_CLAIMS` sin auth | Introspección pública del vocabulario del sistema |

---

## 17. References

| Topic | Document |
|---|---|
| Previous version | `10-identity-v2.md` (archived) |
| Conceptual model | `fluxbee_identity_claims_normative_model.md` |
| Architecture | `01-arquitectura.md` |
| Protocol | `02-protocolo.md` |
| SHM regions | `03-shm.md` |
| Routing / OPA | `04-routing.md` |
| Router status | `09-router-status.md` |
| WF nodes (first consumer of v3 claims for authorization) | `wf-v1.md` |
| SY.timer (pattern for direct-call system nodes) | `sy-timer.md` |
