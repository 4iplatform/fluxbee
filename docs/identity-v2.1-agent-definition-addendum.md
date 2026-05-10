# Fluxbee — Agent Cognitive Definition

**Status:** ready for implementation
**Date:** 2026-05-10 (current revision; supersedes earlier drafts)
**Audience:** SY.identity developer, ai.generic runtime developer, SY.architect (Archi) developer
**Scope:** cognitive definition (role + skills + handbooks + personality) for ILKs of type `agent`. Hash-based asset references. Hot-reload on SHM updates without node restart.

---

## 1. Summary

Identity supports a cognitive definition for ILKs of type `agent`. The definition consists of references (hashes) to role, skills, handbook, and personality assets stored in the blob filesystem. AI nodes resolve these hashes at boot and during runtime, composing their system prompt dynamically. Updates to an agent's definition propagate via SHM and trigger automatic prompt recomposition without restarting the node.

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

- **Role**: what the agent *does* — function, tone, limits. One per agent.
- **Skills**: specific operational capabilities — instructions, examples, constraints. Multiple per agent.
- **Handbooks**: reference documents — context the agent uses to make decisions. Multiple per agent.
- **Personality**: who the agent *is* as a person — nationality, languages, timezone, education, biographical background. At most one per agent. Some fields are system-queryable (timezone, language, country) for routing/scheduling; the rest is rendered into the system prompt.

Each is stored as a JSON asset in the blob filesystem, named by its content hash. The agent's ILK in identity stores only the hashes — not the content.

The role and personality assets are intentionally orthogonal: `role.description` declares the function ("first-line support analyst — receives, classifies, responds with structured output"); the personality declares the person ("Argentinian, mid-career engineer, fluent in Spanish and English, based in America/Argentina/Mendoza"). They render into different sections of the composed prompt and are reusable independently.

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
  "description": "First-line support analyst — receives incoming support messages, analyzes them, classifies the issue, and produces a structured response with analysis, suggested action, and confidence.",
  "tone": "Professional, concise, factual.",
  "limits": [
    "Do not take action on external systems",
    "Do not escalate (no escalation path in this solution)",
    "Respond only with structured JSON containing analysis and recommendations"
  ]
}
```

`description` is the canonical functional statement and is rendered as the role section of the composed prompt. It must be self-contained and read well as part of a system prompt (1–3 sentences typical). There is no separate `persona` field — agent identity comes from the personality asset, not from the role.

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

### 3.4 Personality schema

```json
{
  "asset_type": "personality",
  "id": "personality:argentine-engineer-1985",
  "version": 1,
  "name": "Argentine Engineer (1985)",
  "description": "Argentine engineer profile, mid-career, Mendoza background",

  "system_fields": {
    "timezone": "America/Argentina/Mendoza",
    "country_code": "AR",
    "region_code": "AR-M",
    "primary_language": "es-AR",
    "additional_languages": [
      { "code": "en", "level": "C1" },
      { "code": "pt-BR", "level": "B1" }
    ]
  },

  "biographical": {
    "nationality": "Argentinian",
    "display_name": "Lucía",
    "birth_year": 1985,
    "birth_place": "Mendoza, Argentina",
    "current_residence": "Buenos Aires, Argentina",
    "education": [
      { "institution": "Universidad Nacional de Cuyo", "degree": "Ingeniería en Sistemas", "year_completed": 2008, "field": "Software Engineering" }
    ],
    "professional_background": [
      { "role": "Backend engineer", "organization": "Globant", "years": "2008–2014" },
      { "role": "Tech lead", "organization": "Mercado Libre", "years": "2014–2020" }
    ]
  },

  "narrative": {
    "summary": "Mid-career engineer with strong systems background, comfortable in formal and informal contexts. Direct communicator, prefers concrete examples over theory.",
    "personality_traits": ["analytical", "patient", "concise"],
    "communication_style": "Direct but friendly, uses regional Spanish (voseo). Comfortable with technical jargon.",
    "interests": ["distributed systems", "cycling", "Andean geography"]
  },

  "extensions": {}
}
```

**Field groups:**

- `system_fields` — **rigid, system-queryable.** These are the fields routing/scheduling/policy code may read directly from SHM or via `data.identity` projection. Their shape is fixed:
  - `timezone` (string, IANA timezone name; e.g. `America/Argentina/Mendoza`) — **required when the asset is present.**
  - `country_code` (string, ISO 3166-1 alpha-2 uppercase; e.g. `AR`) — **required when the asset is present.**
  - `region_code` (string, ISO 3166-2 like `AR-M`) — optional.
  - `primary_language` (string, BCP-47 like `es-AR`) — **required when the asset is present.**
  - `additional_languages` (array of `{ code: BCP-47, level: A1|A2|B1|B2|C1|C2|native }`) — optional, max 8 entries.
- `biographical` — **optional structured.** LLM consumes for grounding; the system may index later but does not depend on it for routing today. Shape is fixed but every field is optional.
  - `nationality` (string, free text)
  - `display_name` (string) — first name the agent may present as
  - `birth_year` (integer, 1900–current year)
  - `birth_place` (string, free text)
  - `current_residence` (string, free text)
  - `education[]` (objects with `institution`, `degree`, `year_completed`, `field`)
  - `professional_background[]` (objects with `role`, `organization`, `years`)
- `narrative` — **optional free-form prose for the LLM only.** The system never queries these.
  - `summary` (string, ≤2000 chars)
  - `personality_traits[]` (array of short strings)
  - `communication_style` (string)
  - `interests[]` (array of short strings)
- `extensions` — **completely free-form `object`.** Solution-specific or domain-specific traits the spec does not standardize (e.g. `medical_specialty`, `years_in_industry`, `clearance_level`). Renders into the prompt as `key: value` pairs.

**Absence semantics (no override).** There is no override layer. If the personality asset is absent, no personality block is rendered. If a field within the asset is missing, that field is simply not rendered. If the role and personality contradict each other (e.g. role says "you are a 65-year-old judge" and personality says `birth_year: 1985`), the asset author is responsible — the system does not arbitrate. Treat any contradiction as a misconfiguration to be fixed at authoring time, not at runtime.

**Validation budgets:**

| Limit | Value |
|---|---:|
| Personality assets per agent | 1 |
| `additional_languages` entries | 8 |
| `education[]` entries | 8 |
| `professional_background[]` entries | 12 |
| `extensions` total serialized size | 8 KiB |
| Personality asset file total size | 64 KiB max |

### 3.5 Schema enforcement

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
  ],
  "personality_hash": "9f8e7d6c5b4a..."
}
```

`personality_hash` is optional. Absent or empty string ⇒ no personality block in the composed prompt and no `data.identity[*].personality_*` projection from SHM.

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
- `personality_hash` is optional but if present must be 64 hex chars (single hash, not array — at most one personality per agent).
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
    pub personality_hash: [u8; 32],       // all zeros if no personality
}
```

Total addition: 32 + (16 × 32) + 1 + (8 × 32) + 1 + 32 = 834 bytes per IlkEntry.

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
| `INVALID_DEFINITION` | Definition payload doesn't match expected schema (too many hashes, invalid hex, etc.). Also covers a malformed `personality_hash`. |

---

## 5. AI Node Behavior (`ai.generic` runtime)

### 5.1 Boot sequence

When an AI node starts:

1. Connects to router, registers self if needed (creates ILK if first boot).
2. Reads its own `IlkEntry` from identity SHM.
3. Extracts `role_hash`, `skill_hashes[..skill_count]`, `handbook_hashes[..handbook_count]`, and `personality_hash` (zero if absent).
4. If all hashes are zero (no definition set yet), uses the default minimal prompt and enters `empty` state.
5. If hashes are present, attempts to load each from blob:
   - Reads `blob://agent-assets/<hash>.json` for each hash.
   - Parses JSON and validates `asset_type` matches expected (role/skill/handbook/personality).
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

The composition is hardcoded in `ai.generic`. The order is deterministic: personality first (so the LLM grounds its identity before its function), then role, then skills, then handbooks. If personality is absent the section is omitted entirely:

```
[PERSONALITY]                                 # omitted if no personality_hash
You are {personality.biographical.display_name},
a {personality.biographical.nationality} based in {personality.system_fields.timezone}.
Primary language: {personality.system_fields.primary_language}.
Also speaks: {personality.system_fields.additional_languages[*].code} ({level}).
Born {personality.biographical.birth_year} in {personality.biographical.birth_place}.
Education: {personality.biographical.education[*]}.
Background: {personality.biographical.professional_background[*]}.

{personality.narrative.summary}

Communication style: {personality.narrative.communication_style}.
Traits: {personality.narrative.personality_traits[*]}.
Interests: {personality.narrative.interests[*]}.

Additional traits:
- {personality.extensions.<key>}: {personality.extensions.<value>}
- ...

[ROLE]
{role.description}

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
| Personality assets per agent | 1 |
| Asset file size | 256 KiB max |
| Personality asset file size | 64 KiB max |
| Composed system prompt | 64 KiB max |

Composition is deterministic and budgeted in this order:

1. personality (full block if present)
2. role
3. skills in listed order
4. handbooks in listed order

If the composed prompt would exceed 64 KiB, `ai.generic` truncates handbook content first, then skill examples, then personality `narrative.summary` and `extensions`. It preserves: role, skill names, skill instructions, personality `system_fields`, personality `biographical.display_name` / `nationality` / `primary_language` / `timezone`, and asset hash provenance. The node reports truncation in `CONFIG_GET`.

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
    "personality_hash_loaded": "9f8e7d6c...",
    "skill_hashes_failed": [],
    "handbook_hashes_failed": [],
    "personality_hash_failed": null,
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
generate_personality_asset(name, description, system_fields, biographical?, narrative?, extensions?) → personality JSON
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

After bootstrap, Archi has visibility of available assets and can answer questions like "what skills do I have available for support?" or "is there a handbook about billing procedures?". When the operator asks to inspect content, Archi reads the asset JSON from `blob://agent-assets/<hash>.json` by hash; it does not reconstruct the content from chat history.

### 6.5 Asset lifecycle

Assets are immutable once created (their hash is their identity). Updating an asset means generating a new file with a new hash and updating references in the ILKs that should use the new version. The old asset remains in blob — useful for audit, rollback, or other agents still using it.

Archi can delete an explicit local asset by hash when the operator asks. Deletion only removes `blob://agent-assets/<hash>.json` and the in-memory catalog entry; it does not mutate ILK definitions. If an active ILK still references the deleted hash, the agent can become `partial` until the definition is updated.

Automatic garbage collection of unreferenced assets is still optional and not in v1.

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

    // personality asset reference (single, optional)
    pub personality_hash: [u8; 32],

    pub created_at: u64,
    pub updated_at: u64,
}
```

Total addition over the base IlkEntry: 32 (role) + 16×32 (skills) + 1 (skill_count) + 8×32 (handbooks) + 1 (handbook_count) + 32 (personality) = 834 bytes plus padding for alignment, ~848 bytes per entry.

For a system with 8192 max ILKs, total SHM growth is ~6.8MB. Acceptable.

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
  "handbook_hashes": ["64hex..."],
  "personality_hash": "64hex..."
}
```

When `personality_hash` is non-zero, the router also exposes a flattened view of personality `system_fields` (the only fields routing/policy reasonably need to match on) directly inside `data.identity[<ilk>]`:

```json
{
  "personality_hash": "9f8e7d6c...",
  "personality_timezone": "America/Argentina/Mendoza",
  "personality_country_code": "AR",
  "personality_primary_language": "es-AR",
  "personality_additional_languages": ["en", "pt-BR"]
}
```

This projection is read by the router from blob (asset content) once at definition-change time and cached per ILK; it is not re-read per OPA evaluation. If the asset cannot be loaded, the projection stays empty and the personality_hash is the only field present, which is enough for OPA rules that match by hash.

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

[x] Archi: asset builder sub-agent with typed functions
[x] Archi: hash computation (sha256 of canonical JSON)
[x] Archi: write asset files to blob with hash filename
[x] Archi: in-memory catalog of assets
[x] Archi: bootstrap catalog by scanning blob on startup
[x] Archi: validate filename matches content hash on bootstrap

[x] E2E: add reproducible harness at `scripts/agent_cognitive_definition_e2e.sh`

[ ] SDK: add set_ilk_definition (Rust)
[ ] SDK: add SetIlkDefinition (Go)

[ ] ILK_GET_RESPONSE: include definition in response
[ ] CONFIG_GET on AI nodes: include definition_state and load info
```

---

## 12. What is NOT in this scope

- Tools/actions for agents (`tool_refs`). Deferred to a later iteration.
- Garbage collection of unreferenced assets in blob. Manual.
- Multi-language assets (per-locale skills, per-locale personality `narrative.summary`). Not in scope.
- Asset versioning beyond hash-based content addressing. Sufficient.
- Tagging humans with claims/responsibilities. Schema is ready (definition field exists for all ILK types) but no semantics defined for non-agents.
- Frontdesk cognitive definition. SY.frontdesk.gov keeps its fixed role/capabilities and is not governed by this spec.
- Hot-reload of role-only changes (today, any change recomposes the full prompt). Optimization for later.
- Asset signing or trust verification. Files are trusted because blob is internal.
- Privacy / scope gating on personality biographical fields. Authors are responsible for what they put in.
- Automated provisioning of personality from external HR systems. Personalities are generated via Archi's asset builder.
- Asset editor UI (role/skill/handbook/personality). Manual JSON editing or asset builder agent only.

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
| Personality is a separate asset, not a field on the role | Roles describe function; personalities describe person. Reusable independently — one personality alongside many roles and vice versa. Conflating them blocks reuse. |
| Personality `system_fields` are projected flat into `data.identity` for OPA | Routing/scheduling need to query timezone/language/country directly. Putting these in the prompt only would make them invisible to the system. |
| **No override semantics anywhere in the cognitive definition** | Empty/missing fields are simply not rendered. Conflicts between assets (e.g. role and personality contradicting each other) are misconfigurations to fix at authoring time, not runtime arbitration. |
| Personality renders first in the composed prompt | Identity grounds the LLM before function — reduces tone/persona drift across responses. |
