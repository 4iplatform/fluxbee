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

`fluxbee_sdk::protocol::Meta` currently exposes `context` (object) but not dedicated `src_ilk/dst_ilk` fields.

For now, io-common stores identity hints in `meta.context`:

- `meta.context.src_ilk`
- `meta.context.dst_ilk`

When Fluxbee protocol metadata includes native `src_ilk/dst_ilk` fields, io-common should move these values to native fields.
