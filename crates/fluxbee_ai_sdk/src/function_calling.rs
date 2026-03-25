use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::immediate_memory::{
    ConversationSummary, FunctionRunInput, ImmediateConversationMemory, ImmediateInteraction,
    ImmediateInteractionKind, ImmediateOperation, ImmediateRole,
};
use crate::{AiSdkError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_json_schema: Value,
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
    SystemText { content: String },
    UserText { content: String },
    AssistantText { content: String },
    ToolResult { result: FunctionToolResult },
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionLoopRunResult {
    pub final_assistant_text: Option<String>,
    pub items: Vec<FunctionLoopItem>,
}

#[derive(Debug, Clone)]
pub struct FunctionCallingConfig {
    pub max_turns: usize,
}

impl Default for FunctionCallingConfig {
    fn default() -> Self {
        Self { max_turns: 8 }
    }
}

#[async_trait]
pub trait FunctionTool: Send + Sync {
    fn definition(&self) -> FunctionToolDefinition;
    async fn call(&self, arguments: Value) -> Result<Value>;
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

#[derive(Default, Clone)]
pub struct FunctionToolRegistry {
    tools: HashMap<String, Arc<dyn FunctionTool>>,
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
        let mut last_assistant_text = None;

        for _ in 0..self.config.max_turns {
            let response = model
                .run_turn(FunctionModelTurnRequest {
                    items: items.clone(),
                    tools: tools.definitions(),
                })
                .await?;

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
                });
            }

            let results = dispatch_tool_calls(tools, response.tool_calls).await;
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
    items.push(FunctionLoopItem::UserText {
        content: trim_text(&input.current_user_message, DEFAULT_INTERACTION_MAX_CHARS),
    });
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
                }),
                _ => Ok(FunctionModelTurnResponse {
                    assistant_text: Some(
                        "The tool failed, so there is nothing to confirm.".to_string(),
                    ),
                    tool_calls: Vec::new(),
                }),
            }
        }
    }

    struct DemoTool;

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
}
