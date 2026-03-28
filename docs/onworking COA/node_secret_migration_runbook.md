# Node Secret Migration Runbook

Status: working draft  
Scope: operator/dev procedure for moving node secrets out of `hive.yaml`

## 1. Goal

Move node secrets to node-local `secrets.json` using the node control plane:

- discover requirements with `CONFIG_GET`
- apply secrets with `CONFIG_SET`
- verify the node is reading from local file storage

This runbook is intentionally focused on the first migrated case:

- `AI.common`

## 2. Preconditions

- `SY.admin` is reachable
- the target node instance already exists
- the node supports `CONFIG_GET` / `CONFIG_SET`
- the secret value is available to the operator

## 3. AI.common procedure

Example target:

- node: `AI.chat@motherbee`
- hive: `motherbee`

### 3.1 Discover contract and current state

SCMD:

```text
SCMD: curl -X POST /hives/motherbee/nodes/AI.chat@motherbee/control/config-get -d '{"requested_by":"archi"}'
```

Direct curl:

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes/AI.chat@motherbee/control/config-get" \
  -H 'Content-Type: application/json' \
  -d '{"requested_by":"archi"}'
```

Expected checks:

- `payload.response.contract.secrets[*].field = "config.secrets.openai.api_key"`
- `payload.response.contract.secrets[*].required = true`
- `payload.response.contract.secrets[*].configured = false|true`
- `payload.response.api_key_source = "missing" | "local_file" | legacy fallback`

### 3.2 Apply the secret

SCMD:

```text
SCMD: curl -X POST /hives/motherbee/nodes/AI.chat@motherbee/control/config-set -d '{"requested_by":"archi","schema_version":1,"config_version":7,"apply_mode":"replace","config":{"behavior":{"kind":"openai_chat","model":"gpt-4.1-mini"},"secrets":{"openai":{"api_key":"sk-REPLACE_ME","api_key_env":"OPENAI_API_KEY"}}}}'
```

Direct curl:

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes/AI.chat@motherbee/control/config-set" \
  -H 'Content-Type: application/json' \
  -d '{"requested_by":"archi","schema_version":1,"config_version":7,"apply_mode":"replace","config":{"behavior":{"kind":"openai_chat","model":"gpt-4.1-mini"},"secrets":{"openai":{"api_key":"sk-REPLACE_ME","api_key_env":"OPENAI_API_KEY"}}}}'
```

Expected checks:

- `status = ok`
- `payload.node_ok = true`
- `payload.response.ok = true`
- `payload.response.effective_config` does not expose the secret in plaintext

### 3.3 Verify post-apply state

Run `CONFIG_GET` again.

Expected checks:

- `payload.response.contract.secrets[*].configured = true`
- `payload.response.api_key_source = "local_file"`
- returned config remains redacted

### 3.4 Restart verification

Restart the node service once.

After restart:

1. run `CONFIG_GET` again
2. confirm `api_key_source = "local_file"`
3. confirm `configured = true`
4. confirm no inline key reappears in returned config

## 4. Migration notes

- During migration, `AI.common` still accepts legacy aliases:
  - `config.behavior.openai.api_key`
  - `config.behavior.api_key`
- The canonical field is still:
  - `config.secrets.openai.api_key`
- On bootstrap, if legacy inline config is present and no local secret exists yet, the runner migrates the secret to `secrets.json`

## 5. Done criteria

A node instance is considered migrated when:

- `CONFIG_GET` advertises the secret in `contract.secrets[*]`
- `CONFIG_SET` succeeds with the canonical field
- `CONFIG_GET` reports `api_key_source = "local_file"`
- the node still works after one restart
- plaintext secret no longer appears in returned config payloads
