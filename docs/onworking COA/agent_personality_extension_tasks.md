# Agent Personality Asset — Implementation Tasks

**Status:** drafted, not started
**Date:** 2026-05-10
**Spec:** [docs/identity-v2.1-agent-definition-addendum.md](../identity-v2.1-agent-definition-addendum.md) (canonical agent cognitive definition spec)
**Related task doc:** [agent_cognitive_definition_tasks.md](agent_cognitive_definition_tasks.md) (role + skill + handbook trio — completed; this extends and supersedes parts of it)
**Scope:** add `personality` as a 4th asset type in the cognitive definition pipeline, end to end. Drop the now-redundant `role.persona` field. No backward-compatibility paths.

---

## Decisions

- Asset name is `personality` (English). Avoided "persona" because the existing role schema had a `persona` field that this work removes (collision plus orthogonal concept).
- Roles describe **function**; personalities describe **person**. They render into different prompt sections and are reusable independently.
- One personality per agent, optional. Default is no personality block.
- Field groups inside the asset: `system_fields` (rigid, system-queryable) + `biographical` (optional structured) + `narrative` (free-form prose) + `extensions` (open object).
- `system_fields` exposes `timezone` (IANA), `country_code` (ISO 3166-1 alpha-2), `region_code` (ISO 3166-2, optional), `primary_language` (BCP-47), `additional_languages[]` (BCP-47 + level). These are the only fields the router projects flat into `data.identity[*]`.
- **No override semantics.** Empty / missing fields are simply not rendered. Conflicts between role and personality are misconfigurations to fix at authoring time; the system does not arbitrate at runtime.
- Personality renders first in the composed prompt (identity grounds the agent before role).
- Truncation order under the 64 KiB prompt budget: handbooks → skill examples → personality `narrative.summary` and `extensions`. Always preserve `system_fields`, `display_name`, `nationality`, `primary_language`, `timezone`, role, skill names, skill instructions.
- SHM gains a single `personality_hash: [u8; 32]` field on `IlkEntry`. SHM version bumps so stale readers fail fast.
- Router caches personality `system_fields` per ILK on definition-change; no per-OPA-request blob reads.
- `role.persona` field is **removed** from the role schema. `role.description` becomes the canonical functional statement. Existing role JSON blobs are regenerated; no backward-read shim.
- System is in active dev. Schema/SHM/protocol changes are applied directly. No migrations, no compat shims, no `if (legacy_format)` branches.

---

## Phase A — Spec & SHM Versioning

- [ ] A1. Confirm spec doc reflects all sections coherently (§3.1 role without `persona`, §3.4 personality, §4.1/§4.2/§4.4 protocol+SHM, §5.5 prompt template, §5.6 budgets, §5.7 CONFIG_GET, §6.2 builder, §8.1 SHM layout, §8.3 OPA projection, §13 decisions).
- [ ] A2. Bump identity SHM version. Stale readers (e.g. router still on the old layout) will refuse to read and the operator must restart them. Acceptable in dev.
- [ ] A3. Decide and document the canonical JSON serialization of the personality asset (key ordering, whitespace) so hashes are reproducible across implementations. Same convention as the existing role/skill/handbook hashing.
- [ ] A4. Add a versioned schema validator (Rust + reused for OpenAI typed function args) for the personality asset.

## Phase B — Role Schema Cleanup

- [ ] B1. Remove the `persona` field from the role asset schema definition (Rust types, validators, OpenAI typed function args).
- [ ] B2. Update Archi's `generate_role_asset` typed function: drop the `persona` parameter; require `description` to be a self-contained functional statement (1–3 sentences).
- [ ] B3. Re-author all existing role assets currently in `blob://agent-assets/`. Delete the old `<hash>.json` files and regenerate via Archi's asset builder; the new content gets a new hash. Update any ILK definitions that referenced the old hashes to the new ones.
- [ ] B4. Update prompt composition in `ai.generic` to use `role.description` instead of `role.persona`.
- [ ] B5. Tests for role asset generation: ensure no path produces or accepts a `persona` field. Validators reject role JSON containing `persona`.

## Phase C — Identity Core

- [ ] C1. Add `personality_hash: Option<String>` to `IlkRecord.definition` parsing/serialization.
- [ ] C2. Extend `ILK_SET_DEFINITION` to accept `personality_hash` (optional 64 hex string). Reject malformed values with `INVALID_DEFINITION`.
- [ ] C3. Persist `personality_hash` in the identity DB.
- [ ] C4. Include `personality_hash` in `ILK_GET_RESPONSE`. Keep `ILK_LIST_RESPONSE` compact: only expose a boolean `has_personality`.
- [ ] C5. Tests: valid hash, empty/cleared, malformed, non-agent ILK rejection, set-and-get round trip preserving role + skill + handbook + personality.

## Phase D — SHM & Readers

- [ ] D1. Extend `src/shm/mod.rs::IlkEntry` with `pub personality_hash: [u8; 32]`.
- [ ] D2. Update `IdentityRegionWriter` snapshot/upsert to write `personality_hash` (zero when absent).
- [ ] D3. Update `crates/fluxbee_sdk/src/identity.rs` reader layout.
- [ ] D4. Update Go identity SHM reader layout if still consuming identity SHM.
- [ ] D5. Ensure `ILK_SET_DEFINITION` increments SHM seq when only `personality_hash` changed (no other field).
- [ ] D6. SHM tests: set personality only, clear personality only (zero out), set everything in one call.

## Phase E — Router / OPA Projection

- [x] E1. Extend router `data.identity` injection with `personality_hash`. Done in [src/router/mod.rs](../../src/router/mod.rs).
- [ ] ~~E2. Per-ILK personality `system_fields` cache in the router~~ **DEFERRED.** Implementing this requires the router to read blob files (currently it does not — that boundary is intentional). Cost is real: blob path coupling, cache invalidation, deserialization, fault tolerance for missing files. Until a concrete OPA-routing-by-language use case justifies it, we skip this. OPA rules that need to route by `personality_*` should match on the hash (a fixed-value match), not on language strings.
- [ ] ~~E3. Graceful degradation when asset cannot be loaded~~ **N/A while E2 is deferred** — the router never tries to load the asset, so there is no degradation path to test. Automatically satisfied.
- [x] E4. Router tests should cover that OPA sees `personality_hash`. Will fold into existing `inject_identity_data_exposes_identity_and_aliases` test.
- [ ] ~~E5. OPA cookbook entry for routing by `personality_primary_language`~~ **DEFERRED** with E2. Documented as out of scope in Phase J.

## Phase F — Admin / SDK Surface

- [ ] F1. Update Rust SDK constants/helpers for `ILK_SET_DEFINITION` to include the new field.
- [ ] F2. Update `SY.admin` wrapper action `set_ilk_definition` to accept `personality_hash`.
- [ ] F3. Update `POST /hives/{hive}/identity/ilks/{ilk_id}/definition` request schema.
- [ ] F4. Update [docs/onworking COA/archi/admin_help_reference.md](archi/admin_help_reference.md) entry for `set_ilk_definition` so the body shape is documented as `{ role_hash, skill_hashes, handbook_hashes, personality_hash }`.
- [ ] F5. Update Archi SCMD translation for the definition endpoint.
- [ ] F6. Update [README.md](../../README.md) endpoint table if it currently specifies the `set_ilk_definition` body shape.

## Phase G — `ai.generic` Runtime

- [ ] G1. Read `personality_hash` from SHM at boot and during the polling loop.
- [ ] G2. Add a `personality` slot to the asset loader. Validate `asset_type == "personality"` and the schema groups (`system_fields` mandatory, others optional).
- [ ] G3. Update prompt composition: drop `role.persona`, use `role.description`, render personality first per spec §5.5. Skip the personality section entirely when no personality is present or the load failed.
- [ ] G4. Update truncation logic per spec §5.6: drop `extensions` first, then `narrative.summary`, then `narrative.interests`/`personality_traits`/`communication_style`. Always keep `system_fields`, `display_name`, `nationality`, `primary_language`, `timezone`.
- [ ] G5. Update `CONFIG_GET` payload to include `personality_hash_loaded` and `personality_hash_failed`.
- [ ] G6. Update `definition_state` rules: a personality load failure when other assets succeeded is `partial`. All other behavior stays the same.
- [ ] G7. Unit tests: empty (no personality), composed (personality + role + skills), partial (personality fails to load, others ok), truncation chooses the right victims.
- [ ] G8. Integration test: SHM seq change that only flips `personality_hash` triggers recomposition without restart.

## Phase H — Archi Asset Builder

- [ ] H1. Add personality asset schema definitions and canonical JSON serializer parallel to role/skill/handbook.
- [ ] H2. Add typed OpenAI function `generate_personality_asset(name, description, system_fields, biographical?, narrative?, extensions?)`. The LLM cannot produce a personality with missing required `system_fields` because the function signature enforces it.
- [ ] H3. Validate generated JSON against the schema before hashing.
- [ ] H4. Write to `blob://agent-assets/<hash>.json` reusing the same path and bootstrap path scan as role/skill/handbook.
- [ ] H5. Update Archi's in-memory catalog metadata model to include personality entries.
- [ ] H6. Add explicit read tool to fetch a personality asset's content by hash (parallel to the existing role/skill/handbook read tool).
- [ ] H7. Add explicit delete tool for unused personality assets by hash.
- [ ] H8. Update plan-compiler flow that wires definitions: when an operator says "make this agent Argentinian and based in Mendoza", the compiler can either reuse an existing personality hash from the catalog or generate a new one and then call `set_ilk_definition` with all four hash slots.

## Phase I — E2E / Operational Validation

- [ ] I1. Extend `scripts/agent_cognitive_definition_e2e.sh` (or a sibling script) to cover the personality lifecycle: start agent → assign role-only → verify composed prompt has no personality → assign personality → verify recomposition → CONFIG_GET shows `personality_hash_loaded` → swap to a different personality (different timezone/language) → verify router projection updates and OPA can now route the new language.
- [ ] I2. Verify `restart_node` preserves personality_hash from DB/SHM.
- [ ] I3. Verify clearing personality (`personality_hash: null`) drops the section from the composed prompt without other assets being affected.
- [ ] I4. Verify two agents sharing the same personality_hash both see the asset (no duplication in blob; cache reuse).

## Phase J — Documentation

- [ ] J1. Update [docs/onworking COA/archi/handbook_fluxbee.md](archi/handbook_fluxbee.md) §8.1.2 with how Archi's asset builder handles personality, including the rule that "make the agent Argentinian / Spanish-speaking / based in Mendoza" must route through `generate_personality_asset` rather than embedding the trait in the role.
- [ ] J2. Add an OPA cookbook entry showing how to route by `personality_primary_language`.
- [ ] J3. Update any agent-creation tutorial in the README that walks through the trio so it now mentions personality as an optional 4th step.

---

## Out of Scope

- Multi-language `narrative.summary` (a single language per personality asset for now — multi-tenant deployments use multiple personality assets if needed).
- Privacy / scope gating on biographical fields. Authors are responsible.
- Automated provisioning of personality from external HR systems.
- A dedicated UI in Archi to browse/edit personalities (the asset builder generates via typed functions; visual curation is a separate iteration).
- Indexing biographical fields for semantic search (e.g. "find agents with finance background"). The data is structured enough that a future iteration can add this without schema changes.
- **Router-side blob reads / flat projection of personality `system_fields` into `data.identity[<ilk>].personality_timezone` etc.** The router intentionally does not read blob today; coupling those layers for a not-yet-exercised use case is over-engineering. OPA can match on `personality_hash` directly (fixed-value match against a known hash) when routing-by-personality is needed. Revisit only when OPA must match on language/timezone strings rather than on a hash that already encodes them.
- OPA cookbook entry for routing by `personality_primary_language` (depends on the deferred router flat projection).

---

## Open Questions

1. **Sync ordering on personality update.** When `ILK_SET_DEFINITION` changes only `personality_hash` and the router needs to refresh its flat `system_fields` cache, what is the contract: does the router block on the asset being readable from blob, or does it project `personality_hash` immediately and backfill the flat view asynchronously? Default: project immediately, backfill async; OPA rules that match by hash work right away, rules that match by language/timezone work after backfill (typically <1s on motherbee, longer across hives).
2. **Asset deletion semantics.** Same as the role/skill/handbook trio: deleting an asset blob does not mutate ILKs that still reference its hash. AI nodes referring to the deleted hash transition `composed → partial` on next polling cycle. Confirm this is the desired behavior for personality too (low cost; reuse existing semantics).
3. **`extensions` key collisions.** If two solutions both add a key like `clearance_level` with different semantic meanings, the renderer just prints both as `key: value` lines. We accept this as an authoring-time concern; the schema does not namespace `extensions` keys.
