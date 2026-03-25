# AI Nodes Plan - Repo Spec Alignment (single-state JSON + spawn)

Status: proposed  
Owner: AI nodes initiative  
Last update: 2026-03-12

Scope:
- AI nodes only (runner/sdk/scripts/docs AI).
- No Fluxbee core/router/orchestrator code changes in this plan.
- Align only with current repo docs (`docs/AI_nodes_spec.md`, `docs/ai-nodes-examples-annex.md`).

## 0) Delta summary vs current implementation

Current implementation (AI):
- Requires `--config <yaml>` to boot.
- Persists dynamic JSON in `${STATE_DIR}/ai-nodes`, but YAML still drives startup behavior.
- Supports `CONFIG_SET/GET`, version checks, redacted persistence.

Current repo spec direction:
- Effective config must be a single JSON state file in `${STATE_DIR}/ai-nodes/<node>.json`.
- Node should boot `UNCONFIGURED` if effective JSON is missing.
- YAML is optional template/reference, not runtime source of truth.
- Spawn responsibility for first JSON creation is still TBD at core level (orchestrator vs node on first `CONFIG_SET`).

## 1) Decision to lock for AI-side implementation (without core changes)

Decision D1 (AI-side, immediate):
- Implement Option B behavior in node runtime:
  - If effective JSON exists, boot from JSON.
  - If missing, boot `UNCONFIGURED` and wait for `CONFIG_SET`.
  - On first valid `CONFIG_SET`, create JSON atomically.

Decision D2 (compat mode, temporary):
- Keep YAML load only as optional bootstrap import path (manual/dev), not as effective runtime source.
- Explicitly mark this as compatibility path until spawn flow is fully closed in core.

## 2) Implementation plan (ordered)

### P0 - Runtime boot semantics (must-have)

- [x] P0.1 Make `ai_node_runner` able to start without per-node YAML.
- [x] P0.2 Add runner args for boot identity when no YAML is provided (`--node-name`, dirs, socket, version).
- [x] P0.3 Startup logic:
  - if `${STATE_DIR}/ai-nodes/<node>.json` exists -> load effective config
  - if missing -> state `UNCONFIGURED` and control-plane only
- [x] P0.4 Keep user-message gate strict: only process `user` when state is `CONFIGURED`.

### P1 - Effective JSON contract (must-have)

- [x] P1.1 Define Rust struct for effective state JSON (schema/config version + node/behavior/runtime/secrets refs).
- [x] P1.2 Validate and materialize defaults into persisted JSON ("defaults materialized" section in repo spec).
- [x] P1.3 Atomic write policy (`tmp + fsync + rename`) for state file updates.
- [ ] P1.4 Persist secrets in effective state (`secrets.openai.api_key`) with at-rest protection policy (encrypted preferred); avoid hard dependency on env vars.
  - [x] P1.4.a Persist `api_key` in effective JSON and resolve runtime key from persisted state first.
  - [ ] P1.4.b Add at-rest protection strategy (encryption/secret-store integration) and migration.

### P2 - Control-plane alignment refinements

- [x] P2.1 Require and validate `apply_mode` in `CONFIG_SET` (`replace` first; `merge_patch` optional in next step).
- [x] P2.2 Implement `apply_mode=replace` fully over effective JSON.
- [x] P2.3 Keep `CONFIG_RESPONSE` canonical (`ok/schema_version/config_version/error/effective_config`).
- [x] P2.4 Add `unknown_system_msg` response for unsupported control-plane commands.
- [x] P2.5 Implement `PING`/`STATUS` minimal contract.

### P3 - Spawn integration bridge (AI-side only)

- [x] P3.1 Extend `ai-nodectl` with "init-state" command to precreate effective JSON from template payload.
- [x] P3.2 Add docs flow:
  - spawn with precreated JSON (operator/tooling path)
  - boot without JSON + first `CONFIG_SET` (self-materialization path)
- [x] P3.3 Keep installer behavior unchanged except documenting new expected state-file lifecycle.

### P4 - Tests and migration

- [ ] P4.1 Contract tests for boot states:
  - no JSON -> `UNCONFIGURED`
  - valid JSON -> `CONFIGURED`
  - invalid JSON -> `FAILED_CONFIG` + control-plane active
- [ ] P4.2 Contract tests for `CONFIG_SET` create/update/idempotency/stale.
- [ ] P4.3 Migration note from YAML-driven runtime to JSON-driven runtime (AI only).

## 3) Explicit dependencies on core (tracked, not implemented here)

- [ ] C-DEP.1 Who creates first effective JSON during spawn (orchestrator vs node-first-SET) - core policy decision.
- [ ] C-DEP.2 Canonical per-node config message in core (`CONFIG_SET/GET` vs `CONFIG_CHANGED` targeting) - protocol decision.
- [ ] C-DEP.3 Full typed L3 fields availability (`ich/ctx/ctx_seq/ctx_window`) - core/router/sdk rollout.

## 4) Recommended execution order

1. P0
2. P1
3. P2 (replace-only first)
4. P4
5. P3 (tooling polish)
