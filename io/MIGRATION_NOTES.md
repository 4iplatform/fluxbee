# Migration Notes

This subtree was imported from `D:\repos\slack-node` and then migrated to Fluxbee SDK transport/protocol.

## Current state

- `io-common` and `io-slack` are integrated with `fluxbee_sdk`.
- Workspace members are now only:
  - `crates/io-common`
  - `crates/io-slack`
- `cargo check` and `cargo test -p io-common` pass in this workspace.

## About shim crates

The following imported crates still exist on disk under `io/legacy/crates` but are no longer used by the workspace:

- `io/legacy/crates/router-client`
- `io/legacy/crates/router-protocol`
- `io/legacy/crates/router-stub`
- `io/legacy/crates/node-test`

They can be removed safely once you want a cleanup-only commit.

## Protocol note

`fluxbee_sdk::protocol::Meta` currently exposes top-level `src_ilk` and `context` (object).

Current IO contract:

- source identity uses canonical `meta.src_ilk`
- channel metadata lives in `meta.context.io.*`
- non-typed L3 fields (for example `dst_ilk`) may still be carried in `meta.context` until SDK/core alignment is complete
