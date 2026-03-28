# Node Secret Migration Inventory

Status: working draft  
Scope: migration away from `hive.yaml` for node secrets

## 1. Current code inventory

### 1.1 Already migrated

- `SY.architect`
  - local OpenAI key now comes only from local `secrets.json`
  - local bootstrap via:
    - `GET /architect/control/config-get`
    - `POST /architect/control/config-set`

- `AI.common`
  - runtime implemented in:
    - `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`
  - canonical secret field advertised by `CONFIG_GET`:
    - `config.secrets.openai.api_key`
  - persisted locally in:
    - `/var/lib/fluxbee/nodes/AI/<node@hive>/secrets.json`
  - compatibility retained temporarily for:
    - `config.behavior.openai.api_key`
    - `config.behavior.api_key`
    - env var via `api_key_env`

### 1.2 Still pending migration

- `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs`
  - still carries the pre-migration AI runner behavior
  - still resolves OpenAI key from inline config/env legacy paths
  - needs re-sync with `AI.common` or the same secret migration applied explicitly

### 1.3 Not using `hive.yaml` directly for secrets, but still legacy in other ways

- `nodes/io/io-slack/src/main.rs`
  - does not read secrets from `hive.yaml`
  - still accepts tokens from:
    - env vars (`SLACK_APP_TOKEN`, `SLACK_BOT_TOKEN`)
    - inline config fields (`slack.app_token`, `slack.bot_token`)
    - env indirection refs (`slack.app_token_ref`, `slack.bot_token_ref`)
  - this is a separate migration path from the AI/OpenAI case

## 2. Docs/examples still suggesting old secret patterns

The main stale surfaces found in this pass:

- `docs/AI_nodes_spec.md`
  - still describes YAML inline secret as primary/current path in several sections
- `docs/ai-nodes-examples-annex.md`
  - still shows inline `openai.api_key` examples
- `docs/onworking NOE/node_runtime_install_ops.md`
  - still describes YAML/env precedence for AI credentials
- `docs/examples/ai_common_chat.config.json`
  - still includes inline `api_key`
- `nodes/gov/ai-frontdesk-gov/src/bin/ai_local_probe.rs`
  - still assumes env-only local probe flow

These are documentation/example migrations, not blockers for the runtime already updated in `AI.common`.

## 3. Recommended transition strategy

### 3.1 Target behavior

- secrets do not live in `hive.yaml`
- operator enters them through `archi`
- `archi` uses `CONFIG_GET` / `CONFIG_SET`
- node persists secret locally in `secrets.json`
- `CONFIG_GET` reports contract + configured state, never plaintext

### 3.2 Safe migration order

1. Keep local secret file as the preferred source.
2. Keep temporary fallback to legacy inline/env only while operators migrate running nodes.
3. On bootstrap, if legacy inline AI secret is present and no local secret exists:
   - migrate it to `secrets.json`
   - strip it from the effective config snapshot
4. Once operational migration is complete for a node family:
   - remove inline/YAML secret examples from docs
   - remove fallback code paths only after explicit rollout decision

### 3.3 Immediate next target

- re-sync `nodes/gov/ai-frontdesk-gov` with the migrated `AI.common` secret behavior

This is necessary because `ai-frontdesk-gov` is currently a duplicated runner tree and would otherwise drift semantically.

## 4. Operational migration notes

For `AI.common` instances:

1. Run `CONFIG_GET`
2. Confirm `contract.secrets[*]` and `configured=false/true`
3. Send `CONFIG_SET` with `config.secrets.openai.api_key`
4. Re-run `CONFIG_GET`
5. Confirm:
   - `configured = true`
   - `api_key_source = local_file`
   - returned config is redacted
6. Restart the node/service once and confirm the same result persists

## 5. Conclusion

The migration status today is:

- `SY.architect`: migrated
- `AI.common`: migrated with temporary compatibility
- `AI.frontdesk.gov`: pending re-sync
- `IO.slack`: separate secret migration still pending
- docs/examples: partial cleanup still pending
