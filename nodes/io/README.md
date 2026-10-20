# IO Slack Workspace (Integrated with fluxbee_sdk)

This subtree hosts IO crates isolated from core routing/AI crates.

## Crates

- `crates/io-common`: shared IO helpers (dedup, identity lookup, io context, inbound relay/sessionization).
- `crates/io-api`: instanced Edge ingress node. It owns one ICH, externalizes through
  `SY.admin`, receives `io.api.inbound.v1` over the router socket and has no HTTP listener.
- `crates/io-sim`: simulation IO node for local/Linux E2E tests (stdin/--once inbound, log-only outbound).
- `crates/io-slack`: Slack IO node (Socket Mode inbound, Web API outbound).

## Integration status

- `io-common` and `io-slack` use `fluxbee_sdk` directly.
- Runtime transport is the same node transport used by Fluxbee (`NodeConfig` + unix socket on linux).
- Local shim crate paths were removed as part of IO cleanup.

## Install for Linux runtime

The package seeds the IO runtimes of `packaging/base-nodes.json` (`io.api` and `io.slack` among
them) into `/var/lib/fluxbee/dist/runtimes`. On a development host, build the package from the
repo root and install it (`VERSION` defaults to the installed one):

```bash
bash scripts/install.sh [VERSION]
```

IO node processes are not installed as fixed systemd services. They are spawned by
the orchestrator as managed runtime instances, with units named
`fluxbee-node-<node-name>.service`.

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

Then everything goes through SY.admin: `POST /hives/{hive}/nodes` launches a node, with its
tenant and config; `POST /hives/{hive}/nodes/{node}/restart` moves a running one to the new
`current`; `CONFIG_SET` changes its config; secrets go to SY.vault. Deploy runbooks:
- `docs/io/io-slack-deploy-runbook.md`
- `docs/io/io-api-node-spec.md` (§10)

## Run (linux target expected in production)

### IO.api

Managed runtime note:

- `io-api` requires `FLUXBEE_NODE_NAME`, `FLUXBEE_NODE_ILK_ID` and
  `FLUXBEE_NODE_TENANT_ID` from Orchestrator.
- It reads managed `config.json` under `/var/lib/fluxbee/nodes/IO/<node_name>/config.json`.
- It can boot unconfigured and receive the canonical business config through `CONFIG_SET`.
- `SY.admin externalize` mints and stores the Edge bearer. It is never accepted in node config or
  persisted by IO.api; a successful `CONFIG_SET` response carries a newly issued token once
  (`runtime.publication.entry_token`).
- Public requests terminate at `SY.edge`; Edge validates the bearer and forwards one message over
  the internal router socket. Requests cannot choose their tenant, ICH or destination.
- Publication state and URL are available through Admin's node `control/config-get` response under
  `runtime.publication`.

Useful helper:

- `scripts/publish-io-api-runtime.sh`

### IO.slack

Managed runtime note:

- `io-slack` runs only as a managed instance: it requires `FLUXBEE_NODE_NAME`,
  `FLUXBEE_NODE_ILK_ID` and `FLUXBEE_NODE_TENANT_ID` from Orchestrator and reads its managed
  `config.json` under `/var/lib/fluxbee/nodes/IO/<node_name>/config.json`.
- The Slack tokens are never in config or env: the config names a SY.vault key
  (`nodes/io/common/src/io_slack_adapter_config.rs`).
  - Required: `slack.auth` = `{"type":"vault_ref","resource_type":"slack","key":"<vault key>"}`
    (any other `type` is refused), `io.workspace_id` and `io.conversation_id` (the Slack binding
    behind the node's own ICH).
  - Optional: `io.dst_node` (without it the router resolves the destination), `io.relay.*`,
    `identity.target`, `identity.timeout_ms`, `io.blob.*`, `node.*`, `runtime.*`.
- The vault secret behind that key is `{"app_token":"xapp-...","bot_token":"xoxb-..."}` with
  `metadata.resource_type: slack`. The node reads it at start, on a `VAULT_SECRET_CHANGED` and
  every 60 s, and runs degraded until it has it.
- Launch, secret and config: `docs/io/io-slack-deploy-runbook.md` §3 (steps 4 and 5) and §5.

Optional env vars (they override the managed config):

- `NODE_VERSION`, `ROUTER_SOCKET` (default `/var/run/fluxbee/routers`), `UUID_PERSISTENCE_DIR`
  (default `/var/lib/fluxbee/state/nodes`), `CONFIG_DIR` (default `/etc/fluxbee`)
- `DEV_MODE` (`true|false`, default `false`), `DEDUP_TTL_MS` (default `600000`),
  `DEDUP_MAX_ENTRIES` (default `50000`)
- `IO_SLACK_WORKSPACE_ID`, `IO_SLACK_CONVERSATION_ID`, `IDENTITY_TARGET`, `IDENTITY_TIMEOUT_MS`
- `IO_SLACK_DST_NODE` (fixed unicast destination, e.g. `AI.chat@sandbox`; for test/recovery only,
  not the production baseline)

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

`IO.api` logging note:

- `publish-io-api-runtime.sh` publishes `start.sh` with default `RUST_LOG=info,io_api=debug,io_common=debug,fluxbee_sdk=info`

Identity behavior is provision-on-miss by default in current IO nodes (no mode flag required).

### Declaring `ilk_type` at provision time

When an IO node provisions a new ILK (lookup miss → `ILK_PROVISION`), it can declare whether the external counterpart is a human user or an automated agent. This is set on `IdentityLookupInput.ilk_type` (alias `ResolveOrCreateInput`) at the call site:

- `Some("human")` — the channel represents a human user (Slack user, email sender, web visitor)
- `Some("agent")` — the channel represents an automated external agent (LinkedHelper bot, partner-system service account, scripted client behind an API token)
- `None` — leave the decision to the server, which defaults to `"human"`

`SY.identity` rejects `"system"` from IO-originated provisioning; that label is reserved for SY-internal creation paths.

The decision is **constructive of the IO node itself**: only the IO runtime knows the nature of its counterpart (by config, subdomain, header, token type, etc.). When in doubt, omit the field and let the server default to human; switch to `"agent"` only when the IO has a deterministic signal that the other side is a bot. The provisional ILK's type is overwritten when `register_ilk` later promotes it to `complete`, so a wrong default is recoverable but produces incorrect short-lived behavior in policy/routing decisions taken during the provisional window.

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
