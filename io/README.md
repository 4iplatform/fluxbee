# IO Slack Workspace (Integrated with fluxbee_sdk)

This subtree hosts IO crates isolated from core routing/AI crates.

## Crates

- `crates/io-common`: shared IO helpers (dedup, identity lookup, io context).
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
bash scripts/publish-runtime.sh --runtime IO.slack --version 0.1.0 --binary io/target/release/io-slack --set-current --sudo
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
- `SLACK_SESSION_WINDOW_MS` (default `0` to disable)
- `SLACK_SESSION_MAX_SESSIONS` (default `10000`)
- `SLACK_SESSION_MAX_FRAGMENTS` (default `8`)

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

Note:
- `SIM_DST_NODE` must match the AI registered name (`AI.chat@<hive_id>`).
- `<hive_id>` comes from `/etc/fluxbee/hive.yaml`.
