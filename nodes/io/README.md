# IO Slack Workspace (Integrated with fluxbee_sdk)

This subtree hosts IO crates isolated from core routing/AI crates.

## Crates

- `crates/io-common`: shared IO helpers (dedup, identity lookup, io context, inbound relay/sessionization).
- `crates/io-sim`: simulation IO node for local/Linux E2E tests (stdin/--once inbound, log-only outbound).
- `crates/io-slack`: Slack IO node (Socket Mode inbound, Web API outbound).

## Integration status

- `io-common` and `io-slack` use `fluxbee_sdk` directly.
- Runtime transport is the same node transport used by Fluxbee (`NodeConfig` + unix socket on linux).
- Local shim crate paths were removed as part of IO cleanup.

## Install for Linux runtime

From repo root:

```bash
bash scripts/install-io.sh
```

This installs `/usr/bin/io-slack`, `/usr/bin/io-sim` and systemd units:
- `fluxbee-io-slack.service`
- `fluxbee-io-sim.service`

## Publish runtime (orchestrator v2 canonical flow)

For rollout via `SYSTEM_UPDATE` + `SPAWN_NODE`, publish IO runtimes into
`/var/lib/fluxbee/dist/runtimes`.

Publish `IO.slack`:

```bash
bash scripts/publish-io-runtime.sh --kind slack --version 0.1.0 --set-current --sudo
```

Publish `IO.sim`:

```bash
bash scripts/publish-io-runtime.sh --kind sim --version 0.1.0 --set-current --sudo
```

Generic publish helper:

```bash
bash scripts/publish-runtime.sh --runtime IO.slack --version 0.1.0 --binary nodes/io/target/release/io-slack --set-current --sudo
```

Expected output includes:
- `manifest_version=<...>`
- `manifest_hash=<...>`

Use those values with:
- `POST /hives/{id}/update` (`category=runtime`)

Deploy runbook:
- `docs/io/io-slack-deploy-runbook.md`

## Run (linux target expected in production)

Required env vars:

- `SLACK_APP_TOKEN`
- `SLACK_BOT_TOKEN`

Spawn/orchestrator mode note:
- `io-slack` can also read tokens/config from spawn `config.json` under
  `/var/lib/fluxbee/nodes/IO/<node_name>/config.json`
  (or `NODE_CONFIG_PATH` override).
- Supported token fields in config:
  - `slack.app_token` / `slack.bot_token`
  - `slack.app_token_ref` / `slack.bot_token_ref` with `env:VAR` indirection
- Precedence: explicit env vars override spawn config values.

Optional env vars:

- `NODE_NAME` (default `IO.slack.T123`)
- `NODE_VERSION` (default `0.1`)
- `ROUTER_SOCKET` (default `/var/run/fluxbee/routers`)
- `UUID_PERSISTENCE_DIR` (default `/var/lib/fluxbee/state/nodes`)
- `CONFIG_DIR` (default `/etc/fluxbee`)
- `ISLAND_ID` (used by identity shm path, default `local`)
- `DEV_MODE` (`true|false`, default `false`)
- `DEDUP_TTL_MS` (default `600000`)
- `DEDUP_MAX_ENTRIES` (default `50000`)
- `IO_SLACK_DST_NODE` (optional fixed unicast destination, e.g. `AI.chat@sandbox`; if omitted uses `resolve`; keep fixed dst for test/recovery only, not production baseline)

Relay note:

- relay config now belongs to formal node config under `config.io.relay.*`
- relevant keys:
  - `config.io.relay.window_ms`
  - `config.io.relay.max_open_sessions`
  - `config.io.relay.max_fragments_per_session`
  - `config.io.relay.max_bytes_per_session`
- `config.io.relay.window_ms=0` means relay passthrough
- `config.io.relay.window_ms>0` means short-window consolidation before the router
- `IO.slack` no longer owns a private `SlackSessionizer`
- `SLACK_SESSION_*` remains only as temporary compatibility fallback for unmanaged/local env boot, not as canonical runtime surface for spawned nodes

Logging note:

- canonical runtime publish defaults include `io_common=debug`, so relay event logs should show in `journalctl`
- local installs via `install-io.sh` only show those debug logs if `/etc/fluxbee/io-slack.env` defines `RUST_LOG`

Identity behavior is provision-on-miss by default in current IO nodes (no mode flag required).

Example:

```bash
SLACK_APP_TOKEN="xapp-..." \
SLACK_BOT_TOKEN="xoxb-..." \
NODE_NAME="IO.slack.T123" \
ROUTER_SOCKET="/var/run/fluxbee/routers" \
cargo run -p io-slack
```

## io-sim quick test

`io-sim` lets you validate `IO -> Router -> AI -> IO` without Slack integration.

Recommended (interactive, keeps process alive and logs replies):

```bash
SIM_DST_NODE="AI.chat@sandbox" \
ROUTER_SOCKET="/var/run/fluxbee/routers" \
CONFIG_DIR="/etc/fluxbee" \
UUID_PERSISTENCE_DIR="/var/lib/fluxbee/state/nodes" \
RUST_LOG=info,io_sim=debug,fluxbee_sdk=info \
cargo run --release -p io-sim
```

One-shot send:

```bash
SIM_DST_NODE="AI.chat@sandbox" \
ROUTER_SOCKET="/var/run/fluxbee/routers" \
CONFIG_DIR="/etc/fluxbee" \
UUID_PERSISTENCE_DIR="/var/lib/fluxbee/state/nodes" \
RUST_LOG=info,io_sim=debug,fluxbee_sdk=info \
cargo run -p io-sim -- --once "hola router"
```

Optional env vars for `io-sim`:

- `SIM_DST_NODE` (optional unicast destination; when omitted uses `resolve`)
- `SIM_CHANNEL` (default `sim`)
- `SIM_SENDER_ID` (default `user.local`)
- `SIM_CONVERSATION_ID` (default `sim-console`)
- `SIM_THREAD_ID` (optional)
- `SIM_TENANT_HINT` (optional)
- `SIM_REPLY_KIND` (default `sim_log`; use `slack_post` to test `IO.slack` outbound)
- `SIM_REPLY_ADDRESS` (default `stdout`; for Slack use target `channel_id`)
- `SIM_REPLY_THREAD_TS` (optional; for Slack thread replies)
- `SIM_REPLY_WORKSPACE_ID` (optional; for Slack context metadata)
- `SIM_ATTACHMENTS_JSON` (optional JSON array de `BlobRef` para simular adjuntos outbound)
- `SIM_CONTENT_REF_JSON` (optional JSON object `BlobRef` para simular texto en `content_ref`)

Note:
- `SIM_DST_NODE` must match the AI registered name (`AI.chat@<hive_id>`).
- `<hive_id>` comes from `/etc/fluxbee/hive.yaml`.
