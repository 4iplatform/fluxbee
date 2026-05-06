# Fluxbee — Identity v2.1 Addendum: Agent Cognitive Definition

**Status:** v2.1 addendum — ready for implementation
**Date:** 2026-05-06
**Audience:** SY.identity developer, ai.generic runtime developer, SY.architect (Archi) developer
**Related:** `identity-v2.1-changes.md` (base v2.1 changes)
**Scope:** add cognitive definition (role + skills + handbooks) for ILKs of type `agent`. Hash-based asset references. Hot-reload on SHM updates without node restart.

---

## 1. Summary

This addendum extends identity v2.1 to support a cognitive definition for ILKs of type `agent`. The definition consists of references (hashes) to role, skills, and handbook assets stored in the blob filesystem. AI nodes resolve these hashes at boot and during runtime, composing their system prompt dynamically. Updates to an agent's definition propagate via SHM and trigger automatic prompt recomposition without restarting the node.

| Concept | Storage | Owner |
|---|---|---|
| Agent's role/skill/handbook references (hashes) | identity DB + SHM | SY.identity |
| Asset content (JSON files) | blob filesystem | Archi (asset builder sub-agent) |
| Active prompt and load state | AI node memory | AI node runtime (`ai.generic`) |
| Asset catalog (hash → metadata) | Archi memory | Archi |

---

## 2. Conceptual Model

### 2.1 What is an agent's definition

An agent (ILK of type `agent`) has a cognitive definition composed of:

- **Role**: the general framework — persona, tone, limits. One per agent.
- **Skills**: specific operational capabilities — instructions, examples, constraints. Multiple per agent.
- **Handbooks**: reference documents — context the agent uses to make decisions. Multiple per agent.

Each is stored as a JSON asset in the blob filesystem, named by its content hash. The agent's ILK in identity stores only the hashes — not the content.

### 2.2 Why hashes as references

Naming files by their content hash gives several properties:

**Content-addressable.** The hash uniquely identifies the content. Two files with the same hash have the same content.

**Immutable.** A given hash always points to the same content. To update an asset, you generate a new file with a new hash and update the references in the ILK.

**No symbolic registry needed in identity.** Identity stores opaque hashes. It doesn't need to know what each hash represents semantically. The semantic mapping (hash → "skill:ticket-analysis") lives in Archi.

**Natural versioning.** Old hashes remain in blob even after newer versions exist. Other agents can still reference them. Audit trails are intact.

### 2.3 Roles in the system

| Component | Responsibility |
|---|---|
| Archi (asset builder sub-agent) | Generate JSON assets with correct schema using typed function calling. Compute hashes. Write to blob. Maintain in-memory catalog. |
| SY.identity | Store the hashes in the agent's ILK. Propagate via SHM and sync. Validate that only `agent` ILKs have definitions. |
| AI node runtime (`ai.generic`) | Read own ILK from SHM at boot and during polling. Resolve hashes by reading blob files. Compose system prompt. Recompose when hashes change. Report state via CONFIG_GET. |
| Blob filesystem | Store asset files by hash filename. Sync between hives (existing infra). |

### 2.4 Legacy roles/capabilities are retired

This addendum supersedes the earlier identity-v2 idea of storing `roles` and `capabilities` arrays in `identity_ilks.definition.current`.

That older model was only partially implemented. `SY.identity` can currently accept those fields, but the live SHM/router projection does not materialize them as usable routing facts. Router-injected `data.identity` exposes empty `roles` and `capabilities` arrays today. Treat them as legacy scaffolding, not as a live contract.

For this implementation:

- `identity_ilks.definition` becomes the cognitive definition document shown below.
- Legacy `roles` and `capabilities` in `ILK_REGISTER` / `ILK_UPDATE` are removed or ignored during this alpha cleanup.
- OPA/routing consumes the new hash facts projected from SHM.
- `behavior.capabilities.*` in AI node config remains operational runtime config and is unrelated to identity cognitive definition.

---

## 3. Asset File Schema

Assets are JSON files stored in `blob://agent-assets/<hash>.json`. The filename is the sha256 of the file content (excluding the filename itself). Each file has a structured schema by type.

### 3.1 Role schema

```json
{
  "asset_type": "role",
  "id": "role:support-l1-analyst",
  "version": 1,
  "name": "Support L1 Analyst",
  "description": "First-line support analyst role",
  "persona": "You are a Level 1 support analyst. Your job is to receive support messages, analyze them, and produce structured responses.",
  "tone": "Professional, concise, factual.",
  "limits": [
    "Do not take action on external systems",
    "Do not escalate (no escalation path in this solution)",
    "Respond only with structured JSON containing analysis and recommendations"
  ]
}
```

### 3.2 Skill schema

```json
{
  "asset_type": "skill",
  "id": "skill:ticket-analysis",
  "version": 2,
  "name": "Ticket Analysis",
  "category": "support",
  "description": "Ability to analyze and classify support tickets",
  "instructions": [
    "Identify the primary issue category: billing, technical, account, or other",
    "Assess urgency based on content, not just stated priority",
    "Extract key facts: affected service, error codes, timeline",
    "Produce a structured response with analysis, suggested_action, confidence"
  ],
  "examples": [
    {
      "input": "Hi, I cannot login to my account since yesterday. Error 502.",
      "output": {
        "analysis": "Authentication issue, server error 502 suggests backend problem",
        "suggested_action": "Check backend status, retry in 5 minutes",
        "confidence": 0.85
      }
    }
  ],
  "constraints": [
    "Always respond in the same language as the ticket"
  ]
}
```

### 3.3 Handbook schema

```json
{
  "asset_type": "handbook",
  "id": "handbook:support-procedures-v2",
  "version": 2,
  "name": "Support Procedures Manual",
  "category": "reference",
  "description": "Reference manual for support operations",
  "sections": [
    {
      "title": "Issue Categories",
      "content": "We classify issues into four categories: billing, technical, account, other...",
      "subsections": [
        {
          "title": "Billing Issues",
          "content": "Billing issues include invoice discrepancies..."
        }
      ]
    }
  ]
}
```

### 3.4 Schema enforcement

The asset builder sub-agent inside Archi uses OpenAI typed function calling to generate these JSON files. The function definitions enforce the schema — the LLM cannot produce invalid structure. This is the only path through which assets should be created in v1. Manual editing is possible but not recommended.

---

## 4. Identity Changes

### 4.1 Definition shape

The `identity_ilks.definition` JSONB column already exists in PostgreSQL. The Rust `IlkRecord` must be updated to carry this JSON value directly. Its shape for agent ILKs is:

```json
{
  "role_hash": "a1b2c3d4e5f6...",
  "skill_hashes": [
    "e5f6a7b8c9d0...",
    "c9d0e1f23a4b..."
  ],
  "handbook_hashes": [
    "3a4b5c6d7e8f..."
  ]
}
```

For non-agent ILKs (humans, system), the definition remains `{}`.

Hashes are sha256, represented as 64-character hex strings in JSON. In SHM they are stored as 32-byte arrays (raw bytes).

The definition contains hashes only. It does not contain role text, skill instructions, handbook content, or symbolic asset names.

### 4.2 New protocol verb: `ILK_SET_DEFINITION`

```
ILK_SET_DEFINITION
  Request:
  {
    "ilk_id": "ilk:ai-support",
    "definition": {
      "role_hash": "a1b2c3d4...",
      "skill_hashes": ["e5f6a7b8...", "c9d0e1f2..."],
      "handbook_hashes": ["3a4b5c6d..."]
    }
  }

  Response (success):
  {
    "status": "ok",
    "ilk_id": "ilk:ai-support",
    "definition": { ... }
  }

  Response (error):
  {
    "status": "error",
    "error_code": "INVALID_ILK_TYPE"  // or other
  }
```

**Validation:**
- `ilk_id` must exist.
- ILK must be of type `agent`. Otherwise returns `INVALID_ILK_TYPE`.
- `role_hash` is optional but if present must be 64 hex chars.
- `skill_hashes` array max 16 entries, each 64 hex chars.
- `handbook_hashes` array max 8 entries, each 64 hex chars.
- Identity does NOT validate that the hashes correspond to actual files in blob. That's the agent's responsibility at load time.

**Authorization:** SY.architect (Archi) and SY.admin. Other nodes are rejected.

### 4.3 Registration flow

`ILK_REGISTER` does not accept a cognitive definition in v1.

AI nodes are born as an operational body first:

1. Orchestrator spawns the node with normal runtime/config/tenant data.
2. The node boots and registers or reuses its ILK through the existing identity path.
3. The node starts with the default minimal prompt and polls identity SHM.
4. Archi later calls `ILK_SET_DEFINITION` with the asset hashes.
5. `ai.generic` sees the SHM seq/hash change, loads assets from blob, and swaps its active prompt.

This keeps operational config and cognitive definition independent. A node can run without a cognitive definition; it is just unconfigured from the perspective of agent behavior.

### 4.4 IlkEntry SHM extension

The `IlkEntry` struct gains fields for the definition hashes:

```rust
#[repr(C)]
pub struct IlkEntry {
    // ... existing fields ...

    pub role_hash: [u8; 32],              // NEW — all zeros if no role
    pub skill_hashes: [[u8; 32]; 16],     // NEW — up to 16 skills, unused slots are zeros
    pub skill_count: u8,                   // NEW — actual number of skills (0-16)
    pub handbook_hashes: [[u8; 32]; 8],   // NEW — up to 8 handbooks, unused slots are zeros
    pub handbook_count: u8,                // NEW — actual number of handbooks (0-8)
}
```

Total addition: 32 + (16 × 32) + 1 + (8 × 32) + 1 = 802 bytes per IlkEntry.

For non-agent ILKs, all hash fields are zeros and counts are 0. The space is "wasted" but avoids needing a separate SHM region. With humans and system nodes also having these fields ready, future use cases (tagging humans, etc.) require no SHM changes.

### 4.5 SHM seq increment

When `ILK_SET_DEFINITION` succeeds, identity:

1. Updates the `IlkRecord` in memory with the new definition.
2. Updates the DB.
3. Updates the `IlkEntry` in SHM (replaces hashes, updates counts).
4. **Increments the global `seq` counter** in the identity SHM region.
5. Emits a sync delta to workers (the delta includes the updated `IlkRecord` with the new definition).

The global seq increment is what AI nodes detect during polling.

### 4.6 New error codes

| Code | Description |
|---|---|
| `INVALID_ILK_TYPE` | `ILK_SET_DEFINITION` called on a non-agent ILK |
| `INVALID_DEFINITION` | Definition payload doesn't match expected schema (too many hashes, invalid hex, etc.) |

---

## 5. AI Node Behavior (`ai.generic` runtime)

### 5.1 Boot sequence

When an AI node starts:

1. Connects to router, registers self if needed (creates ILK if first boot).
2. Reads its own `IlkEntry` from identity SHM.
3. Extracts `role_hash`, `skill_hashes[..skill_count]`, `handbook_hashes[..handbook_count]`.
4. If all hashes are zero (no definition set yet), uses the default minimal prompt and enters `empty` state.
5. If hashes are present, attempts to load each from blob:
   - Reads `blob://agent-assets/<hash>.json` for each hash.
   - Parses JSON and validates `asset_type` matches expected (role/skill/handbook).
   - On success, adds to the composition pool.
   - On failure (file not found, invalid JSON, etc.), records the failed hash and continues with others.
6. Composes the system prompt from successfully loaded assets (see §6).
7. Records the load state (`empty`, `partial`, `composed`, `error`).
8. Starts the polling loop (§5.3).

The node is operational regardless of load state. Even with `empty` state, it can respond (with the default prompt indicating it's not configured).

### 5.2 Default prompt (when definition is empty)

Hardcoded in `ai.generic`:

```
You are an AI agent that has not yet received its operational configuration.

If you receive a message, respond with this exact structure:
{
  "status": "unconfigured",
  "message": "This agent has not been configured yet. Please contact the system administrator to assign a role, skills, and handbooks."
}

Do not interpret messages beyond confirming receipt.
Do not perform any action.
```

This prompt is the same for all unconfigured AI nodes regardless of provider or specialization.

### 5.3 Polling loop

Background task in `ai.generic`:

```
loop {
    sleep(POLL_INTERVAL_SECONDS);  // 5-10 seconds, configurable

    let current_seq = read_identity_shm_seq();

    if current_seq == last_known_seq {
        continue;  // no changes anywhere in identity
    }

    last_known_seq = current_seq;

    let my_entry = read_my_ilk_entry();
    let new_hashes = (my_entry.role_hash, my_entry.skill_hashes[..count], my_entry.handbook_hashes[..count]);

    if new_hashes == last_known_hashes {
        continue;  // changes were elsewhere, not in my definition
    }

    // My definition changed — recompose
    let load_result = load_assets(&new_hashes);
    let new_prompt = compose_prompt(&load_result.loaded);

    swap_active_prompt(new_prompt);
    update_load_state(load_result);
    last_known_hashes = new_hashes;
}
```

The polling interval is configurable per node (default 10 seconds). The cost is negligible — reading the seq is a memory access.

### 5.4 Asset loading and partial state

Loading an asset can fail for several reasons:

- File doesn't exist in blob (Archi hasn't synced yet, or was deleted).
- File exists but JSON is invalid.
- File exists but `asset_type` doesn't match expected slot (a skill hash pointing to a role file).
- IO error reading the file.

The node handles each failure independently:

- Failed assets are recorded in the load state.
- Successfully loaded assets are still composed into the prompt.
- Next polling cycle retries failed assets (in case the blob was eventually consistent).

A node can transition between states: `empty` → `partial` → `composed` as assets become available, or `composed` → `partial` if some assets become unavailable (rare).

### 5.5 Prompt composition template

The composition is hardcoded in `ai.generic` for v1. The order is deterministic:

```
[ROLE]
{role.persona}

Tone: {role.tone}

Limits:
- {role.limits[0]}
- {role.limits[1]}
...

--- SKILLS ---

[{skill_1.name}]
Description: {skill_1.description}

Instructions:
1. {skill_1.instructions[0]}
2. {skill_1.instructions[1]}
...

Examples:
Input: {skill_1.examples[0].input}
Output: {skill_1.examples[0].output}

Constraints:
- {skill_1.constraints[0]}

[{skill_2.name}]
...

--- REFERENCE MATERIAL ---

[{handbook_1.name}]

## {section_1.title}
{section_1.content}

### {subsection_1.title}
{subsection_1.content}

[{handbook_2.name}]
...
```

The template is the same for all AI nodes regardless of provider. If different providers (OpenAI, Anthropic, etc.) need different prompt formats in the future, the composition is parameterized then.

### 5.6 Prompt and asset budgets

The main risk is not storage size; it is loading too much prompt into the model request and into node memory. v1 uses hard limits:

| Limit | Value |
|---|---:|
| Role assets per agent | 1 |
| Skill assets per agent | 16 |
| Handbook assets per agent | 8 |
| Asset file size | 256 KiB max |
| Composed system prompt | 64 KiB max |

Composition is deterministic and budgeted in this order:

1. role
2. skills in listed order
3. handbooks in listed order

If the composed prompt would exceed 64 KiB, `ai.generic` truncates handbook content first, then skill examples, while preserving role, skill names, skill instructions, and asset hash provenance. The node reports truncation in `CONFIG_GET`.

### 5.7 State reporting via CONFIG_GET

The node responds to `CONFIG_GET` with its operational state, including the cognitive definition status:

```json
{
  "node_name": "AI.support@motherbee",
  "ilk_id": "ilk:ai-support",
  "runtime": "ai.generic",
  "runtime_version": "1.2.3",
  "config_version": 5,
  "definition_state": "composed",
  "definition": {
    "role_hash_loaded": "a1b2c3d4...",
    "skill_hashes_loaded": ["e5f6a7b8...", "c9d0e1f2..."],
    "handbook_hashes_loaded": ["3a4b5c6d..."],
    "skill_hashes_failed": [],
    "handbook_hashes_failed": [],
    "prompt_truncated": false,
    "last_recompose_at": "2026-04-16T15:30:00Z"
  },
  "active_prompt_chars": 8243
}
```

`definition_state` values:
- `empty` — no hashes set in ILK, using default prompt.
- `composed` — all hashes loaded successfully.
- `partial` — some hashes loaded, some failed.
- `error` — no hashes could be loaded (all failed).

This is the standard way for Archi or admin to check that an agent is correctly configured.

---

## 6. Archi Asset Builder Sub-Agent

### 6.1 Purpose

A specialized sub-agent within Archi (not a separate Fluxbee node) responsible for creating, updating, and maintaining agent assets. Uses OpenAI typed function calling to generate schema-correct JSON files.

### 6.2 Functions

The asset builder exposes typed functions that match the asset schemas:

```
generate_role_asset(name, description, persona, tone, limits) → role JSON
generate_skill_asset(name, category, description, instructions, examples, constraints) → skill JSON
generate_handbook_asset(name, category, description, sections) → handbook JSON
```

The LLM (within the sub-agent) cannot produce malformed JSON because the function signatures enforce structure. Each generated JSON is then:

1. Validated by Archi against the v1 schema.
2. Hashed (sha256 of the canonical JSON).
3. Written to blob as `agent-assets/<hash>.json`.
4. Indexed in Archi's in-memory catalog.

### 6.3 Catalog maintenance

Archi maintains an in-memory catalog: `hash → metadata`. The metadata includes the asset's `id`, `name`, `description`, `version`, `category`. This catalog is rebuilt on Archi startup by scanning the blob.

### 6.4 Bootstrap on Archi start

On Archi startup:

```
for each file in blob://agent-assets/:
    read file content
    compute sha256 of content
    if sha256 != filename:
        log warning (file may be corrupted or manually edited)
        continue
    parse JSON
    if invalid JSON:
        log warning
        continue
    extract metadata (id, name, version, asset_type, category)
    store in catalog: filename → metadata
```

After bootstrap, Archi has full visibility of available assets and can answer questions like "what skills do I have available for support?" or "is there a handbook about billing procedures?"

### 6.5 Asset lifecycle

Assets are immutable once created (their hash is their identity). Updating an asset means generating a new file with a new hash and updating references in the ILKs that should use the new version. The old asset remains in blob — useful for audit, rollback, or other agents still using it.

Archi may eventually run garbage collection (delete assets in blob that are no longer referenced by any ILK), but this is optional and not in v1.

---

## 7. Workflow Examples

### 7.1 Deploying a new AI agent

```
1. Archi designs solution (e.g., "support assistant").
2. Archi (asset builder) generates assets:
   - role: support-l1-analyst → file role-a1b2c3....json
   - skill: ticket-analysis → file skill-e5f6a7....json
   - skill: empathetic-response → file skill-c9d0e1....json
   - handbook: support-procedures → file handbook-3a4b5c....json
3. Archi writes files to blob://agent-assets/
4. Archi/admin runs the node normally with runtime `ai.generic`.
5. Node boots, registers/reuses its ILK, and runs with the default prompt.
6. Archi reads the node ILK through `get_ilk`.
7. Archi calls `ILK_SET_DEFINITION` with the asset hashes.
8. Identity updates DB/SHM and increments seq.
9. Node detects the hash change, loads assets from blob, composes prompt, and becomes configured.
```

### 7.2 Adding a skill to an existing agent

```
1. Archi (asset builder) generates new skill: file skill-NEW....json
2. Archi writes file to blob.
3. Archi calls ILK_SET_DEFINITION on identity:
   {
     ilk_id: "ilk:ai-support",
     definition: {
       role_hash: "a1b2c3d4...",  // unchanged
       skill_hashes: ["e5f6a7b8...", "c9d0e1f2...", "NEW..."],  // new added
       handbook_hashes: ["3a4b5c6d..."]  // unchanged
     }
   }
4. Identity updates DB, SHM (with new IlkEntry hashes), increments seq.
5. Within polling interval (5-10s), AI node detects seq change.
6. Node reads its IlkEntry, sees skill_count went from 2 to 3.
7. Node loads the new skill from blob.
8. Node recomposes prompt with the additional skill.
9. New requests use the updated prompt; in-flight requests finish with the old.
```

### 7.3 An asset file failed to sync

```
1. Archi sets new definition with hash "FAIL..." that exists in motherbee blob but not yet synced to a worker.
2. AI node on the worker detects seq change, reads new hashes.
3. Node tries to load "FAIL..." from local blob → file not found.
4. Node logs warning, marks state as "partial", continues with other assets.
5. CONFIG_GET on the node shows skill_hashes_failed: ["FAIL..."].
6. Eventually Syncthing syncs the file to the worker.
7. Next polling cycle, node retries "FAIL..." → success.
8. Node recomposes prompt, state goes from "partial" to "composed".
```

---

## 8. SHM Layout Update

### 8.1 IlkEntry final layout

```rust
#[repr(C)]
pub struct IlkEntry {
    pub ilk_id: [u8; 16],
    pub tenant_id: [u8; 16],
    pub ilk_type: u8,
    pub registration_status: u8,
    pub flags: u8,
    pub _pad0: [u8; 5],
    // ... existing identification fields ...

    // NEW — cognitive definition for agents
    pub role_hash: [u8; 32],
    pub skill_hashes: [[u8; 32]; 16],
    pub skill_count: u8,
    pub _pad_skills: [u8; 7],
    pub handbook_hashes: [[u8; 32]; 8],
    pub handbook_count: u8,
    pub _pad_handbooks: [u8; 7],

    pub created_at: u64,
    pub updated_at: u64,
}
```

Total addition: 802 bytes + 14 padding bytes for alignment = 816 bytes per entry.

For a system with 8192 max ILKs, the total SHM growth is ~6.5MB. Acceptable.

### 8.2 No new SHM region

All cognitive definition data fits in the extended `IlkEntry`. No need for a separate SHM region. This simplifies the implementation.

### 8.3 Router/OPA projection

The router already injects identity SHM data into OPA as `data.identity`. After this change, router projection must expose agent definition hashes as hex strings:

```json
{
  "tenant_id": "tnt:...",
  "ilk_type": "agent",
  "registration_status": "complete",
  "handler_node": "AI.support@motherbee",
  "role_hash": "64hex...",
  "skill_hashes": ["64hex..."],
  "handbook_hashes": ["64hex..."]
}
```

OPA can route by these facts:

```rego
target = "AI.support@motherbee" {
  src := object.get(data.identity_aliases, input.meta.src_ilk, input.meta.src_ilk)
  data.identity[src].tenant_id == "tnt:..."
  dst := input.meta.dst_ilk
  some h
  h := data.identity[dst].skill_hashes[_]
  h == "64hex..."
}
```

OPA does not read blob content and does not understand role/skill semantics. It only matches projected hashes and other identity fields. Archi or policy tooling is responsible for translating symbolic concepts such as `skill:ticket-analysis` into hashes before installing routing rules.

---

## 9. Database Schema

No DB schema changes. The `definition JSONB` field already exists in `identity_ilks`. Its old partial use for `current.roles/current.capabilities` is retired by this addendum. The new content shape is enforced by validation in Rust code, not by DB schema.

---

## 10. SDK Changes

### 10.1 Rust SDK (`fluxbee_sdk`)

```rust
/// Set the cognitive definition for an agent ILK.
pub async fn set_ilk_definition(
    sender: &NodeSender,
    ilk_id: &str,
    role_hash: Option<&str>,
    skill_hashes: &[&str],
    handbook_hashes: &[&str],
) -> Result<IlkSetDefinitionResponse, SdkError>;
```

### 10.2 Go SDK (`fluxbee-go-sdk`)

```go
// SetIlkDefinition assigns a cognitive definition to an agent ILK.
func SetIlkDefinition(
    sender *Sender,
    ilkID string,
    roleHash string, // empty string if none
    skillHashes []string,
    handbookHashes []string,
) (*IlkSetDefinitionResponse, error)
```

These are used primarily by Archi's executor. Direct invocation by humans or by AI nodes is rare.

---

## 11. Implementation Checklist

```
[ ] Remove legacy roles/capabilities from IlkRecord and definition persistence
[ ] Remove or ignore legacy roles/capabilities fields from ILK_REGISTER / ILK_UPDATE
[ ] IlkRecord: add definition: Value
[ ] IlkRecord: validate definition shape on ILK_SET_DEFINITION
[ ] IlkEntry SHM: add role_hash, skill_hashes[16], skill_count,
                  handbook_hashes[8], handbook_count + padding
[ ] ilk_entry_from_record: serialize hashes to bytes,
                            zero unused slots, set counts
[ ] New handler: ILK_SET_DEFINITION
[ ] Validation: reject if ilk_type != "agent"
[ ] Validation: reject if hashes don't match expected length/format
[ ] Validation: reject if skill_hashes > 16 or handbook_hashes > 8
[ ] Update DB on ILK_SET_DEFINITION
[ ] Update SHM IlkEntry on ILK_SET_DEFINITION
[ ] Increment SHM seq counter on ILK_SET_DEFINITION
[ ] Emit sync delta with updated IlkRecord
[ ] Authorization: SY.architect, SY.admin only
[ ] Router: project role_hash, skill_hashes, handbook_hashes into data.identity
[ ] Admin: expose set_ilk_definition and update action help

[x] ai.generic: read own IlkEntry at boot
[x] ai.generic: load assets from blob://agent-assets/<hash>.json
[x] ai.generic: parse and validate asset schema
[x] ai.generic: compose system prompt according to template and 64 KiB budget
[x] ai.generic: handle missing/invalid assets gracefully (partial state)
[x] ai.generic: default prompt when definition is empty
[x] ai.generic: polling loop reading SHM seq
[x] ai.generic: detect own hash changes, recompose without restart
[x] ai.generic: report state via CONFIG_GET response
[x] ai.generic: log all asset load successes and failures

[ ] Archi: asset builder sub-agent with typed functions
[ ] Archi: hash computation (sha256 of canonical JSON)
[ ] Archi: write asset files to blob with hash filename
[ ] Archi: in-memory catalog of assets
[ ] Archi: bootstrap catalog by scanning blob on startup
[ ] Archi: validate filename matches content hash on bootstrap

[ ] SDK: add set_ilk_definition (Rust)
[ ] SDK: add SetIlkDefinition (Go)

[ ] ILK_GET_RESPONSE: include definition in response
[ ] CONFIG_GET on AI nodes: include definition_state and load info
```

---

## 12. What is NOT in this addendum

- Tools/actions for agents (`tool_refs`). Deferred to v2 or later.
- Garbage collection of unreferenced assets in blob. Manual for v1.
- Multi-language assets (per-locale skills). Not in scope.
- Asset versioning beyond hash-based content addressing. Sufficient for v1.
- Tagging humans with claims/responsibilities. Schema is ready (definition field exists for all ILK types) but no semantics defined for non-agents.
- Frontdesk cognitive definition. SY.frontdesk.gov keeps its fixed role/capabilities and is not governed by this addendum.
- Hot-reload of role-only changes (today, any change recomposes the full prompt). Optimization for later.
- Asset signing or trust verification. Files are trusted because blob is internal.
- Skill/handbook editor UI. Manual JSON editing or asset builder agent only.

---

## 13. Decisions

| Decision | Rationale |
|---|---|
| Hashes as references, not symbolic IDs | Content-addressable, immutable, no symbolic registry needed in identity |
| Filename = hash of content | Self-verifying; Archi can detect tampering on bootstrap |
| Definition in SHM (hashes only) | Hot-path detection of changes without DB roundtrip |
| Max 16 skills, 8 handbooks per agent | Covers any realistic use case; agents needing more should be split |
| Max composed prompt 64 KiB | Protects node memory and model request budget; large handbooks are truncated deterministically |
| Same fields on all ILK types (Option A) | Simple; future use of fields for non-agents requires no SHM changes |
| Polling on SHM seq, not direct IlkEntry | Most changes don't affect a given agent; cheap to skip |
| Recomposition without restart | Skills can be added/removed dynamically; no operational disruption |
| Archi sub-agent generates assets via typed functions | Schema enforced at generation; no malformed JSON in blob |
| Asset builder is internal to Archi, not a separate node | Simpler; keeps Archi as the cognitive design center |
| Composition template hardcoded in ai.generic for v1 | Same template for all providers; parameterize later if needed |
| Agent reports state via CONFIG_GET, not SHM writes | Identity is the only SHM writer; agents are read-only consumers |
| Default minimal prompt for unconfigured agents | Agent always operational, even if not yet configured |
| Tools/actions deferred to v2 | Tool registry is its own complexity; not blocking for v1 |
| Legacy roles/capabilities retired | They were only partially implemented and are replaced by hash-based cognitive definition |
