# IO Slack Workspace (Integrated with fluxbee_sdk)

This subtree hosts IO crates isolated from core routing/AI crates.

## Crates

- `crates/io-common`: shared IO helpers (dedup, identity lookup, io context).
- `crates/io-sim`: simulation IO node for local/Linux E2E tests (stdin/--once inbound, log-only outbound).
- `crates/io-slack`: Slack IO node (Socket Mode inbound, Web API outbound).

## Integration status

- `io-common` and `io-slack` use `fluxbee_sdk` directly.
- Runtime transport is the same node transport used by Fluxbee (`NodeConfig` + unix socket on linux).
- Local shim crates (`router-client`, `router-protocol`, `router-stub`, `node-test`) are no longer workspace members.

## Run (linux target expected in production)

Required env vars:

- `SLACK_APP_TOKEN`
- `SLACK_BOT_TOKEN`

Optional env vars:

- `NODE_NAME` (default `IO.slack.T123`)
- `NODE_VERSION` (default `0.1`)
- `ROUTER_SOCKET` (default `/var/run/fluxbee/routers`)
- `UUID_PERSISTENCE_DIR` (default `/var/lib/fluxbee/state/nodes`)
- `CONFIG_DIR` (default `/etc/fluxbee`)
- `ISLAND_ID` (used by identity shm path, default `local`)
- `IDENTITY_MODE` (`shm|mock|disabled`, default `mock`)
- `DEV_MODE` (`true|false`, default `false`)
- `DEDUP_TTL_MS` (default `600000`)
- `DEDUP_MAX_ENTRIES` (default `50000`)
- `IO_SLACK_DST_NODE` (optional fixed unicast destination, e.g. `AI.chat@sandbox`; if omitted uses `resolve`)
- `SLACK_SESSION_WINDOW_MS` (default `0` to disable)
- `SLACK_SESSION_MAX_SESSIONS` (default `10000`)
- `SLACK_SESSION_MAX_FRAGMENTS` (default `8`)

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
IDENTITY_MODE="mock" \
RUST_LOG=info,io_sim=debug,fluxbee_sdk=info \
cargo run --release -p io-sim
```

One-shot send:

```bash
SIM_DST_NODE="AI.chat@sandbox" \
ROUTER_SOCKET="/var/run/fluxbee/routers" \
CONFIG_DIR="/etc/fluxbee" \
UUID_PERSISTENCE_DIR="/var/lib/fluxbee/state/nodes" \
IDENTITY_MODE="mock" \
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
