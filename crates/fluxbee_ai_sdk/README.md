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
    (`AiSdkError::is_truncated_answer`); its usage record says `truncated`.
- **Every call has `MODEL_CALL_TIMEOUT` (5 min),** the provider's own retries included (Anthropic's
  `send`). Past it the call fails with `AiSdkError::Timeout` and its usage record says `abandoned`.
  The bound is for stuck connections: a long reasoning answer or an Archi stage fits in it.
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
- **Non-2xx answers and transport errors:** no record. Of Anthropic's retries, only the attempt that
  answered is recorded.
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
  in. ai.generic renders it as a text block in front of the user input.
- **No sequence number:** SY.cognition keeps the order of a communication with its own turn count.

**Not on the message:** the durable cognition entities (threads, contexts, reasons, cooccurrences,
scopes, scope instances, memories, episodes) and the message log (`turns`) live in SY.storage. A node
does not read them; only SY.cognition reads a communication's state back (`COGNITION_STATE_GET`).

So, in an AI node:

- use `meta.memory_package` for conversational context, and `meta.thread_id` as conversation
  metadata;
- keep hard state and immediate memory keyed by `src_ilk`;
- do not read the memory region (`jsr-memory-<hive>`) or PostgreSQL.

## Immediate memory

For continuity between turns, call `run_with_input(...)` with a `FunctionRunInput`. Its
`ImmediateConversationMemory` takes:

- `summary` (a `ConversationSummary`);
- `recent_interactions`;
- `active_operations`;
- `thread_id`, as conversation metadata;
- `scope_id`, which in ai.generic is the sender's `src_ilk`.

ai.generic stores immediate memory by `src_ilk`, not by `thread_id`, and only when
`runtime.immediate_memory.enabled` is true in its config (off by default).

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
  `TTL_EXCEEDED`), or one named as a notice or a reply (`HELLO`, `ECHO_REPLY`, any `*_RESPONSE`).
  `note_infrastructure_message(node_name, &msg)` logs it, a router notice as a WARN with its reason.
  An answer to the router's notice would go to the router's own UUID, and the two would bounce it
  forever (FINDINGS A-112). SY.timer and the WF engine follow the same rule.
- **Run it:** `NodeRuntime::new(dispatcher, node).run_with_config(RuntimeConfig::default())`.
  - The runtime drains the dispatcher's `AI_RUNTIME_CHANNEL` (`"incoming"`). Declare that channel in
    the dispatcher's profile:
    `.command_channel(AI_RUNTIME_CHANNEL).post_pending_rule(RouteMatch::Any, RouteTarget::Command(AI_RUNTIME_CHANNEL))`.
  - `RuntimeConfig::default()` gives 4 workers, a queue of 128 and `DEFAULT_HANDLER_TIMEOUT`
    (47 min) per message: a whole tool loop, `DEFAULT_MAX_TURNS` (8) model turns plus one plain
    call, each bounded by `MODEL_CALL_TIMEOUT`, and 2 min for its tools.
  - A handler runs once. When it returns an error or runs out of time it may already have called a
    model (billed) or a tool that acts, so the runtime never runs it again: a conversational message
    gets an error reply (`ai_runtime_error` or `ai_runtime_timeout`, `retryable: false`); a `system`
    or `admin` request gets none, and its caller's own timeout reports it.
  - Only the write of a reply is retried (`RuntimeConfig::write_retry`): a send that timed out or
    was refused never queued it, so a reply cannot go out twice.
- **A reply goes from the agent to whoever spoke:**
  - its `dst_ilk` is the incoming `src_ilk`;
  - its `src_ilk` is the agent's own ILK (`RuntimeConfig::self_ilk_id`, read from
    `FLUXBEE_NODE_ILK_ID` by default), never the human's;
  - it keeps `thread_id` and drops `memory_package`;
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
