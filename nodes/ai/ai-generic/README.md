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
    default needs `behavior.model`, unless the hive's `ai` lists a model for it. So is one that
    leaves its provider or its model to an installation file no reader can use, or to a hive.yaml
    that cannot be read while the hive holds no file.
  - Its answer shows `ai_provider` and `ai_model`, as `CONFIG_GET` does.

### `config.behavior`

| Field | Meaning |
| --- | --- |
| `kind` | `ai_chat`, or `echo` (answers `Echo: <input>`, no model call) |
| `model` | a model of the node's provider, such as `gpt-5.5` on OpenAI. Without it, the hive's model for the node's provider (`ai.providers.<provider>.model`), read on each request; with neither, every message fails with `missing_ai_model`, which names both. Name one only when the node needs its own |
| `provider` | `openai` or `anthropic`: the API the node speaks and the key it reads ([The key](#the-key)). Without it, the hive's, read on each request |
| `model_settings` | `max_output_tokens`; an answer cut at it is a `provider_output_truncated` error. `temperature` and `top_p` are accepted too, but current models reject `temperature` (gpt-5.5, Claude 5): leave it out |
| `base_url` | optional, and rarely needed: the full URL the request is posted to. It needs `provider`, and it must be `https` on that provider's host (`api.openai.com` or `api.anthropic.com`); the node's environment can lift the host rule with `FLUXBEE_AI_ALLOW_INSECURE_BASE_URL` |
| `multimodal` | `true` by default: images and documents reach the model. With it off, the input goes as plain text, memory and WF blocks included, and whole |

`behavior.vault_key`, `behavior.api_key`, `behavior.api_key_env`, `behavior.openai`,
`config.secrets` and `config.assets` are refused, as is any other unknown field under `behavior`,
`behavior.instructions` included: the prompt is the agent definition (below), never config.

### `config.runtime`

- **`runtime.cognitive_definition`:** `poll_interval_secs` (default 10, between 1 and 300) and
  `blob_root` (default `/var/lib/fluxbee/blob`). It applies at the next start.
- **Nothing else:** any other `runtime` field is refused. Timeouts, workers and queue are the AI
  runtime's (`RuntimeConfig::default()`); see [Messages](#messages).
- **`runtime.immediate_memory` is refused with its reason** (FINDINGS A-133): the node kept that
  memory per sender, so a WF's consultation mixed customers, and it no longer has one. An agent's
  memory is cognition's package, per conversation ([Messages](#messages)). Every `config.json` a
  `CONFIG_SET` wrote before its removal has it, the default included: such a node boots
  FAILED_CONFIG until a `CONFIG_SET` without it.

## The key

On every request the node reads the key of its provider from SY.vault:

- **the provider** is `behavior.provider`; without it, the hive's `ai.default_provider`
  ([The hive's ai](#the-hives-ai)), or OpenAI without an `ai`;
- **the lookup** is by `resource_type` `openai` or `anthropic`, only in its tenant: a key the node
  owns, else one assigned to it, else the tenant's general key (no owner, no assigned node);
- **a failed lookup** is a provider that gives no answer: the person gets the fixed text, and a
  WF's menu or a response envelope a `missing_ai_api_key` error whose `reply` is that text (see
  [Messages](#messages)).

## The hive's ai

What a node leaves to the hive (its provider without `behavior.provider`, its model without
`behavior.model`) is read on each request that needs it, and only then: a node that names both
reads nothing of the hive's ([hive-config-update-v1](../../../docs/hive-config-update-v1.md) §4).

- **The installation file** once the hive holds one: `<dist.path>/installation/installation.json`,
  under the dist its `hive.yaml` names, found at the node's start (`installation_file_of_hive`).
  The motherbee writes it, and every hive receives it; every provider it names has its model.
- **`ai:` in `/etc/fluxbee/hive.yaml`** while the hive holds no file. It is read only then, so an
  old `ai` there never stops a node once the file is there.
- **A file that is there and that no reader can use** is never a fallback to hive.yaml: the
  request fails with `installation_ai_settings_invalid`, not retryable, its reason in `details`.
  A hive.yaml that cannot be read while the hive holds no file fails as before
  (`hive_ai_config_unreadable`).
- **The log:** the node logs where the hive's `ai` came from (the file's version and hash, or
  hive.yaml), or why it could not be read, when that changes; the first request that reads it
  counts as a change.

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

- **Memory:** the router attaches the communication's `meta.memory_package`
  ([docs/reference/cognition.md §7](../../../docs/reference/cognition.md#7-the-memory-region-and-the-memory-package)),
  and the node puts it in front of the user input as a text block, "Conversation memory":
  - its labels and memories: the thread, the dominant context and reason, up to 4 contexts and 4
    reasons, up to 3 memories;
  - the feelings of the people in it, the package's up to 12
    ([docs/cognition-hands-v1.md](../../../docs/cognition-hands-v1.md) §10.3), one line each:
    `- person Ana Pérez (ilk:…): anger about billing, strong, last felt 2026-11-05 10:05: "<what
    was lived>"`. Those of the person being answered come first (its `meta.context.writer`, else
    its `src_ilk`), then the rest, the strongest first;
  - what the package left out;
  - then the recent conversation, the communication's last turns (up to 20), oldest first, one
    line each: `- [2026-10-30 12:05] <who>: "<text>"`, the time in UTC
    ([docs/cognition-hands-v1.md](../../../docs/cognition-hands-v1.md) §6). `<who>` names the
    writer of another's turn: `<kind> <name> (<ILK>)`, the kind `person`, `agent`, `workflow` or
    `system`, and `via <node>` after a person's, the node they wrote through (the IO node, for a
    message a WF forwarded). So a person, a supervisor, another agent and a WF's own text read
    apart. The agent's own turns read `you, to <whom>`. A turn kept before turns named a writer
    reads `<sender ILK> via <node>`. The text is a JSON string, so no text can pass for a
    turn's line of its own; files are counted (`[2 attachments]`), and an answer that chose from
    a WF's menu says so, even with a blank text (`chose invoice {"quantity":10}`). The message
    being answered is not among them.
  - This is the agents' recent conversation, and their only one: SY.cognition takes each turn as
    it arrives, so the next message carries the one before it. The node keeps no history of its
    own (FINDINGS A-133).
  - **The message being answered names its writer the same way,** once a block comes before it:
    `Current user message, from <writer>:`. Its writer is its `meta.context.writer`, else its
    sender's ILK, with the kind and name the identity memory of the agent's hive gives; the node
    is where its cycle started, the first entry of its trace, or else the sender. As text, a line ahead of the message; as parts, a text part of its
    own after the blocks.
  - Nothing ahead of the message is cut, and the message itself never is (FINDINGS A-71): with 20
    turns of 2,000 characters the block passes 40,000 characters (some 10,000 tokens a call), and
    the WF's block and the message still reach the model whole.
- **The message's trace:** `meta.context.trace` ([docs/message-trace-v1.md](../../../docs/message-trace-v1.md)),
  one entry for each node of the message's cycle. The node shows it to the model as a text block
  after the memory block: `Message trace (UTC):`, one line for each entry, such as
  `10:17:03.180 WF.sales.invoice@worker1 -> AI.invoice@worker1 [forward]`, the newest 32 after a
  line that counts the rest.
- **A WF's block:** a WF puts `meta.context.wf`, `{"vars": {...}}`, on a message
  it sends with variables
  ([docs/wf-orchestration-v1.md §5.2.1](../../../docs/wf-orchestration-v1.md#521-the-build-design)),
  with a `menu` when it asks the agent to choose (below). The node shows it to the model as a text
  block after the trace and before the message: `vars` as JSON, cut at 8 KiB with a line that says
  so, and the menu.
  - Text and multimodal input alike: as parts, each block is a text part of its own, first.
- **Its reply** carries the agent's own ILK as `src_ilk` and the sender's as `dst_ilk`. Its
  `meta.context` is the request's without `thread_id`, `src_ilk`, `response_envelope`,
  `final_response_contract`, `wf` and `evaluation`: an answer never carries the request's WF block
  back, and its evaluation is only ever its own. It keeps the request's trace, and the runtime adds
  the agent's entry as it sends: to whoever asked, with the option it chose from a menu.
- **A structured answer:** a message whose `meta.context.response_envelope` holds a contract
  (`json_object_v1`: `required`, `properties` of type `string`, `boolean`, `integer` or `number`, an
  optional `enum` for strings, `strict`) is answered with the contract's object, as JSON in the
  reply's text. Both paths ask the provider for it: every turn of the tool loop, next to the tools,
  with the contract also in its system prompt (FINDINGS A-150), and the call without tools, after a
  loop that wrote nothing, which also checks the object. An invalid contract is an
  `ai_runtime_error`, and an object that fails its check an `invalid_structured_output`, neither
  retryable.
  - **Anthropic** holds the answer to the contract's schema on both paths (structured outputs),
    whatever `strict` says. **OpenAI** holds it only under `strict: true`, which it accepts only
    when every property is `required`; without it, OpenAI takes the schema as a hint, as the
    instruction is.
  - **The journal:** the "AI chat request prepared" line says `output_contract_mode`:
    `structured_output` under a response contract, with tools or without them.
  - **`text_field`** names one of the contract's required `string` properties, for example
    `"text_field": "reply"`. The reply's text is then that field's value, and the whole object goes
    in the reply's `meta.context.evaluation`. The object is checked on both paths, with tools and
    without them. A `text_field` that is not a required `string` property makes the contract
    invalid.
  - **Who uses it:** a caller that is not a WF. A WF asks an agent with a menu (below): wfcel
    refuses `meta.context.response_envelope` in a WF's actions.
- **A WF's menu:** a WF asks an agent to choose with `meta.context.wf.menu`
  ([docs/wf-orchestration-v1.md §5.3](../../../docs/wf-orchestration-v1.md#53-case-3-the-internal-consultation-decided-with-the-operator-2026-10-30)):
  an optional `help`, and 1 to 8 `options`, each with a `name`, a `description` and its `params`
  (`string`, `boolean`, `integer` or `number`, with an optional description and, for a string, an
  optional `enum`).
  - **Checked first:** the node checks the menu again with wfcel's limits. An invalid menu is
    answered `invalid_menu`, and a message with a menu and a `response_envelope`
    `conflicting_contracts`, neither retryable, and the model is not called.
  - **What the model reads:** the menu at the end of the WF's block, with `none` (its param
    `reason`), which the node adds to every menu, and the one line that gives the answer's shape,
    `{"reply", "option", "params"}`. The system prompt stays the agent's role, with tools and
    without them, and the agent may use its tools before it answers.
  - **The answer** is found as a structured answer's is: the first whole JSON object, in a fence
    (also on the fence's opening line) or among other text, where a `{` that starts none is text.
    Its `option` is one of the menu's or `none`, each param of that option is there with its type
    (an integer a JSON integer, a number any number, an `enum`'s value one of its values; `none`
    needs `reason`), and `reply` is a string, maybe empty. A param the option does not declare, or
    a field besides the three, is dropped with a line in the journal. Anything else is an
    `invalid_structured_output`, not retryable, whose message ends with `not_json`,
    `unknown_option:<x>`, `missing_param:<p>` or `type_mismatch:<p>:<type>`.
  - **The reply's** text is `reply`, and its `meta.context.evaluation` the checked object, which a
    WF reads as `event.context.evaluation.option` and `event.context.evaluation.params.<p>`.
  - **`reply` is the text the person gets.** When the tool loop adds to the model's reply (the
    fixed line that names the files, for a blank reply, or a published page's link on a line of its
    own; see [Tools](#tools)), the evaluation's `reply` carries it too. So a WF that forwards only a
    reply that is not blank still delivers the files and the links (FINDINGS A-98, A-99).
  - **The journal:** the "AI chat request prepared" line names the message's contract, `none`,
    `envelope` or `menu`.
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
- **When the provider gives no answer,** the person gets one fixed text: *"There is a problem with
  the AI provider this agent uses, so it could not answer. Please contact your administrator."*
  (`PROVIDER_FAILURE_TEXT`; the operator's call of 2026-11-08, FINDINGS A-184).
  - **No answer means:** no key for the node's provider in SY.vault, an error status after the AI
    SDK's retries, no connection or an answer that cannot be read, none in time, an answer with no
    text, or the model's refusal (`AiSdkError::is_provider_failure`).
  - **How it goes out, by the message's contract:**
    - **no contract:** the agent's text, which reaches the person on any path a text answer takes;
    - **a WF's menu:** the error payload, with its code and the text in `reply`. A WF forwards an
      agent's answer only on its evaluation's `reply`, which a bare text has not, and catches an
      agent's error on `event.payload.type == 'error'`: its catch sends `event.payload.reply` to
      the person (Archi's handbook §4.2.4, atom 6);
    - **a response envelope:** the error payload with `reply` too, for the program that asked.
  - **Its codes:** `missing_ai_api_key`, `provider_invalid_request`, `provider_auth_error`,
    `provider_timeout`, `provider_rate_limited`, `provider_unavailable`, `provider_error`,
    `provider_attachment_invalid_request`, `provider_no_answer`, `provider_refused` or
    `provider_unreachable`.
  - **Where the error goes:** the node's journal, `AI runtime request failed`, with
    `provider_status`, `provider_param`, `provider_request_id` and the error.
  - **Any other failure** is its error payload, as below, with no `reply`: an invalid contract or
    menu, an answer off its contract, an answer cut at its token limit.
- **Time:** the AI runtime's defaults, not config:
  - each model call has 5 min, its provider's own retries included (up to 3 attempts on a 408,
    409, 429 or 5xx, after the wait the provider names);
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
- **A card's values are the code's** (FINDINGS A-103): a limit, a size or a wait that a card
  states is written from the constant the tool uses. Where the card's text is static, or the value
  is another component's, a test holds the text to it: the AI SDK's limit on a file's name, which
  the SDK keeps private, is tested at its edge, and SY.admin's default lifetime of a published page
  by a test of SY.admin's that reads the catalog.
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
