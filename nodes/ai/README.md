# AI nodes

The sources of the AI runtime.

- `ai-generic/`: the `ai.generic` runtime (package `fluxbee-ai-nodes`, binary `ai_node_runner`).
  See [`ai-generic/README.md`](ai-generic/README.md).

## Rules

- **`ai.generic` is the only AI runtime.** Every `AI.*` node (`AI.sales@motherbee`,
  `AI.support@worker1`) is an instance of it, created with `run_node` when it is needed.
- **No instance lives in the repo or starts with the base install.** The `.deb` bakes the runtime
  (`packaging/base-nodes.json`, `"boot": false`), and each node is launched for a tenant.
- **An `AI.*` node runs in a tenant, never in the root tenant.** The orchestrator refuses that with
  `TENANT_ROOT_NOT_ALLOWED`.
- **`SY.frontdesk.gov` is not a runtime.** It is a core system node (`nodes/gov/ai-frontdesk-gov`),
  updated with the `.deb`.
- **`nodes/ai/*` does not depend on `nodes/gov/*`.** `ai-generic` depends only on the two SDKs.

## What makes an AI node

Three separate parts, each set in its own place:

| What | Where it lives | How it is set |
| --- | --- | --- |
| The model provider and its key | the hive, and SY.vault | `ai:` in `/etc/fluxbee/hive.yaml` picks the provider (OpenAI when the section is absent). The node reads the key from SY.vault by `resource_type` (`openai` or `anthropic`) on every request: a key it owns, else one assigned to it, else its tenant's general key |
| Its behavior | the node's `config.json` | `CONFIG_SET` with `config.behavior`: `kind` (`echo` or `ai_chat`), `model`, `instructions`, `model_settings`, `base_url`, `capabilities.multimodal` |
| The agent: role, skills, handbooks, personality | the node's ILK | hashes set with `set_ilk_definition`, each naming a JSON asset in `blob://agent-assets/`. Without them the agent gives only a fixed "unconfigured" answer |

A key is never config. `CONFIG_SET` refuses `config.secrets.*`, `config.behavior.api_key`,
`config.behavior.api_key_env`, `config.behavior.openai` and `config.behavior.vault_key`, and an AI
node keeps no `secrets.json`.

Step by step: [docs/ai-nodes-deploy-runbook.md](../../docs/ai-nodes-deploy-runbook.md).

## References

- [docs/ai-nodes-deploy-runbook.md](../../docs/ai-nodes-deploy-runbook.md): creating, configuring and
  updating a node.
- [docs/identity-v2.1-agent-definition-addendum.md](../../docs/identity-v2.1-agent-definition-addendum.md):
  the agent assets and their schema.
- [docs/ilk-tenant-model-v1.md](../../docs/ilk-tenant-model-v1.md): tenants and which key a node
  uses.
- [docs/node-config-control-plane-spec.md](../../docs/node-config-control-plane-spec.md):
  `CONFIG_GET` and `CONFIG_SET`.
- [crates/fluxbee_ai_sdk/README.md](../../crates/fluxbee_ai_sdk/README.md): the AI SDK.
