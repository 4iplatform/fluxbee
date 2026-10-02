# Fluxbee - 10 Identity (SY.identity) v2

**Status:** v2.0 (rewrite)
**Date:** 2026-03-10
**Audience:** All developers (IO, AI, WF, SY, router, OPA)
**Replaces:** `10-identity-layer3.md` (v1.15)

> **2026-05-06 update:** Agent identity definition is being consolidated by `identity-v2.1-agent-definition-addendum.md`. The older `roles/capabilities/degrees` model in this document is superseded for AI agents by hash-based role/skill/handbook references stored in `identity_ilks.definition` and projected through SHM/router. Treat examples that route by `data.identity[ilk].capabilities` as historical design notes until this document is fully rewritten.
>
> **2026-10-02 update (operator decisions):** the frontdesk (`SY.frontdesk.gov`) never creates a tenant: a person is registered into the tenant the case already has, the one SY.identity holds for the temporary ILK (§6.2, §6.4). An `ILK_REGISTER` whose email already belongs to another human ILK of the tenant merges into that ILK instead of failing (§6.5). Only an active tenant takes registrations. The controlled vocabulary (§8) is superseded.

---

## 1. Purpose

SY.identity is the central registry of all entities that participate in the system. Its responsibilities are:

1. **Generate identities** — assign unique ILK to every entity (human, agent, system).
2. **Associate identities** — link ILKs to tenants, channels (ICH), and nodes.
3. **Maintain metadata** — keep identification, association, and definition data current.
4. **Verify identities** — validate that an entity matches its claimed identity.
5. **Serve identity data** — make the full identity dataset available in SHM for real-time routing (L3/OPA).

SY.identity does NOT handle:

- Authentication (edge/IO responsibility).
- Routing permissions (OPA responsibility).
- Knowledge module management, degree compilation, or graduation lifecycle from the older v2 degree model.
- Asset content for agent cognitive definitions. Identity stores hash references only; Archi owns asset generation/catalog and `ai.generic` owns prompt composition.

---

## 2. Identifier Format Convention

All entity identifiers follow a single convention across the entire system:

| Entity | Protocol format (JSON, messages, OPA) | SHM format (binary) | DB format |
|--------|---------------------------------------|---------------------|-----------|
| Tenant | `tnt:<uuid-v4>` | `[u8; 16]` raw UUID | `UUID` column |
| ILK | `ilk:<uuid-v4>` | `[u8; 16]` raw UUID | `UUID` column |
| ICH | `ich:<uuid-v4>` | `[u8; 16]` raw UUID | `UUID` column |

**Rules:**

- In JSON messages, OPA rules, and API responses: always use the prefixed string format (`ilk:550e8400-...`).
- In SHM binary structures: always use raw 16-byte UUID (no prefix, for space efficiency and fast comparison).
- In PostgreSQL: always use native `UUID` type (no prefix).
- SY.identity handles conversion between formats at the SHM read/write boundary.
- Prefixed strings are the canonical external representation. Raw bytes are an internal optimization.

---

## 3. Core Entities

### 3.1 TNT (Tenant)

A tenant is an organization, company, or account. It is the top-level partition of the system. Every ILK belongs to exactly one tenant.

```json
{
  "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
  "name": "Acme Corp",
  "domain": "acme.com",
  "status": "active",
  "created_at": "2026-03-10T10:00:00Z",
  "approved_by": "ilk:7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "settings": {
    "max_ilks": 10000,
    "allowed_channels": ["whatsapp", "slack", "email"]
  }
}
```

**Rules:**

- `status`: `pending | active | suspended`. Only an `active` tenant takes registrations (`ILK_REGISTER` answers `TENANT_PENDING` / `TENANT_SUSPENDED`, and `TENANT_DELETED` for a marked one).
- A default tenant (`fluxbee`) is created automatically during motherbee bootstrap. All system nodes (SY.*, RT.*) are registered under this tenant.
- That default tenant is the root tenant (`tnt:00000000-0000-0000-0000-000000000001`, fixed by code). It holds the system, never a person: nobody registers into it (operator decision 2026-10-02). An `ILK_REGISTER` of a person into it answers `TENANT_ROOT_NOT_REGISTRABLE`; node ILKs (`agent`) still register there, and IO nodes running in it still provision temporary ILKs, which stay temporary (§6.2).
- Subsequent tenants are created by the operator through SY.admin, by Fluxbee Cloud with its own process (io.cloud `create_tenant`, relayed by SY.admin), or by SY.architect. The frontdesk never creates one (§6.4).
- Without at least one active tenant, no ILK can be registered and no node can be spawned.

### 3.2 ILK (Interlocutor Key)

An ILK is the unique identity of any entity that participates in messaging: humans, AI agents, system processes, and workflows.

**Format:** `ilk:<uuid-v4>` — always UUID v4, always prefixed `ilk:`. Human-readable names are metadata, never identifiers. All ILKs follow this format, including temporary ones (see section 6.2).

**Types:**

| Type | Description | Registered by | Has ICH |
|------|-------------|---------------|---------|
| `human` | Person (operator, customer, admin) | SY.frontdesk.gov | Yes |
| `agent` | AI node that processes messages | SY.orchestrator | Optional |
| `system` | Workflow, bot, integration, sensor | SY.orchestrator | Optional |

**Rules:**

- ILK is immutable once created (the UUID never changes).
- Every ILK belongs to exactly one tenant.
- ILK persists across node restarts — same node_name = same ILK.
- ILKs can be created through two actions:
  - `ILK_PROVISION` (IO nodes only): creates temporary ILKs for unknown channels.
  - `ILK_REGISTER` (SY.orchestrator and SY.frontdesk.gov only): creates node ILKs or upgrades temporary human ILKs (never into the root tenant, §3.1).
- Temporary ILKs (registration_status=temporary) are real ILKs with valid UUIDs, not pseudo-identifiers.

### 3.3 ICH (Interlocutor Channel)

An ICH is a communication channel owned by a single ILK. In the current canonical model, it represents a **local/system-owned operational channel** exposed or operated by Fluxbee through an IO node or IO-node instance: for example, a WhatsApp number owned by the system, a Slack app/channel operated by the system, a mailbox owned by the system, or an API endpoint/asset exposed by the system.

**Format:** `ich:<uuid-v4>`

**Rules:**

- Each ICH belongs to exactly one ILK (1:N relationship, ILK has many ICHs).
- In the active model, the owning ILK is the **internal ILK of the IO node/instance** that operates that channel/asset.
- ICH identifies the local/system channel, not the external handle of the remote interlocutor.
- ICH is embedded in the ILK metadata (not a separate entity with its own lifecycle).
- External interlocutor handles (`+54...`, `user@mail.com`, Slack user IDs, etc.) are not ICHs. They belong to identity/contact/alias resolution outside the ownership semantics of ICH.

---

## 4. ILK Metadata Structure

Every ILK carries metadata organized in three dimensions:

### 4.1 Identification — who is this entity

Data that uniquely distinguishes the entity. Varies by type.

**Human:**
```json
{
  "display_name": "Juan Pérez",
  "email": "juan@acme.com",
  "phone": "+5411...",
  "document_type": "DNI",
  "document_number": "12345678"
}
```

**Agent:**
```json
{
  "display_name": "AI.soporte.l1",
  "node_name": "AI.soporte.l1@produccion",
  "runtime": "ai.soporte",
  "runtime_version": "1.2.0"
}
```

**System:**
```json
{
  "display_name": "WF.onboarding",
  "node_name": "WF.onboarding@produccion",
  "service_type": "workflow"
}
```

### 4.2 Association — what it belongs to and how it communicates

```json
{
  "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
  "channels": [
    {
      "ich_id": "ich:a1b2c3d4-5678-90ab-cdef-1234567890ab",
      "type": "whatsapp",
      "address": "+5411...",
      "added_at": "2026-03-10T10:00:00Z",
      "is_primary": true
    },
    {
      "ich_id": "ich:b2c3d4e5-6789-0abc-def1-234567890abc",
      "type": "slack",
      "handle": "@juan",
      "added_at": "2026-03-15T14:00:00Z"
    }
  ]
}
```

Interpretation note:

- `channels[].type` and `channels[].address` remain the current storage shape in core.
- In the canonical semantics, those fields should be read as the **local/system asset/channel material** operated by Fluxbee.
- They must not be interpreted as saying that the remote interlocutor owns the ICH.

### 4.3 Definition — cognitive references

The earlier roles/capabilities/degrees model below is superseded for AI agents by the v2.1 addendum.

For `ilk_type="agent"`, `definition` now means hash references to cognitive assets:

```json
{
  "role_hash": "64hex...",
  "skill_hashes": ["64hex..."],
  "handbook_hashes": ["64hex..."]
}
```

The referenced JSON assets live in blob under `agent-assets/<hash>.json`. Identity stores hashes only. `ai.generic` resolves the assets and composes the active prompt. OPA can route by hashes after router projection, but it does not read blob content.

Historical v2 shape, retained here only for context:

```json
{
  "current": {
    "roles": ["operator", "supervisor"],
    "capabilities": ["billing", "refund", "escalation"],
    "degrees": [
      {
        "degree_id": "uuid-v4",
        "name": "Soporte L2 Billing",
        "modules": ["billing_basics", "refund_policy", "escalation_protocol"],
        "degree_hash": "sha256:...",
        "graduated_at": "2026-03-10T10:00:00Z",
        "issued_by": "ilk:uuid-of-issuer"
      }
    ]
  },
  "history": [
    {
      "snapshot_at": "2026-03-01T10:00:00Z",
      "roles": ["operator"],
      "capabilities": ["billing"],
      "degrees": [],
      "change_reason": "initial registration"
    }
  ]
}
```

**Historical rules:**

- This shape is not the forward contract for AI agents.
- New implementation should use `ILK_SET_DEFINITION` and hash references.

### 4.4 Complete ILK Document

```json
{
  "ilk_id": "ilk:550e8400-e29b-41d4-a716-446655440000",
  "ilk_type": "human",
  "registration_status": "complete",
  "created_at": "2026-03-10T10:00:00Z",
  "registered_at": "2026-03-10T10:05:00Z",
  "registered_by": "ilk:7c9e6679-7425-40de-944b-e07fc1f90ae7",

  "identification": { ... },
  "association": { ... },
  "definition": { ... }
}
```

**Registration status:**

| Status | Meaning |
|--------|---------|
| `temporary` | First contact — ILK created with ICH only, no identification data yet, pending frontdesk registration |
| `partial` | Some identification data, registration in progress |
| `complete` | Minimum required data present, fully operational |

---

## 5. Architecture

### 5.1 Motherbee / Worker Model

```
┌─────────────────────────────────────────────────────────────┐
│                        Motherbee                            │
│                                                             │
│  ┌──────────────┐      ┌─────────────────────────────────┐ │
│  │ PostgreSQL   │◄────►│ SY.identity@motherbee           │ │
│  │              │      │ mode: PRIMARY                   │ │
│  │ - tenants    │      │                                 │ │
│  │ - ilks       │      │ • Read/write DB (own tables)   │ │
│  │ - ichs       │      │ • Write jsr-identity-<hive>    │ │
│  │ - vocabulary │      │ • Propagate deltas via socket  │ │
│  └──────────────┘      │ • Accept registrations          │ │
│                        └─────────────────────────────────┘ │
│                                    │                        │
│                                    │ socket (deltas)        │
└────────────────────────────────────┼────────────────────────┘
                                     │
                                     │ WAN
                                     ▼
┌─────────────────────────────────────────────────────────────┐
│                         Worker                              │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ SY.identity@worker                                   │   │
│  │ mode: REPLICA                                        │   │
│  │                                                      │   │
│  │ • Receive deltas from motherbee                     │   │
│  │ • Write jsr-identity-<hive> (local SHM)            │   │
│  │ • Forward registration requests to motherbee        │   │
│  │ • NO database access                               │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
│        /dev/shm/jsr-identity-<hive>                        │
│        ┌───────────────────────────────────────────┐       │
│        │ Full identity dataset (all ILKs, all TNTs) │       │
│        └───────────────────────────────────────────┘       │
│                          ▲                                  │
│              ┌───────────┼───────────┬──────────┐          │
│              │           │           │          │          │
│        IO.whatsapp   AI.support    OPA      WF.crm        │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### 5.2 Database Ownership

SY.identity PRIMARY writes directly to PostgreSQL for its own domain tables. This is a formal exception to the general rule that SY.storage is the DB writer. The domains are strictly separated and do not overlap:

| Writer | Domain | Tables |
|--------|--------|--------|
| SY.storage | Cognitive persistence | turns, events, memory_items |
| SY.identity | Identity registry | identity_tenants, identity_ilks, identity_ichs, identity_vocabulary |

This exception exists because identity registration requires synchronous confirmation (orchestrator cannot spawn a node until the ILK is confirmed in DB), making async fire-and-forget via NATS unsuitable.

### 5.3 SHM: Full Dataset in Every Worker

The complete identity dataset (all tenants, all ILKs with full metadata) is loaded into SHM in every worker. This is required because OPA needs the full identity data for L3 routing decisions without round-trips.

**Memory budget:**

| ILKs | Approx. SHM size | Min worker RAM |
|------|-------------------|----------------|
| 10K | 5 MB | 4 GB |
| 100K | 50 MB | 4 GB |
| 500K | 250 MB | 8 GB |
| 1M | 500 MB | 16 GB |

SY.identity sizes the SHM region from `identity.max_ilks` in `hive.yaml` (default 8192 in the code today). It does not refuse a registration at that limit — there is no `IDENTITY_LIMIT_REACHED`: an ILK beyond the region's capacity is persisted and replicated, but its SHM write fails and is logged. Identity exposes the current counts as a metric for capacity planning.

### 5.4 Synchronization

Identity synchronization uses direct socket connections between SY.identity instances. It does NOT use router broadcasts, CONFIG_CHANGED messages, or NATS. This is because:

- Identity deltas can exceed 64KB (full ILK with history).
- Identity traffic should not load the router path.
- Full sync at boot requires streaming, not single messages.

Identity is NOT a CONFIG_CHANGED subsystem. It does not participate in the CONFIG_CHANGED/CONFIG_RESPONSE pattern used by routes, vpns, opa, and storage.

**Full sync (cold start / worker boot):**

Worker SY.identity connects to motherbee SY.identity via socket, requests full dataset, receives it in chunks, and writes to local SHM. This may take seconds for large datasets — acceptable at boot time only.

**Delta sync (runtime):**

When an ILK or TNT is created, updated, or deleted, motherbee SY.identity propagates the individual change via socket to all connected worker SY.identity instances. Workers apply the delta to their local SHM. Latency: sub-millisecond for the SHM write once the delta arrives.

---

## 6. Registration Flows

### 6.1 System Bootstrap

```
1. Install motherbee, start SY.identity (empty DB).
2. SY.identity auto-creates default tenant "fluxbee" (status: active).
3. SY.orchestrator registers ILKs for system nodes (SY.*, RT.*) under "fluxbee" tenant.
4. System nodes start with identity.
5. SY.frontdesk.gov starts (a system node of the motherbee).
6. Humans can now register via frontdesk, into the tenants created afterwards (§6.4), never
   into "fluxbee" (§3.1).
```

### 6.2 Human Registration (via SY.frontdesk.gov)

```
1. Unknown person sends message via WhatsApp.
2. IO.whatsapp generates ICH (first contact, no local retention found).
3. IO.whatsapp checks SHM: no ILK for this ICH.
4. IO.whatsapp requests a temporary ILK from SY.identity:
   - sends ILK_PROVISION with ich_id + channel_type + address + tenant_id: its own tenant
     (io.api / io.cloud: the tenant they were called for).
   - SY.identity creates a real ILK (UUID v4, registration_status=temporary) in that tenant
     (the default tenant `fluxbee` when the IO node sends none: a person there cannot be
     registered, see below), associates the ICH, persists in DB, propagates to SHM, then answers.
   - IO.whatsapp receives the ILK UUID back.
5. IO.whatsapp sends message with the real (temporary) ILK as meta.src_ilk.
6. The router sees registration_status=temporary → routes to SY.frontdesk.gov
   (or the IO node hands the case over as a structured `frontdesk_handoff`).
7. SY.frontdesk.gov collects the minimum data:
   - email (required; it identifies the person within the tenant)
   - name (required)
   It never asks for a tenant.
8. SY.frontdesk.gov sends ILK_REGISTER to SY.identity@motherbee:
   - ilk_id (the temporary ILK already created)
   - identification data
   - tenant_id: the tenant of the case, the one SY.identity holds for the temporary ILK
     (read from SHM). A `frontdesk_handoff.tenant_id` must be that same tenant. When it cannot
     be read, nothing is registered (`TENANT_UNRESOLVED`) and no tenant is created. When it is
     the root tenant, nothing is registered either: the frontdesk answers
     `TENANT_NOT_REGISTRABLE` without calling SY.identity.
9. SY.identity validates and upgrades the ILK:
   - the tenant must be active (TENANT_PENDING / TENANT_SUSPENDED / TENANT_DELETED otherwise)
     and not the root tenant (TENANT_ROOT_NOT_REGISTRABLE);
   - if the email already belongs to another human ILK of the tenant, the person is already
     registered: merge into that ILK (§6.5);
   - otherwise status `temporary` → `complete`; persists and propagates.
10. IO.whatsapp continues using the same ILK — it was always a real UUID (after a merge, its
    channel resolves to the registered ILK).
11. Subsequent messages route normally based on tenant.
```

**Key design decision:** The temporary ILK is a real `ilk:<uuid-v4>` from the start. There are no pseudo-identifiers, no special format, no format inconsistencies. The only difference is `registration_status=temporary` which OPA uses to route to frontdesk.

**Tenant of a registration:** the frontdesk takes it from the case, never from the person, the LLM, its own config or its environment, and never creates one (§6.4). SY.identity itself still accepts an `ILK_REGISTER` that moves a *temporary* ILK to another active tenant (a complete one answers `INVALID_TENANT_TRANSITION`); the frontdesk never asks for that.

**Nobody registers into the root tenant** (operator decision 2026-10-02). SY.identity refuses, with `TENANT_ROOT_NOT_REGISTRABLE` and before any merge by email, every `ILK_REGISTER` that would put a person in the root tenant: one with `ilk_type: human`, or one of an ILK that is a human one (such as a temporary, whatever `ilk_type` it asks for). Every caller gets it; the frontdesk answers it by itself (`TENANT_NOT_REGISTRABLE`), and io.cloud `register_human` refuses the root tenant before provisioning anything. Node ILKs (`agent`) still register there. `ILK_PROVISION` is not restricted: an IO node running in the root tenant, or one that sends no tenant, still provisions the people who write in, and they stay `temporary`. Their messages keep reaching the frontdesk wherever the router force-routes temporaries to it (§14), and it answers that it cannot register them. The base IO instances `fluxbee-firstboot` spawns (`IO.api@motherbee`, `IO.wapp.default`, ...) run in the root tenant, so the people who reach the hive through them are in that situation; what IO nodes do in the root tenant is pending an operator decision.

**Authorized registrars:** SY.identity validates source authorization at two levels:

1. **Router/OPA level:** OPA can reject ILK_REGISTER/ILK_PROVISION from unauthorized sources before they reach identity.
2. **SY.identity level (authoritative):** action-scoped allowlists (`SY.admin`, `SY.architect` and `SY.frontdesk.gov` only from SY.identity's own hive):
   - `ILK_PROVISION`: `IO.*@*`
   - `ILK_REGISTER`: `SY.frontdesk.gov@<hive>`, `SY.orchestrator@*`
   - `ILK_ADD_CHANNEL`: `IO.*@*`, `SY.frontdesk.gov@<hive>`
   - `ILK_UPDATE` (node/system metadata): `SY.orchestrator@*`
   - `TNT_CREATE`, `TNT_UPDATE`, `TNT_SET_SPONSOR`: `SY.admin@<hive>`, `SY.architect@<hive>` (never the frontdesk)
   - `TNT_APPROVE`: `SY.admin@<hive>`
   Requests outside allowlist are rejected with `UNAUTHORIZED_REGISTRAR`.
   This is the definitive enforcement — OPA is defense in depth.

### 6.3 Node Registration (via SY.orchestrator)

```
1. SY.admin receives request to spawn AI.soporte.l1.
2. SY.admin forwards to SY.orchestrator.
3. SY.orchestrator checks SY.identity: does ILK exist for node_name "AI.soporte.l1"?
   a. If yes: reuse existing ILK (node restart case).
   b. If no: send ILK_REGISTER to SY.identity with:
      - ilk_type: agent
      - identification: node_name, runtime, version
      - association: tenant_id (from runtime config or default "fluxbee")
      - definition: roles and capabilities from runtime manifest
4. SY.identity creates ILK, persists, propagates.
5. SY.orchestrator receives confirmation, spawns node.
6. Node starts with its ILK assigned.
```

**Implementation note (2026-03-16):** `SY.orchestrator` executes `ILK_REGISTER` and `ILK_UPDATE` through `fluxbee_sdk::identity::identity_system_call_ok` (relay wrapper local). The SDK centralizes transport/protocol behavior (timeouts, unreachable/ttl, response parsing). Orchestrator keeps spawn business rules (target `SY.identity@motherbee`, payload assembly, node->ilk persistence, and external admin contract).

**Fail-closed registration gate (mandatory):**

- `SY.orchestrator` MUST receive `ILK_REGISTER_RESPONSE` with `payload.status="ok"` before spawning a node.
- If registration cannot be completed (for example: missing `tenant_id`, identity unavailable, transport failure, or non-ok register response), `run_node` MUST fail and return `IDENTITY_REGISTER_FAILED`.
- There is no soft-fail mode for node spawn without successful identity registration.

**Node ILK persistence:** The ILK for a node persists even when the node is stopped. Same node_name = same ILK across restarts. If the node's definition changes (new capabilities, new runtime version), orchestrator sends an ILK_UPDATE to identity, and the change is recorded in the definition history.

### 6.4 Tenant Creation

**During bootstrap:** Automatic, no approval needed (default "fluxbee" tenant).

**Via SY.admin:** The operator creates tenants directly (`TNT_CREATE`; `status` defaults to `pending`, and a name or domain that matches an existing tenant returns that tenant with `created: false`). Fluxbee Cloud creates its tenants with its own process through io.cloud `create_tenant`, which SY.admin relays. SY.architect may create them too. A `pending` tenant is activated by SY.admin (`TNT_APPROVE`).

**Never via the frontdesk** (operator decision 2026-10-02): a message from a person who is not registered cannot create a tenant. SY.frontdesk.gov registers the person into the tenant the case already has (§6.2), and SY.identity does not authorize it for `TNT_CREATE`, `TNT_UPDATE` or `TNT_SET_SPONSOR`. A case without a readable tenant is not registered (`TENANT_UNRESOLVED`).

### 6.5 Known Person on a New Channel: Merge by Email

The email identifies a person within a tenant. When a registered person appears on a new channel:

```
1. Juan (registered via WhatsApp, juan@acme.com, tenant Acme) writes from Slack.
2. IO.slack generates new ICH, checks SHM: no ILK for this ICH.
3. IO.slack requests ILK_PROVISION → new temporary ILK in Acme.
4. Message routed to SY.frontdesk.gov (temporary ILK flow).
5. SY.frontdesk.gov asks for identification → Juan provides name and email.
6. SY.frontdesk.gov sends ILK_REGISTER for the temporary ILK, as for anyone else.
7. SY.identity finds juan@acme.com on another human ILK of Acme and merges, inside the same
   ILK_REGISTER:
   - Juan's ILK keeps its id, its tenant and its `complete` status;
   - the temporary ILK's channels MOVE to Juan's ILK (lookups follow; the temporary keeps none);
   - alias temporary_ilk -> Juan's ILK until `merge_alias_ttl_secs` (§6.6);
   - identification fields Juan's ILK has empty are filled from the registration; a field that
     already has a value is kept;
   - reply: `merged: true`, `ilk_id` = Juan's ILK, `merged_from_ilk_id` = the temporary ILK.
8. SY.frontdesk.gov answers `MERGED`. IO.slack sees updated SHM: this ICH now resolves to
   Juan's ILK.
```

Rules:

- Email is unique per tenant, like its DB index `(email, tenant_id)`: the same email in another tenant is another person, and a marked (deleted) ILK reserves its email only in its own tenant.
- Only a temporary ILK of the same tenant, registered as `human`, merges, and only into a human ILK. When the email belongs to another ILK and the registration cannot merge (the registering ILK is already complete, belongs to another tenant or does not exist, or the registration or the email's holder is not human), the answer is `DUPLICATE_EMAIL` and nothing changes.
- Fill-only is the merge's rule. An explicit update overwrites: an `ILK_REGISTER` of the registered ILK itself (for example a repeated Cloud `register_human` for the same email) replaces its identification whole — last write wins. `ILK_UPDATE` does not touch identification (§12.4).
- Node ILKs (SY.orchestrator) carry `node_name`, not email, and never merge: the same `node_name` keeps resolving to the same ILK.
- Nothing merges into the root tenant: a person's registration there is refused before the email is looked at (`TENANT_ROOT_NOT_REGISTRABLE`, §6.2).
- There is no proof of email ownership yet: whoever types another person's email gets their channel attached to that person's ILK. Verification (for example a one-time code) is a later item.
- `ILK_ADD_CHANNEL` with `merge_from_ilk_id` (§12.3) is the same merge, addressed explicitly.

### 6.6 Temporary ILK Merge Semantics

When identity merges a temporary ILK into an existing ILK (same person, new channel), it must preserve in-flight safety:

1. Move the temporary ILK's channels to the canonical ILK. The DB moves their ICH rows, the alias and both ILK rows in one transaction; replicas receive both ILKs and the alias.
2. Create alias mapping `old_ilk_id -> canonical_ilk_id`.
3. Keep alias active for grace window (`merge_alias_ttl_secs`, default 3600).
4. During grace window, routing/OPA canonicalizes `src_ilk` through alias map before policy evaluation.
5. After grace window, mark old temporary ILK as soft-deleted and remove alias entry.

This avoids message loss or inconsistent routing for in-flight messages carrying the old temporary ILK.

---

## 7. IO Node Behavior with Identity

### 7.1 ICH Lifecycle

1. IO node starts. Checks local retention for previously generated ICH.
2. If no retention (first start): generates new ICH (UUID v4), retains locally.
3. IO reads SHM `jsr-identity-<hive>` to find which ILK is associated with its ICH.
4. If ILK found: uses it for all outgoing messages (`meta.src_ilk`).
5. If no ILK found: sends ILK_PROVISION to SY.identity to create a temporary ILK, then uses the returned ILK UUID.

### 7.2 Automatic Update

When SY.identity updates an ILK (e.g., frontdesk completes registration), the change propagates to SHM. The IO node sees the updated association on its next SHM read — no restart, no notification, no configuration change needed.

---

## 8. Controlled Vocabulary (superseded)

> **Superseded** by `identity-v2.1-agent-definition-addendum.md` §2.4: roles and capabilities are retired, and nothing in this section is implemented. SY.identity keeps no vocabulary (the SHM vocabulary table stays empty) and has no vocabulary action, SY.admin exposes no `/identity/vocabulary` API (§13.3), `ILK_REGISTER` / `ILK_UPDATE` reject `roles` and `capabilities` as unknown fields, and the frontdesk translates nothing into tags. Kept as history only.

### 8.1 Concept

Roles and capabilities are not free-form strings. They are tags from a controlled vocabulary maintained by SY.identity and administered via SY.admin.

**Rules:**

- Lowercase English, no spaces, no accents. Use underscores for multi-word tags.
- Self-explanatory (e.g., `billing`, `support_l1`, `escalation`, `registration`).
- New vocabulary entries are added via SY.admin API.
- SY.identity validates every role/capability assignment against the vocabulary. Invalid tags are rejected.
- AI.frontdesk translates from the interlocutor's language to the correct vocabulary tag.

### 8.2 Administration

```
GET  /identity/vocabulary              — list all valid roles and capabilities
POST /identity/vocabulary              — add new vocabulary entry
DELETE /identity/vocabulary/{tag}       — deprecate (not delete) a vocabulary entry
```

### 8.3 Vocabulary Entry

```json
{
  "tag": "billing",
  "category": "capability",
  "description": "Can handle billing inquiries and operations",
  "created_at": "2026-03-10T10:00:00Z",
  "deprecated_at": null
}
```

Categories: `role`, `capability`.

---

## 9. Degrees (Opaque Metadata)

Degrees are stored as opaque metadata within the ILK `definition.current.degrees[]` array. SY.identity persists, propagates, and serves degree data but does NOT manage the degree lifecycle.

**What identity does with degrees:**

- Stores them in the ILK definition when written via ILK_UPDATE.
- Propagates them to SHM and workers like any other ILK metadata.
- Tracks changes in `definition.history[]`.

**What identity does NOT do:**

- Compile modules into degrees.
- Verify degree hash integrity.
- Manage graduation workflow.
- Maintain a module library.

These responsibilities belong to a future dedicated SY service that will interact with identity exclusively via ILK_UPDATE messages to write degree data into ILK metadata.

**Degree structure (as stored in ILK metadata):**

```json
{
  "degree_id": "uuid-v4",
  "name": "Soporte L2 Billing",
  "modules": ["billing_basics", "refund_policy"],
  "degree_hash": "sha256:...",
  "graduated_at": "2026-03-10T10:00:00Z",
  "issued_by": "ilk:uuid-of-issuer"
}
```

---

## 10. The Three Routing Layers

| Layer | Identifies | Format | Resolver | Location |
|-------|------------|--------|----------|----------|
| **L1** | Connection/Socket | UUID | Router (FIB) | routing.src/dst |
| **L2** | Process/Node | `TYPE.name@hive` | Router (SHM lookup) | routing.dst or meta.target |
| **L3** | Interlocutor | `ilk:<uuid-v4>` | OPA (reads data.identity from SHM) | meta.src_ilk/dst_ilk |

### 10.1 L3 Fields in Meta

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `src_ilk` | string | Yes (L3) | ILK of sending interlocutor (always `ilk:<uuid-v4>` format) |
| `dst_ilk` | string | No | ILK of destination interlocutor (if known) |

### 10.2 Tenant Derivation

The message does NOT carry tenant. OPA first canonicalizes `src_ilk` via alias map, then derives tenant from `data.identity`:

```rego
canonical := object.get(data.identity_aliases, input.meta.src_ilk, input.meta.src_ilk)
tenant := data.identity[canonical].tenant_id
```

Single source of truth — impossible to have ilk/tenant mismatch.

---

## 11. SHM Region: jsr-identity-\<hive\>

### 11.1 Purpose

Stores the full identity dataset for fast local read by any node: routers (OPA), IO nodes (ICH resolution), AI nodes (degree data access).

### 11.2 Single Writer

Only `SY.identity@<hive>` writes to this region. Uses seqlock (same pattern as all other SHM regions: router, config, lsa, opa).

### 11.3 Layout

The layout uses fixed-size arrays based on configured MAX_* limits. The region size is calculated at identity startup.

```
┌────────────────────────────────────────────────────────────────┐
│ IdentityHeader (128 bytes)                                      │
├────────────────────────────────────────────────────────────────┤
│ TenantEntry[MAX_TENANTS]                                        │
├────────────────────────────────────────────────────────────────┤
│ IlkEntry[MAX_ILKS]                                              │
├────────────────────────────────────────────────────────────────┤
│ IchEntry[MAX_ICHS]                                              │
│ (MAX_ICHS = MAX_ILKS * 4, assumes avg 4 channels per ILK)      │
├────────────────────────────────────────────────────────────────┤
│ IchMappingEntry[MAX_ICH_MAPPINGS]                               │
│ Hash-indexed lookup: hash(channel_type, address, tenant_id) → ich_id │
│ (MAX_ICH_MAPPINGS = MAX_ICHS * 2, for hash table load factor)  │
├────────────────────────────────────────────────────────────────┤
│ IlkAliasEntry[MAX_ILK_ALIASES]                                   │
│ Alias map: temporary_ilk -> canonical_ilk (grace window)         │
├────────────────────────────────────────────────────────────────┤
│ VocabularyEntry[MAX_VOCABULARY]                                 │
├────────────────────────────────────────────────────────────────┤
│ Variable data area (roles, capabilities as comma-separated)     │
└────────────────────────────────────────────────────────────────┘
```

### 11.4 Structures

```rust
pub const IDENTITY_MAGIC: u32 = 0x4A534944;  // "JSID"
pub const IDENTITY_VERSION: u32 = 3;

// Configurable limits (defaults)
pub const DEFAULT_MAX_ILKS: u32 = 1_000_000;
pub const DEFAULT_MAX_TENANTS: u32 = 10_000;
pub const DEFAULT_MAX_VOCABULARY: u32 = 4_096;
pub const DEFAULT_MAX_ILK_ALIASES: u32 = 1_000_000;

// ILK types
pub const ILK_TYPE_HUMAN: u8 = 0;
pub const ILK_TYPE_AGENT: u8 = 1;
pub const ILK_TYPE_SYSTEM: u8 = 2;

// Registration status
pub const REG_STATUS_TEMPORARY: u8 = 0;
pub const REG_STATUS_PARTIAL: u8 = 1;
pub const REG_STATUS_COMPLETE: u8 = 2;

// Flags
pub const ILK_FLAG_ACTIVE: u16 = 0x0001;
pub const ILK_FLAG_DELETED: u16 = 0x0002;
pub const ICH_MAP_FLAG_OCCUPIED: u16 = 0x0001;
pub const ICH_MAP_FLAG_TOMBSTONE: u16 = 0x0002;

#[repr(C)]
pub struct IdentityHeader {
    pub magic: u32,
    pub version: u32,
    pub seq: AtomicU64,                // Seqlock (same pattern as other SHM regions)

    // Counts
    pub tenant_count: u32,
    pub ilk_count: u32,
    pub ich_count: u32,
    pub ich_mapping_count: u32,
    pub vocabulary_count: u32,

    // Limits (from config)
    pub max_ilks: u32,
    pub max_tenants: u32,
    pub max_ich_mappings: u32,
    pub max_ilk_aliases: u32,

    // Timestamps
    pub updated_at: u64,
    pub heartbeat: u64,

    // Owner
    pub owner_uuid: [u8; 16],
    pub owner_pid: u32,
    pub is_primary: u8,               // 1=PRIMARY, 0=REPLICA

    pub _reserved: [u8; 39],
}
// Total: 128 bytes

#[repr(C)]
pub struct TenantEntry {
    pub tenant_id: [u8; 16],          // UUID raw bytes
    pub name: [u8; 128],
    pub domain: [u8; 64],
    pub status: u8,                   // 0=pending, 1=active, 2=suspended
    pub flags: u16,
    pub max_ilks: u32,
    pub created_at: u64,
    pub _reserved: [u8; 37],
}
// Total: 256 bytes

#[repr(C)]
pub struct IlkEntry {
    pub ilk_id: [u8; 16],            // UUID raw bytes
    pub ilk_type: u8,                 // ILK_TYPE_*
    pub registration_status: u8,      // REG_STATUS_*
    pub flags: u16,
    pub tenant_id: [u8; 16],         // UUID raw bytes of owning tenant
    pub display_name: [u8; 128],
    pub handler_node: [u8; 64],      // L2 name for agents (e.g., "AI.soporte.l1@prod")
    pub ich_offset: u32,             // Offset into ICH table
    pub ich_count: u16,              // Number of ICHs for this ILK
    pub roles_offset: u32,           // Offset into variable data
    pub roles_len: u16,
    pub capabilities_offset: u32,
    pub capabilities_len: u16,
    pub created_at: u64,
    pub updated_at: u64,
    pub _reserved: [u8; 18],
}
// Total: ~280 bytes

#[repr(C)]
pub struct IchEntry {
    pub ich_id: [u8; 16],            // UUID raw bytes
    pub ilk_id: [u8; 16],            // Owning ILK UUID raw bytes
    pub channel_type: [u8; 32],      // Matches DB VARCHAR(32)
    pub address: [u8; 256],          // Matches DB VARCHAR(256)
    pub flags: u16,
    pub is_primary: u8,
    pub added_at: u64,
    pub _reserved: [u8; 53],
}
// Total: 384 bytes

/// Hash-indexed lookup for fast ICH resolution by IO nodes.
/// IO computes hash(channel_type, address, tenant_id) and probes this table
/// with linear probing to find the matching ich_id → ilk_id.
#[repr(C)]
pub struct IchMappingEntry {
    pub hash: u64,                    // hash(channel_type, address, tenant_id)
    pub channel_type: [u8; 32],      // For collision verification
    pub address: [u8; 256],          // For collision verification
    pub ich_id: [u8; 16],            // Resolved ICH
    pub ilk_id: [u8; 16],            // Resolved ILK (shortcut, avoids second lookup)
    pub tenant_id: [u8; 16],         // Owning tenant UUID raw bytes (part of lookup key)
    pub flags: u16,                  // OCCUPIED / TOMBSTONE
    pub _reserved: [u8; 38],
}
// Total: 384 bytes

#[repr(C)]
pub struct IlkAliasEntry {
    pub old_ilk_id: [u8; 16],        // Temporary ILK
    pub canonical_ilk_id: [u8; 16],  // Final ILK
    pub expires_at: u64,             // Epoch ms; alias valid until this timestamp
    pub flags: u16,
    pub _reserved: [u8; 22],
}
// Total: 64 bytes
```

### 11.5 Writer: SY.identity

SY.identity is the single writer. Uses seqlock pattern (identical to router/config/lsa/opa regions):

```rust
impl IdentityWriter {
    fn write_ilk(&mut self, ilk: &Ilk) -> Result<()> {
        self.header.seq.fetch_add(1, Ordering::SeqCst);  // begin write (odd)

        let slot = self.find_or_create_slot(&ilk.ilk_id)?;
        slot.ilk_type = ilk.ilk_type as u8;
        slot.registration_status = ilk.registration_status as u8;
        copy_bytes(&mut slot.tenant_id, &ilk.tenant_id);
        copy_string(&mut slot.display_name, &ilk.display_name);
        copy_string(&mut slot.handler_node, &ilk.handler_node.unwrap_or_default());
        // ... other fields

        self.header.seq.fetch_add(1, Ordering::SeqCst);  // end write (even)
        Ok(())
    }
}
```

### 11.6 Reader: OPA

OPA loads the identity region as `data.identity` for routing rules. Keys are the prefixed string format (`ilk:uuid`), converted from raw bytes by the router's SHM reader:

```rego
# Derive tenant from src_ilk
tenant := data.identity[input.meta.src_ilk].tenant_id

# Check cognitive hashes for agent routing
data.identity[ilk].role_hash
data.identity[ilk].skill_hashes
data.identity[ilk].handbook_hashes

# Check registration status
data.identity[ilk].registration_status
```

### 11.7 Reader: IO Nodes

IO nodes resolve (channel_type, address, tenant_id) → ILK using the hash-indexed IchMappingEntry table:

```rust
fn resolve_ilk_for_channel(&self, channel_type: &str, address: &str, tenant_id: &str) -> Option<[u8; 16]> {
    let shm = self.identity_region.as_ref()?;
    let tenant_bytes = tenant_id_to_bytes(tenant_id);
    let hash = compute_ich_hash(channel_type, address, tenant_bytes);
    let table_size = shm.header.max_ich_mappings as usize;
    let mut idx = (hash as usize) % table_size;

    // Linear probing
    for _ in 0..table_size {
        let entry = &shm.ich_mappings[idx];
        // Empty slot (never used): key not present.
        if entry.flags == 0 {
            return None;
        }
        // Tombstone (deleted): keep probing.
        if entry.flags & ICH_MAP_FLAG_TOMBSTONE != 0 {
            idx = (idx + 1) % table_size;
            continue;
        }
        if entry.flags & ICH_MAP_FLAG_OCCUPIED != 0
            && entry.hash == hash
            && equal_fixed_str(&entry.channel_type, channel_type)
            && equal_fixed_str(&entry.address, address)
            && entry.tenant_id == tenant_bytes {
            return Some(entry.ilk_id);
        }
        idx = (idx + 1) % table_size;
    }
    None
}
```

This is O(1) average lookup, not O(N) scan. Suitable for millions of ICH entries.

---

## 12. Messages

### 12.1 ILK_PROVISION (unicast to SY.identity)

Creates a temporary ILK for an unknown ICH. Sent by IO nodes when they encounter an unregistered channel.

```json
{
  "routing": {
    "src": "<uuid-of-io-node>",
    "dst": "SY.identity@<hive>",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "ILK_PROVISION"
  },
  "payload": {
    "ich_id": "ich:a1b2c3d4-5678-90ab-cdef-1234567890ab",
    "channel_type": "whatsapp",
    "address": "+5491155551234",
    "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000"  // optional; defaults to default_tenant when absent
  }
}
```

**Response: ILK_PROVISION_RESPONSE**

```json
{
  "meta": { "type": "system", "msg": "ILK_PROVISION_RESPONSE" },
  "payload": {
    "status": "ok",
    "ilk_id": "ilk:550e8400-e29b-41d4-a716-446655440000",
    "registration_status": "temporary"
  }
}
```

The IO node can now use this real ILK UUID for `meta.src_ilk`.

Provisioning in the root tenant (an IO node running in it, or a request without `tenant_id`) works, but the person it creates cannot be registered and stays `temporary` (§6.2).

### 12.2 ILK_REGISTER (unicast to SY.identity@motherbee)

Upgrades a temporary ILK to complete, or creates a new ILK for a node. Sent by SY.frontdesk.gov (for humans) or SY.orchestrator (for nodes).

```json
{
  "routing": {
    "src": "<uuid-of-sender>",
    "dst": "SY.identity@motherbee",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "ILK_REGISTER"
  },
  "payload": {
    "ilk_id": "ilk:550e8400-...",
    "ilk_type": "human",
    "tenant_id": "tnt:uuid-of-tenant",
    "identification": {
      "display_name": "Juan Pérez",
      "email": "juan@acme.com"
    }
  }
}
```

The payload takes exactly these four fields (`ilk_type`: `human` or `agent`); any other field, such as the legacy `roles` / `capabilities`, is rejected with `INVALID_REQUEST`.

**Response: ILK_REGISTER_RESPONSE**

```json
{
  "meta": { "type": "system", "msg": "ILK_REGISTER_RESPONSE" },
  "payload": {
    "status": "ok",
    "ilk_id": "ilk:550e8400-e29b-41d4-a716-446655440000",
    "tenant_id": "tnt:uuid-of-tenant",
    "registration_status": "complete",
    "merged": false
  }
}
```

After a merge by email (§6.5), `ilk_id` is the ILK that already had the email (the person ended on it) and the reply adds where the registration came from:

```json
{
  "status": "ok",
  "ilk_id": "ilk:<registered-ilk>",
  "tenant_id": "tnt:uuid-of-tenant",
  "registration_status": "complete",
  "merged": true,
  "merged_from_ilk_id": "ilk:<temporary-ilk>"
}
```

For node spawn flows (`SY.orchestrator -> ILK_REGISTER`), this response is a hard gate:
- only `payload.status="ok"` allows spawn to continue;
- any other outcome must abort spawn with `IDENTITY_REGISTER_FAILED`.
- In implementation, orchestrator sends this action through the SDK helper (`identity_system_call_ok`) and maps `IdentityError` to orchestrator/admin-facing errors without changing external error contracts.

**Error codes** (`status: "error"` with `error_code`):

| `error_code` | When |
| --- | --- |
| `INVALID_REQUEST` | malformed payload or ids, an unknown field, or an `ilk_type` other than `human` / `agent` |
| `INVALID_TENANT` | the tenant does not exist |
| `TENANT_PENDING`, `TENANT_SUSPENDED` | the tenant is not active |
| `TENANT_DELETED` | the tenant is marked deleted |
| `TENANT_ROOT_NOT_REGISTRABLE` | a person into the root tenant: `ilk_type` `human`, or the ILK being registered is a human one (§6.2); node ILKs (`agent`) still register there |
| `ILK_DELETED` | a marked ILK keeps that `node_name` (across the mesh) or email (in the tenant) reserved |
| `ILK_NOT_FOUND` | the ILK being registered is marked deleted |
| `SYSTEM_ILK_PROTECTED` | the ILK being registered is a system ILK |
| `INVALID_TENANT_TRANSITION` | a complete ILK cannot change tenant |
| `DUPLICATE_EMAIL` | the email belongs to another ILK of the tenant and the registration cannot merge (§6.5); also the DB unique index |
| `DUPLICATE_NODE_NAME`, `DUPLICATE_ICH`, `DUPLICATE_CONSTRAINT` | a DB unique index |
| `UNAUTHORIZED_REGISTRAR` | the sender is not allowed (§6.2) |
| `NOT_PRIMARY` | sent to a replica |
| `DB_NOT_READY`, `DB_WRITE_FAILED` | the primary's DB is not configured, or the write failed |

### 12.3 ILK_ADD_CHANNEL (unicast to SY.identity@motherbee)

Associates a newly discovered ICH with an existing ILK and optionally merges a temporary ILK into the canonical ILK. Sent by IO nodes (their own ICH) and SY.frontdesk.gov.

```json
{
  "meta": { "type": "system", "msg": "ILK_ADD_CHANNEL" },
  "payload": {
    "ilk_id": "ilk:550e8400-...",
    "channel": {
      "ich_id": "ich:12345678-1234-1234-1234-1234567890ab",
      "type": "slack",
      "address": "@juan"
    },
    "merge_from_ilk_id": "ilk:a1b2c3d4-...",
    "change_reason": "new channel discovered via frontdesk"
  }
}
```

With `merge_from_ilk_id` it is the merge of §6.5 and §6.6: the temporary ILK's channels move to `ilk_id` and it becomes an alias. Everything is validated before anything changes. Response: `status`, `ilk_id`, `ich_id`, `owner_l2_name`, `enabled`, `change_reason`. Error codes: `INVALID_REQUEST`, `ILK_NOT_FOUND` (missing or marked target), `ILK_DELETED` (a marked ILK keeps that channel reserved), `INVALID_MERGE_SOURCE` (the merge source is not another active temporary ILK), `UNAUTHORIZED_REGISTRAR`, `NOT_PRIMARY`, and the DB codes of §12.2.

### 12.4 ILK_UPDATE (unicast to SY.identity@motherbee)

Adds channels to an existing ILK. Sent by SY.orchestrator (node ILKs).

```json
{
  "meta": { "type": "system", "msg": "ILK_UPDATE" },
  "payload": {
    "ilk_id": "ilk:550e8400-...",
    "add_channels": [
      { "ich_id": "ich:uuid", "type": "slack", "address": "@juan" }
    ],
    "change_reason": "node channel added"
  }
}
```

It changes nothing else: any other field (identification, the legacy `add_roles` / `add_capabilities`, degrees) is rejected with `INVALID_REQUEST`. Identification changes only through `ILK_REGISTER` of the ILK itself, which replaces it whole (last write wins); a merge only fills empty fields (§6.5). An agent's cognitive definition is written with `ILK_SET_DEFINITION` (addendum).

### 12.5 TNT_CREATE (unicast to SY.identity@motherbee)

Sent by SY.admin (the operator, and Fluxbee Cloud through io.cloud `create_tenant`) and SY.architect; never by the frontdesk (§6.4). `status` defaults to `pending`; a `name` or `domain` that matches an existing tenant returns that tenant (`created: false`, `matched_by`).

```json
{
  "meta": { "type": "system", "msg": "TNT_CREATE" },
  "payload": {
    "name": "Acme Corp",
    "domain": "acme.com",
    "status": "pending",
    "settings": {
      "max_ilks": 10000,
      "allowed_channels": ["whatsapp", "slack", "email"]
    }
  }
}
```

**Response: TNT_CREATE_RESPONSE**

```json
{
  "meta": { "type": "system", "msg": "TNT_CREATE_RESPONSE" },
  "payload": {
    "status": "ok",
    "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
    "created": true,
    "matched_by": null,
    "sponsor_tenant_id": null
  }
}
```

### 12.6 TNT_APPROVE (unicast to SY.identity@motherbee)

Approves a pending tenant. Sent by SY.admin only.

```json
{
  "meta": { "type": "system", "msg": "TNT_APPROVE" },
  "payload": {
    "tenant_id": "tnt:550e8400-...",
    "approved_by": "ilk:7c9e6679-..."
  }
}
```

### 12.7 IDENTITY_DELTA (socket, not router)

Propagated from motherbee to workers via direct socket (not via router messages).

```json
{
  "version": 42,
  "operation": "ilk_created",
  "ilk": { ... full ILK document ... }
}
```

Operations: `ilk_created`, `ilk_updated`, `ilk_deleted`, `tenant_created`, `tenant_updated`, `vocabulary_added`, `vocabulary_deprecated`.

### 12.8 IDENTITY_FULL_SYNC (socket, not router)

Sent from motherbee to a worker during cold start. Streamed in chunks.

```json
{
  "version": 42,
  "operation": "full_sync",
  "chunk": 1,
  "total_chunks": 15,
  "tenants": [ ... ],
  "ilks": [ ... ],
  "vocabulary": [ ... ]
}
```

---

## 13. API (via SY.admin)

### 13.1 Tenant Management

```
POST   /identity/tenants                  — create tenant
GET    /identity/tenants                  — list tenants
GET    /identity/tenants/{id}             — get tenant
PUT    /identity/tenants/{id}             — update tenant
POST   /identity/tenants/{id}/approve     — approve pending tenant
```

### 13.2 ILK Management

```
GET    /identity/ilks                     — list ILKs (paginated, filterable by tenant/type/status)
GET    /identity/ilks/{id}                — get ILK with full metadata
DELETE /identity/ilks/{id}                — soft-delete ILK
```

Note: ILK creation is NOT exposed via HTTP API. ILKs are created only via message protocol: `ILK_PROVISION` (IO nodes, temporary ILK) and `ILK_REGISTER` (SY.frontdesk.gov / SY.orchestrator).

### 13.3 Vocabulary Management (superseded, not implemented — §8)

```
GET    /identity/vocabulary               — list all valid tags
POST   /identity/vocabulary               — add tag
DELETE /identity/vocabulary/{tag}          — deprecate tag
```

### 13.4 Metrics

```
GET    /identity/metrics                  — current counts and limits
```

Response:
```json
{
  "ilk_count": 45230,
  "max_ilks": 1000000,
  "tenant_count": 12,
  "ich_count": 68400,
  "vocabulary_count": 87,
  "shm_size_bytes": 28000000,
  "last_sync_at": "2026-03-10T10:00:00Z"
}
```

---

## 14. OPA Integration

OPA derives tenant from `src_ilk` and can filter by any identity attribute:

```rego
package router

# Derive tenant
get_tenant(ilk) = tenant {
    canonical := object.get(data.identity_aliases, ilk, ilk)
    tenant := data.identity[canonical].tenant_id
}

# Route temporary ILKs to frontdesk
target = "SY.frontdesk.gov@production" {
    src := object.get(data.identity_aliases, input.meta.src_ilk, input.meta.src_ilk)
    data.identity[src].registration_status == "temporary"
}

# Route by tenant
target = "AI.support.l1@production" {
    tenant := get_tenant(input.meta.src_ilk)
    tenant == "tnt:uuid-acme"
}

# Route by configured skill hash
target = node {
    required_hash := input.meta.context.required_skill_hash
    some ilk
    data.identity[ilk].ilk_type == "agent"
    required_hash in data.identity[ilk].skill_hashes
    node := data.identity[ilk].handler_node
}

# Authorization remains explicit policy plus SY.identity source enforcement.
# Do not use legacy capabilities for identity-system authorization.
```

---

## 15. Database Schema (PostgreSQL, motherbee only)

```sql
-- Tenants
CREATE TABLE identity_tenants (
    tenant_id UUID PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    domain VARCHAR(128),
    status VARCHAR(16) NOT NULL DEFAULT 'pending',
    settings JSONB NOT NULL DEFAULT '{}',
    approved_by UUID,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- ILKs
CREATE TABLE identity_ilks (
    ilk_id UUID PRIMARY KEY,
    ilk_type VARCHAR(16) NOT NULL,
    registration_status VARCHAR(16) NOT NULL DEFAULT 'temporary',
    tenant_id UUID NOT NULL REFERENCES identity_tenants(tenant_id), -- temporary ILKs start in default_tenant; reassignment allowed only while status=temporary

    -- Denormalized uniqueness fields (extracted from JSONB for indexing)
    email VARCHAR(256),               -- For humans: uniqueness key
    node_name VARCHAR(128),           -- For agents/systems: uniqueness key

    -- Full metadata as JSONB
    identification JSONB NOT NULL DEFAULT '{}',
    association JSONB NOT NULL DEFAULT '{}',
    definition JSONB NOT NULL DEFAULT '{}',

    registered_by UUID,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    deleted_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX idx_identity_ilks_email
    ON identity_ilks(email, tenant_id)
    WHERE email IS NOT NULL AND deleted_at IS NULL;

CREATE UNIQUE INDEX idx_identity_ilks_node_name
    ON identity_ilks(node_name)
    WHERE node_name IS NOT NULL AND deleted_at IS NULL;

CREATE INDEX idx_identity_ilks_tenant ON identity_ilks(tenant_id);
CREATE INDEX idx_identity_ilks_type ON identity_ilks(ilk_type);
CREATE INDEX idx_identity_ilks_status ON identity_ilks(registration_status);

-- Temporary ILK aliasing during merge grace window
CREATE TABLE identity_ilk_aliases (
    old_ilk_id UUID PRIMARY KEY,
    canonical_ilk_id UUID NOT NULL REFERENCES identity_ilks(ilk_id),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_identity_ilk_aliases_canonical ON identity_ilk_aliases(canonical_ilk_id);
CREATE INDEX idx_identity_ilk_aliases_expires ON identity_ilk_aliases(expires_at);

-- ICH lookup (denormalized for fast resolution and unique constraint)
CREATE TABLE identity_ichs (
    ich_id UUID PRIMARY KEY,
    ilk_id UUID NOT NULL REFERENCES identity_ilks(ilk_id),
    tenant_id UUID NOT NULL REFERENCES identity_tenants(tenant_id),
    channel_type VARCHAR(32) NOT NULL,
    address VARCHAR(256) NOT NULL,
    is_primary BOOLEAN DEFAULT FALSE,
    added_at TIMESTAMPTZ DEFAULT NOW(),

    UNIQUE(channel_type, address, tenant_id)
);

CREATE INDEX idx_identity_ichs_lookup ON identity_ichs(channel_type, address);
CREATE INDEX idx_identity_ichs_ilk ON identity_ichs(ilk_id);

-- Vocabulary
CREATE TABLE identity_vocabulary (
    tag VARCHAR(64) PRIMARY KEY,
    category VARCHAR(16) NOT NULL,    -- 'role' or 'capability'
    description TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    deprecated_at TIMESTAMPTZ
);
```

Note: Tables are prefixed `identity_` to avoid collision with SY.storage's domain (turns, events, memory_items). The `email` and `node_name` columns are denormalized from JSONB specifically to enable proper uniqueness constraints and indexed lookups.

---

## 16. Configuration (hive.yaml)

```yaml
# Motherbee
government:
  identity_frontdesk: "SY.frontdesk.gov@motherbee"

identity:
  max_ilks: 1000000
  max_tenants: 10000
  default_tenant: "fluxbee"
  merge_alias_ttl_secs: 3600
  sync:
    port: 9100              # Socket port for delta/full-sync to workers

# Worker
government:
  identity_frontdesk: "SY.frontdesk.gov@motherbee"  # Resolved cross-hive by router

identity:
  sync:
    upstream: "motherbee:9100"   # Connect to motherbee identity
```

---

## 17. Complete Flow: Incoming Message

```
1. WhatsApp sends message from +5491155551234
                │
                ▼
2. IO.whatsapp receives webhook
                │
                ▼
3. IO.whatsapp reads SHM jsr-identity (hash lookup):
   resolve("whatsapp", "+5491155551234") → ILK found or not
                │
        ┌───────┴───────┐
        │               │
   ILK found       ILK not found
        │               │
        ▼               ▼
   ilk:uuid        IO sends ILK_PROVISION to SY.identity
        │               │
        │               ▼
        │          SY.identity creates temporary ILK (real UUID)
        │          returns ilk:uuid to IO
        │               │
        ▼               ▼
4. IO sends message with meta.src_ilk = ilk:<uuid>
                │
                ▼
5. Router invokes OPA
                │
        ┌───────┴───────┐
        │               │
   status=complete  status=temporary
        │               │
        ▼               ▼
   Normal routing   Route to SY.frontdesk.gov
        │               │
        ▼               ▼
6. AI node processes    SY.frontdesk.gov starts registration
                        │
                        ▼
7.                 Collects data, sends ILK_REGISTER
                        │
                        ▼
8.                 SY.identity upgrades ILK → complete, propagates
                        │
                        ▼
9.                 IO.whatsapp keeps same ILK UUID (was always real)
                        │
                        ▼
10.                Next message routes normally
```

---

## 18. Resolved Friction Points (vs v1.15)

| # | Friction | Resolution |
|---|----------|------------|
| 1 | Version misalignment across docs | This document is the single source of truth for identity |
| 2 | IDENTITY_CHANGED not in protocol | Identity uses direct socket sync between SY.identity instances. Not CONFIG_CHANGED, not router broadcast |
| 3 | SHM limits inconsistent (8192 vs 65536) | Configurable MAX_ILKS, default 1M, scaled to RAM |
| 4 | ILK format contradictions | UUID v4 strict everywhere, including temporary ILKs. See section 2 for format convention |
| 5 | Full sync exceeds 64KB | Socket-based chunked sync, not router messages |
| 6 | DB ownership conflict | Formal exception: SY.identity owns identity_* tables, SY.storage owns cognitive tables. Documented in section 5.2 |
| 7 | Critical vs optional inconsistency | SY.identity is critical in all hives; motherbee=PRIMARY, worker=REPLICA |
| 8 | Module naming collision | Knowledge module lifecycle fully deferred to future SY service. Identity stores degrees as opaque metadata only |
| 9 | ILK/ICH creation ambiguity | Action-scoped registrar allowlists: ILK_PROVISION by IO, ILK_REGISTER by orchestrator/frontdesk, ILK_ADD_CHANNEL by frontdesk |
| 10 | In-flight message race on temporary ILK merge | Alias table + grace TTL (`identity_ilk_aliases`) before soft-delete |

---

## 19. Documents That Need Alignment

When this spec is adopted, the following documents should be updated:

| Document | Change needed |
|----------|---------------|
| `03-shm.md` | Add identity region v2 layout (seqlock, IchMappingEntry), update MAX_ILKS to configurable |
| `07-operaciones.md` | Add formal DB ownership exception for SY.identity. Add identity block to hive.yaml reference |
| `SY_nodes_spec.md` | Remove identity from CONFIG_CHANGED subsystem list. Document socket sync model |
| `02-protocolo.md` | Add ILK_PROVISION, ILK_REGISTER, ILK_ADD_CHANNEL, ILK_UPDATE, TNT_CREATE, TNT_APPROVE message definitions |
| `01-arquitectura.md` | Add AI.frontdesk as system component. Document temporary ILK flow |
| `README.md` | Update identity implementation status |

---

## 20. Constants

```rust
// Identity SHM
pub const IDENTITY_MAGIC: u32 = 0x4A534944;  // "JSID"
pub const IDENTITY_VERSION: u32 = 3;

// Defaults (configurable in hive.yaml)
pub const DEFAULT_MAX_ILKS: u32 = 1_000_000;
pub const DEFAULT_MAX_TENANTS: u32 = 10_000;
pub const DEFAULT_MAX_VOCABULARY: u32 = 4_096;
pub const DEFAULT_IDENTITY_SYNC_PORT: u16 = 9100;

// ILK types
pub const ILK_TYPE_HUMAN: u8 = 0;
pub const ILK_TYPE_AGENT: u8 = 1;
pub const ILK_TYPE_SYSTEM: u8 = 2;

// Registration status
pub const REG_STATUS_TEMPORARY: u8 = 0;
pub const REG_STATUS_PARTIAL: u8 = 1;
pub const REG_STATUS_COMPLETE: u8 = 2;

// Tenant status
pub const TNT_STATUS_PENDING: u8 = 0;
pub const TNT_STATUS_ACTIVE: u8 = 1;
pub const TNT_STATUS_SUSPENDED: u8 = 2;

// Flags
pub const FLAG_ACTIVE: u16 = 0x0001;
pub const FLAG_DELETED: u16 = 0x0002;

// Timers
pub const IDENTITY_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const IDENTITY_SYNC_TIMEOUT_MS: u64 = 30_000;
```

---

## 21. References

| Topic | Document |
|-------|----------|
| Architecture | `01-arquitectura.md` |
| Protocol | `02-protocolo.md` |
| SHM | `03-shm.md` |
| Routing L1/L2 | `04-routing.md` |
| SY nodes | `SY_nodes_spec.md` |
| Cognitive | `12-cognition.md` |
| Storage | `13-storage.md` |
| Operations | `07-operaciones.md` |
