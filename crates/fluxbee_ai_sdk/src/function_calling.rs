use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub output: Value,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionLoopItem {
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
    async fn run_turn(&self, request: FunctionModelTurnRequest) -> Result<FunctionModelTurnResponse>;
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
        let mut items = vec![FunctionLoopItem::UserText {
            content: user_input.into(),
        }];
        let mut last_assistant_text = None;

        for _ in 0..self.config.max_turns {
            let response = model
                .run_turn(FunctionModelTurnRequest {
                    items: items.clone(),
                    tools: tools.definitions(),
                })
                .await?;

            if let Some(text) = response.assistant_text.clone() {
                last_assistant_text = Some(text.clone());
                items.push(FunctionLoopItem::AssistantText { content: text });
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
                    output,
                    is_error: false,
                },
                Err(err) => FunctionToolResult {
                    call_id: call.call_id,
                    response_id: call.response_id,
                    name: call.name,
                    output: Value::String(err.to_string()),
                    is_error: true,
                },
            },
            None => FunctionToolResult {
                call_id: call.call_id,
                response_id: call.response_id,
                name: call.name,
                output: Value::String("unknown_tool".to_string()),
                is_error: true,
            },
        };
        out.push(result);
    }
    out
}
