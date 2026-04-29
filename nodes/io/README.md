# IO Slack Workspace (Integrated with fluxbee_sdk)

This subtree hosts IO crates isolated from core routing/AI crates.

## Crates

- `crates/io-common`: shared IO helpers (dedup, identity lookup, io context, inbound relay/sessionization).
- `crates/io-api`: generic HTTP ingress IO node (`GET /`, `POST /`, auth bearer, relay, attachments/blob; legacy aliases: `GET /schema`, `POST /messages`).
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

`IO.api` has its own local install helper:

```bash
bash scripts/install-io-api.sh --node-name IO.api.local@motherbee
```

This installs:
- `/usr/bin/io-api`
- `/etc/fluxbee/io-api.env`
- managed bootstrap config under `/var/lib/fluxbee/nodes/IO/<node_name>/config.json`
- `fluxbee-io-api.service`

## Publish runtime (orchestrator v2 canonical flow)

For rollout via `SYSTEM_UPDATE` + `SPAWN_NODE`, publish IO runtimes into
`/var/lib/fluxbee/dist/runtimes`.

Publish `io.slack`:

```bash
bash scripts/publish-io-runtime.sh --kind slack --version 0.1.0 --set-current --sudo
```

Publish `IO.sim`:

```bash
bash scripts/publish-io-runtime.sh --kind sim --version 0.1.0 --set-current --sudo
```

Publish `io.api`:

```bash
bash scripts/publish-io-api-runtime.sh --version 0.1.0 --set-current --sudo
```

Generic publish helper:

```bash
bash scripts/publish-runtime.sh --runtime io.slack --version 0.1.0 --binary nodes/io/target/release/io-slack --set-current --sudo
```

Expected output includes:
- `manifest_version=<...>`
- `manifest_hash=<...>`

Use those values with:
- `POST /hives/{id}/update` (`category=runtime`)

Deploy runbook:
- `docs/io/io-slack-deploy-runbook.md`
- `docs/onworking NOE/io-api-runtime-validation-runbook.md`

Deploy helper for `IO.api`:

```bash
bash scripts/deploy-io-api.sh --base http://127.0.0.1:8080 --hive-id motherbee --version 0.1.0 --node-name IO.api.frontdesk@motherbee --update-existing --sync-hint --sudo
```

## Run (linux target expected in production)

### IO.api

Managed runtime note:

- `io-api` expects `FLUXBEE_NODE_NAME` and managed `config.json` under `/var/lib/fluxbee/nodes/IO/<node_name>/config.json`
- the process can boot with bootstrap config that is empty or minimal and remain non-configured until `CONFIG_SET`
- canonical runtime/business config stays in `CONFIG_GET` / `CONFIG_SET`

Useful helpers:

- `scripts/publish-io-api-runtime.sh`
- `scripts/deploy-io-api.sh`
- `scripts/install-io-api.sh`

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

`IO.api` logging note:

- `publish-io-api-runtime.sh` publishes `start.sh` with default `RUST_LOG=info,io_api=debug,io_common=debug,fluxbee_sdk=info`
- `install-io-api.sh` writes the same default into `/etc/fluxbee/io-api.env`

Identity behavior is provision-on-miss by default in current IO nodes (no mode flag required).

### Declaring `ilk_type` at provision time

When an IO node provisions a new ILK (lookup miss → `ILK_PROVISION`), it can declare whether the external counterpart is a human user or an automated agent. This is set on `IdentityLookupInput.ilk_type` (alias `ResolveOrCreateInput`) at the call site:

- `Some("human")` — the channel represents a human user (Slack user, email sender, web visitor)
- `Some("agent")` — the channel represents an automated external agent (LinkedHelper bot, partner-system service account, scripted client behind an API token)
- `None` — leave the decision to the server, which defaults to `"human"`

`SY.identity` rejects `"system"` from IO-originated provisioning; that label is reserved for SY-internal creation paths.

The decision is **constructive of the IO node itself**: only the IO runtime knows the nature of its counterpart (by config, subdomain, header, token type, etc.). When in doubt, omit the field and let the server default to human; switch to `"agent"` only when the IO has a deterministic signal that the other side is a bot. The provisional ILK's type is overwritten when `register_ilk` later promotes it to `complete`, so a wrong default is recoverable but produces incorrect short-lived behavior in policy/routing decisions taken during the provisional window.

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
