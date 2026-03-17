# Node Runtime Install/Ops (AI + IO)

Status: working notes  
Last update: 2026-03-04

## 1) AI runtime install

Use:

```bash
bash scripts/install-ia.sh
```

This installs:
- `/usr/bin/ai-node-runner`
- `/usr/bin/ai-nodectl`
- `/etc/systemd/system/fluxbee-ai-node@.service`

Config location:
- `/etc/fluxbee/ai-nodes/<name>.yaml`
- Canonical YAML example:
  - `docs/onworking/ai_node_runner_config.example.yaml`

Mode per instance (`default|gov`):
- Unit now starts with:
  - `ExecStart=/usr/bin/ai-node-runner --mode ${AI_NODE_MODE} --config /etc/fluxbee/ai-nodes/%i.yaml`
- Default mode is:
  - `AI_NODE_MODE=default`
- Override per instance via:
  - `/etc/fluxbee/ai-nodes/<name>.env`

## 2) AI instance lifecycle

One install, many instances.

You do **not** rerun install for each node.  
You create one YAML per node and run one systemd instance per YAML.

Examples:

```bash
ai-nodectl add ai-chat /tmp/ai_chat.yaml
ai-nodectl add ai-sales /tmp/ai_sales.yaml

sudo systemctl enable --now fluxbee-ai-node@ai-chat
sudo systemctl enable --now fluxbee-ai-node@ai-sales
```

Useful commands:

```bash
ai-nodectl list
ai-nodectl status ai-chat
ai-nodectl logs ai-chat --follow
ai-nodectl restart ai-chat
ai-nodectl remove ai-sales --purge-config
```

### 2.0 Spawn flow A: precreated effective JSON (JSON-first)

Use this when you want a node to boot already `CONFIGURED`, without waiting for control-plane provisioning.

1. Prepare a JSON file with the effective config body (same shape as `payload.config` in `CONFIG_SET`).
2. Precreate the state file with `init-state`:

```bash
ai-nodectl init-state ai-chat /tmp/ai_chat_effective_config.json \
  --schema-version 1 \
  --config-version 1
```

3. Start or restart the instance:

```bash
sudo systemctl restart fluxbee-ai-node@ai-chat
sudo systemctl status fluxbee-ai-node@ai-chat
```

Notes for flow A:
- Input file must be a JSON object representing `payload.config` (effective config body).
- Output is wrapped as:
  - `schema_version`
  - `config_version`
  - `node_name`
  - `config`
  - `updated_at`
- If destination exists, command fails unless `--force` is passed.
- Default target path: `/var/lib/fluxbee/state/ai-nodes/<name>.json`.

Example overwrite:

```bash
ai-nodectl init-state ai-chat /tmp/ai_chat_effective_config.json --force
```

### 2.1 Spawn flow B: boot `UNCONFIGURED` then first `CONFIG_SET`

Use this when you want to provision from control-plane messages.

1. Start node instance without precreated state file:

```bash
sudo systemctl start fluxbee-ai-node@ai-chat
```

2. Node boots in `UNCONFIGURED` and accepts control-plane commands.
3. Send first valid `CONFIG_SET` (`apply_mode=replace`, `config_version>=1`).
4. Node persists state file under `/var/lib/fluxbee/state/ai-nodes/ai-chat.json` and transitions to `CONFIGURED`.

Notes for flow B:
- User messages are rejected with `node_not_configured` until first valid `CONFIG_SET`.
- `CONFIG_SET` remains the source of runtime updates after bootstrap.

### 2.2 Environment variables per AI instance (systemd)

When running with `systemd`, shell `export` variables are **not** inherited by the service.

For instance `fluxbee-ai-node@ai-chat`, define env vars in:
- `/etc/fluxbee/ai-nodes/ai-chat.env`

Example for OpenAI key:

```bash
sudo tee /etc/fluxbee/ai-nodes/ai-chat.env >/dev/null <<'EOF'
OPENAI_API_KEY=sk-REPLACE_ME
RUST_LOG=info,fluxbee_ai_nodes=debug,fluxbee_ai_sdk=debug
EOF
```

Example for `AI.frontdesk.gov` (gov mode + identity targets):

```bash
sudo tee /etc/fluxbee/ai-nodes/ai-frontdesk-gov.env >/dev/null <<'EOF'
AI_NODE_MODE=gov
OPENAI_API_KEY=sk-REPLACE_ME
GOV_IDENTITY_TARGET=SY.identity
GOV_IDENTITY_FALLBACK_TARGET=SY.identity@motherbee
GOV_IDENTITY_TIMEOUT_MS=10000
RUST_LOG=info,fluxbee_ai_nodes=debug,fluxbee_ai_sdk=debug
EOF
```

Example for a regular AI node (default mode):

```bash
sudo tee /etc/fluxbee/ai-nodes/ai-chat.env >/dev/null <<'EOF'
AI_NODE_MODE=default
OPENAI_API_KEY=sk-REPLACE_ME
RUST_LOG=info
EOF
```

What this command does:
- `tee /etc/fluxbee/ai-nodes/ai-chat.env`: writes stdin to that file.
- `sudo`: allows writing under `/etc/fluxbee/...`.
- `<<'EOF' ... EOF`: sends the block as stdin.
- `>/dev/null`: hides `tee` echo output (file is still written).

After changing env file:

```bash
sudo systemctl daemon-reload
sudo systemctl restart fluxbee-ai-node@ai-chat
sudo systemctl status fluxbee-ai-node@ai-chat
```

Important:
- `api_key_env` in YAML must match variable name in `.env`.
- If YAML uses default `api_key_env: OPENAI_API_KEY`, `.env` must define `OPENAI_API_KEY=...`.
- `AI_NODE_MODE=gov` is required for gov-only tools (for example `ilk_register`).

OpenAI key precedence in current MVP runner:
1. `CONFIG_SET.payload.config.behavior.openai.api_key` (hot override in memory, not persisted as plaintext)
2. YAML inline key (`behavior.openai.api_key` or `behavior.api_key` when configured)
3. env var pointed by `behavior.api_key_env` (default `OPENAI_API_KEY`)

### 2.2.1 NODE_STATUS_GET default handler (MVP)

AI nodes now answer orchestrator health probes (`NODE_STATUS_GET`) directly from the runtime.

Supported env vars (per instance):
- `NODE_STATUS_DEFAULT_HANDLER_ENABLED` (default: `true`)
- `NODE_STATUS_DEFAULT_HEALTH_STATE` (default: `HEALTHY`, accepted: `HEALTHY|DEGRADED|ERROR|UNKNOWN`)

Example for instance `ai-chat`:

```bash
sudo tee /etc/fluxbee/ai-nodes/ai-chat.env >/dev/null <<'EOF'
OPENAI_API_KEY=sk-REPLACE_ME
RUST_LOG=info,fluxbee_ai_nodes=debug,fluxbee_ai_sdk=debug
NODE_STATUS_DEFAULT_HANDLER_ENABLED=true
NODE_STATUS_DEFAULT_HEALTH_STATE=HEALTHY
EOF

sudo systemctl daemon-reload
sudo systemctl restart fluxbee-ai-node@ai-chat
```

Validation (from admin API):

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE="AI.chat@$HIVE_ID"

curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE/status" | jq '.payload.node_status.health_source'
```

Expected:
- `NODE_REPORTED`

Quick override test:

```bash
sudo sed -i 's/^NODE_STATUS_DEFAULT_HEALTH_STATE=.*/NODE_STATUS_DEFAULT_HEALTH_STATE=DEGRADED/' /etc/fluxbee/ai-nodes/ai-chat.env
sudo systemctl restart fluxbee-ai-node@ai-chat
curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE/status" | jq '.payload.node_status.health_state'
```

Expected:
- `DEGRADED`

### 2.3 Persistence model (UUID vs dynamic config)

There are two different persistence concerns:

- Node identity persistence (already existed):
  - path from YAML: `node.uuid_persistence_dir` (default `/var/lib/fluxbee/state/nodes`)
  - purpose: keep stable node UUID/identity across restarts.

- AI dynamic config persistence (new control-plane path):
  - path from YAML: `node.dynamic_config_dir` (default `/var/lib/fluxbee/state/ai-nodes`)
  - file: `<dynamic_config_dir>/<node_name>.json`
  - purpose: persist `CONFIG_SET` state (`schema_version`, `config_version`, redacted config snapshot, `updated_at`).

Operational note:
- Effective JSON state is the runtime source of truth when present.
- YAML remains as optional/operator-managed compatibility input.
- If no effective JSON exists, node boots `UNCONFIGURED` and can be provisioned by first valid `CONFIG_SET`.

## 3) IO runtime install

Use:

```bash
bash scripts/install-io.sh
```

This installs:
- `/usr/bin/io-slack`
- `/usr/bin/io-sim`
- `/etc/systemd/system/fluxbee-io-slack.service`
- `/etc/systemd/system/fluxbee-io-sim.service`
- env templates:
  - `/etc/fluxbee/io-slack.env`
  - `/etc/fluxbee/io-sim.env`

## 4) IO lifecycle

Start/enable:

```bash
sudo systemctl enable --now fluxbee-io-slack
sudo systemctl enable --now fluxbee-io-sim
```

Status/logs:

```bash
systemctl status fluxbee-io-slack
journalctl -u fluxbee-io-slack -f
```

## 5) Notes

- Core installer `scripts/install.sh` remains unchanged.
- These scripts are additive and focused on AI/IO runtime deployment.
- `io-slack` fixed destination test mode:
  - set `IO_SLACK_DST_NODE=AI.chat@<hive_id>` in `/etc/fluxbee/io-slack.env` when needed.

## 6) Update / reinstall behavior

`install-ia.sh` and `install-io.sh` are idempotent for runtime assets:
- binaries in `/usr/bin` are overwritten with the new build
- systemd unit files are overwritten
- `systemctl daemon-reload` is executed

What they do not change automatically:
- existing node YAML configs (`/etc/fluxbee/ai-nodes/*.yaml`)
- existing env files for IO if already present (`/etc/fluxbee/io-slack.env`, `/etc/fluxbee/io-sim.env`)
- running service instances are not force-restarted by the scripts

After reinstall/update, restart affected services manually:

```bash
sudo systemctl restart fluxbee-ai-node@ai-chat
sudo systemctl restart fluxbee-io-slack
sudo systemctl restart fluxbee-io-sim
```

Useful flags:
- `SKIP_BUILD=1` to reinstall from an existing build output
- `BIN_DIR=/path/to/bin` to install from a custom binary directory
