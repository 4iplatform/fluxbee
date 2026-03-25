# AI Nodes v16 - Implementation Tasks (Thread State + Data Plane Gaps)

Status: proposed  
Owner: AI nodes initiative  
Last update: 2026-03-13

Scope:
- AI nodes only (runner/sdk/docs AI).
- No core/router/orchestrator code changes in this plan.
- `thread_id` se toma temporalmente como top-level en mensajes `user`.

Non-goals (MVP):
- No vector search.
- No memoria cognitiva completa.
- No persistencia de conversación completa.

## 1) Baseline and guardrails

- [x] T1.1 Freeze extraction contract for `thread_id` (top-level only in this phase).
  - Runtime note (current SDK wire shape): temporary extractor reads `meta.context.thread_id`.
- [x] T1.2 Add explicit validation for `thread_id` on `meta.type=user`:
  - missing/empty -> `invalid_payload` (con error claro).
- [ ] T1.3 Keep control-plane behavior unchanged (`CONFIG_SET/GET`, `PING`, `STATUS`).

## 2) Thread State Store (LanceDB) foundation

- [x] T2.1 Add storage module in AI SDK for thread state (trait + concrete LanceDB adapter).
- [x] T2.2 Define minimal data model (1 document per `thread_id`):
  - key: `thread_id`
  - value: `data` (opaque JSON)
  - metadata: `updated_at`, optional `ttl_seconds`.
- [x] T2.3 Define node-local storage path:
  - `${STATE_DIR}/ai-nodes/<node_name>/lancedb/`.
- [x] T2.4 Add startup initialization and graceful failure mode if store is unavailable.

## 3) Agents SDK parity slice: function calling (MVP)

- [x] T3.1 Port minimal `function calling` loop inspired by `openai-agents-python`:
  - tool declaration
  - tool call dispatch
  - tool result marshaling back to model loop.
- [x] T3.2 Add runtime/SDK interfaces so AI behavior can register tools safely.
- [x] T3.3 Keep this slice minimal (no handoffs/guardrails/advanced orchestration yet).

## 4) Thread State API (runtime/tools)

- [x] T4.1 Implement `thread_state_get(thread_id)`.
- [x] T4.2 Implement `thread_state_put(thread_id, data, ttl_seconds?)`.
- [x] T4.3 Implement `thread_state_delete(thread_id)`.
- [x] T4.4 Add per-thread serialization (mutex/actor map) to avoid concurrent write races.

## 5) Runtime integration (MVP behavior)

- [x] T5.1 Resolve `thread_id` from incoming user message and pass it to behavior context.
- [x] T5.2 Expose `thread_state_*` as callable tools through the new function-calling slice.
- [x] T5.3 Keep state store private per node instance (no cross-node reads).
- [x] T5.4 Add structured logs for thread-state ops (`thread_id`, op, status, latency).

## 6) Data Plane pending implementation (`attachments` / `content_ref`)

- [ ] T6.1 Input parsing: support `payload.content_ref` and `payload.attachments[]`.
- [ ] T6.2 Blob resolution via `fluxbee_sdk::blob` (no local redefinition of blob rules).
- [ ] T6.3 Build model input with textual attachments and deterministic assembly.
- [ ] T6.4 Enforce MIME/size limits and return canonical errors (`BLOB_*`, `unsupported_attachment_mime`).
- [ ] T6.5 Output path: allow behaviors to return attachments via BlobRef when needed.

## 7) Validation and operational docs

- [ ] T7.1 Add contract tests for thread state lifecycle:
  - put/get/delete
  - overwrite semantics
  - TTL behavior (if implemented in this phase).
- [ ] T7.2 Add contract tests for `attachments/content_ref` parsing and blob errors.
- [ ] T7.3 Update `docs/ai-nodes-examples-annex.md` with implementation-aligned examples once T2-T6 are done.
- [ ] T7.4 Update runtime install/ops notes with LanceDB operational checklist (path, cleanup, backup caveats).

## Recommended execution order

1. T1 (contract freeze for `thread_id`)
2. T2 (LanceDB foundation)
3. T3 (function calling MVP)
4. T4 (API get/put/delete)
5. T5 (runtime integration)
6. T6 (`attachments/content_ref`)
7. T7 (tests/docs)







