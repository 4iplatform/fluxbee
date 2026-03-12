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

### 2.1 Environment variables per AI instance (systemd)

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

OpenAI key precedence in current MVP runner:
1. `CONFIG_SET.payload.config.behavior.openai.api_key` (hot override in memory, not persisted as plaintext)
2. YAML inline key (`behavior.openai.api_key` or `behavior.api_key` when configured)
3. env var pointed by `behavior.api_key_env` (default `OPENAI_API_KEY`)

### 2.2 Persistence model (UUID vs dynamic config)

There are two different persistence concerns:

- Node identity persistence (already existed):
  - path from YAML: `node.uuid_persistence_dir` (default `/var/lib/fluxbee/state/nodes`)
  - purpose: keep stable node UUID/identity across restarts.

- AI dynamic config persistence (new control-plane path):
  - path from YAML: `node.dynamic_config_dir` (default `/var/lib/fluxbee/state/ai-nodes`)
  - file: `<dynamic_config_dir>/<node_name>.json`
  - purpose: persist `CONFIG_SET` state (`schema_version`, `config_version`, redacted config snapshot, `updated_at`).

Operational note:
- YAML remains current startup source in this stage.
- Dynamic config persistence is used for control-plane continuity and version tracking, and is prepared for later full `UNCONFIGURED -> CONFIG_SET` bootstrap flow.

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
