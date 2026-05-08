# Messages Log Viewer — Implementation Tasks

**Status:** drafted, not started
**Date:** 2026-05-08
**Audience:** `SY.architect` developers
**Related docs:** `docs/13-storage.md`, `docs/onworking COA/archi/admin_help_reference.md`

---

## Goal

Add a new section to archi (the SY.architect web frontend) that shows the system-wide log of ILK↔ILK messages persisted by `sy.storage` in `storage_inbox`. The user wants:

- A list ordered by `received_at DESC` (newest on top), minimalist rows.
- A detail panel on the right showing the full JSON of the selected message plus all metadata.
- Live tail when the page is active.
- A new lateral rail to navigate between sections (Archi chat, Messages, future logs) — replaces the single-screen layout. Tabs were rejected for not scaling.

This is **read-only** for archi. archi must **not** be able to mutate `storage_inbox`.

---

## Architecture Summary

```
sy.storage  ──persist──▶  Postgres (storage db)
                              │
                              │  storage_inbox (canonical, raw envelopes)
                              │
                              ▼
                          [read-only role]
                              ▲
                              │  SELECT only
                              │
archi  ──tokio_postgres──┘
   │
   ├── /api/messages         (paginated list, cursor by received_at + dedupe_key)
   ├── /api/messages/stream  (SSE — backend polls DB on a tick)
   └── HTML inline (rail + Messages view)
```

**Source of truth in DB:**
[`storage_inbox`](../../../src/bin/sy_storage.rs#L436-L444):

```sql
CREATE TABLE storage_inbox (
    dedupe_key  TEXT PRIMARY KEY,
    subject     TEXT NOT NULL,
    payload     BYTEA NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts    INT NOT NULL DEFAULT 0,
    processed_at TIMESTAMPTZ,
    last_error  TEXT
);
CREATE INDEX idx_storage_inbox_pending ON storage_inbox(processed_at, received_at);
```

`payload` is BYTEA but contains the raw JSON envelope encoded as UTF-8 bytes — needs `from_utf8` + `serde_json::from_str` on the archi side for the detail view.

---

## Design Decisions (locked)

These were closed in the design conversation, recorded here so future readers have the trace:

1. **Endpoint name**: `/api/messages` (not `/api/router-log` or `/api/ilk-messages`). Rationale: in archi context the noun is unambiguous; subject of dispute later can be split into `/api/messages/<kind>`.
2. **Navigation**: lateral rail, **angosto fijo + tooltip** (no expand toggle), to the left of everything. Sections become first-class. Existing chat-history sidebar moves *inside* the Archi section.
3. **Live tail**: yes, via SSE with backend polling DB every ~1s. No NATS coupling from archi-frontend.
4. **DB access**: cross-process direct read against storage's Postgres, with **a dedicated read-only Postgres role** (option (b) of the design). Mínimo privilegio.
5. **Secret delivery**: a new key `messages_db_url` in archi's existing `secrets.json`, written via `architect_local_config_set` (already implemented for OpenAI). No env-only path. Compatible with the storage / identity pattern.
6. **List row**: 2 lines. Line 1: `HH:MM:SS.mmm · subject`. Line 2 (apagada): `ich · size · estado(✓/…/!)`. `ich` is the chosen identifier for now; can change later.
7. **Filters in MVP**: temporal window (`15m / 1h / 24h / All`) + toggle "sólo con error". Subject filter deferred to iteration 2.
8. **Permissions**: none specific to this view. Same access as the rest of archi.
9. **Volume**: paginated, default 200 rows per page, cursor-based.

---

## Prerequisite — DBA / Operations (not code)

Before any code in archi can be useful, the operator (or an automation step we add later) must create a read-only role in the storage Postgres:

```sql
-- Run as superuser against the storage DB.
CREATE ROLE archi_messages_reader LOGIN PASSWORD '<random-strong>';
GRANT CONNECT ON DATABASE <storage_db_name> TO archi_messages_reader;
GRANT USAGE ON SCHEMA public TO archi_messages_reader;
GRANT SELECT ON public.storage_inbox TO archi_messages_reader;
-- Future-proof if more log tables appear:
ALTER DEFAULT PRIVILEGES IN SCHEMA public
  GRANT SELECT ON TABLES TO archi_messages_reader;
```

The resulting connection string is what gets stored in archi via CONFIG SET. **Do not** reuse the storage write user.

---

## Phase 1 — CONFIG SET extension in archi

archi already has the secret-record machinery ([sy_architect.rs:9115](../../../src/bin/sy_architect.rs#L9115), [sy_architect.rs:4904](../../../src/bin/sy_architect.rs#L4904)). It currently stores **one** key (`ARCHITECT_LOCAL_SECRET_KEY_OPENAI`). Goal: add a second key without breaking the first.

- [ ] `MSGS-T1` Define constant `ARCHITECT_LOCAL_SECRET_KEY_MESSAGES_DB_URL = "messages_db_url"` (next to `ARCHITECT_LOCAL_SECRET_KEY_OPENAI` at [sy_architect.rs:86](../../../src/bin/sy_architect.rs#L86)).
- [ ] ~~`MSGS-T2` Define enum~~ **DROPPED.** archi today does not use a `SecretSource` enum for the OpenAI key (only `ai_configured: AtomicBool` + `ai_runtime: Arc<Mutex<Option<...>>>`). To match the established archi pattern, MSGS-T4 mirrors that lightweight shape instead.
- [ ] `MSGS-T3` Implement `resolve_messages_db_url(node_name: &str) -> Option<String>` that reads `secrets.json` via `load_architect_secret_record(...)` and extracts `secrets[ARCHITECT_LOCAL_SECRET_KEY_MESSAGES_DB_URL]` as a non-empty string. Returns `None` when missing or empty.
- [ ] `MSGS-T4` Extend `ArchitectState` (at [sy_architect.rs:315](../../../src/bin/sy_architect.rs#L315)) with two fields, mirroring the existing OpenAI pair:
  - `messages_db_configured: AtomicBool`
  - `messages_db_url: Arc<RwLock<Option<String>>>` (the resolved URL string; Phase 2 reads this to build the Postgres client)
- [ ] `MSGS-T5` Generalize `handle_architect_local_config_set`:
  - Accept `config.storage.messages_db_url` (string) in the body in addition to the existing OpenAI field.
  - Partial set semantics: each field is independent. If only one is present, leave the other untouched. If neither is present, return error (matches today's behavior for OpenAI).
  - Persist via the same `save_node_secret_record_with_root` path; `secrets.insert(...)` adds the new key without disturbing OpenAI.
  - Response payload `stored_secrets` array includes a second descriptor when the DB url was set.
- [ ] `MSGS-T6` Extend the GET-config response (`load_architect_secret_record` consumer around [sy_architect.rs:9050](../../../src/bin/sy_architect.rs#L9050)) to emit a second `NodeSecretDescriptor` for the messages DB url with `state: configured | missing_secret`.
- [ ] `MSGS-T7` On archi startup, call `resolve_messages_db_url` and stash the source in control state. Do **not** fail boot if missing — feature degrades gracefully (the Messages section shows a "Not configured. Use CONFIG SET to set `messages_db_url`." panel).

**Acceptance:**
- `architect_local_config_set` with only OpenAI still works exactly as before.
- `architect_local_config_set` with only `messages_db_url` sets it and leaves OpenAI alone.
- `architect_local_config_set` with both sets both atomically.
- GET config reports both descriptors with correct redacted/configured/missing state.
- Startup with neither secret still boots; archi serves chat as today and Messages section shows an empty-state message.

---

## Phase 2 — Postgres read-only client in archi

- [ ] `MSGS-T8` Add `tokio_postgres` as a dependency on the archi binary (storage already uses it — same crate, NoTls). No new top-level dep beyond what storage already pulls in.
- [ ] `MSGS-T9` Create a `MessagesDb` module under `src/bin/sy_architect/messages_db.rs`:
  - Wraps a `tokio_postgres::Client` (or a small pool — start with a single client; if contention shows up, switch to `deadpool-postgres` later).
  - Constructor `MessagesDb::connect(connection_string) -> Result<Self, ArchitectError>`.
  - Reconnect-on-error loop with exponential backoff (mirror what storage does).
- [ ] `MSGS-T10` Define query method `list_messages(cursor: Option<MessagesCursor>, filters: MessagesFilters, limit: u32) -> Result<Vec<MessagesRow>>`:
  - Cursor is `(received_at, dedupe_key)` tuple, descending.
  - SQL:
    ```sql
    SELECT dedupe_key, subject, payload, received_at, attempts, processed_at, last_error
    FROM storage_inbox
    WHERE ($1::timestamptz IS NULL OR received_at < $1
           OR (received_at = $1 AND dedupe_key < $2))
      AND ($3::timestamptz IS NULL OR received_at >= $3)
      AND ($4::bool IS NULL OR ($4 = true AND last_error IS NOT NULL)
                            OR ($4 = false AND last_error IS NULL))
    ORDER BY received_at DESC, dedupe_key DESC
    LIMIT $5;
    ```
  - `MessagesRow`: includes a parsed-or-raw payload representation. Detail view needs the JSON; list view does not — so list endpoint can avoid sending `payload` (only size + summary).
- [ ] `MSGS-T11` Define `tail_since(after: (TimestampTz, String)) -> Result<Vec<MessagesRow>>` for the SSE poller:
  - Same projection but filtered by `received_at > $1 OR (received_at = $1 AND dedupe_key > $2)`, ordered ASC, bounded LIMIT (e.g., 500 — backstop in case of bursts).
- [ ] `MSGS-T12` Implement `get_message(dedupe_key) -> Result<Option<MessagesRow>>` that returns the row including the decoded payload (BYTEA → utf8 → serde_json::Value, or raw text if not valid JSON — never panic).

**Acceptance:**
- Connection initializes when `messages_db_url` is configured at boot.
- If `messages_db_url` is missing or invalid, the module is `None` in `ArchitectState`; no panic, just unavailability of the Messages endpoints.
- Manual integration check against a local storage DB returns rows in the expected order.

---

## Phase 3 — `/api/messages` (paginated list)

- [ ] `MSGS-T13` Register route `GET /api/messages` in the axum router around [sy_architect.rs:4622](../../../src/bin/sy_architect.rs#L4622), routed through the existing `dynamic_handler`.
- [ ] `MSGS-T14` Implement handler. Query params:
  - `cursor` — opaque base64 of `{"ts": "...", "dk": "..."}`. Optional (first page).
  - `limit` — default 200, max 500.
  - `since` — one of `15m | 1h | 24h | all` (server resolves to absolute timestamp).
  - `with_error` — `true | false`. Optional (default: include both).
- [ ] `MSGS-T15` Response shape:
  ```json
  {
    "items": [
      {
        "dedupe_key": "...",
        "subject": "inv.events.v1.hold.created",
        "received_at": "2026-05-08T14:32:07.412Z",
        "attempts": 0,
        "processed_at": "2026-05-08T14:32:07.498Z",
        "has_error": false,
        "size_bytes": 248,
        "ich": "abc"
      }
    ],
    "next_cursor": "<base64>" | null
  }
  ```
  - `ich` is extracted from the payload during query (cheap pre-parse for the small subset of fields the list needs — see MSGS-T16).
  - Payload itself is **not** included in the list response.
- [ ] `MSGS-T16` `ich` extraction strategy:
  - Try a JSON path inside payload (e.g., `envelope.ich` or whatever the canonical location is — verify against actual envelopes in `storage_inbox` before fixing the path).
  - If extraction fails, return `null` for `ich` — never block the row.
- [ ] `MSGS-T17` Error handling:
  - If `MessagesDb` is unavailable → `503` with body `{"error": "messages_db_not_configured"}`. Frontend renders the empty-state.
  - If query fails → `500` with redacted error message.

**Acceptance:**
- Hitting `/api/messages` against a populated storage DB returns 200 rows or fewer, newest first, with valid cursor for the next page.
- Filtering with `since=1h` excludes rows older than 1h.
- `with_error=true` returns only rows with `last_error IS NOT NULL`.

---

## Phase 4 — `/api/messages/stream` (SSE tail)

- [ ] `MSGS-T18` Register route `GET /api/messages/stream`. Returns `text/event-stream`.
- [ ] `MSGS-T19` Handler logic:
  - Open SSE connection. Initial cursor from `?after_ts=...&after_dk=...` query (defaults to "now" if absent).
  - Loop every 1000 ms: call `MessagesDb::tail_since(cursor)`, advance cursor to last seen row.
  - For each new row, emit `event: message\ndata: {<same row shape as list>}\n\n`.
  - Heartbeat every 15s with `: ping\n\n` to keep the connection alive through proxies.
  - Cancellation: stop loop when client disconnects (axum's `Sse` stream handles drop).
- [ ] `MSGS-T20` Backpressure / safety:
  - If a tick returns ≥ 500 rows, log a warning and continue — likely the page was paused or the system bursted; consumer can refresh via `/api/messages` to resync.
  - If DB query fails, emit `event: error\ndata: {...}\n\n` and exit (let frontend reconnect with backoff).
- [ ] `MSGS-T21` Concurrency: each open SSE keeps its own poll loop. For sane caps, register max simultaneous SSE per archi instance (e.g., 16) — beyond that return `503`.

**Acceptance:**
- Open stream, insert a test row in storage_inbox, see it on the stream within ~1.5s.
- Killing client cancels the loop server-side (no orphan tasks).

---

## Phase 5 — Front-end: rail + hash routing reorganization

The front-end is inlined as a Rust raw string at [sy_architect.rs:12485-16429](../../../src/bin/sy_architect.rs#L12485-L16429). All HTML/CSS/JS lives there.

- [ ] `MSGS-T22` Add CSS for `.app-rail` (fixed left, ~56px, dark, vertical):
  - One slot per section: small icon (SVG) + tooltip on hover.
  - Active state: brighter background or accent bar on the left edge.
  - The rail sits **outside** `.workspace`. Layout becomes: `.page > .app-rail` + `.page > .main-stage`, and `.main-stage` contains masthead + workspace.
- [ ] `MSGS-T23` Add JS hash router:
  - Parse `location.hash` → section id (`archi` | `messages`).
  - Default `#/archi` if empty or unknown.
  - On `hashchange`, swap the visible section, update active rail item, lazy-init the section's JS module on first entry.
- [ ] `MSGS-T24` Wrap the existing chat workspace (sidebar + shell) in `<section data-section="archi">`. No DOM/CSS changes inside; just enclose. Hidden via `display:none` when not active.
- [ ] `MSGS-T25` Add a new `<section data-section="messages">` skeleton (filled in Phase 6). Hidden by default.
- [ ] `MSGS-T26` Rail icons for MVP:
  - Archi → chat-bubble icon, label "Archi"
  - Messages → list-with-lightning icon, label "Messages"
  - Reserve a "more sections" pattern in the JS so adding `cognitive` later is one entry.

**Acceptance:**
- Loading `/` lands on `#/archi` and shows the existing UI unchanged.
- Clicking the Messages rail item updates the hash and swaps the workspace.
- Refreshing the page on `#/messages` lands directly on Messages.
- Existing E2E flows for chat / publish / sessions are unaffected.

---

## Phase 6 — Vista Messages

- [ ] `MSGS-T27` HTML skeleton inside `<section data-section="messages">`:
  ```
  .messages-view
    .messages-toolbar      (filters: temporal window dropdown + with-error toggle + status pill "live | paused")
    .messages-body
      .messages-list       (left, fixed width ~360px, virtualized scroll if rows > 200)
      .messages-detail     (right, fills remaining)
        .messages-detail-meta   (subject, received_at, dedupe_key, ich, attempts, processed_at, last_error)
        .messages-detail-json   (pretty-printed payload, copy button)
        .messages-detail-empty  (shown when no row selected)
  ```
- [ ] `MSGS-T28` CSS:
  - List row two-line layout (Line 1 mono small; Line 2 muted smaller).
  - Status icon: `✓` (processed_at NOT NULL, last_error NULL), `…` (processed_at NULL), `!` (last_error NOT NULL, red).
  - Selected row: subtle accent border on the left, slightly elevated background.
  - JSON pane: monospace, comfortable line-height, syntax-color via a tiny inline highlighter (or just bold keys). Avoid heavyweight libs.
- [ ] `MSGS-T29` JS module `messages-view.js` (inlined in the same raw string):
  - State: `{ rows: [], selected: null, cursor: null, live: true, eventSource: null, filters: { since: 'all', withError: null } }`.
  - `loadInitial()` → `GET /api/messages?since=...&limit=200` → `state.rows`, `state.cursor = response.next_cursor`.
  - `loadMore()` → uses cursor.
  - `startStream()` → opens `/api/messages/stream?after_ts=<top-row.received_at>&after_dk=<top-row.dedupe_key>`. On message events: prepend if user is at top; if scrolled, increment a "N nuevos" pill instead.
  - `selectRow(dedupe_key)` → `GET /api/messages/<dedupe_key>` (see MSGS-T31) → render detail.
  - On filter change: cancel stream, reload from scratch, restart stream.
  - On section deactivate: cancel stream.
- [ ] `MSGS-T30` Empty / unconfigured state:
  - If `/api/messages` returns 503 `messages_db_not_configured`, render a single-pane informational panel: "messages_db_url no está configurado en archi. Setealo via CONFIG SET para habilitar este panel."
- [ ] `MSGS-T31` Single-message endpoint `GET /api/messages/<dedupe_key>`:
  - Returns full row including decoded payload.
  - Used by the detail panel to fetch the JSON only when the user clicks (avoids sending payloads for every list row).

**Acceptance:**
- Open `#/messages` with a populated DB → see latest rows in list, click a row → JSON appears on the right.
- Toggle "sólo con error" → list filters live, detail clears.
- Change temporal window → list reloads.
- New message arrives in DB → appears at top within ~2s when scroll is at top, otherwise "N nuevos" pill appears.
- Click "N nuevos" → scrolls to top and surfaces new rows.

---

## Phase 7 — Wiring & smoke test

- [ ] `MSGS-T32` Smoke checklist (manual, no automated E2E in this iteration):
  - With a fresh archi instance, neither secret configured → boot succeeds, chat works, Messages section shows empty state.
  - Configure OpenAI only via existing flow → unchanged.
  - Configure `messages_db_url` only → Messages section becomes functional, chat unaffected.
  - Configure both → both work.
  - Send messages through NATS so storage_inbox grows → list reflects them, detail JSON renders.
  - Error rows (last_error populated) render with red `!` and surface `last_error` in the detail meta.
- [ ] `MSGS-T33` Update [admin_help_reference.md](./admin_help_reference.md) with the new CONFIG SET field `config.storage.messages_db_url` and a short note on what enables.
- [ ] `MSGS-T34` Add a brief section to [handbook_fluxbee.md](./handbook_fluxbee.md) describing the rail and the Messages view (one paragraph + one screenshot or ASCII once it exists).

---

## Out of Scope (deferred)

- Subject filter / autocomplete in the toolbar (iteration 2).
- Search inside payload JSON (expensive — needs full-text index or external store).
- Cognitive log section (separate task doc when ready).
- Migrating storage's Postgres connection out of `EnvCompat` env-var fallback (separate cleanup if desired).
- Pushing the SSE through a NATS-direct path instead of DB polling (only worth doing if 1s latency proves insufficient).
- Authn/authz for the Messages view (the design says "no permisos en esta página" — revisit when archi gains general operator gating).

---

## Open Questions — Resolved

Locked in conversation, recorded here for the implementer:

1. **`ich` JSON path inside payload** — `payload → JSON → meta.ich` (canonical, see [docs/02-protocolo.md:83](../../02-protocolo.md#L83) and [02-protocolo.md:295](../../02-protocolo.md#L295)). Best-effort fallback chain: `meta.ich → meta.thread_id → null`. **`ich = null` is unexpected in practice** — every well-formed bus message should carry one. Treat null as a debugging signal worth surfacing in the UI (e.g., subtle italic), not as routine. `storage_inbox` is heterogeneous (turns vs cognition envelopes); cognition envelopes legitimately won't carry `meta.ich` — for those, `thread_id` may still be present, otherwise null is fine.
2. **archi node identity for `secrets.json`** — confirmed: use `state.node_name` (`SY.architect@<hive_id>`, see [sy_architect.rs:4808](../../../src/bin/sy_architect.rs#L4808)). Same pattern as the existing OpenAI key. `messages_db_url` becomes per-hive automatically because each hive's archi gets its own `secrets.json` directory under `architect_nodes_root()`. This aligns with each hive having its own storage Postgres.
3. **Pool size for `MessagesDb`** — confirmed: single `Arc<tokio_postgres::Client>` with reconnect-on-error. `tokio_postgres::Client` query methods take `&self`, so concurrent reads from multiple tasks share one connection (pipelined). Expected load: ≤20 q/s (16 SSE polling + UI reads), all indexed `SELECT`. Same pattern storage uses internally. If full-text search or heavier joins arrive later, switch to `deadpool-postgres`.
