# AI Attachments - Immediate Plan (router inbound first, checklist)

Date: 2026-04-01
Scope: `AI.common`, `AI.frontdesk.gov`, `fluxbee_ai_sdk`

## Objective (immediate)

Prioritize one thing: any attachment/blob (or most of them, with explicit limits) that arrives from Router in `text/v1` must be processable by AI nodes without breaking runtime.

This plan is intentionally **inbound-first**:
- Router -> AI ingestion
- blob resolve/read
- provider input mapping
- deterministic fallback/errors

Out of immediate scope (deferred unless needed):
- upload + reuse with `file_id`
- `file_url` transport strategy

## General compatibility rule (requested)

Adopt this rule for implementation:
1. Accept, as a default objective, every attachment/content mode supported by OpenAI Agents SDK / Responses API.
2. Apply it only when compatible with Fluxbee router inbound reality (`text/v1` + `BlobRef` attachments arriving from Router).
3. If a mode is not currently representable from Router inbound payload, document it as "deferred by transport contract", not as "rejected by product decision".

Compatibility matrix for this cycle:
1. `input_text`: compatible now (from `content`/`content_ref` and textual attachments).
2. `input_image.image_url`: compatible now (from blob bytes as data URL).
3. `input_image.file_id`: deferred unless Router/Fluxbee carries provider file-id metadata.
4. `input_file.file_data`: compatible now (from blob bytes).
5. `input_file.file_id`: deferred unless Router/Fluxbee carries provider file-id metadata.
6. `input_file.file_url`: deferred unless Router/Fluxbee carries trusted URL metadata per attachment.
7. `input_image.detail`: compatible now (default + override policy).

## Decision on transport modes (what is valid now)

Current valid baseline for immediate goal:
1. `input_text` for main text and textual attachments.
2. `input_image.image_url` for supported image MIME (`image/png`, `image/jpeg`, `image/webp`).
3. `input_file.file_data` for non-image binary attachments that must be sent as file content.

Defer for later:
1. `input_image.file_id`
2. `input_file.file_id`
3. `input_file.file_url`

Reason:
- They are useful for optimization/reuse, but not required to satisfy "process inbound router attachments now".
- Immediate blocker is reliable inbound processing and provider-compatible behavior for common MIME, especially PDF.

## Work checklist (tracking)

Status legend:
- `[ ]` pending
- `[x]` completed

### A. Scope and acceptance (must be frozen first)

- [x] A1. Freeze inbound contract for AI nodes:
  - `payload.type="text"`
  - `content` or `content_ref`
  - `attachments[]` with canonical `BlobRef`
- [x] A2. Define "processable" criteria:
  - node does not crash
  - attachment is transformed to provider input or rejected with canonical error
  - response path remains valid
- [x] A3. Freeze explicit limits/policy:
  - max attachments
  - max attachment bytes
  - MIME allowlist/reject list
- [x] A4. Publish this acceptance matrix as the active source for implementation validation.
- [x] A5. Enforce Agents-SDK compatibility rule in implementation decisions:
  - prefer Agents-supported modes whenever representable from Router inbound contract
  - document any deferred mode as "transport-not-representable yet" (not silently discarded)

#### A1 Frozen contract (effective 2026-04-01)

Inbound message contract accepted for this implementation cycle:
1. Envelope: `meta.type="user"` with `payload.type="text"`.
2. Text body: exactly one of:
   - `content` (inline), or
   - `content_ref` (blob reference for long text).
3. Attachments: `attachments[]` always interpreted as canonical `BlobRef` entries.
4. Contract invariants:
   - `content` and `content_ref` are mutually exclusive.
   - `attachments` may be empty, but when present each entry must validate as canonical `BlobRef`.

Any inbound shape outside this contract is out-of-scope for this immediate milestone and must return canonical validation/error handling.

#### A2 Processable criteria (effective 2026-04-01)

A router inbound attachment message is considered "processable" only if all checks below hold:
1. Runtime stability:
   - AI node process remains alive.
   - Control-plane endpoints/messages remain responsive after the request.
2. Deterministic attachment outcome (per attachment):
   - either mapped to a provider input part (`input_text`, `input_image`, `input_file`), or
   - rejected with canonical error (no silent drop).
3. Deterministic message-level outcome:
   - success path: valid AI response message emitted, or
   - failure path: canonical error payload emitted with actionable code/message.
4. No invalid downgrade:
   - when multimodal is enabled, supported images are not downgraded to plain text-only handling.
5. Error quality:
   - failures include canonical code and retryability semantics.
   - provider-origin errors are mapped to stable node-facing error codes.

#### A3 Frozen limits and MIME policy (effective 2026-04-01)

Initial enforced limits for this milestone:
1. `max_attachments`: 8 per inbound message.
2. `max_attachment_bytes`: 10 MiB per attachment (10 * 1024 * 1024).
3. `content_ref` for main text follows same per-blob byte gate used by SDK payload resolution in this phase.

MIME routing policy for immediate processing:
1. When `multimodal=false`:
   - accepted as textual-only: `text/plain`, `text/markdown`, `application/json`
   - all non-textual attachments: reject with canonical `unsupported_attachment_mime`
2. When `multimodal=true`:
   - textual allowlist (`text/plain`, `text/markdown`, `application/json`) -> textual attachment path
   - images (`image/png`, `image/jpeg`, `image/webp`) -> `input_image.image_url`
   - all other attachment MIME (including `audio/*`, office docs, spreadsheets, `text/csv`, and other binaries) -> `input_file.file_data`
3. No silent drop and no implicit "best effort" downgrade outside this routing.
4. Any future tightening/expansion must be explicit here and mirrored in canonical docs (section F).

#### A4 Active validation source (effective 2026-04-01)

For this implementation cycle, this file is the active validation baseline for inbound attachments:
1. Any implementation/test task in sections B-E must validate against A1-A3 frozen rules.
2. If implementation behavior conflicts with A1-A3, the task is not considered complete until:
   - either behavior is fixed, or
   - A1-A3 are explicitly updated in this file first.
3. E2E results recorded in section E must reference these acceptance rules.
4. During closure, section F must migrate the final accepted behavior into canonical docs, then this onworking file can be removed.

#### A5 Enforcement policy (effective 2026-04-01)

Implementation decisions for inbound attachments MUST follow:
1. If a mode is supported by Agents SDK and can be built from Router inbound `BlobRef`, it is in-scope for this milestone.
2. If a mode is supported by Agents SDK but requires metadata not present in Router inbound today (`file_id`, trusted `file_url`), mark it as deferred-by-transport and keep traceability in docs.
3. Do not introduce local ad-hoc modes when an Agents-compatible mode already exists for the same payload shape.
4. Rejections must be explicit and policy-driven (limits/security/compatibility), never implicit due to missing mapping.

### B. SDK inbound processing (fluxbee_ai_sdk)

- [x] B1. Confirm deterministic classifier in SDK:
  - `text/plain`, `text/markdown`, `application/json` -> textual path
  - `image/png|jpeg|webp` -> `input_image.image_url`
  - `application/pdf` -> `input_file.file_data` (with controlled behavior)
  - audio and office/binary formats (`audio/*`, `application/msword`, OOXML, spreadsheets, etc.) -> explicit policy-based decision (`input_file.file_data` or reject)
  - textual but currently non-allowlisted formats (for example `text/csv`) -> explicit policy decision (treat as text or as file)
- [x] B2. Guarantee no silent drop of attachments.
- [x] B3. Guarantee "no image-to-text downgrade" when multimodal=true.
- [x] B4. Ensure attachment failure returns canonical error (or explicit partial-policy if adopted).

#### B1 current implementation reality (to validate and refine)

Current behavior in `fluxbee_ai_sdk`:
1. Only these MIME are treated as textual inline attachment content:
   - `text/plain`
   - `text/markdown`
   - `application/json`
2. Only these MIME are mapped to image input:
   - `image/png`
   - `image/jpeg`
   - `image/webp`
3. Any non-image attachment that passes validation is mapped to:
   - `input_file.file_data` (+ `filename`)
4. There is no dedicated `input_audio` mapping today.

Implications:
1. With `multimodal=false`, non-textual attachments are rejected (`unsupported_attachment_mime`).
2. With `multimodal=true`, many non-image formats (audio, doc/docx, xls/xlsx, etc.) currently go through `input_file.file_data`.
3. `text/csv` is not in current textual allowlist and therefore does not get textual-inline treatment by default.

Target implication under the new compatibility rule:
1. Audio/office/csv should not be excluded by default just because they are not image/text classes.
2. If they can be represented from Router inbound blobs, they should route through compatible Agents mode (`input_file.file_data`) unless a policy/limit blocks them explicitly.

#### B1 frozen classifier decision (effective 2026-04-01)

Classifier decision for this milestone:
1. Textual attachments:
   - `text/plain`, `text/markdown`, `application/json` -> attachment text extraction + `input_text` augmentation.
2. Image attachments:
   - `image/png`, `image/jpeg`, `image/webp` -> `input_image.image_url` (+ `detail` policy).
3. Non-image attachments (including audio, office, csv, binary):
   - if `multimodal=true` -> `input_file.file_data` (+ `filename`).
   - if `multimodal=false` -> `unsupported_attachment_mime`.
4. `input_audio`:
   - not implemented in this cycle; audio travels via `input_file.file_data` under `multimodal=true`.

#### B2 evidence (no silent drop)

Implemented contract test in SDK:
1. `build_openai_user_content_parts_keeps_one_output_per_attachment`
2. Location:
   - `crates/fluxbee_ai_sdk/src/text_payload.rs`
3. Assertion:
   - with empty main text, output part count equals attachment count,
   - textual/image/file attachments map to `input_text`/`input_image`/`input_file` respectively.

#### B3 evidence (no image downgrade when multimodal=true)

Implemented checks:
1. SDK serialization test:
   - `build_openai_user_content_parts_does_not_downgrade_image_to_text`
   - asserts an image attachment yields only `input_image` part (no `input_text` substitute part).
2. Runner wiring confirmation:
   - with `multimodal=true`, runners build and pass structured `input_parts` from resolved attachments into OpenAI request path (normal and function-calling flow), instead of relying on text-only prompt assembly.

#### B4 evidence (canonical attachment failures preserved)

Implemented runtime behavior:
1. Structured input-part build errors (`build_openai_user_content_parts`) are now handled before provider call and returned directly as canonical payload via `err.to_error_payload()`.
2. This avoids collapsing attachment errors into generic provider errors.
3. Applied in both runners:
   - `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`
   - `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs`

### C. Provider hardening (PDF first)

- [x] C1. Add structured logging for provider rejection on files:
  - status
  - provider field path/detail
  - model
  - MIME
  - size
  - without sensitive file/text content
- [ ] C2. Add pre-send validation for outgoing `input_file` shape.
- [ ] C3. Add compatibility gate for known bad MIME/model combos:
  - fail early with canonical actionable error
- [ ] C4. Define and implement PDF fallback policy:
  - fallback option A: local extraction to text (if available)
  - fallback option B: deterministic canonical error

#### C1 evidence (structured provider rejection logging)

Implemented in both runners:
1. Capture and log structured provider rejection fields when available:
   - `provider_status`
   - `provider_param` (extracted from provider error body when present)
   - `provider_detail` (trimmed)
   - `model`
   - `attachment_count`
   - `attachment_total_bytes`
   - `attachment_mimes`
2. Sensitive content is not logged (no attachment bytes/content payload logged).
3. Files:
   - `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`
   - `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs`

### D. Runner parity (AI.common + AI.frontdesk.gov)

- [x] D1. Confirm both runners use the exact same SDK attachment path.
- [x] D2. Remove/avoid diverging per-runner attachment logic.
- [x] D3. Preserve defaults:
  - `AI.common` multimodal=true
  - `AI.frontdesk.gov` multimodal=false (override allowed)
- [x] D4. Enforce behavior for multimodal=false:
  - reject non-text attachments with canonical error
  - keep runtime alive and control-plane responsive

### E. E2E validation matrix (required before close)

- [ ] E1. Text only.
- [ ] E2. Text + PNG.
- [ ] E3. Text + JPG.
- [ ] E4. Text + WEBP.
- [ ] E5. Text + PDF.
- [ ] E6. Text + JSON attachment.
- [ ] E7. Unsupported binary (example DOCX) with expected policy result.
- [ ] E8. Mixed attachments (text + image + pdf).
- [ ] E9. For each case, verify:
  - inbound parsed
  - blob resolved/read
  - provider request built with expected input part type
  - node response valid + runtime healthy
  - canonical error mapping on failures

### F. Documentation update (mandatory before closing this onworking file)

- [ ] F1. Update canonical AI spec with real state and remaining gaps:
  - `docs/AI_nodes_spec.md`
- [ ] F2. Update rollout/tasks doc to match current truth (remove stale assumptions):
  - `docs/onworking NOE/ai_multimodal_rollout_tasks.md`
- [ ] F3. Update current status snapshot with what is done/pending after implementation:
  - `docs/onworking NOE/ai_attachments_status.md`
- [ ] F4. Add explicit "supported now vs deferred" table (`file_data`, `file_id`, `file_url`) in canonical docs.
- [ ] F5. Ensure E2E evidence references (commands/results) are documented.
- [ ] F6. Once F1-F5 are complete and validated, this file can be deleted.

### G. Deferred optimization phase (not blocking immediate objective)

- [ ] G1. Add optional upload/reuse flow to obtain `file_id`.
- [ ] G2. Add policy chooser `file_data` vs `file_id` vs `file_url` (size/type/cost/latency).
- [ ] G3. Add caching/reuse lifecycle for uploaded provider files.

## Implementation order (recommended)

1. A (scope and acceptance)
2. B (SDK inbound)
3. C (PDF/provider hardening)
4. D (runner parity)
5. E (E2E matrix)
6. F (documentation update)
7. G (deferred optimizations)

## Exit criteria for immediate milestone

Milestone is done when:
1. A-E are completed.
2. F1-F5 are completed (canonical docs reflect real status and gaps).
3. Router inbound attachments are processable for common MIME (text + png/jpg/webp + pdf with deterministic behavior).
4. Node never crashes on attachment errors.
5. Errors are canonical and actionable.
6. AI.common and AI.frontdesk.gov behave consistently through shared SDK path.
