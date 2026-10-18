# AI Nodes MVP Alignment Tasks (spec update)

Status: active  
Owner: AI nodes initiative  
Last update: 2026-03-11

Scope rules:
- Focus only on AI nodes MVP.
- Do not modify Fluxbee core/router contracts.
- Implement in AI crates/binaries and AI docs only.

## Phase A - Control plane mínimo (primero)

- [x] A1. Implement `CONFIG_SET` handling in `ai_node_runner` for `meta.type in {system,admin}`.
- [x] A2. Validate `CONFIG_SET` minimum contract: `subsystem`, `node_name`, `schema_version`, `config_version`, `config`.
- [x] A3. Add `config_version` monotonic checks (`stale_config_version` rejection, equal version idempotent).
- [x] A4. Return canonical `CONFIG_RESPONSE` (`status`, `schema_version`, `config_version`, `state`, `error_*` when needed).
- [x] A5. Implement `CONFIG_GET` and return current effective config snapshot (secret redacted).

## Phase B - Estado y ciclo de vida de instancia

- [x] B1. Add per-instance state machine: `UNCONFIGURED`, `CONFIGURED`, `FAILED_CONFIG`.
- [x] B2. Route `user` messages through state gate (`node_not_configured` in `UNCONFIGURED/FAILED_CONFIG`).
- [ ] B3. Keep control plane commands available in `FAILED_CONFIG` for recovery.

## Phase C - Persistencia dinámica AI (no core)

- [x] C1. Persist dynamic AI config to `${STATE_DIR}/ai-nodes/<node>.json` (without plaintext secrets).
- [x] C2. Rehydrate persisted config on startup and recompute effective state.
- [x] C3. Document clearly: UUID persistence already exists; this phase adds config persistence.

## Phase D - Secrets MVP (alineado a spec nueva)

- [x] D1. Support `behavior.openai.api_key` inline via `CONFIG_SET` as in-memory hot override.
- [x] D2. Keep YAML/env source compatibility, with clear precedence for current MVP behavior.
- [x] D3. Ensure secret never appears in persisted state/config responses/logs.

## Phase E - Data plane ajustes mínimos

- [ ] E1. Ensure `text/v1` parsing handles `content` and `content_ref` path correctly in runner flow.
- [ ] E2. Normalize user-facing error payload strategy (prefer typed error, keep `text/v1` fallback).
- [ ] E3. Add/adjust contract tests for control plane and state-gated user flow.

## Phase F - Opcional diferido (no bloquear MVP)

- [ ] F1. Add `PING`/`STATUS` commands for AI node control plane.
- [ ] F2. Add `apply_mode=merge_patch` (`replace`/`merge_patch`) support for `CONFIG_SET`.
- [ ] F3. Add `api_key_ref` resolver backends (`env/file/...`) per final security model.

## Execution order (strict)

1. Phase A
2. Phase B
3. Phase C
4. Phase D
5. Phase E
6. Phase F (only if requested)
