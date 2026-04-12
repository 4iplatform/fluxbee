# JSON Router - 10 Identity and Layer 3 (ILK)

**Status:** v1.16  
**Date:** 2026-04-12  
**Audience:** IO, AI, WF node developers, L3 routing

---

## 1. The Three Routing Layers

| Layer | Identifies | Format | Resolver | Location |
|-------|------------|--------|----------|----------|
| **L1** | Connection/Socket | UUID | Router (FIB) | routing.src/dst |
| **L2** | Process/Node | `TYPE.name@hive` | Router (authoritative stamp) | routing.src_l2_name, routing.dst, meta.target |
| **L3** | Interlocutor | `ilk:<uuid>` | OPA (reads meta) | meta.src_ilk/dst_ilk |

---

## 2. ILK — Interlocutor Key

### 2.1 Definition

An **ILK** (Interlocutor Key) is the unique identifier for any entity participating in the system:

- **Tenants** (organizations, accounts, companies)
- **Agents** (AI responders)
- **Systems** (workflows, bots, integrations)
- **Humans** (internal operators or external customers)

### 2.2 Format

```
ilk:<uuid-v4>

Examples:
ilk:550e8400-e29b-41d4-a716-446655440000  (human)
ilk:7c9e6679-7425-40de-944b-e07fc1f90ae7  (agent)
ilk:f47ac10b-58cc-4372-a567-0e02b2c3d479  (tenant)
```

**Rules:**
- Always prefix `ilk:`
- Always UUID v4
- Never human names as identifier (those go in metadata)
- Immutable once created

### 2.3 ILK Types

| Type | Subtype | Roles/Capabilities | Can query system | Description |
|------|---------|-------------------|------------------|-------------|
| `tenant` | - | ❌ | - | Organization, account, company |
| `agent` | - | ✅ (via degree) | - | AI agent with degree |
| `system` | - | ✅ | - | Workflow, bot, integration |
| `human` | `internal` | ❌ | ✅ Yes | Operator, staff |
| `human` | `external` | ❌ | ❌ No | Customer, client |

### 2.4 Hierarchy

All ILKs (except tenant) belong to a tenant:

```
ilk:tenant-acme (tenant)
│
├── ilk:maria (human/internal)
│   └── can query system status
│
├── ilk:john (human/external)  
│   └── only interacts as customer
│
├── ilk:agent-support-1 (agent)
│   ├── degree: support-l1-en
│   └── handler: AI.support.l1@production
│
└── ilk:workflow-onboarding (system)
    └── role: onboarding-flow
```

---

## 3. Message Structure with L3

```json
{
  "routing": {
    "src": "uuid-node-io-whatsapp",
    "src_l2_name": "IO.whatsapp@motherbee",
    "dst": null,
    "ttl": 16,
    "trace_id": "uuid-trace"
  },
  "meta": {
    "type": "user",
    "target": "AI.support.*",
    
    "src_ilk": "ilk:550e8400-e29b-41d4-a716-446655440000",
    "dst_ilk": "ilk:7c9e6679-7425-40de-944b-e07fc1f90ae7",
    "conversation_id": "conv:a1b2c3d4-5678-90ab-cdef-1234567890ab",
    
    "context": {
      "channel": "whatsapp",
      "external_id": "+5491155551234"
    }
  },
  "payload": {
    "type": "text",
    "content": "Hello, I need help with my order"
  }
}
```

### 3.1 L3 Fields in meta

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `src_ilk` | string | Yes (L3) | ILK of sending interlocutor. OPA derives tenant via `data.identity` lookup |
| `dst_ilk` | string | No | ILK of destination interlocutor (if known) |
| `conversation_id` | string | Recommended | Conversation/thread ID |

### 3.2 Tenant is derived, not sent

The message does NOT carry `tenant`. OPA derives it from `src_ilk`:

### 3.3 L1 -> L2 visibility for consumers

The router is the authority for the L2 identity of the message origin.

Rules:
- nodes receive `routing.src` as the canonical L1 UUID of the origin
- nodes receive `routing.src_l2_name` as the canonical L2 name of the same origin
- nodes should not perform SHM or filesystem lookup to derive the sender L2 during normal message handling
- any value supplied by a sender in `routing.src_l2_name` must be ignored and overwritten by the router

```rego
# OPA reads tenant from identity table
tenant := data.identity[input.meta.src_ilk].tenant_ilk
```

**Why this is better:**
- Single source of truth (identity table)
- No redundancy in messages
- Impossible to have ilk/tenant mismatch
- Simpler IO nodes (just resolve ilk, done)

---

## 4. Knowledge Library (Modules)

### 4.1 Concept

The knowledge library contains reusable **modules** that combine to create **degrees**.

```
┌─────────────────────────────────────────────────────────────┐
│                    LIBRARY (knowledge modules)               │
│                                                              │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐           │
│  │ Module:     │ │ Module:     │ │ Module:     │           │
│  │ base-       │ │ technical-  │ │ sales-      │           │
│  │ english     │ │ support     │ │ closing     │           │
│  │             │ │             │ │             │           │
│  │ "Always     │ │ "You know   │ │ "You are    │           │
│  │ respond in  │ │ products    │ │ an expert   │           │
│  │ English..." │ │ X, Y, Z..." │ │ in closing."│           │
│  └─────────────┘ └─────────────┘ └─────────────┘           │
│                                                              │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐           │
│  │ Module:     │ │ Module:     │ │ Module:     │           │
│  │ tone-       │ │ escalation  │ │ compliance- │           │
│  │ formal      │ │             │ │ financial   │           │
│  └─────────────┘ └─────────────┘ └─────────────┘           │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

### 4.2 Module Structure

```sql
CREATE TABLE modules (
    id VARCHAR(64) PRIMARY KEY,           -- "module:base-english"
    name VARCHAR(128) NOT NULL,
    description TEXT,
    category VARCHAR(64),                 -- "language", "technical", "tone", "compliance"
    prompt_text TEXT NOT NULL,            -- Module content
    version VARCHAR(16) NOT NULL,         -- "1.0.0"
    hash VARCHAR(72) NOT NULL,            -- sha256 of prompt_text
    created_by VARCHAR(48),               -- ilk of creator (human/internal)
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    deprecated_at TIMESTAMPTZ
);
```

### 4.3 Module Categories

| Category | Purpose | Examples |
|----------|---------|----------|
| `language` | Base language and communication style | base-english, base-spanish |
| `technical` | Domain knowledge | technical-support, product-catalog |
| `tone` | Communication tone | tone-formal, tone-friendly |
| `compliance` | Regulatory requirements | compliance-financial, compliance-health |
| `escalation` | Escalation rules | escalation-to-l2, escalation-to-human |
| `persona` | Personality traits | persona-helpful, persona-professional |

---

## 5. Degrees (Compiled Careers)

### 5.1 Concept

A **degree** is a compiled career: ordered combination of modules with an integrity hash.

```
┌─────────────────────────────────────────────────────────────┐
│                    DEGREES (compiled careers)                │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ Degree: support-l1-en                                │   │
│  │ id: degree:abc123                                    │   │
│  │ version: 2.3.1                                       │   │
│  │ hash: sha256:xxx (integrity seal)                   │   │
│  │                                                      │   │
│  │ modules: [base-english, technical-support, escal.]  │   │
│  │                                                      │   │
│  │ compiled_prompt: "You are a support agent..."       │   │
│  │ (verifiable concatenation of modules)               │   │
│  │                                                      │   │
│  │ capabilities: [support, english, level-1]           │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ Degree: sales-closer-en                              │   │
│  │ id: degree:def456                                    │   │
│  │ version: 1.0.0                                       │   │
│  │ hash: sha256:yyy                                     │   │
│  │                                                      │   │
│  │ modules: [base-english, sales-closing, tone-formal] │   │
│  │ compiled_prompt: "You are an expert salesperson..." │   │
│  │ capabilities: [sales, english, closing]             │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

### 5.2 Degree Structure

```sql
CREATE TABLE degrees (
    id VARCHAR(48) PRIMARY KEY,           -- "degree:uuid"
    name VARCHAR(128) NOT NULL,           -- "support-l1-en"
    description TEXT,
    version VARCHAR(16) NOT NULL,         -- "2.3.1"
    
    -- Modules composing this degree (order matters)
    modules JSONB NOT NULL,               -- ["module:base-english", ...]
    modules_versions JSONB NOT NULL,      -- {"module:base-english": "1.0.0", ...}
    
    -- Compiled prompt (concatenation of modules)
    compiled_prompt TEXT NOT NULL,
    
    -- Integrity seal
    hash VARCHAR(72) NOT NULL,            -- sha256 of compiled_prompt
    
    -- Capabilities granted by this degree
    capabilities TEXT[] NOT NULL,         -- ["support", "english", "level-1"]
    
    -- Metadata
    created_by VARCHAR(48),
    created_at TIMESTAMPTZ DEFAULT NOW(),
    deprecated_at TIMESTAMPTZ
);
```

### 5.3 Compilation Process

```
1. Designer specifies modules:
   ["module:base-english", "module:technical-support", "module:escalation"]

2. System reads each module (specific versions)

3. System concatenates prompts in order:
   compiled_prompt = module1.prompt + "\n\n" + module2.prompt + "\n\n" + ...

4. System calculates hash:
   hash = sha256(compiled_prompt)

5. System saves degree with seal
```

---

## 6. Graduation (Agent Creation)

### 6.1 Concept

**Graduation** is the process of assigning a degree to an agent ILK. The agent receives compiled knowledge and becomes operational.

```
┌─────────────────────────────────────────────────────────────┐
│                    GRADUATES (agents)                        │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ ilk:agent-support-1                                  │   │
│  │ type: agent                                          │   │
│  │ degree_id: degree:abc123                            │   │
│  │ degree_version: 2.3.1                               │   │
│  │ degree_hash: sha256:xxx  ← verifiable               │   │
│  │ graduated_at: 2026-01-30T10:00:00Z                 │   │
│  │                                                      │   │
│  │ handler: AI.support.l1@production                   │   │
│  │ (this agent KNOWS what the degree says)             │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

### 6.2 Graduation Process

```
1. Designer creates/updates modules in library
                │
                ▼
2. Designer defines degree:
   POST /degrees
   {
     "name": "support-l1-en",
     "modules": ["module:base-english", "module:technical-support"],
     "capabilities": ["support", "english", "level-1"]
   }
                │
                ▼
3. System compiles:
   - Read each module
   - Concatenate prompts in order
   - Calculate hash
   - Save degree with seal
                │
                ▼
4. Operator graduates agent:
   POST /ilks/graduate
   {
     "ilk": "ilk:new-agent",
     "degree_id": "degree:abc123"
   }
                │
                ▼
5. System records:
   - degree_id
   - degree_version (current at graduation time)
   - degree_hash (seal)
   - graduated_at
                │
                ▼
6. Agent is ready to operate
   (AI.* handler receives compiled_prompt from degree)
```

### 6.3 Integrity Verification

When AI.* starts or receives a message:

```rust
fn verify_graduation(ilk: &Ilk, degree: &Degree) -> Result<()> {
    // 1. Verify degree hash hasn't changed since graduation
    if ilk.degree_hash != degree.hash {
        return Err("Degree was modified after graduation");
    }
    
    // 2. Verify compiled_prompt matches hash
    let current_hash = sha256(&degree.compiled_prompt);
    if current_hash != degree.hash {
        return Err("Compiled prompt corrupted");
    }
    
    Ok(())
}
```

**If verification fails:** The agent is invalid - tampered degree, cannot operate.

### 6.4 Degree Versioning

When a module is updated:
1. Existing degrees **don't change** (they have fixed module versions)
2. New version of degree can be created with updated modules
3. Agents graduated with old version keep old version
4. To update agent → re-graduate with new degree

```
degree:support-l1-en v2.3.1 (hash: xxx)
    └── ilk:agent-1 (graduated 2026-01-15)
    └── ilk:agent-2 (graduated 2026-01-20)

degree:support-l1-en v2.4.0 (hash: yyy) ← updated module
    └── ilk:agent-3 (graduated 2026-01-30)
    
# agent-1 and agent-2 stay with v2.3.1 until re-graduation
```

---

## 7. SY.identity — Identity Service

### 7.1 Purpose

SY.identity is the central registry for all ILKs, modules, and degrees. It maintains:

- Registry of interlocutors (tenants, agents, systems, humans)
- Mapping of external channels to ILKs
- Knowledge library (modules)
- Compiled degrees
- Graduation records

### 7.2 Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Mother Hive                         │
│                                                              │
│  ┌──────────────┐      ┌─────────────────────────────────┐ │
│  │ PostgreSQL   │◄────►│ SY.identity@mother              │ │
│  │              │      │ mode: PRIMARY                   │ │
│  │ - ilks       │      │                                 │ │
│  │ - channels   │      │ • Read/write DB                │ │
│  │ - modules    │      │ • Write jsr-identity-mother    │ │
│  │ - degrees    │      │ • Emit IDENTITY_CHANGED        │ │
│  └──────────────┘      └─────────────────────────────────┘ │
│                                    │                         │
│                                    │ broadcast               │
└────────────────────────────────────┼─────────────────────────┘
                                     │
                                     │ WAN
                                     ▼
┌─────────────────────────────────────────────────────────────┐
│                      Production Hive                       │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ SY.identity@production                               │   │
│  │ mode: REPLICA                                        │   │
│  │                                                      │   │
│  │ • Receive IDENTITY_CHANGED                          │   │
│  │ • Write jsr-identity-production                     │   │
│  │ • NO database access                                │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                              │
│        /dev/shm/jsr-identity-production                     │
│        ┌───────────────────────────────────────────┐       │
│        │ (fast read for all nodes)                  │       │
│        └───────────────────────────────────────────┘       │
│                          ▲                                   │
│              ┌───────────┼───────────┬──────────┐           │
│              │           │           │          │           │
│        IO.whatsapp   AI.support    OPA      WF.crm         │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

### 7.3 Operation Modes

| Mode | Condition | Capabilities |
|------|-----------|-------------|
| **PRIMARY** | `hive.yaml` has `database.connection` | Read/write DB, emit broadcast |
| **REPLICA** | No `database.connection` | Receive broadcast, write local SHM |

### 7.4 Configuration (hive.yaml)

**Mother (primary):**
```yaml
hive_id: mother
is_mother: true

database:
  connection: "postgres://user:pass@localhost:5432/jsonrouter"
```

**Other hives (replica):**
```yaml
hive_id: production
is_mother: false
# No database.connection → REPLICA mode automatic
```

---

## 8. Database Schema (PostgreSQL)

### 8.1 Core Tables

```sql
-- ILK Types
CREATE TYPE ilk_type AS ENUM ('tenant', 'agent', 'system', 'human');
CREATE TYPE human_subtype AS ENUM ('internal', 'external');

-- Main ILK table
CREATE TABLE ilks (
    id VARCHAR(48) PRIMARY KEY,           -- "ilk:uuid"
    type ilk_type NOT NULL,
    
    -- Only for humans
    human_subtype human_subtype,
    
    -- All (except tenant) belong to a tenant
    tenant_ilk VARCHAR(48) REFERENCES ilks(id),
    
    -- Only for agents
    degree_id VARCHAR(48) REFERENCES degrees(id),
    degree_version VARCHAR(16),
    degree_hash VARCHAR(72),              -- Integrity seal at graduation
    graduated_at TIMESTAMPTZ,
    
    -- Only for agents/systems
    handler_node VARCHAR(128),            -- "AI.support.l1@production"
    
    -- For all
    name VARCHAR(128),                    -- Friendly name
    metadata JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    deleted_at TIMESTAMPTZ
);

-- Constraints
ALTER TABLE ilks ADD CONSTRAINT chk_tenant_no_parent 
    CHECK (type != 'tenant' OR tenant_ilk IS NULL);

ALTER TABLE ilks ADD CONSTRAINT chk_non_tenant_has_parent 
    CHECK (type = 'tenant' OR tenant_ilk IS NOT NULL);

ALTER TABLE ilks ADD CONSTRAINT chk_human_subtype 
    CHECK (type != 'human' OR human_subtype IS NOT NULL);

ALTER TABLE ilks ADD CONSTRAINT chk_agent_has_degree
    CHECK (type != 'agent' OR degree_id IS NOT NULL);

-- Indexes
CREATE INDEX idx_ilks_type ON ilks(type);
CREATE INDEX idx_ilks_tenant ON ilks(tenant_ilk);
CREATE INDEX idx_ilks_handler ON ilks(handler_node) WHERE handler_node IS NOT NULL;
CREATE INDEX idx_ilks_degree ON ilks(degree_id) WHERE degree_id IS NOT NULL;
```

### 8.2 Channel Mapping

```sql
CREATE TABLE ilk_channels (
    id SERIAL PRIMARY KEY,
    ilk_id VARCHAR(48) NOT NULL REFERENCES ilks(id),
    channel VARCHAR(32) NOT NULL,         -- "whatsapp", "email", "slack"
    external_id VARCHAR(256) NOT NULL,    -- "+5491155551234", "user@example.com"
    tenant_ilk VARCHAR(48) REFERENCES ilks(id),
    created_at TIMESTAMPTZ DEFAULT NOW(),
    
    UNIQUE(channel, external_id, tenant_ilk)
);

CREATE INDEX idx_ilk_channels_ilk ON ilk_channels(ilk_id);
CREATE INDEX idx_ilk_channels_lookup ON ilk_channels(channel, external_id, tenant_ilk);
```

### 8.3 Knowledge Library

```sql
-- Modules (knowledge fragments)
CREATE TABLE modules (
    id VARCHAR(64) PRIMARY KEY,           -- "module:base-english"
    name VARCHAR(128) NOT NULL,
    description TEXT,
    category VARCHAR(64),
    prompt_text TEXT NOT NULL,
    version VARCHAR(16) NOT NULL,
    hash VARCHAR(72) NOT NULL,
    created_by VARCHAR(48) REFERENCES ilks(id),
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    deprecated_at TIMESTAMPTZ
);

CREATE INDEX idx_modules_category ON modules(category);
CREATE INDEX idx_modules_hash ON modules(hash);

-- Degrees (compiled careers)
CREATE TABLE degrees (
    id VARCHAR(48) PRIMARY KEY,           -- "degree:uuid"
    name VARCHAR(128) NOT NULL,
    description TEXT,
    version VARCHAR(16) NOT NULL,
    modules JSONB NOT NULL,
    modules_versions JSONB NOT NULL,
    compiled_prompt TEXT NOT NULL,
    hash VARCHAR(72) NOT NULL,
    capabilities TEXT[] NOT NULL,
    created_by VARCHAR(48) REFERENCES ilks(id),
    created_at TIMESTAMPTZ DEFAULT NOW(),
    deprecated_at TIMESTAMPTZ
);

CREATE INDEX idx_degrees_name ON degrees(name);
CREATE INDEX idx_degrees_hash ON degrees(hash);
CREATE INDEX idx_degrees_capabilities ON degrees USING GIN(capabilities);
```

---

## 9. SHM Region: jsr-identity-\<hive\>

### 9.1 Purpose

Stores ILK table, degrees, and capabilities for fast local read by any node.

### 9.2 Single Writer

Only `SY.identity@<hive>` writes to this region.

### 9.3 Structure

```rust
pub struct IdentityHeader {
    pub magic: u32,                    // 0x4A534944 ("JSID")
    pub version: u32,
    pub seqlock: AtomicU64,
    pub identity_version: u64,
    pub ilk_count: u32,
    pub channel_count: u32,
    pub degree_count: u32,
    pub updated_at: u64,
    pub _reserved: [u8; 64],
}

pub struct IlkEntry {
    pub id: [u8; 48],
    pub ilk_type: u8,                  // 0=tenant, 1=agent, 2=system, 3=human
    pub human_subtype: u8,             // 0=none, 1=internal, 2=external
    pub flags: u8,
    pub tenant_ilk: [u8; 48],
    pub handler_node: [u8; 128],
    pub degree_id: [u8; 48],
    pub degree_hash: [u8; 72],
    pub capabilities_offset: u32,
    pub capabilities_count: u16,
    pub metadata_offset: u32,
    pub metadata_len: u32,
    pub created_at: u64,
    pub updated_at: u64,
}

pub struct ChannelEntry {
    pub channel: [u8; 32],
    pub external_id: [u8; 128],
    pub tenant_ilk: [u8; 48],
    pub ilk_id: [u8; 48],
}

pub struct DegreeEntry {
    pub id: [u8; 48],
    pub name: [u8; 128],
    pub version: [u8; 16],
    pub hash: [u8; 72],
    pub capabilities_offset: u32,
    pub capabilities_count: u16,
    pub compiled_prompt_offset: u32,
    pub compiled_prompt_len: u32,
}
```

### 9.4 Constants

```rust
pub const IDENTITY_MAGIC: u32 = 0x4A534944;  // "JSID"
pub const IDENTITY_VERSION: u32 = 1;
pub const MAX_ILKS: u32 = 65536;
pub const MAX_CHANNELS: u32 = 131072;
pub const MAX_DEGREES: u32 = 4096;

pub const ILK_TYPE_TENANT: u8 = 0;
pub const ILK_TYPE_AGENT: u8 = 1;
pub const ILK_TYPE_SYSTEM: u8 = 2;
pub const ILK_TYPE_HUMAN: u8 = 3;

pub const HUMAN_SUBTYPE_NONE: u8 = 0;
pub const HUMAN_SUBTYPE_INTERNAL: u8 = 1;
pub const HUMAN_SUBTYPE_EXTERNAL: u8 = 2;

pub const ILK_FLAG_ACTIVE: u8 = 0x01;
pub const ILK_FLAG_DELETED: u8 = 0x02;
```

---

## 10. Messages

### 10.1 IDENTITY_CHANGED (broadcast)

Emitted by SY.identity@mother when changes occur.

```json
{
  "routing": {
    "src": "<uuid-sy-identity-mother>",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "IDENTITY_CHANGED"
  },
  "payload": {
    "version": 42,
    "operation": "full_sync",
    "ilks": [...],
    "degrees": [...],
    "modules": [...]
  }
}
```

### 10.2 MODULE_CREATE (unicast to mother)

```json
{
  "meta": {
    "type": "command",
    "target": "SY.identity@mother",
    "action": "module_create"
  },
  "payload": {
    "id": "module:technical-support-v2",
    "name": "Technical Support v2",
    "category": "technical",
    "prompt_text": "You are knowledgeable about products X, Y, Z...",
    "version": "1.0.0"
  }
}
```

### 10.3 DEGREE_CREATE (unicast to mother)

```json
{
  "meta": {
    "type": "command",
    "target": "SY.identity@mother",
    "action": "degree_create"
  },
  "payload": {
    "name": "support-l1-en",
    "modules": ["module:base-english", "module:technical-support"],
    "capabilities": ["support", "english", "level-1"]
  }
}
```

### 10.4 ILK_GRADUATE (unicast to mother)

```json
{
  "meta": {
    "type": "command",
    "target": "SY.identity@mother",
    "action": "ilk_graduate"
  },
  "payload": {
    "ilk": "ilk:new-agent-uuid",
    "degree_id": "degree:abc123",
    "handler_node": "AI.support.l1@production"
  }
}
```

### 10.5 ILK_CREATE (unicast to mother)

```json
{
  "meta": {
    "type": "command",
    "target": "SY.identity@mother",
    "action": "ilk_create"
  },
  "payload": {
    "type": "human",
    "human_subtype": "external",
    "tenant_ilk": "ilk:tenant-acme",
    "name": "John Doe",
    "channels": [
      { "channel": "whatsapp", "external_id": "+5491155551234" }
    ]
  }
}
```

### 10.6 ILK_LINK (unicast to mother)

```json
{
  "meta": {
    "type": "command",
    "target": "SY.identity@mother",
    "action": "ilk_link"
  },
  "payload": {
    "ilk": "ilk:abc123",
    "channel": "email",
    "external_id": "john@example.com"
  }
}
```

### 10.7 ILK_DELETE (unicast to mother)

```json
{
  "meta": {
    "type": "command",
    "target": "SY.identity@mother",
    "action": "ilk_delete"
  },
  "payload": {
    "ilk": "ilk:abc123"
  }
}
```

---

## 11. OPA and Layer 3

OPA derives `tenant` from `src_ilk` via `data.identity`. Rules can filter by tenant OR by specific ilk:

```rego
package router

# Helper: get tenant from ilk
get_tenant(ilk) = tenant {
    tenant := data.identity[ilk].tenant_ilk
}

# ============================================
# RULES BY SPECIFIC ILK (highest priority)
# ============================================

# VIP client gets direct CEO support
target = "AI.support.ceo@production" {
    input.meta.src_ilk == "ilk:cliente-vip-especial"
}

# New agent needs supervision
target = "AI.supervisor@production" {
    input.meta.src_ilk == "ilk:agente-nuevo"
    input.meta.context.needs_review == true
}

# ============================================
# RULES BY TENANT
# ============================================

# ACME: Route based on dst_ilk (if it's an agent)
target = node {
    tenant := get_tenant(input.meta.src_ilk)
    tenant == "ilk:tenant-acme"
    ilk := input.meta.dst_ilk
    ilk != null
    handler := data.identity[ilk].handler_node
    handler != null
    node := handler
}

# ACME: Route based on required capability
target = node {
    tenant := get_tenant(input.meta.src_ilk)
    tenant == "ilk:tenant-acme"
    required := input.meta.context.required_capability
    some ilk
    data.identity[ilk].type == "agent"
    required in data.identity[ilk].capabilities
    node := data.identity[ilk].handler_node
}

# BETA: Different routing logic for this tenant
target = "AI.support.premium@production" {
    tenant := get_tenant(input.meta.src_ilk)
    tenant == "ilk:tenant-beta"
    # All BETA traffic goes to premium support
}

# ============================================
# AUTHORIZATION RULES
# ============================================

# Only internal humans can query system status (any tenant)
allow {
    input.meta.action == "system_status"
    ilk := input.meta.src_ilk
    data.identity[ilk].type == "human"
    data.identity[ilk].human_subtype == "internal"
}
```

**Key principles:**
- One policy file per hive, containing rules for ALL tenants
- Rules by specific ilk have highest priority (most specific)
- Rules by tenant are general (apply to all ilks of that tenant)
- Tenant is derived from `src_ilk`, never sent in message

---

## 12. Complete Flow: Incoming Message

```
1. WhatsApp sends message from +5491155551234
                │
                ▼
2. IO.whatsapp receives webhook
                │
                ▼
3. IO.whatsapp reads SHM jsr-identity:
   resolve("whatsapp", "+5491155551234") → ilk:abc123
                │
        ┌───────┴───────┐
        │               │
   ILK exists      ILK not found
        │               │
        ▼               ▼
   ilk:abc123      Send ILK_CREATE to mother
                        │
                        ▼
                   Receive ilk:new-uuid
                        │
                        ▼
4. IO.whatsapp builds message with src_ilk (NO tenant)
                │
                ▼
5. Send to router:
   {
     routing: { dst: null },
     meta: { 
       target: "AI.support.*",
       src_ilk: "ilk:abc123"
     }
   }
                │
                ▼
6. Router invokes OPA
                │
                ▼
7. OPA derives tenant: data.identity["ilk:abc123"].tenant_ilk
   OPA evaluates rules by tenant and/or ilk
   OPA decides: → AI.support.l1@production
                │
                ▼
8. Router delivers to AI.support.l1
                │
                ▼
9. AI.support.l1 reads degree from SHM:
    - Verifies degree_hash integrity
    - Gets compiled_prompt
    - Processes message
                │
                ▼
10. AI.support.l1 responds with dst_ilk
                │
                ▼
11. OPA resolves dst_ilk → IO.whatsapp
                │
                ▼
12. IO.whatsapp sends to WhatsApp
```

---

## 13. Administration Hierarchy

```
┌─────────────────────────────────────────────────────────────┐
│              Administration (outside the system)             │
│                                                              │
│  Level 1: Create/update knowledge modules                   │
│           (defines capabilities for AI)                     │
│                                                              │
│  Level 2: Create degrees (compile modules)                  │
│           (defines careers/roles)                           │
│                                                              │
│  Level 3: Graduate agents with degrees                      │
│           (assigns knowledge to agents)                     │
│                                                              │
│  Level 4: Register humans (internal/external)               │
│           (contact data only, no roles for humans)          │
│                                                              │
│  Level 5: Register tenants                                  │
│           (organizations, billing entities)                 │
│                                                              │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ configuration
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    JSON Router System                        │
│                                                              │
│  • Executes according to configuration                      │
│  • Does NOT self-modify                                     │
│  • Does NOT assign roles                                    │
│  • Does NOT verify human capabilities                       │
│  • AI agents managed 100% by AI                            │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

---

## 14. SHM Regions Summary

| Region | Writer | Content | Magic |
|--------|--------|---------|-------|
| `jsr-<router-uuid>` | Router | Connected nodes | 0x4A535352 |
| `jsr-config-<hive>` | SY.config.routes | Routes, VPNs | 0x4A534343 |
| `jsr-lsa-<hive>` | Gateway | Remote topology | 0x4A534C41 |
| `jsr-opa-<hive>` | SY.opa.rules | Compiled WASM | 0x4A534F50 |
| **`jsr-identity-<hive>`** | **SY.identity** | **ILKs, degrees, modules** | **0x4A534944** |

---

## 15. Entity Summary

| Entity | Identifier | Versioned | Hash/Seal | Created by |
|--------|------------|-----------|-----------|------------|
| Module | `module:<name>` | ✅ Yes | ✅ Yes | Designer (human/internal) |
| Degree | `degree:<uuid>` | ✅ Yes | ✅ Integrity seal | System (compiles) |
| ILK | `ilk:<uuid>` | References degree | Verifies | System (registers/graduates) |
| Tenant | `ilk:<uuid>` | ❌ | ❌ | Admin |

---

## 16. References

| Topic | Document |
|-------|----------|
| General architecture | `01-arquitectura.md` |
| Message protocol | `02-protocolo.md` |
| Shared memory | `03-shm.md` |
| Routing L1/L2 | `04-routing.md` |
| SY nodes | `SY_nodes_spec.md` |
