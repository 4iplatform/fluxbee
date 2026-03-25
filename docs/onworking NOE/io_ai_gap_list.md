# IO + AI Gap List (Working Backlog)

Status: draft
Last update: 2026-03-04
Scope: `io-common`, `io-slack`, `fluxbee_ai_sdk`, `ai_node_runner`

## Goal
Track what is missing or inconsistent after validating end-to-end flow (`Slack -> IO -> Router -> AI -> IO`) so we can close items one by one.

## P0 (must close first)

- [ ] P0.1 Move identity fields to native protocol metadata.
Description: `src_ilk/dst_ilk` are currently stored in `meta.context` as a temporary workaround.
Current location: `nodes/io/common/src/router_message.rs`.
Target: use native metadata fields once protocol supports them.
> La especificación/documentación de protocolo en el repo contempla src_ilk/dst_ilk como metadata principal.
Pero la implementación actual del struct Meta en fluxbee_sdk no tiene esos campos.
Por eso hoy estamos usando el workaround en meta.context.src_ilk / meta.context.dst_ilk.

- [ ] P0.2 Remove fixed destination test path from production behavior.
Description: `IO_SLACK_DST_NODE` is useful for tests but should not be required for normal routing.
Current location: `nodes/io/io-slack/src/main.rs`.
Target: Router/OPA-driven destination in production; keep fixed dst only as explicit test mode.

- [x] P0.3 Add deploy/runtime path for AI and IO nodes.
Description: `scripts/install.sh` installs core `sy-*` services, not `ai_node_runner` nor `io-*` runtime units.
Current location: `scripts/install.sh`.
Target: clear install/run strategy (systemd units, env files, restart policy, logs).

- [x] P0.4 Make AI runtime behavior explicit on idle read timeout.
Description: operators interpret node exit as crash when timeout ends read loop.
Current location: `crates/fluxbee_ai_sdk/src/runtime.rs` and `crates/fluxbee_ai_nodes/src/bin/ai_node_runner.rs`.
Target: explicit policy (`exit_on_read_timeout` vs `keep_alive`) and documented defaults.
Resolution:
- Adopted `keep_alive`.
- Read timeout is treated as idle tick, not fatal/stop condition.
- Runtime metrics include `idle_read_timeouts` for observability.

## P1 (important, next)

- [ ] P1.1 Harden IO outbound reliability.
Description: Slack outbound is still best-effort (memory only).
Current location: `nodes/io/io-slack/src/main.rs`.
Target: durable outbox/retry policy (or explicit product decision that MVP remains in-memory).

- [ ] P1.2 Improve observability for dropped/ignored outbound messages.
Description: messages without parseable `reply_target` are silently skipped.
Current location: `nodes/io/io-slack/src/main.rs` (`run_outbound_loop`).
Target: structured warn/metric with reason.

- [ ] P1.3 Identity resolver configurability.
Description: SHM path/limits are hardcoded (`/dev/shm/jsr-identity-*`, len limits).
Current location: `nodes/io/common/src/identity.rs`.
Target: configurable path + documented limits, with safe defaults.

- [x] P1.4 Replace placeholder reply source in AI runner path.
Description: reply source was coupled to a placeholder in runner path and rewritten later by runtime.
Current location: `crates/fluxbee_ai_nodes/src/bin/ai_node_runner.rs`.
Target: remove placeholder dependency and make source assignment single-responsibility.
Resolution:
- `ai_node_runner` now builds replies with runtime-assigned source helper.
- Runtime remains the single owner of final `routing.src` assignment.

## P2 (cleanup / consistency)

- [ ] P2.1 Normalize defaults and config documentation across IO and AI.
Description: path/model defaults exist in code and docs; risk of drift.
Targets:
- `nodes/io/io-slack/src/main.rs`
- `crates/fluxbee_ai_nodes/src/bin/ai_node_runner.rs`
- `docs/AI_nodes_spec.md`
- `nodes/io/README.md`

- [ ] P2.2 Fix encoding artifact in Slack truncate helper.
Description: ellipsis appears as malformed character in source.
Current location: `nodes/io/io-slack/src/main.rs`.
Target: ASCII-safe output (e.g. `...`).

- [x] P2.3 Cleanup legacy imported shim crates.
Description: imported shim crates were removed from the repository.
Current location: n/a.
Target: completed.

## Open decisions (product/architecture)

- [ ] D1. Keep `meta.context` compatibility shim until protocol change is merged, or schedule protocol change now?
- [ ] D2. Should `io-slack` support fixed dst only in non-production mode, or remove after initial rollout?
- [ ] D3. Is durable outbox mandatory for v1, or acceptable as v1.1?
- [ ] D4. Should AI nodes be managed by systemd directly or by orchestrator runtime rollout first?

## Suggested execution order

1. P0.1
2. P0.2
3. P1.2
4. P1.3
5. P1.1
6. P2.x cleanup
