# ai.generic

The runner every `AI.*` node runs.

- **Names:** package `fluxbee-ai-nodes`, binary `ai_node_runner`, published as the runtime
  `ai.generic`. A live node has a full L2 name, for example `AI.sales@motherbee`.
- **Build** from the repo root, as `scripts/publish-ia-runtime.sh` does:

  ```bash
  cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner
  ```

To create and configure a node, follow [docs/ai-nodes-deploy-runbook.md](../../../docs/ai-nodes-deploy-runbook.md).

## Start

- **What the orchestrator gives it:** `run_node` launches the node with `FLUXBEE_NODE_NAME`,
  `FLUXBEE_NODE_ILK_ID` and `FLUXBEE_NODE_TENANT_ID`, and no `--config` argument.
- **Its one config** is `/var/lib/fluxbee/nodes/AI/<node@hive>/config.json`:
  - only the spawn boilerplate (no operator config): **UNCONFIGURED**;
  - a valid config: **CONFIGURED**;
  - an invalid one: **FAILED_CONFIG**, until a valid `CONFIG_SET` arrives.
- **Until it is CONFIGURED**, it answers every `user` message with a `node_not_configured` error and
  reports `DEGRADED` to `NODE_STATUS_GET`, with its `lifecycle_state` and, for a rejected
  `config.json`, the reason in `last_error`. CONFIGURED reports `HEALTHY`.
- **A `user` message needs `meta.thread_id`.** Without it the node answers an `invalid_payload`
  error.
- **Without `FLUXBEE_NODE_ILK_ID`** the node has no SY.vault client: it cannot read its key, and it
  has no `publish_html_page` tool.

## Control plane

- **`CONFIG_GET`** returns:
  - the contract;
  - the effective config, redacted;
  - `api_key_source` (`vault` or `missing`);
  - the state of the agent definition (`definition_state`, `definition`, `active_prompt_chars`).
- **`CONFIG_SET`** (`subsystem: "ai_node"`, `apply_mode: "replace"`) validates the config, applies it
  without a restart, and writes it to `config.json`.
  - A `config_version` lower than the current one is refused (`stale_config_version`).
  - An equal one changes nothing.

### `config.behavior`

| Field | Meaning |
| --- | --- |
| `kind` | `ai_chat`, or `echo` (answers `Echo: <input>`, no model call) |
| `model` | required for `ai_chat`: a model of the hive's provider, such as `gpt-5.5` on an OpenAI hive |
| `model_settings` | `max_output_tokens`; an answer cut at it is a `provider_output_truncated` error. `temperature` and `top_p` are accepted too, but current models reject `temperature` (gpt-5.5, Claude 5): leave it out |
| `base_url` | optional, and rarely needed: the full URL the request is posted to. Only `https` on `api.openai.com` or `api.anthropic.com`, unless the node's environment sets `FLUXBEE_AI_ALLOW_INSECURE_BASE_URL` |
| `capabilities.multimodal` | `true` by default: images and documents reach the model. With it off, the input goes as plain text, memory block included, and the SDK's tool loop cuts it at 1,200 characters |

`behavior.vault_key`, `behavior.api_key`, `behavior.api_key_env`, `behavior.openai`,
`config.secrets` and `config.assets` are refused, as is any other unknown field under `behavior`,
`behavior.instructions` included: the prompt is the agent definition (below), never config.

### `config.runtime`

- **`runtime.immediate_memory`:** short-term memory per sender `src_ilk`: `enabled` (default
  `false`), `recent_interactions_max` (default 10), `summary_max_chars` (default 1,600). It applies
  with the `CONFIG_SET`.
- **`runtime.cognitive_definition`:** `poll_interval_secs` (default 10, between 1 and 300) and
  `blob_root` (default `/var/lib/fluxbee/blob`). It applies at the next start.
- **Nothing else:** any other `runtime` field is refused. Timeouts, workers and queue are the AI
  runtime's (`RuntimeConfig::default()`); see [Messages](#messages).

## The key

On every request the node reads the key of the hive's provider from SY.vault:

- **the provider** is `ai.default_provider` of `/etc/fluxbee/hive.yaml`, or OpenAI without an `ai:`
  section;
- **the lookup** is by `resource_type` `openai` or `anthropic`, in its tenant: a key the node owns,
  else one assigned to it, else the tenant's general key (no owner, no assigned node);
- **a failed lookup** answers the message with a `missing_ai_api_key` error.

## The agent definition

- **The node finds its own ILK** in its hive's identity SHM by `handler_node` (its node name). It
  looks every `poll_interval_secs`, and only an `agent` ILK counts.
- **The ILK carries the hashes:** `role_hash`, `skill_hashes` (up to 16), `handbook_hashes` (up to 8)
  and `personality_hash`. Each names a JSON asset, `blob://agent-assets/<hash>.json`, which is
  `/var/lib/fluxbee/blob/agent-assets/<hash>.json` on the node's hive.
  - Archi (SY.architect) writes the assets.
  - `set_ilk_definition` (SY.admin) puts the hashes on the ILK.
  - An asset loads only if it is a regular file of at most 256 KB, its SHA-256 equals its name,
    and its `asset_type` matches the slot. Schema:
    [docs/identity-v2.1-agent-definition-addendum.md](../../../docs/identity-v2.1-agent-definition-addendum.md).
  - The name is opened without following a link or waiting (`O_NOFOLLOW | O_NONBLOCK`) and at most
    256 KB plus one byte is read: a symlink, a FIFO or a directory under it is an invalid asset.
- **The node composes the prompt** from them: personality, role, skills, then handbooks, cut at
  64 KB. It composes it again, without a restart, when the hashes change.
- **Which prompt runs:**

  | The node's ILK | `definition_state` | Prompt |
  | --- | --- | --- |
  | has a definition | `composed`, or `partial` when some asset failed | the composed prompt |
  | has no hashes, or none of its assets loaded | `empty`, `error` | a fixed prompt: the model answers only `{"status": "unconfigured", ...}` |
  | not found before its first definition, or no longer an `agent` ILK | `unresolved` | the same fixed prompt |

  Every model call has a system prompt, and the tools are the same in every state.
- **Once the agent has a definition, only a definite new one replaces it:** the ILK is there, is
  an agent, and every asset it names was read. An asset that is there but invalid (wrong hash, JSON
  or type, not a regular file) is left out of it, and an ILK that is no longer an agent is definite
  too. Anything else changes nothing: the identity SHM unreadable, the ILK absent from it (a degraded
  identity replica), an asset missing or unreadable. The agent keeps its definition,
  `definition.last_poll_error` in `CONFIG_GET` says why, and the next poll tries again.
- **Before its first definition** the agent takes what a poll finds: the fixed prompt while its ILK
  is not found or an asset cannot be read, and a definition without the assets that are missing.

- **On a worker:** Archi (SY.architect) writes the assets on the motherbee, and the Syncthing folder
  `fluxbee-dist-agent-assets` (send-only on the motherbee, receive-only on every worker) brings
  `blob/agent-assets/` to every worker, about 10 to 20 s after they are written; the ILK's new
  hashes arrive in about 1 s.
  - An agent with a definition keeps it until the new assets are there, then takes the new one at
    the next poll (`poll_interval_secs`, 10 s by default).
  - An agent that starts before its assets arrive answers with the "unconfigured" prompt and picks
    its definition up at a poll after they arrive.

## Messages

- **Memory:** the router attaches the communication's `meta.memory_package`, and the node puts it in
  front of the user input as a text block.
- **Its reply** carries the agent's own ILK as `src_ilk` and the sender's as `dst_ilk`.
- **Attachments** (`text/v1` `attachments[]` and `content_ref`):
  - text (`text/plain`, `text/markdown`, `application/json`) goes in as text;
  - images (`png`, `jpeg`, `webp`, `gif`) go as image parts;
  - other files go as documents, with `multimodal` on;
  - Anthropic takes only PDF documents;
  - limits: 8 attachments, 10 MB each.
- **Tools:** the node always offers the model tools that make files: csv, text, json, markdown,
  html, pdf, xlsx, docx, png, jpeg. The files go out as `attachments[]` of the reply.
  - The thread-state tools are added when the message has a `src_ilk`.
  - `publish_html_page` is added when the node has its ILK. It publishes a page through SY.admin and
    returns its public URL.
- **Every model call** is recorded and charged to the node's tenant
  ([crates/fluxbee_ai_sdk/README.md](../../../crates/fluxbee_ai_sdk/README.md#usage-records-who-pays-for-each-call)).
- **Time:** the AI runtime's defaults, not config:
  - each model call has 5 min, its provider's own retries included; past it the sender gets a
    `provider_timeout` error;
  - a message has 47 min: up to 8 model turns of the tool loop, one plain call after them, and
    2 min for its tools;
  - 4 workers and a queue of 128.
- **A message runs once.** When its handler fails or runs out of time, the sender gets an
  `ai_runtime_error` or `ai_runtime_timeout` error and the message is not run again: it may already
  have called the model or a tool such as `publish_html_page`.
