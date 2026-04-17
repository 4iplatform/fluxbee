# SY.architect — Attachment Support Tasks

**Status:** proposed
**Audience:** `SY.architect`, `fluxbee-ai-sdk`, UI/runtime developers
**Goal:** allow the operator to attach documents/images in Archi and send them to the model naturally, using Fluxbee blob storage and the existing OpenAI Responses multimodal path.

---

## 1. Recommendation

Recommended approach:

- **Do not** extend `POST /api/chat` as raw multipart first.
- **Do** add a two-step flow:
  1. upload files to `SY.architect` local HTTP API
  2. persist them as Fluxbee blobs
  3. return `blob_ref` descriptors to the browser
  4. send chat messages with `message + attachments[]`
  5. build `current_user_parts` from `TextV1Payload(content, attachments)`

Why this is the right shape for Fluxbee:

- it reuses the existing `BlobToolkit` / `BlobRef` contract
- it matches `TextV1Payload.attachments`
- it keeps chat retries simple
- it avoids stuffing large binary bodies into the current JSON-only `/api/chat`
- it gives Archi stable attachment handles that can be persisted in session history

---

## 2. Current State

What already exists:

- `SY.architect` already uses `fluxbee_ai_sdk::FunctionRunInput`, and that type already supports `current_user_parts`.
- `fluxbee_ai_sdk` already supports:
  - `input_image`
  - `input_file`
  - textual extraction for text-like attachments
  - blob-backed resolution via `BlobRef`
- `fluxbee_sdk::payload::TextV1Payload` already supports `attachments: Vec<BlobRef>`.
- `fluxbee_sdk::blob::BlobToolkit` already supports:
  - `put`
  - `put_bytes`
  - `promote`
  - blob naming/validation
- Archi chat persistence already has `metadata_json`, so attachment metadata can be stored without changing Lance schema immediately.

What does **not** exist yet:

- no upload endpoint in `SY.architect`
- no file picker / draft attachment UI
- `ChatRequest` only carries `session_id` + `message`
- `handle_ai_chat()` always sends `current_user_parts: None`
- persisted user messages are currently treated as plain text only

---

## 3. Problems To Solve

### 3.1 Ingress contract

Current `/api/chat` is JSON-only and capped with a 1 MB body read. That is not a suitable carrier for documents/images.

### 3.2 Persistence model

Session history currently stores user text in `content` and marks metadata as `{ "kind": "text" }`.

If attachments are added, Archi must preserve:

- original filename
- mime
- blob ref
- whether the message had attachments

Without that, history replay and UI rendering drift immediately.

### 3.3 Current-turn multimodal input

The model only sees multimodal structured parts if `current_user_parts` is populated.

Today Archi always uses:

- `current_user_message = input`
- `current_user_parts = None`

That means attachments would be ignored even if uploaded.

### 3.4 Prior-turn continuity

Recent conversation memory currently reduces prior user messages to text only.

That is acceptable for v1 if we persist a compact attachment summary into the message text/memory, but it means:

- prior turns will not be replayed as true structured `input_file` / `input_image`
- only the current turn gets full multimodal fidelity

This is probably fine for the first implementation, but it should be explicit.

### 3.5 Blob lifecycle

If Archi starts uploading files to blob storage, someone has to own lifecycle expectations:

- chat attachment blobs may outlive the chat
- session deletion probably will not delete blobs in v1
- blob GC becomes the real cleanup mechanism

This is acceptable, but it should be acknowledged.

### 3.6 Security / privacy

Attachments may contain secrets or regulated content.

At minimum:

- metadata persistence must avoid dumping full binary content
- logs must not print file bytes/base64
- only `blob_ref` + safe metadata should be stored

### 3.7 Scope of feature

This capability is for **AI chat mode** first.

Do not mix it into:

- SCMD
- CONFIRM / CANCEL
- impersonation dispatch

Those flows use different carriers and should stay unchanged in v1.

---

## 4. Proposed Runtime Design

### 4.1 HTTP flow

Add a dedicated upload endpoint in `SY.architect`:

- `POST /api/attachments`

Accepted shape:

- multipart form upload
- one or more files
- optional `session_id`

Response shape:

```json
{
  "status": "ok",
  "attachments": [
    {
      "attachment_id": "att_...",
      "filename": "invoice.pdf",
      "mime": "application/pdf",
      "size": 182233,
      "blob_ref": { ... }
    }
  ]
}
```

Then extend `POST /api/chat` request shape:

```json
{
  "session_id": "sess_...",
  "message": "revisa este documento",
  "attachments": [
    {
      "attachment_id": "att_...",
      "blob_ref": { ... }
    }
  ]
}
```

### 4.2 Model input flow

Inside `handle_ai_chat()`:

1. build a `TextV1Payload::new(message, attachments_blob_refs)`
2. convert it with:
   - `resolve_model_input_from_payload_with_options(...)`
   - `build_openai_user_content_parts(...)`
3. pass:
   - `current_user_message = message`
   - `current_user_parts = Some(parts)`

Set `multimodal = true` in the model input options.

### 4.3 Persistence flow

Persist the user row with:

- `content`: the typed message only, or a compact rendered summary
- `metadata.kind = "text_with_attachments"` when attachments exist
- `metadata.attachments = [{ filename, mime, size, blob_ref }]`

For recent-memory rendering, summarize prior attachments as compact text, for example:

- `Attachment: invoice.pdf (application/pdf, 182233 bytes)`

That keeps memory continuity without having to replay old file parts as multimodal content.

### 4.4 UI flow

Add in Archi UI:

- file picker button near composer
- draft attachment chips before send
- remove-chip action before send
- attachment rendering in message history

Do not auto-send on upload. Upload should stage the file; send still happens with the normal chat submit.

---

## 5. Constraints / Non-Goals

### v1 constraints

- current-turn attachments only are true multimodal inputs
- previous turns are summarized, not rehydrated as structured file parts
- no automatic blob deletion on session delete
- no attachment support for impersonation or SCMD
- no cross-node blob sync orchestration from Archi itself

### non-goals

- full document management UI
- attachment search/indexing
- OCR pipeline beyond what the model/provider already does
- attachment sharing between independent chats

---

## 6. Task List

- [x] `ARCHI-ATTACH-1` — Define request/response contracts

- Add `AttachmentUploadResponse` and `ChatAttachmentRef` structs in `sy_architect.rs`.
- Extend `ChatRequest` with optional `attachments`.
- Define persisted metadata shape for attachment-bearing user messages.

Acceptance:

- JSON contract for `/api/chat` is explicit.
- Upload response returns stable attachment descriptors based on `blob_ref`.

- [x] `ARCHI-ATTACH-2` — Add upload endpoint

- Add `POST /api/attachments` to `dynamic_handler`.
- Parse multipart form uploads.
- Reject invalid or empty files cleanly.
- Enforce max per-file size and max file count.

Acceptance:

- Browser can upload one or more files without using `/api/chat`.
- Endpoint returns `blob_ref` metadata only, not file bytes.

- [x] `ARCHI-ATTACH-3` — Persist uploads as Fluxbee blobs

- Use `BlobToolkit::put_bytes(...)` + `promote(...)`.
- Use Fluxbee blob root/config defaults.
- Capture `filename`, `mime`, `size`, `blob_ref`.

Acceptance:

- uploaded files land in standard Fluxbee blob storage
- returned refs validate as `blob_ref`

- [x] `ARCHI-ATTACH-4` — Validate allowed attachment types

- Reuse `fluxbee_ai_sdk` validation semantics where possible.
- Define local upload allowlist/denylist:
  - images supported by OpenAI
  - pdf/docx/txt/md/json/csv
- Reject clearly unsupported binary formats with explicit error.

Acceptance:

- unsupported mime fails at upload or chat-build time with a stable error code

- [x] `ARCHI-ATTACH-5` — Build multimodal chat input

- In `handle_ai_chat()`, build a `TextV1Payload` with attachments.
- Use:
  - `resolve_model_input_from_payload_with_options`
  - `build_openai_user_content_parts`
- pass structured `current_user_parts` to `FunctionRunInput`

Acceptance:

- current-turn images reach the model as `input_image`
- current-turn docs/files reach the model as `input_file` or text-expanded attachment blocks

- [x] `ARCHI-ATTACH-6` — Add persistence for attachment-bearing messages

- Persist safe attachment metadata in `metadata_json`
- Add a new metadata kind such as:
  - `text_with_attachments`
- Do not persist base64 or raw bytes

Acceptance:

- reloading a session shows which files were attached
- no binary payload is stored in chat rows

- [x] `ARCHI-ATTACH-7` — Extend memory rendering for previous attachment turns

- Update `immediate_interaction_from_message()` or a nearby helper so prior messages with attachments produce compact textual summaries.
- Example:
  - user text
  - plus one line per attachment

Acceptance:

- prior attachment turns remain understandable in immediate memory
- no attempt is made to replay old blobs as live multipart model input in v1

- [x] `ARCHI-ATTACH-8` — Add composer UI for attachments

- Add file input control
- Add draft attachment state in browser
- Add upload action before send
- Add remove attachment action before send
- Include returned attachment descriptors in `/api/chat` payload

Acceptance:

- operator can attach files naturally from the Archi UI
- files are visible before send and removable

- [x] `ARCHI-ATTACH-9` — Render attachments in message history

- Show attachment pills/cards in user messages
- Include filename, mime, size
- Optional: download/open link if a local route is later added

Acceptance:

- session reload preserves attachment visibility

- [ ] `ARCHI-ATTACH-10` — Logging and redaction pass

- Ensure uploads are not logged as raw data
- Ensure persisted message sanitization does not dump secrets accidentally
- Keep only safe metadata in logs and persistence

Acceptance:

- logs contain filenames/blob refs at most, never raw payload bytes

- [ ] `ARCHI-ATTACH-11` — Tests

- Unit tests for:
  - upload contract validation
  - blob persistence helper
  - chat request parsing with attachments
  - multimodal parts generation
  - persistence metadata shape
- Integration tests for:
  - upload -> chat -> persisted session
  - text-only chat remains unchanged

Acceptance:

- attachment path is covered without regressing current chat flow

- [x] `ARCHI-ATTACH-12` — Documentation

- Update `DEVELOPMENT.md` or Archi-specific docs with:
  - supported file types
  - size limits
  - current-turn-only multimodal fidelity
  - storage/lifecycle note for blobs

Acceptance:

- operators and developers understand the feature boundaries

---

## 7. Suggested Implementation Order

1. `ARCHI-ATTACH-1`
2. `ARCHI-ATTACH-2`
3. `ARCHI-ATTACH-3`
4. `ARCHI-ATTACH-5`
5. `ARCHI-ATTACH-6`
6. `ARCHI-ATTACH-8`
7. `ARCHI-ATTACH-9`
8. `ARCHI-ATTACH-7`
9. `ARCHI-ATTACH-10`
10. `ARCHI-ATTACH-11`
11. `ARCHI-ATTACH-12`

Reason:

- first get upload + blob refs + model input working
- then persist/render it properly
- then harden memory/docs/tests

---

## 8. Final Assessment

I do **not** see a blocking architectural problem.

This feature fits Fluxbee naturally because:

- blob storage already exists
- text payload attachments already exist
- OpenAI structured content parts already exist
- Archi already has a clean local HTTP surface and persistence model

The only important design choice is to avoid forcing binary uploads through the current `/api/chat` JSON path.

The recommended design is:

- upload to blob first
- send blob refs in chat
- render current turn as multimodal parts
- summarize prior turns textually in memory

That is the lowest-risk extension and the one most aligned with the current Fluxbee infrastructure.
