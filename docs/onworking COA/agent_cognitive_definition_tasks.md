# Agent Cognitive Definition Tasks

**Status:** in progress
**Date:** 2026-05-06
**Spec:** `docs/identity-v2.1-agent-definition-addendum.md`
**Scope:** implement hash-based cognitive definitions for `ai.generic` agents.

## Decisions

- `ai.generic` is the only AI runtime in scope.
- `SY.frontdesk.gov` is out of scope; it keeps fixed role/capability behavior.
- AI nodes boot normally with operational config, register/reuse their ILK, and start with the default unconfigured prompt.
- The cognitive definition is applied later through identity via `ILK_SET_DEFINITION`.
- Legacy `roles/capabilities` in identity are retired. They were only partially implemented and are replaced by role/skill/handbook hash references.
- OPA/routing can use cognitive hashes after router projection, but OPA does not read blob content.
- Hard prompt limits for v1: 1 role, 16 skills, 8 handbooks, 256 KiB asset file max, 64 KiB composed prompt max.

## Phase A - Spec And Cleanup

- [x] A1. Align addendum with current code reality.
- [x] A2. Declare legacy identity `roles/capabilities` retired.
- [x] A3. Declare `ai.generic` as the runtime target.
- [x] A4. Remove `ILK_REGISTER with definition` from v1 flow.
- [x] A5. Add OPA/routing projection rule for definition hashes.
- [x] A6. Update old identity/routing docs that still describe routing by `data.identity[ilk].capabilities`.

## Phase B - Identity Core

- [x] B1. Add `definition: Value` to `IlkRecord`.
- [x] B2. Replace `definition_json_from_ilk()` so it persists the cognitive definition directly.
- [x] B3. Remove legacy `roles` / `capabilities` fields from `IlkRecord`.
- [x] B4. Remove legacy `roles` / `capabilities` parsing from `ILK_REGISTER`.
- [x] B5. Remove legacy `add_roles` / `remove_roles` / `add_capabilities` / `remove_capabilities` behavior from `ILK_UPDATE`.
- [x] B6. Add `ILK_SET_DEFINITION` / `ILK_SET_DEFINITION_RESPONSE`.
- [x] B7. Validate `ILK_SET_DEFINITION` only applies to `ilk_type="agent"`.
- [x] B8. Validate definition hash format, role count, skill count, handbook count.
- [x] B9. Persist definition update in DB.
- [x] B10. Include definition in `ILK_GET_RESPONSE`.
- [x] B11. Keep `ILK_LIST_RESPONSE` compact, but expose whether a definition is present.
- [x] B12. Add tests for valid, empty, malformed, non-agent, and not-found definitions.

## Phase C - SHM And Readers

- [x] C1. Extend `src/shm/mod.rs::IlkEntry` with role hash, skill hashes, skill count, handbook hashes, handbook count.
- [x] C2. Bump identity SHM version.
- [x] C3. Update `IdentityRegionWriter` snapshot/upsert logic.
- [x] C4. Update `crates/fluxbee_sdk/src/identity.rs` reader layout.
- [x] C5. Update Go identity SHM reader layout if still consuming identity SHM.
- [x] C6. Add helpers to convert 64-hex hashes to `[u8; 32]` and back.
- [x] C7. Ensure `ILK_SET_DEFINITION` increments SHM seq.
- [x] C8. Add SHM tests for empty definitions and populated definitions.

## Phase D - Router / OPA Projection

- [x] D1. Extend router `data.identity` injection with `role_hash`, `skill_hashes`, and `handbook_hashes`.
- [x] D2. Remove always-empty `roles` / `capabilities` projection.
- [x] D3. Add router tests proving OPA-visible identity contains hash facts.
- [x] D4. Document that OPA matches hashes only and never reads blob assets.

## Phase E - Admin / SDK Surface

- [x] E1. Add Rust SDK constant/helper for `ILK_SET_DEFINITION`.
- [ ] E2. Add `SY.admin` wrapper action `set_ilk_definition`.
- [ ] E3. Expose `POST /hives/{hive}/identity/ilks/{ilk_id}/definition`.
- [ ] E4. Add admin help entry with request contract and example SCMD.
- [ ] E5. Add Archi SCMD translation for the definition endpoint.
- [ ] E6. Update README endpoint table.

## Phase F - ai.generic Runtime

- [ ] F1. Locate and normalize the runtime name/documentation to `ai.generic`.
- [ ] F2. Add identity SHM reader state to `nodes/ai/ai-generic`.
- [ ] F3. Resolve the node's own ILK at boot.
- [ ] F4. Read current definition hashes from SHM at boot.
- [ ] F5. Add default unconfigured prompt.
- [ ] F6. Add asset loader for `blob://agent-assets/<hash>.json`.
- [ ] F7. Validate role/skill/handbook asset schema.
- [ ] F8. Compose prompt deterministically with a 64 KiB budget.
- [ ] F9. Add truncation behavior for handbooks and skill examples.
- [ ] F10. Add polling loop for identity SHM seq and own hash changes.
- [ ] F11. Swap active prompt without restart.
- [ ] F12. Preserve in-flight request behavior during prompt swap.
- [ ] F13. Report `definition_state`, loaded/failed hashes, truncation, and prompt size through `CONFIG_GET`.
- [ ] F14. Add unit tests for empty, partial, composed, malformed asset, and truncation states.

## Phase G - Archi Asset Builder

- [ ] G1. Add asset schemas for role, skill, and handbook.
- [ ] G2. Add canonical JSON serializer and sha256 content hash.
- [ ] G3. Write assets to blob path `agent-assets/<hash>.json`.
- [ ] G4. Bootstrap in-memory asset catalog by scanning blob.
- [ ] G5. Validate filename hash matches content on bootstrap.
- [ ] G6. Add typed generation path for role assets.
- [ ] G7. Add typed generation path for skill assets.
- [ ] G8. Add typed generation path for handbook assets.
- [ ] G9. Add UI/chat flow to show generated assets and target agent.
- [ ] G10. Compile plan: run node, get ILK, set definition, verify CONFIG_GET.

## Phase H - E2E / Operational Validation

- [ ] H1. Start `AI.test@motherbee` on `ai.generic` without definition and verify default prompt.
- [ ] H2. Generate role/skill/handbook assets through Archi.
- [ ] H3. Apply `ILK_SET_DEFINITION` and verify SHM seq increments.
- [ ] H4. Verify `CONFIG_GET` reaches `definition_state=composed`.
- [ ] H5. Verify missing asset produces `partial`.
- [ ] H6. Verify later asset sync moves `partial` to `composed`.
- [ ] H7. Verify router/OPA can route by a configured skill hash.
- [ ] H8. Verify restart preserves definition from DB/SHM.

## Open Questions

- Whether Go needs an RPC helper for `ILK_SET_DEFINITION`, or only SHM reader support.
- Whether Archi's asset catalog should remain purely in memory for v1 or persist an index for faster startup.
- Whether routing by hash should stay explicit in policy or be wrapped by an Archi-side symbolic policy compiler.
