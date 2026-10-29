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
  has neither `publish_html_page` nor `unpublish_html_page`.

## Control plane

- **Who may ask:** a verb runs only as the router's authority gate delivers it, a `system` message
  with the verb in its own case, and only from the node that sends it (the AI SDK's
  `control_request`):
  - `CONFIG_GET` and `CONFIG_SET` from SY.admin@motherbee, which sends them straight to the node on
    any hive;
  - `NODE_STATUS_GET` and `NODE_HELP` from the orchestrator of the node's own hive: SY.admin asks
    the motherbee's orchestrator, which asks the node or the orchestrator of its hive;
  - `PING` and `STATUS` from either.

  Anything else that names a verb is refused, with a WARN in the journal and an `unauthorized`
  answer under the verb's response: an `admin` message, the verb in another case (`config_set`),
  another node, another hive's orchestrator.
- **A notice or a reply is never answered,** only logged (the AI SDK's
  `is_infrastructure_message`, which SY.frontdesk.gov runs too; SY.timer's and wf-generic's rule): a
  message the router made, which carries no `src_l2_name` (`UNREACHABLE`, `TTL_EXCEEDED`), or one
  named as a notice or a reply (`HELLO`, `ECHO_REPLY`, any `*_RESPONSE`). A router notice is a
  WARN with its trace and its reason. An answer to one would go to the router's own UUID, and the
  two would bounce it forever (FINDINGS A-112).
- **`CONFIG_GET`** returns:
  - the contract;
  - the effective config, redacted;
  - `api_key_source` (`vault` or `missing`);
  - `ai_provider`: the provider in effect, `{name, source}`, where `source` is
    `behavior.provider` or `hive` (with an `error` when hive.yaml cannot say);
  - `ai_model`: the model in effect, `{name, source}`, where `source` is `behavior.model` or
    `hive` (with an `error` when the hive has no model for the node's provider);
  - the state of the agent definition (`definition_state`, `definition`, `active_prompt_chars`).
- **`NODE_HELP`** returns the runtime's tool catalog: `{status: "ok", node_name, runtime,
  runtime_version, tools}`, the cards of [Tools](#tools). `runtime_version` is the one the
  orchestrator launched it at (`_system.runtime_version` of its `config.json`), `null` for a node
  started by hand. Like `NODE_STATUS_GET`, it is answered in any state.
- **`CONFIG_SET`** (`subsystem: "ai_node"`, `apply_mode: "replace"`) validates the config, applies it
  without a restart, and writes it to `config.json`.
  - A `config_version` lower than the current one is refused (`stale_config_version`).
  - An equal one changes nothing.
  - A config whose model cannot be resolved then is refused (`invalid_config`, naming
    `behavior.model` and `ai.providers.<provider>.model`): a provider other than the hive's
    default needs `behavior.model`, unless the hive's hive.yaml lists a model for it. So is one
    that leaves its provider or its model to a hive.yaml that cannot be read.
  - Its answer shows `ai_provider` and `ai_model`, as `CONFIG_GET` does.

### `config.behavior`

| Field | Meaning |
| --- | --- |
| `kind` | `ai_chat`, or `echo` (answers `Echo: <input>`, no model call) |
| `model` | a model of the node's provider, such as `gpt-5.5` on OpenAI. Without it, the hive's model for the node's provider (`ai.providers.<provider>.model`), read on each request; with neither, every message fails with `missing_ai_model`, which names both. Name one only when the node needs its own |
| `provider` | `openai` or `anthropic`: the API the node speaks and the key it reads ([The key](#the-key)). Without it, the hive's, read on each request |
| `model_settings` | `max_output_tokens`; an answer cut at it is a `provider_output_truncated` error. `temperature` and `top_p` are accepted too, but current models reject `temperature` (gpt-5.5, Claude 5): leave it out |
| `base_url` | optional, and rarely needed: the full URL the request is posted to. It needs `provider`, and it must be `https` on that provider's host (`api.openai.com` or `api.anthropic.com`); the node's environment can lift the host rule with `FLUXBEE_AI_ALLOW_INSECURE_BASE_URL` |
| `multimodal` | `true` by default: images and documents reach the model. With it off, the input goes as plain text, memory block included, and whole: the tool loop cuts only the history before it, 1,200 characters an interaction |

`behavior.vault_key`, `behavior.api_key`, `behavior.api_key_env`, `behavior.openai`,
`config.secrets` and `config.assets` are refused, as is any other unknown field under `behavior`,
`behavior.instructions` included: the prompt is the agent definition (below), never config.

`behavior.capabilities` is refused too: `capabilities.multimodal` became `multimodal`
(agent-tools-v1 T9). A `config.json` that still has the old key is rewritten once when the node
starts, when `capabilities` is `null` or holds `multimodal` alone, and the node's journal reports
it:
`config.json: behavior.capabilities converted to behavior.multimodal (agent-tools-v1 T9)`.

- **The file as it was** stays next to it, `config.json.pre-0.1.86` (mode 0600), and the line names
  it in `original_copy`: a rollback to 0.1.85 restores it
  ([the runbook](../../../docs/ai-nodes-deploy-runbook.md#rolling-back-to-0185)).
- **The rewrite is safe to interrupt:** each file is written to a temp file of its own (0600,
  synced) and renamed into place; a failed write leaves `config.json` as it was. Without its copy,
  nothing is converted, and the node boots FAILED_CONFIG with the reason.

### `config.runtime`

- **`runtime.immediate_memory`:** short-term memory per sender `src_ilk`: `enabled` (default
  `false`), `recent_interactions_max` (default 10), `summary_max_chars` (default 1,600). It applies
  with the `CONFIG_SET`.
- **`runtime.cognitive_definition`:** `poll_interval_secs` (default 10, between 1 and 300) and
  `blob_root` (default `/var/lib/fluxbee/blob`). It applies at the next start.
- **Nothing else:** any other `runtime` field is refused. Timeouts, workers and queue are the AI
  runtime's (`RuntimeConfig::default()`); see [Messages](#messages).

## The key

On every request the node reads the key of its provider from SY.vault:

- **the provider** is `behavior.provider`; without it, `ai.default_provider` of
  `/etc/fluxbee/hive.yaml`, or OpenAI without an `ai:` section;
- **the lookup** is by `resource_type` `openai` or `anthropic`, only in its tenant: a key the node
  owns, else one assigned to it, else the tenant's general key (no owner, no assigned node);
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
  - text (`text/plain`, `text/markdown`, `application/json`, `text/csv`, `text/html`) goes in as
    text, decoded with U+FFFD for bytes that are not UTF-8, and cut at 100,000 characters with a
    line that says so, names its blob and gives the byte offset to read on from (FINDINGS A-111: a
    CSV or HTML file sent as a document failed the whole message on Claude). The line names no
    tool: SY.frontdesk.gov reads attachments the same way and has no `read_blob`;
  - every attachment shows its blob name, `<name>_<hash16>.<ext>`, which `read_blob` reads;
  - images (`png`, `jpeg`, `webp`, `gif`) go as image parts;
  - other files go as documents, with `multimodal` on;
  - Anthropic takes only PDF documents;
  - limits: 8 attachments, 10 MB each.
- **Tools:** see [Tools](#tools).
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

## Tools

Every agent has every base tool of the runtime (agent-tools-v1 T5). Each tool declares one card in
its code, an `AgentTool` of the AI SDK: what it does, when to use it, its limits, when it is
present, its input and output schemas and its error codes. The model's definition of the tool is
rendered from the card, and so are the catalog and the `NODE_HELP` answer.

- **The catalog:** [docs/reference/agent-tools.md](../../../docs/reference/agent-tools.md), every
  card with its limits, and its JSON, which the package ships next to the runtime as
  `dist/runtimes/ai.generic/<version>/tools.json`. A test (`the_tool_catalog_matches_the_cards`)
  fails when they differ from the code; it writes them again with
  `UPDATE_AGENT_TOOLS_CATALOG=1`.
- **Which tools a message gets:**
  - always the ten file generators, csv, text, json, markdown, html, pdf, xlsx, docx, png and
    jpeg. Each keeps its file for the reply and answers the model only the file's name, type and
    size, never its bytes. The reply is the model's own text with every file its calls made, in
    the order made, as `attachments[]`; at most 8 (`REPLY_FILES_MAX`, the IO nodes' limit for one
    message), and a call past that makes no file (`reply_files_full`). A file named as an earlier
    one of the reply takes its place. The model's text is what it writes after its last tool
    call, or, when that is blank, the last text it wrote alongside a call; without either, a fixed
    line names the files. The PDF is Helvetica in WinAnsiEncoding: Western European text prints as
    written, anything else as `?`, and long lines wrap onto as many pages as they need;
  - always `get_current_time`: the date and time from SY.timer of the node's own hive
    (`TIMER_NOW_IN`), in UTC and in an IANA zone, with the local weekday. Where SY.timer does not
    answer (it runs on the motherbee and the workers), it is `time_unavailable`;
  - always `read_blob`: a text file of the hive's blob store (csv, json, md, html, txt, logs…), by
    its blob name, as an attachment shows it. Text is judged by its content, whatever the name: the
    file is decoded as an attachment is, with U+FFFD for bytes that are not UTF-8, and a piece with
    a NUL byte, as binary files have, is `not_text`. Up to 10 MiB, 64 KiB of text a call from a
    byte `offset` of that text (the offset a cut attachment's line gives), cut where a character
    starts, with `next_offset`. One message reads 192 KiB of text at most, over all its calls: the
    tool loop sends every piece back to the model on each turn. Past it, `read_budget_exhausted`,
    not retryable within the message; the next message starts again. It reads with the SDK's
    `read_blob_by_name` (no link followed, the bytes checked against the hash in the name) and
    waits up to 20 s for a blob on its way from another hive. The store has no tenant: an agent
    reads any text blob of its hive whose name reached it in an attachment (FINDINGS A-106);
  - the thread-state tools when the message has a `src_ilk`: they read and write only that
    sender's record;
  - `publish_html_page` and `unpublish_html_page` when the node has its ILK. The first publishes a
    page through SY.admin and returns its public URL, which expires after 24 h. A reply whose text
    does not carry the URL gets it on a line of its own, unless the turn took the page down. A page
    SY.admin publishes without a public URL is taken down, and the model told it was not
    published. `unpublish_html_page` takes down a page this node published, by its link or its
    `publication_id` (`unpublish_artifact`). Of the link it sends SY.admin the key: the letters and
    digits after `/public/`, so a `)`, a `.` or Slack's `|label>` after it are left out. A key that
    is not 64 lowercase hex characters is `not_found` before SY.admin is asked, never a retryable
    failure. The journal names the publication's id, never the URL, which is the key to the page
    (FINDINGS A-95); the reply that carries the URL is logged by its files' names and its text's
    length, never its text.
- **The journal holds no permission to read:** the "incoming user message" line previews the
  input with every `/public/<key>` as `/public/<redacted>` and every blob name as `<blob>` (A-95,
  A-106), and `read_blob` logs a failure without the blob's name.
- **A CSV cell never runs as a formula:** one that starts with `=`, `+`, `@`, a tab or a carriage
  return, or with `-` and is not a plain number, gets a `'` in front (OWASP's CSV injection
  advice).
- **Each description fits the model APIs:** the description a card renders is at most 1,024
  characters, which OpenAI enforces; the rules of an argument, such as a file's name, are in the
  input schema.
