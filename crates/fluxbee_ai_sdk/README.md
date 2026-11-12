# fluxbee-ai-sdk

The AI layer on top of [`fluxbee-sdk`](../fluxbee_sdk/README.md). Every model call in Fluxbee goes
through it: `ai.generic` (every `AI.*` node), SY.frontdesk.gov, SY.architect, SY.admin's executor and
SY.cognition.

It provides:

- the only way to call a model: two factories that return an OpenAI or Anthropic client, and every
  call they make is charged to a tenant (usage records);
- the tool loop: `FunctionCallingRunner`, `FunctionTool`, `FunctionToolRegistry`, and the card an
  agent's tool is declared by (`AgentTool`, `ToolCard`, `ToolCatalog`);
- immediate conversation memory and a helper that refreshes its summary;
- thread-state tools and their LanceDB store;
- the AI node runtime (`AiNode`, `NodeRuntime`), reply builders, and helpers for `text/v1` input
  (attachments) and output (artifacts).

## Calling a model

### The provider and the key

- **The hive's provider** is the `ai:` section of `/etc/fluxbee/hive.yaml`, read as a
  `HiveAiConfig`. `HiveAiConfig::effective()` returns the provider and the model configured for it.
  Without the section, `HiveAiConfig::fallback()` applies: OpenAI with `gpt-5.5`
  (`DEFAULT_HIVE_OPENAI_MODEL`). The SY nodes that call a model (SY.cognition, Archi, SY.admin's
  executor, the frontdesk) use it. An `ai.generic` node takes the hive's provider and the hive's
  model for that provider (`ai.providers.<provider>.model`; the fallback's `gpt-5.5` for OpenAI
  without the section) unless it names its own in `behavior.provider` and `behavior.model`
  (docs/agent-tools-v1.md T8, FINDINGS A-104).

  ```yaml
  ai:
    default_provider: openai        # openai | anthropic
    providers:
      openai:
        model: "gpt-5.5"
  ```

- **The installation file** ([hive-config-update-v1](../../docs/hive-config-update-v1.md) §4) will
  hold the operator's `ai` for the whole installation, one version on every hive:
  `installation/installation.json` in the hive's dist (`installation_file_path(dist_root)`;
  `INSTALLATION_FILE_PATH` under the default dist), written by the motherbee's orchestrator and
  received through dist.
  - `ai_settings_at(path, || hive_yaml_ai)` returns its `ai`, with its version and sha256, once
    the hive holds it. Until then it returns the `ai` of the hive's own hive.yaml, which the
    closure reads only in that case (`AiSettingsSource::HiveYaml`; a reader says so).
  - A file that is there and is not a valid one is an error, never a fallback to an older
    hive.yaml: whether a reader stops on it or reports it and goes on is the reader's call.
  - Valid (`InstallationFile::validate`, which `parse` and `render` both apply): this release's
    schema, a version of 1 or more, and an `ai` whose default provider has its model and whose
    every provider named has one.
  - `installation_file_state(path)` tells `/versions` what a hive holds: absent, valid, or invalid
    with the sha256 of its bytes and why.

  ```json
  {
    "schema_version": 1,
    "version": 1,
    "ai": {
      "default_provider": "anthropic",
      "providers": {
        "openai": { "model": "gpt-5.5" },
        "anthropic": { "model": "claude-sonnet-5-5" }
      }
    }
  }
  ```

- **The key comes from SY.vault, never from config.** Look it up with
  `fluxbee_sdk::VaultClient::resolve_resource(provider.resource_type(), tenant, timeout)`.
  `AiProvider::resource_type()` gives `openai` or `anthropic`. The lookup returns the key the node
  owns, else one assigned to it, else its tenant's general key.

### The two factories

```rust
pub fn create_llm_client(
    provider: AiProvider,
    api_key: impl Into<String>,
    base_url: Option<String>,
    account: AiAccount,
) -> Arc<dyn LlmClient>;

pub fn create_function_calling_model(
    provider: AiProvider,
    api_key: impl Into<String>,
    base_url: Option<String>,
    model: impl Into<String>,
    system: Option<String>,
    model_settings: ModelSettings,
    account: AiAccount,
) -> Arc<dyn FunctionCallingModel>;
```

- **The factories are the only way in.** Since 0.1.79 the provider clients are private to the SDK.
- **`create_llm_client`** is the plain path: `LlmClient::generate(LlmRequest)`.
- **`create_function_calling_model`** is the tool-loop path, run by `FunctionCallingRunner`.
- **`account`** says who spends and who pays (see [Usage records](#usage-records-who-pays-for-each-call)).
- **`base_url`:** pass `None` for the provider's own endpoint. When set, it is the full URL the
  request is posted to: `https://api.openai.com/v1/responses` or `https://api.anthropic.com/v1/messages`.
- **`ModelSettings { temperature, top_p, max_output_tokens }`:**
  - leave `temperature` unset: current models reject it (gpt-5.5 and Claude 5, FINDINGS A-69);
  - Anthropic needs a token limit on every request, so the SDK sends 4,096 when
    `max_output_tokens` is unset; OpenAI uses the model's default;
  - an answer cut at the limit is an error on both providers, plain path and tool turns alike
    (`AiSdkError::Truncated`, `is_truncated_answer`); its usage record says `truncated`.
- **Each call is retried the same way on both providers** (`send` in `llm.rs`, FINDINGS A-184):
  - up to 3 attempts, on a transport error and on 408, 409, 429 or any 5xx, or as the provider's
    `x-should-retry` says;
  - the wait is the provider's `retry-after-ms` or `retry-after` (at most 60 s), else 0.5 s
    doubling, with jitter. Each retry is logged before its wait, with the status and the request
    id, so a caller whose own deadline is shorter (cognition's 8 s) still leaves the line;
  - the status and the headers decide before the body is read, so a proxy's 502 with an HTML
    page keeps its status.
- **The errors are typed** (`errors.rs`), and a caller matches the type, never the text:
  - **`AiSdkError::Provider(ProviderError)`:** the provider answered an error status after the
    retries: its `provider`, `status`, `kind()` (`InvalidRequest`, `Auth`, `Timeout`,
    `RateLimited`, `Unavailable`, `Other`), the body's `error_type`, OpenAI's `param` (the request
    field it refused), the `request_id` and the `message`. Never the body itself: OpenAI's 401
    echoes the key, and the message is scrubbed of it. Its `Display` is one line for the log,
    `anthropic error status=429 type=rate_limit_error request_id=… message=…`.
  - **`AiSdkError::Refused`:** the model declined: Anthropic's `stop_reason: refusal`, an OpenAI
    refusal part with no text, or OpenAI's content filter (`status: incomplete`, reason
    `content_filter`), whatever text came before the cut. A tool turn that refuses is this error
    too, never an empty turn its caller asks again without tools, nor a call to run.
  - **`AiSdkError::NoAnswer`:** a 2xx with nothing to send, no text or only blanks, and no
    refusal; its `reason` says what the answer held (`stop_reason=…`, `status=…`). A tool turn's
    blank text is `None`.
  - **`AiSdkError::MissingKey`:** there is no key to ask the provider with; the caller makes it
    when SY.vault gives none.
  - **`is_provider_failure()`:** the provider gave no answer: `MissingKey`, `Provider`, `Http` (no
    connection, or an answer that could not be read), `Timeout`, `NoAnswer` or `Refused`. Neither
    the request nor its contract is at fault, and a cut answer (`Truncated`) is not one. An AI node
    answers a person with `PROVIDER_FAILURE_TEXT` then, one fixed text in English, and logs the
    error (the operator's call of 2026-11-08): as its text with no contract, and in the error
    payload's `reply` under a WF's menu or a response envelope. SY.cognition counts it apart from
    an invalid output.
- **Every call has `MODEL_CALL_TIMEOUT` (5 min),** the provider's own retries included (each
  provider's `send`). Past it the call fails with `AiSdkError::Timeout` and its usage record says
  `abandoned`. The bound is for stuck connections: a long reasoning answer or an Archi stage fits
  in it.
- **Prompt caching on Anthropic:** a tool turn marks two cache breakpoints,
  `cache_control: {"type": "ephemeral"}`, on the last tool definition and on the `system` prompt
  given to the factory. The tools and that prompt are the same on every turn, so Anthropic reads
  them from its prompt cache (5 min) instead of billing them in full; the usage record counts them
  as `cache_read` and `cache_write`. The run's own system text (immediate memory) goes after the
  breakpoints, in a block of its own. A prompt shorter than the model's minimum is sent uncached,
  without an error. OpenAI caches a repeated prefix by itself. The plain path marks nothing.

### A tool loop

```rust
use fluxbee_ai_sdk::{
    create_function_calling_model, AiAccount, AiProvider, FunctionCallingConfig,
    FunctionCallingRunner, FunctionToolRegistry, ModelSettings, TenantSource, UsageRecorder,
};

// Once per process, after the node joined its router (`dispatcher: Arc<RouterDispatcher>`).
let usage = UsageRecorder::start(dispatcher.clone());

// Per operation: this node spends and its own tenant pays.
let account = AiAccount::charge(
    usage.clone(),
    "AI.sales@motherbee",
    fluxbee_sdk::read_self_tenant_from_env(),
    TenantSource::Node,
)
.with_node_ilk(fluxbee_sdk::read_self_ilk_from_env())
.with_counterpart(msg.meta.src_ilk.clone())
.with_trace(Some(msg.routing.trace_id.clone()))
.with_thread(msg.meta.thread_id.clone());

let model = create_function_calling_model(
    AiProvider::OpenAi,
    api_key, // from SY.vault
    None,    // the provider's endpoint
    "gpt-5.5",
    Some("You are a Fluxbee node.".to_string()),
    ModelSettings::default(),
    account,
);
let tools = FunctionToolRegistry::new();
let runner = FunctionCallingRunner::new(FunctionCallingConfig::default()); // max_turns: 8
let result = runner.run(model.as_ref(), &tools, "hello").await?;
let answer = result.final_assistant_text;
```

- A run that needs more than `max_turns` model turns ends in an error.
- **After a failed run, whether a tool ran:** run the loop through
  `ToolRunWatch::new(model.as_ref(), &tool_ran)` (an `AtomicBool`). Once a tool has run, a failure
  is not retryable: sending the same request again would run that tool again.
- `result.tokens_used` sums what the provider reported over the run: each turn's whole prompt,
  cached or not, and its answer.
- The runner cuts a text input at 1,200 characters. A longer input has to go as
  `FunctionRunInput::current_user_parts`, which is sent as it is.
- **An answer schema** (FINDINGS A-150):
  `FunctionCallingConfig { output_schema: Some(spec), ..FunctionCallingConfig::default() }` asks
  the provider for `spec` on every turn, next to the tools, since any turn without a tool call is
  the final answer.
  - Anthropic gets the plain path's `output_config.format`: structured outputs hold the model's
    text to the schema and leave its tool calls and its thinking free. A model that refuses
    `output_config` with a 400 has that turn asked again without it.
  - OpenAI gets `text.format`, and holds the text to it only when `spec` is `strict`, which OpenAI
    accepts only with every property required. A 400 that names `text.format` (a strict schema
    with an optional property, or a model without `json_schema`) has that turn asked again without
    it.
  - The runner does not check the answer, and adds no instruction: the caller does, as ai.generic
    does with a response contract (`build_output_schema_fallback_instruction` in its system prompt;
    the object checked afterwards only when the contract names a `text_field`).
  - The schema is the same on every turn of a run, so the turns still read Anthropic's prompt
    cache; a different `output_config` invalidates it.

### A plain call

```rust
use fluxbee_ai_sdk::{create_llm_client, AiProvider, LlmClient, LlmRequest};

let llm = create_llm_client(AiProvider::OpenAi, api_key, None, account);
let response = llm
    .generate(LlmRequest {
        model: "gpt-5.5".to_string(),
        system: Some("Answer in one sentence.".to_string()),
        input: "What is a hive?".to_string(),
        input_parts: None,
        output_schema: None,
        max_output_tokens: None,
        model_settings: None,
    })
    .await?;
println!("{}", response.content);
```

`output_schema: Some(OutputSchemaSpec::new(name, schema, strict)?)` asks for structured JSON; the
answer is checked against the schema.

### Writing a tool

A tool an AI agent has is an `AgentTool`: it declares one `ToolCard` and nothing else
([docs/agent-tools-v1.md](../../docs/agent-tools-v1.md) T2). The card says what the tool does,
when to use it, its limits, when it is present, its input and output as JSON schemas, and its
error codes.

```rust
use std::sync::Arc;

use async_trait::async_trait;
use fluxbee_ai_sdk::{AgentTool, FunctionToolRegistry, ToolCard, ToolErrorCode};
use serde_json::{json, Value};

struct HiveName;

#[async_trait]
impl AgentTool for HiveName {
    fn card() -> ToolCard {
        ToolCard {
            name: "hive_name".to_string(),
            summary: "Say which hive this node runs on.".to_string(),
            when_to_use: "When the user asks where the agent runs.".to_string(),
            limits: vec!["Only the hive's name, nothing about its nodes.".to_string()],
            present_when: "always".to_string(),
            input: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            output: json!({
                "description": "`{hive}`, the hive's name.",
                "type": "object",
                "properties": {"hive": {"type": "string"}}
            }),
            errors: vec![ToolErrorCode::new("no_hive", "The node has no hive.")],
        }
    }

    async fn call(&self, _arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        Ok(json!({"hive": "motherbee"}))
    }
}

let mut tools = FunctionToolRegistry::new();
tools.register(Arc::new(HiveName))?; // an empty or duplicate name is refused
```

- **The model receives the card** (`ToolCard::model_definition`): the summary, `When to use:`, the
  limits, `Returns:` with the output schema's `description`, and the input schema. Provider APIs
  take only a name, a description and the input; `present_when` and the error codes stay out.
- **The same card** is the runtime's tool catalog (`ToolCatalog`, generated into
  [docs/reference/agent-tools.json](../../docs/reference/agent-tools.json)) and its `NODE_HELP`
  answer. `ToolCard::validate` lists what a card is missing.
- **The rendered description fits in `TOOL_DESCRIPTION_MAX_CHARS` (1,024 characters):** OpenAI
  refuses a longer one with 400 `string_above_max_length`, and `ToolCard::validate` reports it. The
  descriptions inside the input schema do not count, so the rules of one argument go there.
- **Every `AgentTool` is a `FunctionTool`** whose definition is rendered from its card. A SY node's
  own tools (Archi's, SY.admin's executor's, the frontdesk's `ilk_register`) implement
  `FunctionTool` directly, with a hand-written `FunctionToolDefinition`.
- **Errors go back to the model.** A tool that returns `Err`, or a call to an unknown tool, becomes
  a tool result with `is_error: true`, and the loop continues. A failure the model should act on
  is a result `{"status": "error", "error_code", "message", "retryable"}` with a code of its card.
- **Providers of several tools** implement `FunctionToolProvider`, as the thread-state tools do.

### Testing without a provider

- **`MockLlmClient`** implements `LlmClient` with queued responses (`with_responses`,
  `push_response`, `take_requests`).
- **For a tool loop**, implement `FunctionCallingModel` on a test type, as
  `tests/immediate_memory.rs` does.
- **An `AiAccount` on `UsageRecorder::disabled()`** records nothing.

## Usage records: who pays for each call

Every model call leaves a usage record charged to a tenant. The record goes to the node's router
stream under `storage.usage`, and SY.storage keeps it in `llm_usage`
([docs/router-stream-v1.md §7](../../docs/router-stream-v1.md)). That is why both factories require an
`AiAccount`.

- **`UsageRecorder::start(dispatcher)`:** call it once per process, inside the Tokio runtime, with the
  node's `Arc<RouterDispatcher>`.
  - A task appends each record with the dispatcher's current sender, so it follows reconnections.
  - It holds up to 1,024 records (`USAGE_RECORDER_QUEUE`). Past that it drops them and counts the
    drops (`stats()`).
  - A failed append is logged in the node's journal. It is never returned to the caller.
- **`UsageRecorder::disabled()`** records nothing: unit tests, and tools without a router.
- **`AiAccount::charge(recorder, node_name, tenant, source)`:** `node_name` is the spending node's full
  L2 name. `tenant` pays, for the reason `source` gives:

  | `TenantSource` | Used by |
  | --- | --- |
  | `Node` | a tenant's node, paying its own tenant |
  | `Agent` | SY.cognition, with the tenant of the communication's agent |
  | `Case` | SY.frontdesk.gov, with the tenant of the case |
  | `Root` | `AiAccount::root(recorder, node_name)`: SY.architect, SY.admin's executor |
  | `FallbackRoot` | set by the SDK when `tenant` is `None` or empty: the root tenant pays |

- **Builders:** `with_node_ilk`, `with_counterpart` (the ILK the call serves), `with_trace`,
  `with_thread`, `with_work(UsageWork { .. })`. All are optional, and a blank value is left out.
- **One account per operation:** its trace, thread and counterpart are those of the work the call
  serves. Clone the recorder, not an old account.

What is recorded:

- **Every 2xx answer:** one record, queued before the answer is checked, because a truncated or
  invalid answer still cost. Its `outcome` is `ok`, `truncated`, `refused` or `unreadable`.
- **A call dropped while its request is outstanding** (a caller's timeout): `abandoned`, without
  tokens.
- **Non-2xx answers and transport errors:** no record. Of a call's retries, on either provider, only
  the attempt that answered is recorded.
- **The counts, normalized across providers:** `uncached_input`, `cache_read`, `cache_write`,
  `output` (reasoning included), `reasoning` (OpenAI only), plus the provider's raw usage object.

SY.storage checks every record. A node outside `SY.*` is charged to its own tenant as identity knows
it, whatever the record claims; a record that claims another tenant is stored with the node's tenant
and counted as a mismatch. For a node, the meaningful account is `TenantSource::Node` with its own
tenant (`FLUXBEE_NODE_TENANT_ID`, read by `fluxbee_sdk::read_self_tenant_from_env()`).

## Cognition: what reaches an AI node

Cognition belongs to the communication: the thread between an interlocutor and the agent that serves
it, `(thread_id, agent ILK)` ([docs/router-stream-v1.md §6](../../docs/router-stream-v1.md)).

**On the message:**

- **`meta.thread_id`:** the conversation's thread. ai.generic answers a `user` message without one
  with an `invalid_payload` error.
- **`meta.memory_package`:** set by the agent's router when it delivers a message with a `thread_id`
  to an `AI.*` node and SY.cognition has already built the memory of that communication. The router
  takes it from its hive's memory region, for each destination, and drops any package the sender put
  in. It carries the communication's memories, the feelings of the person at the other end, the
  news of the people in the conversation (version 5), and the last turns, each naming its writer;
  a WF's relay between people names whom it was said to (`to_via`, and `to` when the message names
  the person). ai.generic renders it as a text block in front of the user input, and tells the
  model to take the news into account and never repeat them, and that a line between other people
  was not said to it.
- **No sequence number:** SY.cognition keeps the order of a communication with its own turn count.

**Not on the message:** cognition's state (contexts, reasons, cooccurrences, scopes, memories,
feelings, episodes) lives in SY.cognition's own file on its hive, and the message log (`turns`) in
SY.storage. A node reads neither.

So, in an AI node:

- use `meta.memory_package` for conversational context, and `meta.thread_id` as conversation
  metadata;
- keep hard state keyed by `src_ilk`, and no immediate memory: the package is the agent's memory
  (FINDINGS A-133);
- do not read the memory region (`jsr-memory-<hive>`) or PostgreSQL.

## Immediate memory

For continuity between turns, call `run_with_input(...)` with a `FunctionRunInput`. Its
`ImmediateConversationMemory` takes:

- `summary` (a `ConversationSummary`);
- `recent_interactions`;
- `active_operations`;
- `thread_id`, as conversation metadata;
- `scope_id`, which in SY.frontdesk.gov is the sender's `src_ilk`.

SY.architect uses it, and SY.frontdesk.gov keeps it by `src_ilk`, off in its baked config.
ai.generic no longer has it: an agent's memory is cognition's package, per conversation (FINDINGS
A-133).

```rust
use fluxbee_ai_sdk::{
    ConversationSummary, FunctionRunInput, ImmediateConversationMemory, ImmediateInteraction,
    ImmediateInteractionKind, ImmediateOperation, ImmediateRole,
};

let input = FunctionRunInput {
    immediate_memory: Some(ImmediateConversationMemory {
        thread_id: msg.meta.thread_id.clone(),
        scope_id: msg.meta.src_ilk.clone(),
        summary: Some(ConversationSummary {
            goal: Some("Provision worker1".to_string()),
            current_focus: Some("Review the add_hive result".to_string()),
            decisions: vec!["Mutations require an explicit CONFIRM".to_string()],
            confirmed_facts: vec![],
            open_questions: vec!["Whether add_hive already completed".to_string()],
        }),
        recent_interactions: vec![
            ImmediateInteraction {
                role: ImmediateRole::User,
                kind: ImmediateInteractionKind::Text,
                content: "create hive worker1".to_string(),
            },
            ImmediateInteraction {
                role: ImmediateRole::Assistant,
                kind: ImmediateInteractionKind::Text,
                content: "Hive creation prepared. Reply CONFIRM to execute.".to_string(),
            },
        ],
        active_operations: vec![ImmediateOperation {
            operation_id: "op-1".to_string(),
            resource_scope: Some("hive:worker1".to_string()),
            origin_thread_id: msg.meta.thread_id.clone(),
            origin_session_id: None,
            scope_id: None,
            action: "add_hive".to_string(),
            target: Some("worker1".to_string()),
            status: "timeout_unknown".to_string(),
            summary: "Inspect before retrying".to_string(),
            created_at_ms: None,
            updated_at_ms: None,
        }],
    }),
    ..FunctionRunInput::new("confirm the previous hive operation")
};

let result = runner.run_with_input(model.as_ref(), &tools, input).await?;
```

The runner sends, in this order:

1. the system prompt (the model's `system`);
2. the conversation summary;
3. the recent interactions: the last 10, each cut at 1,200 characters;
4. the active operations: up to 8, open ones first;
5. the current input: `current_user_parts` when set, as they are; else `current_user_message`, cut
   at 1,200 characters like the interactions.

### Refreshing the summary

The summary is the caller's state. The SDK does not refresh it on every turn. To refresh it
explicitly:

```rust
use fluxbee_ai_sdk::{refresh_conversation_summary, SummaryRefreshConfig, SummaryRefreshInput};

let llm = create_llm_client(AiProvider::OpenAi, api_key, None, account);
let refreshed = refresh_conversation_summary(
    llm.as_ref(),
    SummaryRefreshConfig {
        model: "gpt-5.5".to_string(),
        system_prompt: None,
        model_settings: None,
    },
    SummaryRefreshInput {
        thread_id: msg.meta.thread_id.clone(),
        scope_id: msg.meta.src_ilk.clone(),
        existing_summary: current_summary,
        recent_interactions,
        active_operations,
    },
)
.await?;
```

The helper:

- makes one plain call, which leaves its usage record like any other;
- asks for strict JSON;
- reads `existing_summary`, the recent interactions (up to 12) and the active operations (up to 8);
- returns a normalized `ConversationSummary`.

It does no advanced semantic compression; that belongs to cognition.

## Thread-state tools

`ThreadStateToolsProvider::new(store, src_ilk)` registers `thread_state_get`, `thread_state_put`
and `thread_state_delete` over a `ThreadStateStore` (`LanceDbThreadStateStore`), bound to one record:
the sender's. ai.generic builds it with the message's `src_ilk`. The frontdesk's tools are its own
(agent-tools-v1 T10): it registers `ThreadStateGetTool` and `ThreadStatePutTool` alone, because a
registered case ends saved as `completed` and is never deleted by the model.

- **The tools always read and write that record,** whatever key the model passes: the model never
  reaches another person's state. The input still asks for a non-empty `state_key`, and a call
  that passes the legacy `thread_id` argument is refused.
- **A put replaces the whole record.** `ttl_seconds` is stored with it, but nothing expires it.
- **Their cards** are the first three of [the tool catalog](../../docs/reference/agent-tools.md).

## Writing an AI node

- **Implement `AiNode`:** `on_message(&self, msg) -> Result<Option<Message>>`. Return the reply built
  with `build_reply_message_runtime_src(&msg, payload)`; the runtime fills in `routing.src`.
- **Its control plane:** `control_request(&msg, node_name, verbs)` says whether to run a control
  verb (`ControlRequest::Run`), refuse it (`Refuse`, with the reason) or leave the message to the
  node (`Other`). A verb runs only as the router's authority gate delivers it, a `system` message
  with the verb in its own case, and only from the node that sends it today
  (`control_caller_authorized`): `CONFIG_GET` and `CONFIG_SET` from SY.admin@motherbee,
  `NODE_STATUS_GET` and `NODE_HELP` from the orchestrator of the node's hive, `PING` and `STATUS`
  from either. ai.generic and the frontdesk answer a refusal with `unauthorized`.
- **What it never answers:** `is_infrastructure_message(&msg)`, checked first, says the message is
  a notice or a reply: one the router made, without `src_l2_name` (`UNREACHABLE`,
  `TTL_EXCEEDED`), or one named as a notice or a reply (`HELLO`, `ANNOUNCE`, `WITHDRAW`, any
  `*_RESPONSE`). `note_infrastructure_message(node_name, &msg)` logs it, a router notice as a WARN
  with its reason: the first of each notice and reason at once, then at most one every 10 s, with
  how many came since (`notices`; the ones between are debug lines), since a node that answers at
  the rate messages come (`ai_runtime_busy`, below) may get as many back. An answer to the
  router's notice would go to the router's own UUID, and the two would bounce it forever (FINDINGS
  A-112). SY.timer and the WF engine leave the same ones unanswered (FINDINGS A-218).
- **A tap copy never reaches it:** the runtime takes no message with `meta.via_tap` to
  `on_message` and answers none (`ignored_tap_copy`, FINDINGS A-248), whatever its kind; nor does
  a copy take a worker or a place in the queue (below). Each copy is a debug line and counts in
  `tap_copies_ignored`, in the runtime's periodic `ai runtime metrics` line (every 30 s by
  default), which shows at info. A copy is another node's message, which a router sends a tap's
  target to be observed: it keeps the original sender, so an answer would go to that sender.
- **Run it:** `NodeRuntime::new(dispatcher, node).run_with_config(RuntimeConfig::default())`.
  - The runtime drains the dispatcher's `AI_RUNTIME_CHANNEL` (`"incoming"`). Declare that channel in
    the dispatcher's profile:
    `.command_channel(AI_RUNTIME_CHANNEL).post_pending_rule(RouteMatch::Any, RouteTarget::Command(AI_RUNTIME_CHANNEL))`.
  - `RuntimeConfig::default()` gives 4 workers, a queue of 128 and `DEFAULT_HANDLER_TIMEOUT`
    (47 min) per LLM turn: a whole tool loop, `DEFAULT_MAX_TURNS` (8) model turns plus one plain
    call, each bounded by `MODEL_CALL_TIMEOUT`, and 2 min for its tools. Beside them, 64 places
    for what runs without the model (`DEFAULT_DIRECT_CAPACITY`), 8 of them at most for its `user`
    messages (`DEFAULT_DIRECT_USER_CAPACITY`), and 30 s for each (`DEFAULT_DIRECT_HANDLER_TIMEOUT`).
  - **Two lanes** (since 0.1.136, FINDINGS A-257). The runtime reads each message as it comes,
    however busy its workers are, and places it:
    - **What runs without the model** (`runs_without_model`) starts at once on the direct lane,
      beside the workers:
      - any `system` or `admin` message, which ai.generic and the frontdesk take to their control
        plane, never to the model (`NODE_STATUS_GET`, `NODE_HELP`, `CONFIG_GET`, `CONFIG_SET`,
        `VAULT_SECRET_CHANGED`, a notice or a reply);
      - the frontdesk's handoff, a `user` message from an IO node, marked `frontdesk_handoff` (its
        `meta.msg` and its payload's `type`) and whose payload parses as gov-common's
        `FrontdeskHandoffPayload`, which the frontdesk runs in code while the IO node waits for
        the answer (io.api 10 s, IO.cloud 20 s). A marked payload that does not parse (a field of
        the wrong type) is a conversation's turn at the frontdesk, which calls the model, so it is
        an LLM turn here (`tests/frontdesk_handoff_contract.rs` holds the runtime's copy to
        gov-common's).
    - **A known limit:** that rule is the runtime's, the same for every node on it. ai.generic,
      which has no handoff path, runs its model for a handoff with a thread, on the direct lane:
      up to 8 at once beside the 4 workers, each cut at 30 s (`ai_runtime_timeout`, maybe after
      billed model calls). No IO node sends an agent a handoff unless it is set to (io.api's
      `frontdesk_target` may name any node). FINDINGS A-257 plans an `AiNode` method by which each
      node says what it answers without its model: by default only `system` and `admin`
      messages, which ai.generic keeps.
    - Up to `direct_capacity` run at once, each within `direct_handler_timeout` (30 s: no sender
      waits longer; SY.admin waits 25 s for a node's `CONFIG_RESPONSE`, within its action's 30 s).
      A `system` or `admin` message may take any of those places; the `user` ones, the handoffs
      and the `ai_runtime_busy` answers, at most `direct_user_capacity`, so a burst of handoffs
      held by a slow SY.identity never takes the places of the status and config requests, and
      SY.identity gets the registrations of at most 8 handoffs at once (4 before, when they ran
      on the workers).
    - A message that finds every place it may take taken is dropped, neither run nor answered, as
      a `system` request whose handler fails: its caller's own timeout reports it. A handoff is
      dropped too rather than answered with an error: io.api reports `frontdesk_unavailable`
      either way, and IO.cloud takes any error answer for the frontdesk's refusal
      (`FRONTDESK_REJECTED`), its timeout for `FRONTDESK_UNAVAILABLE`.
    - **Everything else is an LLM turn:** a conversation's turn, a WF's message. It takes one of
      the `worker_pool_size` workers, in the order turns came, as before; up to `queue_capacity`
      wait for one. One more is refused and never run: it is answered at once with
      `ai_runtime_busy` (`retryable: true`: it never ran), sent from the direct lane in one of its
      `user` places, and its entry in the message's trace names the refusal (`busy`). When those
      places are all taken, the answer is not sent either.
    - So a status request or a handoff never waits behind an LLM turn, nothing waits without a
      bound, and with a free worker each message runs as it comes, as before. A `user` message the
      node answers in code that the runtime cannot tell apart (the frontdesk's case of the root
      tenant, a turn to an agent that is not configured, a handoff from a node that is no IO node)
      is a turn.
    - A config change runs on the direct lane, so it applies to the turns still waiting for a
      worker; a turn that has started keeps the config it read.
    - Each one turned away counts in the `ai runtime metrics` line (`turns_refused`,
      `direct_dropped`, `busy_answers_dropped`, beside `turns_queued` and `turns_waiting`). A WARN
      line names it at once (`ai runtime refused an LLM turn without running it`, `ai runtime
      dropped a message, neither run nor answered`, `ai runtime sent no ai_runtime_busy answer to a
      refused LLM turn`), then at most one every 10 s with how many since (`refused`, `dropped`);
      the metrics loop names the end of a burst that no later one would.
  - A handler runs once. When it returns an error or runs out of time (its lane's) it may already
    have called a model (billed) or a tool that acts, so the runtime never runs it again: a
    conversational message or a handoff gets an error reply (`ai_runtime_error` or
    `ai_runtime_timeout`, `retryable: false`); a `system` or `admin` request gets none, and its
    caller's own timeout reports it.
  - A task that ends with an error the runtime does not go on after (a reply too large to send, a
    handler that panicked) stops it, and no waiting turn starts then.
  - Only the write of a reply is retried (`RuntimeConfig::write_retry`): a send that timed out or
    was refused never queued it, so a reply cannot go out twice.
- **A reply goes from the agent to whoever spoke:**
  - its `dst_ilk` is the incoming `src_ilk`;
  - its `src_ilk` is the agent's own ILK (`RuntimeConfig::self_ilk_id`, read from
    `FLUXBEE_NODE_ILK_ID` by default), never the human's;
  - it keeps `thread_id` and drops `memory_package`;
  - it never carries `via_tap`, which only the router sets, on its copies;
  - its `meta.context` is the request's without `thread_id`, `src_ilk`, `response_envelope`, a WF's
    block (`wf`) and an `evaluation`, and without `final_response_contract` unless
    `ReplyContextOptions` keeps it.
- **A response contract** (`meta.context.response_envelope`, `json_object_v1`):
  `resolve_response_envelope_contract` reads it as a `ResponseContract`, its `output_schema` for the
  model and, when the contract names one, its `text_field`, a required `string` property.
  `ResponseContract::answer` turns the model's output into the answer: without `text_field`, the
  output as it is; with it, that field's value as the text and the whole object, checked against
  the schema, as the evaluation, which `set_reply_evaluation` puts in the reply's
  `meta.context.evaluation`. An output that is not that object is `InvalidStructuredOutput`.
- **A WF's menu** (`meta.context.wf.menu`,
  [docs/wf-orchestration-v1.md §5.3.6](../../docs/wf-orchestration-v1.md#536-the-build-design-and-the-gate)):
  `resolve_message_contract` reads the contract a message asks its answer under, a
  `MessageContract`: its menu, or its response contract. A message with both is
  `ConflictingContracts`.
  - `MenuContract::from_value` checks a menu again with wfcel's limits (`InvalidMenu`, naming the
    field) and adds `none`, with its param `reason`, last.
  - `MenuContract::prompt` is the menu as the model reads it, at the end of the WF's block: the
    help, the options with their params, and the line that gives the answer's shape. A menu has no
    `output_schema`, so the system prompt stays the role.
  - `MenuContract::answer` takes the object in the model's output, as a structured call does, and
    checks it: an option of the menu or `none`, each of its params with its type, a string
    `reply`. The answer's text is the `reply`, and its evaluation the checked
    `{reply, option, params}`, without the params the option does not declare. Anything else is
    `InvalidStructuredOutput`: `not_json`, `unknown_option:<x>`, `missing_param:<p>` or
    `type_mismatch:<p>:<type>`.
- **Input and output helpers:**
  - `resolve_model_input_from_payload_with_options` reads a `text/v1` payload, its `content_ref` and
    its attachments;
  - `build_model_user_content_parts` turns them into text, image and document parts;
  - `build_text_response` and `build_ai_behavior_response` build the reply payload, with artifacts.

## References

- [`fluxbee-sdk`](../fluxbee_sdk/README.md): connecting a node, the protocol, blobs, the admin
  gateway.
- [docs/router-stream-v1.md](../../docs/router-stream-v1.md): §6 cognition per communication, §7
  usage records.
- [docs/immediate-conversation-memory-spec.md](../../docs/immediate-conversation-memory-spec.md).
- [docs/ai-nodes-deploy-runbook.md](../../docs/ai-nodes-deploy-runbook.md): running an `ai.generic`
  node.
