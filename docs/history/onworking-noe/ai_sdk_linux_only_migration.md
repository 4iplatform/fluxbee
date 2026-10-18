# AI SDK Linux-Only Migration (remove Windows stub)

Status: guide  
Last update: 2026-03-03

## Goal

Run and test `fluxbee_sdk` / `fluxbee_ai_sdk` only on Linux, removing the `not(unix)` compatibility stub.

## Current state

- `fluxbee_sdk` currently exposes:
  - Unix implementation: `crates/fluxbee_sdk/src/node_client.rs`
  - Non-Unix stub: `crates/fluxbee_sdk/src/node_client_stub.rs`
- `crates/fluxbee_sdk/src/lib.rs` uses `cfg(unix)` / `cfg(not(unix))` to select one.

## Steps to migrate to 100% Linux

1. Remove stub module file:
- Delete `crates/fluxbee_sdk/src/node_client_stub.rs`

2. Simplify module export in SDK:
- Edit `crates/fluxbee_sdk/src/lib.rs`
- Replace conditional module selection with:
```rust
pub mod node_client;
```

3. (Optional but recommended) fail early on non-Linux targets:
- Add compile guard in `crates/fluxbee_sdk/src/lib.rs`:
```rust
#[cfg(not(target_os = "linux"))]
compile_error!("fluxbee_sdk is Linux-only. Build on Linux target.");
```

4. Make tests Linux-only in CI:
- Ensure CI matrix runs `cargo test` for these crates on Linux runners only.
- If Windows jobs remain in the repo, skip:
  - `cargo test -p fluxbee-sdk`
  - `cargo test -p fluxbee-ai-sdk`

5. Standard local commands (Linux):
```bash
cargo test -p fluxbee-sdk
cargo test -p fluxbee-ai-sdk
cargo test -p fluxbee-ai-sdk --test contracts
```

## What changes after migration

- Windows builds for these crates will fail by design.
- No dead-code warnings caused by stub-only paths.
- Behavior is fully aligned with production environment assumptions (Unix sockets).

## Rollback

If cross-platform compilation is needed again:
- Reintroduce `node_client_stub.rs`
- Restore `cfg(unix)` / `cfg(not(unix))` in `crates/fluxbee_sdk/src/lib.rs`
