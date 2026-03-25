# L3 Meta Contract Alignment Tasks (IO -> AI)

Date: 2026-03-19  
Scope: align production IO/AI nodes with current canonical contract used by `IO.test` and `AI.test.gov`.

---

## 0. Baseline (source of truth)

Validated in code:
- `IO.test` sends identity in `meta.src_ilk` (not in `meta.context.src_ilk`).
- `AI.test.gov` reads identity from `meta.src_ilk`.
- `meta.context` remains for auxiliary metadata (including `io.reply_target` flow), not as primary identity carrier.

References:
- `nodes/test/io-test/src/main.rs`
- `nodes/test/ai-test-gov/src/main.rs`
- `nodes/test/README.md`
- `crates/fluxbee_sdk/src/protocol.rs` (`Meta.src_ilk`)

---

## 1. What changed vs legacy

Canonical now:
- `src_ilk`: `meta.src_ilk`

Still in transition / mixed:
- `dst_ilk`, `ich`, `ctx`, `ctx_seq`, `ctx_window`: not consistently modeled in SDK typed `Meta` + docs + node implementations.

Must remain in `meta.context`:
- channel-specific metadata under `meta.context.io.*` (reply target, conversation, sender, etc.).

---

## 2. IO-first execution plan

### IO-P0 Contract freeze

- [x] IO-P0.1 Freeze canonical identity field as `meta.src_ilk` for all IO adapters.
- [x] IO-P0.2 Remove `meta.context.src_ilk` from IO envelopes (do not keep compatibility carrier if core/router do not require it).
- [x] IO-P0.3 Keep `meta.context.io.*` as canonical channel metadata carrier.

Acceptance:
- no active IO docs describe `meta.context.src_ilk` as primary path.

### IO-P1 io-common migration

- [x] IO-P1.1 Update `io-common` message builder to set `meta.src_ilk` (primary).
- [x] IO-P1.2 Remove dual-write behavior for `src_ilk`; only `meta.src_ilk` must be emitted.
- [x] IO-P1.3 Update `io-common` inbound tests to assert `meta.src_ilk` first.
- [x] IO-P1.4 Keep `thread_id` handling unchanged for now (`meta.context.thread_id` legacy path), only decouple from identity carrier migration.

Acceptance:
- io-common-generated inbound messages always contain `meta.src_ilk`.

### IO-P2 io-slack and io-sim adoption

- [x] IO-P2.1 Update `io-slack` logging/validation from `meta.context.src_ilk` to `meta.src_ilk`.
- [x] IO-P2.2 Update `io-sim` debug and assertions to print/check `meta.src_ilk`.
- [x] IO-P2.3 Keep `meta.context.io.reply_target` unchanged and tested.
- [x] IO-P2.4 Add parity checks vs `IO.test` envelope shape.

Acceptance:
- `io-sim` and `io-slack` emit same identity location as `IO.test`.

### IO-P3 docs cleanup

- [x] IO-P3.1 Update `docs/io/README.md`.
- [x] IO-P3.2 Update `docs/io/io-common.md`.
- [x] IO-P3.3 Update `docs/io/io-slack.md`.
- [x] IO-P3.4 Update `docs/io/nodos_io_spec.md`.
- [x] IO-P3.5 Update `docs/io/io-slack-tasks.md`.

Acceptance:
- all IO docs consistently state `meta.src_ilk` primary + `meta.context.io.*` for channel data.

---

## 3. AI-second execution plan

### AI-P0 Runner alignment

- [x] AI-P0.1 Change `ai_node_runner::extract_src_ilk()` to read `meta.src_ilk` first.
- [x] AI-P0.2 Keep legacy fallback to `meta.context.src_ilk` during transition.
- [x] AI-P0.3 Add structured log field indicating source (`meta` vs `context_legacy`) for rollout diagnostics.

Acceptance:
- AI runner accepts canonical field and still works with legacy payloads.

### AI-P1 Tests and fixtures

- [x] AI-P1.1 Update runner tests currently asserting `meta.context.src_ilk`.
- [x] AI-P1.2 Add tests for precedence (`meta.src_ilk` overrides legacy context).
- [x] AI-P1.3 Update frontdesk examples/fixtures to emit/use canonical `meta.src_ilk`.

Acceptance:
- tests explicitly enforce canonical-first behavior.

### AI-P2 Docs cleanup

- [x] AI-P2.1 Update `docs/AI_nodes_spec.md` where it still states `meta.context.src_ilk`.
- [x] AI-P2.2 Update `docs/ai-frontdesk-gov-spec.md` examples.
- [x] AI-P2.3 Update `docs/ai-nodes-examples-annex.md` references to legacy identity carrier.

Acceptance:
- no AI active doc claims `meta.context.src_ilk` as primary.

---

## 4. Cross-cut task (after IO+AI)

- [ ] X1 Decide and document roadmap for `dst_ilk/ich/ctx/ctx_seq/ctx_window`:
  - SDK typed `Meta` fields vs temporary carrier strategy,
  - migration window and cutover release,
  - compatibility matrix.

Acceptance:
- one explicit contract matrix for L3 fields, no ambiguity across core/sdk/io/ai docs.

---

## 5. Recommended order for execution

1. IO-P0
2. IO-P1
3. IO-P2
4. IO-P3
5. AI-P0
6. AI-P1
7. AI-P2
8. X1
