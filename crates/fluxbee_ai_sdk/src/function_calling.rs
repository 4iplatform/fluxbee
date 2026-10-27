use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::immediate_memory::{
    ConversationSummary, FunctionRunInput, ImmediateConversationMemory, ImmediateInteraction,
    ImmediateInteractionKind, ImmediateOperation, ImmediateRole,
};
use crate::text_payload::ModelContentPart;
use crate::{AiSdkError, Result};

/// The longest description a provider API takes for one tool, in characters: OpenAI refuses a
/// longer one with 400 `string_above_max_length`. A card's rendered description must fit
/// ([`ToolCard::validate`]); the descriptions inside the input schema do not count.
pub const TOOL_DESCRIPTION_MAX_CHARS: usize = 1024;

/// What the model receives for one tool. Provider APIs take only these three: a name, a
/// description and the input's JSON schema. An agent tool's is rendered from its card
/// ([`ToolCard::model_definition`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_json_schema: Value,
}

/// A tool's card (docs/agent-tools-v1.md T2, §3): the one declaration of a tool an AI agent has.
/// The model's definition ([`Self::model_definition`]), the runtime's tool catalog and a node's
/// `NODE_HELP` answer all come from it, so none of them can say something the others do not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCard {
    /// The name the model calls it by.
    pub name: String,
    /// What it does, in one line.
    pub summary: String,
    /// When the agent should use it.
    pub when_to_use: String,
    /// What it cannot do, or does differently from what one would assume.
    pub limits: Vec<String>,
    /// When a message gets the tool: `always`, or the condition.
    pub present_when: String,
    /// The arguments, as a JSON schema: what the model receives.
    pub input: Value,
    /// The result, as a JSON schema. Its `description` is the "Returns:" line the model reads.
    pub output: Value,
    /// The codes of a `{"status": "error", "error_code": ..}` result.
    pub errors: Vec<ToolErrorCode>,
}

/// A runtime's tool catalog (docs/agent-tools-v1.md §3.3): the cards of every base tool it has,
/// generated from its code. It ships next to the runtime, as
/// `dist/runtimes/<runtime>/<version>/tools.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCatalog {
    pub runtime: String,
    /// The version it ships with. `None` in the repository's copy: the package writes the version.
    pub runtime_version: Option<String>,
    pub tools: Vec<ToolCard>,
}

/// One `error_code` a tool returns, and what it means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolErrorCode {
    pub code: String,
    pub meaning: String,
}

impl ToolErrorCode {
    pub fn new(code: impl Into<String>, meaning: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            meaning: meaning.into(),
        }
    }
}

impl ToolCard {
    /// What the model receives: the summary, when to use the tool, its limits and one line on what
    /// it returns (the output schema's `description`), with the input schema. `present_when` and
    /// `errors` stay out: the model gets a tool only when it is present, and reads an error's code
    /// in the result.
    pub fn model_definition(&self) -> FunctionToolDefinition {
        let mut description = self.summary.trim().to_string();
        let when_to_use = self.when_to_use.trim();
        if !when_to_use.is_empty() {
            description.push_str("\n\nWhen to use: ");
            description.push_str(when_to_use);
        }
        if !self.limits.is_empty() {
            description.push_str("\n\nLimits:");
            for limit in &self.limits {
                description.push_str("\n- ");
                description.push_str(limit.trim());
            }
        }
        if let Some(returns) = self.returns() {
            description.push_str("\n\nReturns: ");
            description.push_str(returns);
        }
        FunctionToolDefinition {
            name: self.name.clone(),
            description,
            parameters_json_schema: self.input.clone(),
        }
    }

    /// What the tool returns, in one line: the output schema's `description`.
    pub fn returns(&self) -> Option<&str> {
        self.output
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|returns| !returns.is_empty())
    }

    /// Whether the card says all it has to: a name, a one-line summary, when to use the tool and
    /// when it is present, an object schema for the input, an output schema with its one-line
    /// description, and each error code once with its meaning; and whether the description the
    /// model receives fits in [`TOOL_DESCRIPTION_MAX_CHARS`]. Lists every gap.
    pub fn validate(&self) -> std::result::Result<(), Vec<String>> {
        let mut gaps = Vec::new();
        let one_line = |text: &str| !text.trim().is_empty() && !text.contains('\n');
        if self.name.trim().is_empty() {
            gaps.push("name is empty".to_string());
        }
        if !one_line(&self.summary) {
            gaps.push("summary must be one non-empty line".to_string());
        }
        if self.when_to_use.trim().is_empty() {
            gaps.push("when_to_use is empty".to_string());
        }
        if self.present_when.trim().is_empty() {
            gaps.push("present_when is empty".to_string());
        }
        if self.limits.iter().any(|limit| limit.trim().is_empty()) {
            gaps.push("a limit is empty".to_string());
        }
        if self.input.get("type").and_then(Value::as_str) != Some("object") {
            gaps.push("input must be an object schema".to_string());
        }
        if !self.returns().is_some_and(one_line) {
            gaps.push("output needs a one-line description".to_string());
        }
        for (index, error) in self.errors.iter().enumerate() {
            if error.code.trim().is_empty() || error.meaning.trim().is_empty() {
                gaps.push(format!("error {index} needs a code and a meaning"));
            }
            if self.errors[..index]
                .iter()
                .any(|earlier| earlier.code == error.code)
            {
                gaps.push(format!("error code {} is listed twice", error.code));
            }
        }
        let description = self.model_definition().description.chars().count();
        if description > TOOL_DESCRIPTION_MAX_CHARS {
            gaps.push(format!(
                "the model's description has {description} characters, more than \
                 {TOOL_DESCRIPTION_MAX_CHARS}"
            ));
        }
        if gaps.is_empty() {
            Ok(())
        } else {
            Err(gaps)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionToolCall {
    pub call_id: String,
    #[serde(default)]
    pub response_id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionToolResult {
    pub call_id: String,
    #[serde(default)]
    pub response_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
    pub output: Value,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionLoopItem {
    SystemText {
        content: String,
    },
    UserText {
        content: String,
    },
    UserContentParts {
        content: Vec<ModelContentPart>,
    },
    AssistantText {
        content: String,
    },
    AssistantToolCalls {
        #[serde(default)]
        content: Option<String>,
        calls: Vec<FunctionToolCall>,
    },
    ToolResult {
        result: FunctionToolResult,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionModelTurnRequest {
    pub items: Vec<FunctionLoopItem>,
    pub tools: Vec<FunctionToolDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionModelTurnResponse {
    #[serde(default)]
    pub assistant_text: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<FunctionToolCall>,
    #[serde(default)]
    pub tokens_used: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionLoopRunResult {
    pub final_assistant_text: Option<String>,
    pub items: Vec<FunctionLoopItem>,
    #[serde(default)]
    pub tokens_used: u32,
}

/// Model turns a tool loop takes at most by default (`FunctionCallingConfig::default`).
pub const DEFAULT_MAX_TURNS: usize = 8;

#[derive(Debug, Clone)]
pub struct FunctionCallingConfig {
    pub max_turns: usize,
}

impl Default for FunctionCallingConfig {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
        }
    }
}

/// A tool of the tool loop. A tool an AI agent has implements [`AgentTool`] instead, and gets this
/// trait from its card; a SY node's own tools (Archi's, SY.admin's executor's, the frontdesk's
/// `ilk_register`) implement it directly.
#[async_trait]
pub trait FunctionTool: Send + Sync {
    fn definition(&self) -> FunctionToolDefinition;
    async fn call(&self, arguments: Value) -> Result<Value>;
}

/// A tool an AI agent has (docs/agent-tools-v1.md T1, T2). Its card is its only declaration: it
/// is a [`FunctionTool`] whose definition is rendered from the card, and it cannot declare another.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// The tool's card. It belongs to the tool, not to one instance of it: a catalog is built
    /// without one.
    fn card() -> ToolCard
    where
        Self: Sized;

    async fn call(&self, arguments: Value) -> Result<Value>;
}

#[async_trait]
impl<T: AgentTool> FunctionTool for T {
    fn definition(&self) -> FunctionToolDefinition {
        T::card().model_definition()
    }

    async fn call(&self, arguments: Value) -> Result<Value> {
        AgentTool::call(self, arguments).await
    }
}

#[async_trait]
pub trait FunctionCallingModel: Send + Sync {
    async fn run_turn(
        &self,
        request: FunctionModelTurnRequest,
    ) -> Result<FunctionModelTurnResponse>;
}

pub trait FunctionToolProvider: Send + Sync {
    fn register_tools(&self, registry: &mut FunctionToolRegistry) -> Result<()>;
}

/// The tools one model turn offers. Kept in name order, so every message sends the same
/// definitions in the same order: a stable prefix a provider can cache.
#[derive(Default, Clone)]
pub struct FunctionToolRegistry {
    tools: BTreeMap<String, Arc<dyn FunctionTool>>,
}

impl FunctionToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn FunctionTool>) -> Result<()> {
        let name = tool.definition().name;
        if name.trim().is_empty() {
            return Err(AiSdkError::Protocol(
                "function tool registration failed: empty tool name".to_string(),
            ));
        }
        if self.tools.contains_key(&name) {
            return Err(AiSdkError::Protocol(format!(
                "function tool registration failed: duplicate tool '{name}'"
            )));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    pub fn definitions(&self) -> Vec<FunctionToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn FunctionTool>> {
        self.tools.get(name)
    }
}

pub struct FunctionCallingRunner {
    config: FunctionCallingConfig,
}

impl FunctionCallingRunner {
    pub fn new(config: FunctionCallingConfig) -> Self {
        Self { config }
    }

    pub async fn run(
        &self,
        model: &dyn FunctionCallingModel,
        tools: &FunctionToolRegistry,
        user_input: impl Into<String>,
    ) -> Result<FunctionLoopRunResult> {
        self.run_with_input(model, tools, FunctionRunInput::new(user_input.into()))
            .await
    }

    pub async fn run_with_input(
        &self,
        model: &dyn FunctionCallingModel,
        tools: &FunctionToolRegistry,
        input: FunctionRunInput,
    ) -> Result<FunctionLoopRunResult> {
        let mut items = build_initial_items(input);
        let mut last_assistant_text = None;
        let mut total_tokens_used: u32 = 0;

        for _ in 0..self.config.max_turns {
            let response = model
                .run_turn(FunctionModelTurnRequest {
                    items: items.clone(),
                    tools: tools.definitions(),
                })
                .await?;

            total_tokens_used = total_tokens_used.saturating_add(response.tokens_used);

            let should_commit_assistant_text = response.tool_calls.is_empty();
            if should_commit_assistant_text {
                if let Some(text) = response.assistant_text.clone() {
                    last_assistant_text = Some(text.clone());
                    items.push(FunctionLoopItem::AssistantText { content: text });
                }
            }

            if response.tool_calls.is_empty() {
                return Ok(FunctionLoopRunResult {
                    final_assistant_text: last_assistant_text,
                    items,
                    tokens_used: total_tokens_used,
                });
            }

            let tool_calls = response.tool_calls;
            items.push(FunctionLoopItem::AssistantToolCalls {
                content: response.assistant_text,
                calls: tool_calls.clone(),
            });
            let results = dispatch_tool_calls(tools, tool_calls).await;
            for result in results {
                items.push(FunctionLoopItem::ToolResult { result });
            }
        }

        Err(AiSdkError::Protocol(format!(
            "function calling loop exceeded max_turns={}",
            self.config.max_turns
        )))
    }
}

/// Wraps a tool-loop model to note when a tool has run: a turn whose input carries a tool's result
/// comes after that tool ran. After a failed run, the caller knows from `tool_ran` whether sending
/// the same request again would run a tool again, so whether the failure can be called retryable.
pub struct ToolRunWatch<'a> {
    model: &'a dyn FunctionCallingModel,
    tool_ran: &'a AtomicBool,
}

impl<'a> ToolRunWatch<'a> {
    pub fn new(model: &'a dyn FunctionCallingModel, tool_ran: &'a AtomicBool) -> Self {
        Self { model, tool_ran }
    }
}

#[async_trait]
impl FunctionCallingModel for ToolRunWatch<'_> {
    async fn run_turn(
        &self,
        request: FunctionModelTurnRequest,
    ) -> Result<FunctionModelTurnResponse> {
        if request
            .items
            .iter()
            .any(|item| matches!(item, FunctionLoopItem::ToolResult { .. }))
        {
            self.tool_ran.store(true, Ordering::Relaxed);
        }
        self.model.run_turn(request).await
    }
}

const DEFAULT_RECENT_INTERACTIONS_MAX: usize = 10;
const DEFAULT_ACTIVE_OPERATIONS_MAX: usize = 8;
const DEFAULT_SUMMARY_MAX_CHARS: usize = 1600;
const DEFAULT_INTERACTION_MAX_CHARS: usize = 1200;

fn build_initial_items(input: FunctionRunInput) -> Vec<FunctionLoopItem> {
    let mut items = Vec::new();
    if let Some(memory) = normalize_immediate_memory(input.immediate_memory) {
        if let Some(summary) = render_summary_block(
            memory.thread_id.as_deref(),
            memory.scope_id.as_deref(),
            memory.summary.as_ref(),
        ) {
            items.push(FunctionLoopItem::SystemText { content: summary });
        }
        for interaction in memory.recent_interactions {
            items.push(interaction_to_loop_item(interaction));
        }
        if let Some(operations) = render_active_operations_block(&memory.active_operations) {
            items.push(FunctionLoopItem::SystemText {
                content: operations,
            });
        }
    }
    if let Some(parts) = input.current_user_parts.filter(|parts| !parts.is_empty()) {
        items.push(FunctionLoopItem::UserContentParts { content: parts });
    } else {
        // The message being answered goes whole: the limit is for the history around it (FINDINGS
        // A-71: a node that put its memory before the message lost the message itself).
        items.push(FunctionLoopItem::UserText {
            content: input.current_user_message.trim().to_string(),
        });
    }
    items
}

fn normalize_immediate_memory(
    memory: Option<ImmediateConversationMemory>,
) -> Option<ImmediateConversationMemory> {
    let mut memory = memory?;
    memory.summary = memory.summary.map(normalize_summary);
    memory.recent_interactions = normalize_recent_interactions(memory.recent_interactions);
    memory.active_operations = normalize_active_operations(memory.active_operations);
    Some(memory)
}

fn normalize_summary(summary: ConversationSummary) -> ConversationSummary {
    ConversationSummary {
        goal: summary.goal.map(|value| trim_text(&value, 240)),
        current_focus: summary.current_focus.map(|value| trim_text(&value, 240)),
        decisions: summary
            .decisions
            .into_iter()
            .map(|value| trim_text(&value, 200))
            .take(6)
            .collect(),
        confirmed_facts: summary
            .confirmed_facts
            .into_iter()
            .map(|value| trim_text(&value, 200))
            .take(6)
            .collect(),
        open_questions: summary
            .open_questions
            .into_iter()
            .map(|value| trim_text(&value, 200))
            .take(6)
            .collect(),
    }
}

fn normalize_recent_interactions(
    interactions: Vec<ImmediateInteraction>,
) -> Vec<ImmediateInteraction> {
    let start = interactions
        .len()
        .saturating_sub(DEFAULT_RECENT_INTERACTIONS_MAX);
    interactions
        .into_iter()
        .skip(start)
        .map(|interaction| ImmediateInteraction {
            content: trim_text(&interaction.content, DEFAULT_INTERACTION_MAX_CHARS),
            ..interaction
        })
        .collect()
}

fn normalize_active_operations(mut operations: Vec<ImmediateOperation>) -> Vec<ImmediateOperation> {
    operations.sort_by_key(|op| {
        let priority = if is_terminal_operation_status(&op.status) {
            1usize
        } else {
            0usize
        };
        (priority, std::cmp::Reverse(op.updated_at_ms.unwrap_or(0)))
    });
    operations
        .into_iter()
        .take(DEFAULT_ACTIVE_OPERATIONS_MAX)
        .map(|op| ImmediateOperation {
            resource_scope: op.resource_scope.map(|value| trim_text(&value, 120)),
            origin_thread_id: op.origin_thread_id.map(|value| trim_text(&value, 120)),
            origin_session_id: op.origin_session_id.map(|value| trim_text(&value, 120)),
            scope_id: op.scope_id.map(|value| trim_text(&value, 120)),
            action: trim_text(&op.action, 120),
            target: op.target.map(|value| trim_text(&value, 120)),
            status: trim_text(&op.status, 64),
            summary: trim_text(&op.summary, 240),
            ..op
        })
        .collect()
}

fn render_summary_block(
    thread_id: Option<&str>,
    scope_id: Option<&str>,
    summary: Option<&ConversationSummary>,
) -> Option<String> {
    let mut lines = vec!["Immediate conversation summary:".to_string()];
    if let Some(thread_id) = thread_id.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("thread_id: {thread_id}"));
    }
    if let Some(scope_id) = scope_id.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("scope_id: {scope_id}"));
    }
    if let Some(summary) = summary {
        if let Some(goal) = summary
            .goal
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            lines.push(format!("goal: {goal}"));
        }
        if let Some(focus) = summary
            .current_focus
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            lines.push(format!("current_focus: {focus}"));
        }
        append_list_lines(&mut lines, "decisions", &summary.decisions);
        append_list_lines(&mut lines, "confirmed_facts", &summary.confirmed_facts);
        append_list_lines(&mut lines, "open_questions", &summary.open_questions);
    }
    if lines.len() == 1 {
        return None;
    }
    Some(trim_text(&lines.join("\n"), DEFAULT_SUMMARY_MAX_CHARS))
}

fn append_list_lines(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    lines.push(format!("{label}:"));
    for value in values {
        if !value.trim().is_empty() {
            lines.push(format!("- {value}"));
        }
    }
}

fn render_active_operations_block(operations: &[ImmediateOperation]) -> Option<String> {
    if operations.is_empty() {
        return None;
    }
    let mut lines = vec!["Active operations:".to_string()];
    for operation in operations {
        let mut parts = vec![
            format!("operation_id={}", operation.operation_id),
            format!("action={}", operation.action),
            format!("status={}", operation.status),
        ];
        if let Some(target) = operation
            .target
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(format!("target={target}"));
        }
        if let Some(resource_scope) = operation
            .resource_scope
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(format!("resource_scope={resource_scope}"));
        }
        if let Some(origin_thread_id) = operation
            .origin_thread_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(format!("origin_thread_id={origin_thread_id}"));
        }
        if let Some(origin_session_id) = operation
            .origin_session_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(format!("origin_session_id={origin_session_id}"));
        }
        if let Some(scope_id) = operation
            .scope_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(format!("scope_id={scope_id}"));
        }
        parts.push(format!("summary={}", operation.summary));
        lines.push(format!("- {}", parts.join(" | ")));
    }
    Some(lines.join("\n"))
}

fn interaction_to_loop_item(interaction: ImmediateInteraction) -> FunctionLoopItem {
    let rendered = render_interaction_text(&interaction);
    match interaction.role {
        ImmediateRole::User => FunctionLoopItem::UserText { content: rendered },
        ImmediateRole::Assistant => FunctionLoopItem::AssistantText { content: rendered },
        ImmediateRole::System => FunctionLoopItem::SystemText { content: rendered },
    }
}

fn render_interaction_text(interaction: &ImmediateInteraction) -> String {
    let prefix = match interaction.kind {
        ImmediateInteractionKind::Text => None,
        ImmediateInteractionKind::ToolResult => Some("[tool_result] "),
        ImmediateInteractionKind::ToolError => Some("[tool_error] "),
        ImmediateInteractionKind::OperationSummary => Some("[operation_summary] "),
        ImmediateInteractionKind::SystemNote => Some("[system_note] "),
    };
    match prefix {
        Some(prefix) => format!("{prefix}{}", interaction.content),
        None => interaction.content.clone(),
    }
}

fn trim_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out = trimmed
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

fn is_terminal_operation_status(status: &str) -> bool {
    matches!(
        status,
        "succeeded" | "succeeded_after_timeout" | "failed" | "canceled" | "replaced"
    )
}

pub async fn dispatch_tool_calls(
    tools: &FunctionToolRegistry,
    calls: Vec<FunctionToolCall>,
) -> Vec<FunctionToolResult> {
    let mut out = Vec::with_capacity(calls.len());
    for call in calls {
        let maybe_tool = tools.get(&call.name);
        let result = match maybe_tool {
            Some(tool) => match tool.call(call.arguments.clone()).await {
                Ok(output) => FunctionToolResult {
                    call_id: call.call_id,
                    response_id: call.response_id,
                    name: call.name,
                    arguments: call.arguments,
                    output,
                    is_error: false,
                },
                Err(err) => FunctionToolResult {
                    call_id: call.call_id,
                    response_id: call.response_id,
                    name: call.name,
                    arguments: call.arguments,
                    output: Value::String(err.to_string()),
                    is_error: true,
                },
            },
            None => FunctionToolResult {
                call_id: call.call_id,
                response_id: call.response_id,
                name: call.name,
                arguments: call.arguments,
                output: Value::String("unknown_tool".to_string()),
                is_error: true,
            },
        };
        out.push(result);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::*;

    #[test]
    fn the_message_being_answered_is_never_cut() {
        let memory = "Conversation memory: ".to_string() + &"x".repeat(3_000);
        let message = format!("{memory}\n\nCurrent user message:\nwhat is my balance?");
        let items = build_initial_items(FunctionRunInput {
            current_user_message: message.clone(),
            current_user_parts: None,
            immediate_memory: None,
        });
        match items.last() {
            Some(FunctionLoopItem::UserText { content }) => assert_eq!(content, &message),
            other => panic!("expected the user's text last, got {other:?}"),
        }
    }

    #[derive(Clone, Default)]
    struct SequencedModel {
        calls: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl FunctionCallingModel for SequencedModel {
        async fn run_turn(
            &self,
            _request: FunctionModelTurnRequest,
        ) -> Result<FunctionModelTurnResponse> {
            let mut calls = self.calls.lock().await;
            let current = *calls;
            *calls += 1;
            match current {
                0 => Ok(FunctionModelTurnResponse {
                    assistant_text: Some("I will prepare the action now.".to_string()),
                    tool_calls: vec![FunctionToolCall {
                        call_id: "call-1".to_string(),
                        response_id: Some("resp-1".to_string()),
                        name: "demo_write".to_string(),
                        arguments: json!({"path":"/hives"}),
                    }],
                    tokens_used: 0,
                }),
                _ => Ok(FunctionModelTurnResponse {
                    assistant_text: Some(
                        "The tool failed, so there is nothing to confirm.".to_string(),
                    ),
                    tool_calls: Vec::new(),
                    tokens_used: 0,
                }),
            }
        }
    }

    struct DemoTool;

    #[derive(Clone, Default)]
    struct CaptureModel {
        last_request: Arc<Mutex<Option<FunctionModelTurnRequest>>>,
    }

    #[async_trait]
    impl FunctionCallingModel for CaptureModel {
        async fn run_turn(
            &self,
            request: FunctionModelTurnRequest,
        ) -> Result<FunctionModelTurnResponse> {
            *self.last_request.lock().await = Some(request);
            Ok(FunctionModelTurnResponse {
                assistant_text: Some("ok".to_string()),
                tool_calls: Vec::new(),
                tokens_used: 0,
            })
        }
    }

    #[async_trait]
    impl FunctionTool for DemoTool {
        fn definition(&self) -> FunctionToolDefinition {
            FunctionToolDefinition {
                name: "demo_write".to_string(),
                description: "demo".to_string(),
                parameters_json_schema: json!({
                    "type": "object"
                }),
            }
        }

        async fn call(&self, _arguments: Value) -> Result<Value> {
            Err(AiSdkError::Protocol("missing required fields".to_string()))
        }
    }

    /// A tool known only by its name, for the order test.
    struct NamedTool(&'static str);

    #[async_trait]
    impl FunctionTool for NamedTool {
        fn definition(&self) -> FunctionToolDefinition {
            FunctionToolDefinition {
                name: self.0.to_string(),
                description: "named".to_string(),
                parameters_json_schema: json!({ "type": "object" }),
            }
        }

        async fn call(&self, _arguments: Value) -> Result<Value> {
            Ok(json!({}))
        }
    }

    #[test]
    fn definitions_come_in_name_order_whatever_the_registration_order() {
        let mut registry = FunctionToolRegistry::new();
        for name in [
            "publish_html_page",
            "generate_csv_artifact",
            "thread_state_get",
        ] {
            registry
                .register(Arc::new(NamedTool(name)))
                .expect("tool should register");
        }
        let names: Vec<String> = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        assert_eq!(
            names,
            [
                "generate_csv_artifact",
                "publish_html_page",
                "thread_state_get"
            ]
        );
    }

    #[tokio::test]
    async fn ignores_provisional_assistant_text_when_tool_calls_exist() {
        let model = SequencedModel::default();
        let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
        let mut tools = FunctionToolRegistry::new();
        tools
            .register(Arc::new(DemoTool))
            .expect("tool should register");

        let result = runner
            .run(&model, &tools, "create hive worker-220")
            .await
            .expect("runner should succeed");

        assert_eq!(
            result.final_assistant_text.as_deref(),
            Some("The tool failed, so there is nothing to confirm.")
        );

        let assistant_messages = result
            .items
            .iter()
            .filter_map(|item| match item {
                FunctionLoopItem::AssistantText { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            assistant_messages,
            vec!["The tool failed, so there is nothing to confirm."]
        );
    }

    /// Fails the turn after the first: the first one asks for `demo_write`.
    struct FailsAfterTheFirstTurn(SequencedModel);

    #[async_trait]
    impl FunctionCallingModel for FailsAfterTheFirstTurn {
        async fn run_turn(
            &self,
            request: FunctionModelTurnRequest,
        ) -> Result<FunctionModelTurnResponse> {
            if *self.0.calls.lock().await > 0 {
                return Err(AiSdkError::Timeout(
                    "model call took longer than 300 s".into(),
                ));
            }
            self.0.run_turn(request).await
        }
    }

    struct FailsAtOnce;

    #[async_trait]
    impl FunctionCallingModel for FailsAtOnce {
        async fn run_turn(
            &self,
            _request: FunctionModelTurnRequest,
        ) -> Result<FunctionModelTurnResponse> {
            Err(AiSdkError::Timeout(
                "model call took longer than 300 s".into(),
            ))
        }
    }

    #[tokio::test]
    async fn the_watch_tells_whether_a_tool_ran_before_a_failure() {
        let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
        let mut tools = FunctionToolRegistry::new();
        tools
            .register(Arc::new(DemoTool))
            .expect("tool should register");

        let tool_ran = AtomicBool::new(false);
        let model = FailsAfterTheFirstTurn(SequencedModel::default());
        let watched = ToolRunWatch::new(&model, &tool_ran);
        assert!(runner.run(&watched, &tools, "write it").await.is_err());
        assert!(
            tool_ran.load(Ordering::Relaxed),
            "the second turn came after demo_write"
        );

        let tool_ran = AtomicBool::new(false);
        let watched = ToolRunWatch::new(&FailsAtOnce, &tool_ran);
        assert!(runner.run(&watched, &tools, "write it").await.is_err());
        assert!(
            !tool_ran.load(Ordering::Relaxed),
            "no tool ran before the first turn"
        );
    }

    struct CardedTool;

    #[async_trait]
    impl AgentTool for CardedTool {
        fn card() -> ToolCard {
            ToolCard {
                name: "hive_name".to_string(),
                summary: "Say which hive this node runs on.".to_string(),
                when_to_use: "When the user asks where the agent runs.".to_string(),
                limits: vec![
                    "Only the hive's name.".to_string(),
                    "Nothing about its nodes.".to_string(),
                ],
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

        async fn call(&self, _arguments: Value) -> Result<Value> {
            Ok(json!({"hive": "motherbee"}))
        }
    }

    /// The model reads the card: its summary, when to use the tool, its limits and what it
    /// returns, with the input schema; not when it is present, nor its error codes.
    #[tokio::test]
    async fn an_agent_tool_is_offered_to_the_model_as_its_card_says() {
        let mut tools = FunctionToolRegistry::new();
        tools
            .register(Arc::new(CardedTool))
            .expect("tool should register");
        let definitions = tools.definitions();
        assert_eq!(definitions, vec![CardedTool::card().model_definition()]);
        assert_eq!(definitions[0].name, "hive_name");
        assert_eq!(
            definitions[0].description,
            "Say which hive this node runs on.\n\n\
             When to use: When the user asks where the agent runs.\n\n\
             Limits:\n- Only the hive's name.\n- Nothing about its nodes.\n\n\
             Returns: `{hive}`, the hive's name."
        );
        assert_eq!(
            definitions[0].parameters_json_schema,
            CardedTool::card().input
        );

        let result = tools
            .get("hive_name")
            .expect("registered")
            .call(json!({}))
            .await
            .expect("call");
        assert_eq!(result, json!({"hive": "motherbee"}));
    }

    #[test]
    fn a_card_without_its_parts_is_reported() {
        assert_eq!(CardedTool::card().validate(), Ok(()));

        let mut card = CardedTool::card();
        card.summary = "Two\nlines.".to_string();
        card.when_to_use = " ".to_string();
        card.input = json!({"type": "string"});
        card.output = json!({"type": "object"});
        card.errors.push(ToolErrorCode::new("no_hive", "Again."));
        let gaps = card.validate().expect_err("an incomplete card");
        assert_eq!(
            gaps,
            vec![
                "summary must be one non-empty line",
                "when_to_use is empty",
                "input must be an object schema",
                "output needs a one-line description",
                "error code no_hive is listed twice",
            ]
        );
    }

    /// OpenAI refuses a tool whose description passes 1024 characters, so a card that renders a
    /// longer one is reported. The input schema's descriptions do not count.
    #[test]
    fn a_card_whose_description_passes_the_providers_limit_is_reported() {
        let mut card = CardedTool::card();
        let room = TOOL_DESCRIPTION_MAX_CHARS - card.model_definition().description.chars().count();
        card.input["properties"]["note"] = json!({
            "type": "string",
            "description": "x".repeat(2 * TOOL_DESCRIPTION_MAX_CHARS)
        });
        card.limits.push("é".repeat(room - "\n- ".len()));
        assert_eq!(
            card.model_definition().description.chars().count(),
            TOOL_DESCRIPTION_MAX_CHARS
        );
        assert_eq!(
            card.validate(),
            Ok(()),
            "exactly the limit, counted in characters"
        );

        card.limits.push("!".to_string());
        assert_eq!(
            card.validate(),
            Err(vec![format!(
                "the model's description has {} characters, more than 1024",
                TOOL_DESCRIPTION_MAX_CHARS + "\n- !".len()
            )])
        );
    }

    #[tokio::test]
    async fn function_run_input_can_seed_multimodal_user_parts() {
        let model = CaptureModel::default();
        let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
        let tools = FunctionToolRegistry::new();
        let input = FunctionRunInput {
            current_user_message: "fallback".to_string(),
            current_user_parts: Some(vec![
                ModelContentPart::Text {
                    text: "hola".to_string(),
                },
                ModelContentPart::Image {
                    media_type: "image/png".to_string(),
                    data_base64: "AAA".to_string(),
                    detail: None,
                },
            ]),
            immediate_memory: None,
        };

        let result = runner
            .run_with_input(&model, &tools, input)
            .await
            .expect("run should succeed");
        assert_eq!(result.final_assistant_text.as_deref(), Some("ok"));

        let request = model
            .last_request
            .lock()
            .await
            .clone()
            .expect("request should be captured");
        assert!(request.items.iter().any(|item| matches!(
            item,
            FunctionLoopItem::UserContentParts { content } if content.len() == 2
        )));
    }
}
