use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::errors::{AiSdkError, Result};
use crate::function_calling::{
    FunctionCallingModel, FunctionLoopItem, FunctionModelTurnRequest, FunctionModelTurnResponse,
    FunctionToolCall,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub system: Option<String>,
    pub input: String,
    pub max_output_tokens: Option<u32>,
    pub model_settings: Option<ModelSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LlmStreamEvent {
    Delta(String),
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelSettings {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_output_tokens: Option<u32>,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse>;

    async fn generate_stream(&self, request: LlmRequest) -> Result<Vec<LlmStreamEvent>> {
        let response = self.generate(request).await?;
        Ok(vec![
            LlmStreamEvent::Delta(response.content),
            LlmStreamEvent::Completed,
        ])
    }
}

#[derive(Clone, Default)]
pub struct MockLlmClient {
    responses: Arc<Mutex<VecDeque<LlmResponse>>>,
    stream_responses: Arc<Mutex<VecDeque<Vec<LlmStreamEvent>>>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl MockLlmClient {
    pub fn with_responses(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            stream_responses: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_stream_responses(responses: Vec<Vec<LlmStreamEvent>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            stream_responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn push_response(&self, response: LlmResponse) {
        self.responses.lock().await.push_back(response);
    }

    pub async fn push_stream_response(&self, response: Vec<LlmStreamEvent>) {
        self.stream_responses.lock().await.push_back(response);
    }

    pub async fn take_requests(&self) -> Vec<LlmRequest> {
        let mut guard = self.requests.lock().await;
        std::mem::take(&mut *guard)
    }
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse> {
        self.requests.lock().await.push(request);
        let maybe = self.responses.lock().await.pop_front();
        match maybe {
            Some(value) => Ok(value),
            None => Err(AiSdkError::Protocol(
                "mock llm has no queued response".to_string(),
            )),
        }
    }

    async fn generate_stream(&self, request: LlmRequest) -> Result<Vec<LlmStreamEvent>> {
        self.requests.lock().await.push(request);

        if let Some(stream) = self.stream_responses.lock().await.pop_front() {
            return Ok(stream);
        }

        let maybe = self.responses.lock().await.pop_front();
        match maybe {
            Some(value) => Ok(vec![
                LlmStreamEvent::Delta(value.content),
                LlmStreamEvent::Completed,
            ]),
            None => Err(AiSdkError::Protocol(
                "mock llm has no queued response".to_string(),
            )),
        }
    }
}

#[derive(Clone)]
pub struct OpenAiResponsesClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl OpenAiResponsesClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: "https://api.openai.com/v1/responses".to_string(),
            api_key: api_key.into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn function_model(
        self,
        model: impl Into<String>,
        system: Option<String>,
        model_settings: ModelSettings,
    ) -> OpenAiFunctionCallingModel {
        OpenAiFunctionCallingModel {
            client: self,
            model: model.into(),
            system,
            model_settings,
        }
    }
}

#[async_trait]
impl LlmClient for OpenAiResponsesClient {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse> {
        let mut input = vec![];
        if let Some(system) = request.system.clone() {
            input.push(json!({
                "role": "system",
                "content": [{"type":"input_text","text": system}],
            }));
        }
        input.push(json!({
            "role": "user",
            "content": [{"type":"input_text","text": request.input}],
        }));

        let max_output_tokens = request
            .model_settings
            .as_ref()
            .and_then(|v| v.max_output_tokens)
            .or(request.max_output_tokens);
        let temperature = request.model_settings.as_ref().and_then(|v| v.temperature);
        let top_p = request.model_settings.as_ref().and_then(|v| v.top_p);

        let body = json!({
            "model": request.model,
            "input": input,
            "max_output_tokens": max_output_tokens,
            "temperature": temperature,
            "top_p": top_p,
        });

        let auth = format!("Bearer {}", self.api_key);
        let response = self
            .http
            .post(&self.base_url)
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let value: serde_json::Value = response.json().await?;
        if !status.is_success() {
            return Err(AiSdkError::Protocol(format!(
                "openai error status={} body={}",
                status, value
            )));
        }

        // Responses API returns output_text for text generations.
        let text = value
            .get("output_text")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .or_else(|| {
                value
                    .get("output")
                    .and_then(|out| out.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|item| item.get("content"))
                    .and_then(|content| content.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|item| item.get("text"))
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string)
            })
            .ok_or_else(|| AiSdkError::Protocol("responses payload missing text output".into()))?;

        Ok(LlmResponse { content: text })
    }
}

#[derive(Clone)]
pub struct OpenAiFunctionCallingModel {
    client: OpenAiResponsesClient,
    model: String,
    system: Option<String>,
    model_settings: ModelSettings,
}

#[async_trait]
impl FunctionCallingModel for OpenAiFunctionCallingModel {
    async fn run_turn(
        &self,
        request: FunctionModelTurnRequest,
    ) -> Result<FunctionModelTurnResponse> {
        let (pending_tool_results, request_items) = split_pending_tool_results(request.items);
        let previous_response_id = pending_tool_results
            .first()
            .and_then(|result| result.response_id.clone());

        let mut input = Vec::new();
        if previous_response_id.is_some() {
            for result in pending_tool_results {
                let output_text = match result.output {
                    Value::String(s) => s,
                    other => serde_json::to_string(&other).unwrap_or_else(|_| "{}".to_string()),
                };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": result.call_id,
                    "output": output_text,
                }));
            }
        } else {
            if let Some(system) = &self.system {
                input.push(json!({
                    "role": "system",
                    "content": [{"type":"input_text","text": system}],
                }));
            }

            for item in request_items {
                match item {
                    FunctionLoopItem::UserText { content } => {
                        input.push(json!({
                            "role": "user",
                            "content": [{"type":"input_text","text": content}],
                        }));
                    }
                    FunctionLoopItem::ToolResult { result } => {
                        let output_text = match result.output {
                            Value::String(s) => s,
                            other => {
                                serde_json::to_string(&other).unwrap_or_else(|_| "{}".to_string())
                            }
                        };
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": result.call_id,
                            "output": output_text,
                        }));
                    }
                    other => input.push(loop_item_to_openai_input(other)),
                }
            }
        }

        let tools = request
            .tools
            .into_iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters_json_schema,
                })
            })
            .collect::<Vec<_>>();

        let mut body = json!({
            "model": self.model,
            "input": input,
            "tools": tools,
            "parallel_tool_calls": true,
            "max_output_tokens": self.model_settings.max_output_tokens,
            "temperature": self.model_settings.temperature,
            "top_p": self.model_settings.top_p,
        });
        if let Some(previous_response_id) = previous_response_id {
            body["previous_response_id"] = Value::String(previous_response_id);
        }

        let auth = format!("Bearer {}", self.client.api_key);
        let response = self
            .client
            .http
            .post(&self.client.base_url)
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let value: Value = response.json().await?;
        if !status.is_success() {
            return Err(AiSdkError::Protocol(format!(
                "openai function calling error status={} body={}",
                status, value
            )));
        }

        let response_id = value
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let mut assistant_text = value
            .get("output_text")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let mut tool_calls = Vec::<FunctionToolCall>::new();

        if let Some(items) = value.get("output").and_then(Value::as_array) {
            for item in items {
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
                if item_type == "function_call" {
                    let call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("id").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arguments = parse_tool_arguments(
                        item.get("arguments")
                            .cloned()
                            .unwrap_or(Value::Object(Default::default())),
                    );
                    if !call_id.is_empty() && !name.is_empty() {
                        tool_calls.push(FunctionToolCall {
                            call_id,
                            response_id: response_id.clone(),
                            name,
                            arguments,
                        });
                    }
                    continue;
                }

                if assistant_text.is_none() && item_type == "message" {
                    assistant_text =
                        item.get("content")
                            .and_then(Value::as_array)
                            .and_then(|arr| {
                                arr.iter().find_map(|content_item| {
                                    content_item
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .map(ToString::to_string)
                                })
                            });
                }
            }
        }

        Ok(FunctionModelTurnResponse {
            assistant_text,
            tool_calls,
        })
    }
}

fn split_pending_tool_results(
    items: Vec<FunctionLoopItem>,
) -> (
    Vec<crate::function_calling::FunctionToolResult>,
    Vec<FunctionLoopItem>,
) {
    let mut prefix = items;
    let mut tail = Vec::new();
    let mut expected_response_id: Option<Option<String>> = None;

    loop {
        let Some(last) = prefix.last() else {
            break;
        };
        match last {
            FunctionLoopItem::ToolResult { result } => {
                if let Some(expected) = &expected_response_id {
                    if expected != &result.response_id {
                        break;
                    }
                } else {
                    expected_response_id = Some(result.response_id.clone());
                }
                let popped = prefix.pop().expect("last item exists");
                if let FunctionLoopItem::ToolResult { result } = popped {
                    tail.push(result);
                }
            }
            _ => break,
        }
    }

    tail.reverse();
    (tail, prefix)
}

fn parse_tool_arguments(raw: Value) -> Value {
    match raw {
        Value::String(s) => serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)),
        other => other,
    }
}

fn loop_item_to_openai_input(item: FunctionLoopItem) -> Value {
    match item {
        FunctionLoopItem::SystemText { content } => json!({
            "role": "system",
            "content": [{"type":"input_text","text": content}],
        }),
        FunctionLoopItem::UserText { content } => json!({
            "role": "user",
            "content": [{"type":"input_text","text": content}],
        }),
        FunctionLoopItem::AssistantText { content } => json!({
            "role": "assistant",
            "content": [{"type":"output_text","text": content}],
        }),
        FunctionLoopItem::ToolResult { result } => {
            let output_text = match result.output {
                Value::String(s) => s,
                other => serde_json::to_string(&other).unwrap_or_else(|_| "{}".to_string()),
            };
            json!({
                "type": "function_call_output",
                "call_id": result.call_id,
                "output": output_text,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::function_calling::FunctionToolResult;

    #[test]
    fn assistant_items_are_serialized_as_output_text() {
        let value = loop_item_to_openai_input(FunctionLoopItem::AssistantText {
            content: "previous assistant".to_string(),
        });
        assert_eq!(value["role"], "assistant");
        assert_eq!(value["content"][0]["type"], "output_text");
        assert_eq!(value["content"][0]["text"], "previous assistant");
    }

    #[test]
    fn system_and_user_items_stay_as_input_text() {
        let system = loop_item_to_openai_input(FunctionLoopItem::SystemText {
            content: "summary".to_string(),
        });
        let user = loop_item_to_openai_input(FunctionLoopItem::UserText {
            content: "current user".to_string(),
        });
        assert_eq!(system["content"][0]["type"], "input_text");
        assert_eq!(user["content"][0]["type"], "input_text");
    }

    #[test]
    fn tool_results_remain_function_call_outputs() {
        let value = loop_item_to_openai_input(FunctionLoopItem::ToolResult {
            result: FunctionToolResult {
                call_id: "call-1".to_string(),
                response_id: Some("resp-1".to_string()),
                name: "demo".to_string(),
                arguments: json!({"arg":"value"}),
                output: json!({"ok": true}),
                is_error: false,
            },
        });
        assert_eq!(value["type"], "function_call_output");
        assert_eq!(value["call_id"], "call-1");
        assert_eq!(value["output"], "{\"ok\":true}");
    }
}
