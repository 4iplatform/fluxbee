use std::collections::VecDeque;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use reqwest::header::{HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::errors::{AiSdkError, Result};
use crate::function_calling::{
    FunctionCallingModel, FunctionLoopItem, FunctionModelTurnRequest, FunctionModelTurnResponse,
    FunctionToolCall,
};
use crate::text_payload::ModelContentPart;

pub const DEFAULT_HIVE_AI_PROVIDER: AiProvider = AiProvider::OpenAi;
pub const DEFAULT_HIVE_OPENAI_MODEL: &str = "gpt-5.5";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
    // Canonical wire form is "openai" — it MUST match as_str()/Display/FromStr, the
    // `providers.openai` config key, and the spoke hive.yaml the orchestrator GENERATES
    // (sy_orchestrator writes `default_provider: openai` via Display). The bare
    // rename_all=snake_case would expect "open_ai", which broke fresh installs (the
    // packaged hive.yaml.example says `openai`) and would break every generated spoke
    // yaml. Caught by the vault cold-boot VM E2E on 2026-07-21. "open_ai" stays as a
    // read-only alias for anything written during the brief window it was the wire form.
    #[default]
    #[serde(rename = "openai", alias = "open_ai")]
    OpenAi,
    Anthropic,
}

impl AiProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn resource_type(self) -> fluxbee_sdk::ResourceType {
        match self {
            Self::OpenAi => fluxbee_sdk::ResourceType::Openai,
            Self::Anthropic => fluxbee_sdk::ResourceType::Anthropic,
        }
    }
}

impl fmt::Display for AiProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AiProvider {
    type Err = AiSdkError;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            other => Err(AiSdkError::Protocol(format!(
                "unsupported AI provider '{other}' (expected openai or anthropic)"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiProviderModelConfig {
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AiProviderConfigs {
    #[serde(default)]
    pub openai: Option<AiProviderModelConfig>,
    #[serde(default)]
    pub anthropic: Option<AiProviderModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct HiveAiConfig {
    #[serde(default)]
    pub default_provider: AiProvider,
    #[serde(default)]
    pub providers: AiProviderConfigs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveAiEngine {
    pub provider: AiProvider,
    pub model: String,
}

impl HiveAiConfig {
    pub fn effective(&self) -> Result<EffectiveAiEngine> {
        let config = match self.default_provider {
            AiProvider::OpenAi => self.providers.openai.as_ref(),
            AiProvider::Anthropic => self.providers.anthropic.as_ref(),
        }
        .ok_or_else(|| {
            AiSdkError::Protocol(format!(
                "hive ai.providers.{} is required for selected default_provider",
                self.default_provider
            ))
        })?;
        let model = config.model.trim();
        if model.is_empty() {
            return Err(AiSdkError::Protocol(format!(
                "hive ai.providers.{}.model must not be empty",
                self.default_provider
            )));
        }
        Ok(EffectiveAiEngine {
            provider: self.default_provider,
            model: model.to_string(),
        })
    }

    pub fn fallback() -> EffectiveAiEngine {
        EffectiveAiEngine {
            provider: DEFAULT_HIVE_AI_PROVIDER,
            model: DEFAULT_HIVE_OPENAI_MODEL.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub system: Option<String>,
    pub input: String,
    #[serde(default)]
    pub input_parts: Option<Vec<ModelContentPart>>,
    #[serde(default)]
    pub output_schema: Option<OutputSchemaSpec>,
    pub max_output_tokens: Option<u32>,
    pub model_settings: Option<ModelSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutputSchemaSpec {
    pub name: String,
    pub schema: Value,
    pub strict: bool,
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

impl OutputSchemaSpec {
    pub fn new(name: impl Into<String>, schema: Value, strict: bool) -> Result<Self> {
        let spec = Self {
            name: name.into(),
            schema,
            strict,
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn json_schema(&self) -> &Value {
        &self.schema
    }

    pub fn strict(&self) -> bool {
        self.strict
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(AiSdkError::InvalidResponseContract {
                detail: "schema name is required".to_string(),
            });
        }
        if !self.schema.is_object() {
            return Err(AiSdkError::InvalidResponseContract {
                detail: "schema must be a JSON object".to_string(),
            });
        }
        Ok(())
    }
}

pub fn build_output_schema_fallback_instruction(
    output_schema: &OutputSchemaSpec,
) -> Result<String> {
    output_schema.validate()?;
    let schema_obj = output_schema.json_schema().as_object().ok_or_else(|| {
        AiSdkError::InvalidResponseContract {
            detail: "schema must be a JSON object".to_string(),
        }
    })?;
    let properties = schema_obj
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "object schema.properties is required".to_string(),
        })?;
    let required = schema_obj
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "object schema.required is required".to_string(),
        })?;

    let mut field_specs = Vec::with_capacity(properties.len());
    for (name, value) in properties {
        let field_type = value.get("type").and_then(Value::as_str).ok_or_else(|| {
            AiSdkError::InvalidResponseContract {
                detail: format!("schema.type is required for field '{name}'"),
            }
        })?;
        let mut part = format!("{name}:{field_type}");
        if let Some(enum_values) = value.get("enum").and_then(Value::as_array) {
            let joined = enum_values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|");
            if !joined.is_empty() {
                part.push('(');
                part.push_str(&joined);
                part.push(')');
            }
        }
        field_specs.push(part);
    }

    let required_fields = required
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let fields = field_specs.join(", ");
    Ok(format!(
        "Return only valid JSON. Do not include markdown fences or extra text. The top-level value must be an object with exactly these fields: {fields}. Required fields: {required_fields}. Do not include properties outside this schema."
    ))
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
        let input = build_openai_input_items(&request);

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
        let mut body = body;
        if let Some(output_schema) = &request.output_schema {
            body["text"] = json!({
                "format": build_openai_response_format(output_schema)?,
            });
        }

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
            return Err(openai_status_error(status.as_u16(), &value));
        }
        openai_check_truncation(&value)?;

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

        let text = match &request.output_schema {
            Some(output_schema) => validate_structured_output(&text, output_schema)?,
            None => text,
        };

        Ok(LlmResponse { content: text })
    }
}

fn build_openai_input_items(request: &LlmRequest) -> Vec<Value> {
    let mut input = vec![];
    if let Some(system) = request.system.clone() {
        input.push(json!({
            "role": "system",
            "content": [{"type":"input_text","text": system}],
        }));
    }
    let user_content = request
        .input_parts
        .as_ref()
        .map(|parts| parts.iter().map(model_part_to_openai).collect::<Vec<_>>())
        .unwrap_or_else(|| {
            vec![json!({
                "type":"input_text",
                "text": request.input
            })]
        });
    input.push(json!({
        "role": "user",
        "content": user_content,
    }));
    input
}

fn model_part_to_openai(part: &ModelContentPart) -> Value {
    match part {
        ModelContentPart::Text { text } => json!({
            "type": "input_text",
            "text": text,
        }),
        ModelContentPart::Image {
            media_type,
            data_base64,
            detail,
        } => json!({
            "type": "input_image",
            "image_url": format!("data:{media_type};base64,{data_base64}"),
            "detail": detail.clone().unwrap_or_else(|| "auto".to_string()),
        }),
        ModelContentPart::Document {
            media_type,
            filename,
            data_base64,
        } => json!({
            "type": "input_file",
            "filename": filename,
            "file_data": format!("data:{media_type};base64,{data_base64}"),
        }),
    }
}

fn build_openai_response_format(output_schema: &OutputSchemaSpec) -> Result<Value> {
    output_schema.validate()?;
    Ok(json!({
        "type": "json_schema",
        "name": output_schema.name(),
        "schema": output_schema.json_schema(),
        "strict": output_schema.strict(),
    }))
}

fn validate_structured_output(raw: &str, output_schema: &OutputSchemaSpec) -> Result<String> {
    let candidate = extract_json_candidate(raw)?;
    let parsed: Value =
        serde_json::from_str(&candidate).map_err(|err| AiSdkError::InvalidStructuredOutput {
            detail: format!("json_parse_error: {err}"),
        })?;
    validate_value_against_schema(&parsed, output_schema.json_schema())?;
    serde_json::to_string(&parsed).map_err(AiSdkError::from)
}

fn extract_json_candidate(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AiSdkError::InvalidStructuredOutput {
            detail: "empty_response".to_string(),
        });
    }

    if let Some(stripped) = trimmed.strip_prefix("```") {
        let stripped = stripped.trim_start_matches(|c| c != '\n');
        let stripped = stripped.strip_prefix('\n').unwrap_or(stripped);
        let stripped = stripped.strip_suffix("```").unwrap_or(stripped);
        let inner = stripped.trim();
        if inner.is_empty() {
            return Err(AiSdkError::InvalidStructuredOutput {
                detail: "empty_json_fence".to_string(),
            });
        }
        return Ok(inner.to_string());
    }

    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Ok(trimmed.to_string());
    }

    match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if start < end => Ok(trimmed[start..=end].to_string()),
        _ => Err(AiSdkError::InvalidStructuredOutput {
            detail: "json_candidate_not_found".to_string(),
        }),
    }
}

fn validate_value_against_schema(value: &Value, schema: &Value) -> Result<()> {
    let schema_obj = schema
        .as_object()
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "schema must be a JSON object".to_string(),
        })?;
    let schema_type = schema_obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "schema.type is required".to_string(),
        })?;

    match schema_type {
        "object" => validate_object_against_schema(value, schema_obj),
        other => Err(AiSdkError::InvalidResponseContract {
            detail: format!("unsupported schema root type: {other}"),
        }),
    }
}

fn validate_object_against_schema(
    value: &Value,
    schema_obj: &serde_json::Map<String, Value>,
) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| AiSdkError::InvalidStructuredOutput {
            detail: "root_not_object".to_string(),
        })?;
    let properties = schema_obj
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "object schema.properties is required".to_string(),
        })?;
    let required = schema_obj
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "object schema.required is required".to_string(),
        })?;

    for field in required {
        let field_name = field
            .as_str()
            .ok_or_else(|| AiSdkError::InvalidResponseContract {
                detail: "schema.required entries must be strings".to_string(),
            })?;
        if !properties.contains_key(field_name) {
            return Err(AiSdkError::InvalidResponseContract {
                detail: format!("required field '{field_name}' missing from schema.properties"),
            });
        }
        if !obj.contains_key(field_name) {
            return Err(AiSdkError::InvalidStructuredOutput {
                detail: format!("missing_required_field:{field_name}"),
            });
        }
    }

    let additional_properties = schema_obj
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !additional_properties {
        for key in obj.keys() {
            if !properties.contains_key(key) {
                return Err(AiSdkError::InvalidStructuredOutput {
                    detail: format!("unexpected_property:{key}"),
                });
            }
        }
    }

    for (key, field_value) in obj {
        if let Some(field_schema) = properties.get(key) {
            validate_field_against_schema(key, field_value, field_schema)?;
        }
    }

    Ok(())
}

fn validate_field_against_schema(field_name: &str, value: &Value, schema: &Value) -> Result<()> {
    let schema_obj = schema
        .as_object()
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: format!("schema for field '{field_name}' must be an object"),
        })?;
    let field_type = schema_obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: format!("schema.type is required for field '{field_name}'"),
        })?;

    let type_matches = match field_type {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        other => {
            return Err(AiSdkError::InvalidResponseContract {
                detail: format!("unsupported field type '{other}' for field '{field_name}'"),
            });
        }
    };
    if !type_matches {
        return Err(AiSdkError::InvalidStructuredOutput {
            detail: format!("type_mismatch:{field_name}:{field_type}"),
        });
    }

    if let Some(enum_values) = schema_obj.get("enum") {
        let enum_items =
            enum_values
                .as_array()
                .ok_or_else(|| AiSdkError::InvalidResponseContract {
                    detail: format!("enum for field '{field_name}' must be an array"),
                })?;
        if field_type != "string" {
            return Err(AiSdkError::InvalidResponseContract {
                detail: format!("enum is only supported for string field '{field_name}'"),
            });
        }
        let field_value = value
            .as_str()
            .ok_or_else(|| AiSdkError::InvalidStructuredOutput {
                detail: format!("type_mismatch:{field_name}:string"),
            })?;
        let matches = enum_items
            .iter()
            .any(|candidate| candidate.as_str() == Some(field_value));
        if !matches {
            return Err(AiSdkError::InvalidStructuredOutput {
                detail: format!("enum_mismatch:{field_name}"),
            });
        }
    }

    Ok(())
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
                    FunctionLoopItem::UserContentParts { content } => {
                        input.push(json!({
                            "role": "user",
                            "content": content.iter().map(model_part_to_openai).collect::<Vec<_>>(),
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
                    FunctionLoopItem::AssistantToolCalls { .. } => {}
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
            // Same clean format as the Responses path (audit B6/M3): no raw body (key echo), and
            // it carries type/param/message so the consumer can classify + scrub.
            return Err(openai_status_error(status.as_u16(), &value));
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

        let tokens_used = value
            .get("usage")
            .and_then(|u| u.get("total_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;

        Ok(FunctionModelTurnResponse {
            assistant_text,
            tool_calls,
            tokens_used,
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
        FunctionLoopItem::UserContentParts { content } => json!({
            "role": "user",
            "content": content.iter().map(model_part_to_openai).collect::<Vec<_>>(),
        }),
        FunctionLoopItem::AssistantText { content } => json!({
            "role": "assistant",
            "content": [{"type":"output_text","text": content}],
        }),
        FunctionLoopItem::AssistantToolCalls { content, .. } => json!({
            "role": "assistant",
            "content": content.map(|text| vec![json!({"type":"output_text","text":text})]).unwrap_or_default(),
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

const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
// Anthropic requires max_tokens on every request (unlike OpenAI, which treats an absent
// max_output_tokens as "model default"). The old 1024 default silently truncated every
// admin/architect turn that left ModelSettings at default (audit A2); 4096 is a safe general
// ceiling, and truncation is now detected loudly (anthropic_check_truncation) so a caller that
// still needs more gets an explicit error instead of a corrupted/short completion.
const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 4096;
const ANTHROPIC_MAX_ATTEMPTS: usize = 3;

#[derive(Clone)]
pub struct AnthropicMessagesClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl AnthropicMessagesClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: ANTHROPIC_MESSAGES_URL.to_string(),
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
    ) -> AnthropicFunctionCallingModel {
        AnthropicFunctionCallingModel {
            client: self,
            model: model.into(),
            system,
            model_settings,
        }
    }

    async fn send(&self, body: &Value) -> Result<Value> {
        for attempt in 0..ANTHROPIC_MAX_ATTEMPTS {
            let response = match self
                .http
                .post(&self.base_url)
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/json")
                .header(
                    HeaderName::from_static("anthropic-version"),
                    HeaderValue::from_static(ANTHROPIC_VERSION),
                )
                .header(HeaderName::from_static("x-api-key"), &self.api_key)
                .json(body)
                .send()
                .await
            {
                Ok(response) => response,
                Err(err) if attempt + 1 < ANTHROPIC_MAX_ATTEMPTS => {
                    tokio::time::sleep(anthropic_retry_delay(attempt, None)).await;
                    tracing::warn!(attempt = attempt + 1, error = %err, "retrying Anthropic transport error");
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            let status = response.status();
            let headers = response.headers().clone();
            let request_id = headers
                .get("request-id")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            // Decide retry from STATUS + headers BEFORE touching the body (audit M1): a
            // proxy/LB 5xx or 429 often carries an HTML or empty body, and parsing it first
            // (`response.json().await?`) aborted the whole retry loop on a decode error exactly
            // when a retry was warranted.
            if !status.is_success()
                && attempt + 1 < ANTHROPIC_MAX_ATTEMPTS
                && anthropic_should_retry(status.as_u16(), &headers)
            {
                let retry_after = anthropic_retry_after(&headers);
                tokio::time::sleep(anthropic_retry_delay(attempt, retry_after)).await;
                tracing::warn!(attempt = attempt + 1, status = %status, request_id = %request_id, "retrying Anthropic response");
                continue;
            }
            if status.is_success() {
                let value: Value = response.json().await?;
                return Ok(value);
            }
            // Non-retryable (or last attempt): parse tolerantly so a non-JSON error body
            // surfaces the status/request-id instead of a masking decode error.
            let value: Value = response.json().await.unwrap_or(Value::Null);
            let error_type = value
                .pointer("/error/type")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error");
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Anthropic request failed");
            return Err(AiSdkError::Protocol(format!(
                "anthropic error status={} type={} request_id={} message={}",
                status, error_type, request_id, message
            )));
        }
        Err(AiSdkError::Protocol(
            "Anthropic retry loop exhausted unexpectedly".to_string(),
        ))
    }
}

fn anthropic_should_retry(status: u16, headers: &reqwest::header::HeaderMap) -> bool {
    match headers
        .get("x-should-retry")
        .and_then(|value| value.to_str().ok())
    {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    matches!(status, 408 | 409 | 429) || status >= 500
}

fn anthropic_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    if let Some(ms) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Some(Duration::from_millis(ms).min(Duration::from_secs(60)));
    }
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|seconds| Duration::from_secs_f64(seconds.min(60.0)))
}

fn anthropic_retry_delay(attempt: usize, server_delay: Option<Duration>) -> Duration {
    if let Some(delay) = server_delay {
        return delay;
    }
    let base_ms = 500_u64.saturating_mul(1_u64 << attempt.min(4));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let jitter_percent = 75 + (nanos % 51);
    Duration::from_millis(base_ms.saturating_mul(jitter_percent) / 100)
}

/// Build an OpenAI error WITHOUT embedding the raw response body (audit B6): OpenAI's own 401
/// body echoes a partially-masked form of the api key ("Incorrect API key provided: sk-…"),
/// which the old `body={value}` leaked into logs and the mesh error envelope. We keep the
/// `openai error status=` prefix so the consumer classifier still recognizes it, but carry only
/// the extracted error type/message.
fn openai_status_error(status: u16, body: &Value) -> AiSdkError {
    let error_type = body
        .pointer("/error/type")
        .and_then(Value::as_str)
        .unwrap_or("unknown_error");
    // `param` identifies the offending request field on a 400 (e.g. an attachment path); the
    // consumer classifier reads it to raise provider_attachment_invalid_request, so carry it
    // explicitly (before message=) now that the raw JSON body no longer travels.
    let param = body
        .pointer("/error/param")
        .and_then(Value::as_str)
        .unwrap_or("");
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("OpenAI request failed");
    // OpenAI's 401 message itself echoes a masked key ("Incorrect API key provided: sk-…"),
    // so extracting the message is not enough (audit B6) — scrub any sk- token before it reaches
    // logs / the mesh error envelope.
    AiSdkError::Protocol(format!(
        "openai error status={status} type={error_type} param={param} message={}",
        scrub_secret_like(message)
    ))
}

/// Redact OpenAI-style key tokens (`sk-…`) that provider error messages can echo, so a key
/// fragment never lands in a log line or the mesh error envelope (audit B6). Replaces each
/// `sk-` run of non-whitespace characters with `sk-<redacted>`.
fn scrub_secret_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find("sk-") {
        // Only redact when "sk-" BEGINS a token (start of string, or preceded by a non-alphanumeric
        // char). Otherwise ordinary words that merely contain the sequence — "task-id", "risk-",
        // "disk-" — would be corrupted (audit review).
        let at_token_boundary = rest[..pos]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        if at_token_boundary {
            out.push_str(&rest[..pos]);
            out.push_str("sk-<redacted>");
            let after = &rest[pos + 3..];
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            rest = &after[end..];
        } else {
            out.push_str(&rest[..pos + 3]);
            rest = &rest[pos + 3..];
        }
    }
    out.push_str(rest);
    out
}

/// Detect a silently-truncated OpenAI Responses completion (audit A2): the API returns
/// status="incomplete" with incomplete_details.reason="max_output_tokens". Surface it as a loud
/// error instead of returning a short/invalid body that downstream parses as an unrelated failure.
fn openai_check_truncation(body: &Value) -> Result<()> {
    if body.get("status").and_then(Value::as_str) == Some("incomplete")
        && body
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
            == Some("max_output_tokens")
    {
        return Err(AiSdkError::Protocol(
            "openai response truncated: hit max_output_tokens (raise model_settings.max_output_tokens)"
                .to_string(),
        ));
    }
    Ok(())
}

/// Heuristic: an Anthropic 400 that indicates the model does not support the `output_config`
/// structured-outputs feature (audit M4), so `generate` can retry once with a prompt-instruction
/// fallback instead of surfacing the raw 400. Conservative — only a 400 that names the feature.
fn anthropic_output_config_unsupported(err: &AiSdkError) -> bool {
    let AiSdkError::Protocol(msg) = err else {
        return false;
    };
    if !msg.contains("anthropic error status=400") {
        return false;
    }
    let lower = msg.to_ascii_lowercase();
    lower.contains("output_config") || lower.contains("json_schema") || lower.contains("structured")
}

/// Detect a silently-truncated Anthropic completion (audit A2): stop_reason="max_tokens" means
/// the output was cut at the (now 4096) cap. Fail loudly so a caller needing more raises
/// model_settings.max_output_tokens rather than parsing a truncated tool_use/structured output.
fn anthropic_check_truncation(value: &Value) -> Result<()> {
    if value.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
        return Err(AiSdkError::Protocol(
            "anthropic response truncated: hit max_tokens (raise model_settings.max_output_tokens)"
                .to_string(),
        ));
    }
    Ok(())
}

#[async_trait]
impl LlmClient for AnthropicMessagesClient {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse> {
        let content = match request.input_parts.as_ref() {
            Some(parts) => parts
                .iter()
                .map(model_part_to_anthropic)
                .collect::<Result<Vec<_>>>()?,
            None => vec![json!({"type":"text", "text": request.input})],
        };
        let max_tokens = request
            .model_settings
            .as_ref()
            .and_then(|settings| settings.max_output_tokens)
            .or(request.max_output_tokens)
            .unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS);
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), Value::String(request.model));
        body.insert("max_tokens".to_string(), Value::from(max_tokens));
        body.insert(
            "messages".to_string(),
            json!([{"role":"user", "content":content}]),
        );
        if let Some(system) = request.system.filter(|value| !value.trim().is_empty()) {
            body.insert("system".to_string(), Value::String(system));
        }
        if let Some(temperature) = request
            .model_settings
            .as_ref()
            .and_then(|settings| settings.temperature)
        {
            body.insert("temperature".to_string(), Value::from(temperature));
        }
        if let Some(top_p) = request
            .model_settings
            .as_ref()
            .and_then(|settings| settings.top_p)
        {
            body.insert("top_p".to_string(), Value::from(top_p));
        }
        if let Some(output_schema) = &request.output_schema {
            output_schema.validate()?;
            body.insert(
                "output_config".to_string(),
                json!({
                    "format": {
                        "type": "json_schema",
                        "schema": output_schema.json_schema(),
                    }
                }),
            );
        }
        // Capability fallback (audit B5/M4): output_config is sent optimistically; a model that
        // does not support structured outputs answers 400. Instead of surfacing that raw 400,
        // retry ONCE without output_config, steering the schema via a system instruction — the
        // local validate_structured_output below still enforces the contract on the result.
        let value = match self.send(&Value::Object(body.clone())).await {
            Ok(value) => value,
            Err(err)
                if request.output_schema.is_some()
                    && anthropic_output_config_unsupported(&err) =>
            {
                let output_schema = request
                    .output_schema
                    .as_ref()
                    .expect("output_schema present in fallback arm");
                let instruction = build_output_schema_fallback_instruction(output_schema)?;
                body.remove("output_config");
                let system = match body.get("system").and_then(Value::as_str) {
                    Some(existing) if !existing.trim().is_empty() => {
                        format!("{existing}\n\n{instruction}")
                    }
                    _ => instruction,
                };
                body.insert("system".to_string(), Value::String(system));
                tracing::warn!(
                    "anthropic model rejected output_config; retrying with prompt-instruction structured-output fallback"
                );
                self.send(&Value::Object(body)).await?
            }
            Err(err) => return Err(err),
        };
        anthropic_check_truncation(&value)?;
        let text = anthropic_response_text(&value)?;
        let text = match &request.output_schema {
            Some(output_schema) => validate_structured_output(&text, output_schema)?,
            None => text,
        };
        Ok(LlmResponse { content: text })
    }
}

fn model_part_to_anthropic(part: &ModelContentPart) -> Result<Value> {
    match part {
        ModelContentPart::Text { text } => Ok(json!({"type":"text", "text":text})),
        ModelContentPart::Image {
            media_type,
            data_base64,
            ..
        } => match media_type.as_str() {
            "image/jpeg" | "image/png" | "image/gif" | "image/webp" => Ok(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data_base64,
                }
            })),
            _ => Err(AiSdkError::Protocol(format!(
                "anthropic unsupported image media_type={media_type}"
            ))),
        },
        ModelContentPart::Document {
            media_type,
            filename,
            data_base64,
        } => {
            if media_type != "application/pdf" {
                return Err(AiSdkError::Protocol(format!(
                    "anthropic unsupported document media_type={media_type} filename={filename}"
                )));
            }
            Ok(json!({
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data_base64,
                },
                "title": filename,
            }))
        }
    }
}

fn anthropic_response_text(value: &Value) -> Result<String> {
    let text = value
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        return Err(AiSdkError::Protocol(
            "anthropic response missing text content".to_string(),
        ));
    }
    Ok(text)
}

#[derive(Clone)]
pub struct AnthropicFunctionCallingModel {
    client: AnthropicMessagesClient,
    model: String,
    system: Option<String>,
    model_settings: ModelSettings,
}

#[async_trait]
impl FunctionCallingModel for AnthropicFunctionCallingModel {
    async fn run_turn(
        &self,
        request: FunctionModelTurnRequest,
    ) -> Result<FunctionModelTurnResponse> {
        let (system, messages) = build_anthropic_function_messages(&self.system, &request.items)?;
        let tools = request
            .tools
            .into_iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters_json_schema,
                })
            })
            .collect::<Vec<_>>();
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), Value::String(self.model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));
        body.insert("tools".to_string(), Value::Array(tools));
        body.insert(
            "max_tokens".to_string(),
            Value::from(
                self.model_settings
                    .max_output_tokens
                    .unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS),
            ),
        );
        if !system.is_empty() {
            body.insert("system".to_string(), Value::String(system));
        }
        if let Some(temperature) = self.model_settings.temperature {
            body.insert("temperature".to_string(), Value::from(temperature));
        }
        if let Some(top_p) = self.model_settings.top_p {
            body.insert("top_p".to_string(), Value::from(top_p));
        }
        let value = self.client.send(&Value::Object(body)).await?;
        anthropic_check_truncation(&value)?;
        let mut assistant_text = Vec::new();
        let mut tool_calls = Vec::new();
        if let Some(content) = value.get("content").and_then(Value::as_array) {
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            assistant_text.push(text.to_string());
                        }
                    }
                    Some("tool_use") => {
                        let call_id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if !call_id.is_empty() && !name.is_empty() {
                            tool_calls.push(FunctionToolCall {
                                call_id,
                                response_id: None,
                                name,
                                arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        let tokens_used = value
            .get("usage")
            .map(|usage| {
                usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .saturating_add(
                        usage
                            .get("output_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
            })
            .unwrap_or(0) as u32;
        Ok(FunctionModelTurnResponse {
            assistant_text: if assistant_text.is_empty() {
                None
            } else {
                Some(assistant_text.join(""))
            },
            tool_calls,
            tokens_used,
        })
    }
}

fn build_anthropic_function_messages(
    configured_system: &Option<String>,
    items: &[FunctionLoopItem],
) -> Result<(String, Vec<Value>)> {
    let mut system_parts = configured_system.iter().cloned().collect::<Vec<_>>();
    let mut messages = Vec::new();
    let mut index = 0usize;
    while index < items.len() {
        match &items[index] {
            FunctionLoopItem::SystemText { content } => system_parts.push(content.clone()),
            FunctionLoopItem::UserText { content } => messages.push(json!({
                "role": "user",
                "content": [{"type":"text", "text":content}],
            })),
            FunctionLoopItem::UserContentParts { content } => messages.push(json!({
                "role": "user",
                "content": content.iter().map(model_part_to_anthropic).collect::<Result<Vec<_>>>()?,
            })),
            FunctionLoopItem::AssistantText { content } => messages.push(json!({
                "role": "assistant",
                "content": [{"type":"text", "text":content}],
            })),
            FunctionLoopItem::AssistantToolCalls { content, calls } => {
                let mut blocks = Vec::new();
                if let Some(text) = content.as_deref().filter(|value| !value.is_empty()) {
                    blocks.push(json!({"type":"text", "text":text}));
                }
                blocks.extend(calls.iter().map(|call| {
                    json!({
                        "type": "tool_use",
                        "id": call.call_id,
                        "name": call.name,
                        "input": call.arguments,
                    })
                }));
                messages.push(json!({"role":"assistant", "content":blocks}));
            }
            FunctionLoopItem::ToolResult { .. } => {
                let mut blocks = Vec::new();
                while index < items.len() {
                    let FunctionLoopItem::ToolResult { result } = &items[index] else {
                        break;
                    };
                    let output = match &result.output {
                        Value::String(value) => value.clone(),
                        value => serde_json::to_string(value)?,
                    };
                    blocks.push(json!({
                        "type": "tool_result",
                        "tool_use_id": result.call_id,
                        "content": output,
                        "is_error": result.is_error,
                    }));
                    index += 1;
                }
                messages.push(json!({"role":"user", "content":blocks}));
                continue;
            }
        }
        index += 1;
    }
    // Anthropic requires messages[0] to be role=user (audit M5). A truncated immediate-memory
    // window can begin with an assistant turn; drop leading assistant messages and any leading
    // tool_result user block they leave orphaned, so the first message is always a real user turn.
    while let Some(first) = messages.first() {
        let role = first.get("role").and_then(Value::as_str);
        let orphan_tool_result = role == Some("user")
            && first
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| {
                    !blocks.is_empty()
                        && blocks.iter().all(|b| {
                            b.get("type").and_then(Value::as_str) == Some("tool_result")
                        })
                });
        if role == Some("assistant") || orphan_tool_result {
            messages.remove(0);
        } else {
            break;
        }
    }
    Ok((system_parts.join("\n\n"), messages))
}

pub fn create_llm_client(
    provider: AiProvider,
    api_key: impl Into<String>,
    base_url: Option<String>,
) -> Arc<dyn LlmClient> {
    let api_key = api_key.into();
    match provider {
        AiProvider::OpenAi => {
            let mut client = OpenAiResponsesClient::new(api_key);
            if let Some(base_url) = base_url {
                client = client.with_base_url(base_url);
            }
            Arc::new(client)
        }
        AiProvider::Anthropic => {
            let mut client = AnthropicMessagesClient::new(api_key);
            if let Some(base_url) = base_url {
                client = client.with_base_url(base_url);
            }
            Arc::new(client)
        }
    }
}

pub fn create_function_calling_model(
    provider: AiProvider,
    api_key: impl Into<String>,
    base_url: Option<String>,
    model: impl Into<String>,
    system: Option<String>,
    model_settings: ModelSettings,
) -> Arc<dyn FunctionCallingModel> {
    let api_key = api_key.into();
    let model = model.into();
    match provider {
        AiProvider::OpenAi => {
            let mut client = OpenAiResponsesClient::new(api_key);
            if let Some(base_url) = base_url {
                client = client.with_base_url(base_url);
            }
            Arc::new(client.function_model(model, system, model_settings))
        }
        AiProvider::Anthropic => {
            let mut client = AnthropicMessagesClient::new(api_key);
            if let Some(base_url) = base_url {
                client = client.with_base_url(base_url);
            }
            Arc::new(client.function_model(model, system, model_settings))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use serde_json::json;

    use super::*;

    #[test]
    fn openai_status_error_omits_raw_body_and_key_echo() {
        // B6: the error must carry only the extracted type/message, never the raw body (OpenAI's
        // 401 body echoes a masked key), and must keep the "openai error status=" prefix the
        // consumer classifier matches.
        let body = json!({"error": {"type": "invalid_request_error", "message": "Incorrect API key provided: sk-abc...xyz"}});
        let err = openai_status_error(401, &body);
        let AiSdkError::Protocol(msg) = err else {
            panic!("expected Protocol error");
        };
        assert!(msg.starts_with("openai error status=401"));
        assert!(msg.contains("type=invalid_request_error"));
        assert!(msg.contains("Incorrect API key"));
        assert!(!msg.contains("sk-abc...xyz")); // the masked-key echo must NOT be present
        assert!(!msg.contains("body="));
    }

    #[test]
    fn scrub_secret_like_redacts_keys_but_not_ordinary_words() {
        assert_eq!(
            scrub_secret_like("Incorrect API key provided: sk-abc...xyz please"),
            "Incorrect API key provided: sk-<redacted> please"
        );
        assert_eq!(scrub_secret_like("token sk-live-REALKEY"), "token sk-<redacted>");
        // "sk-" inside ordinary words must be preserved (task-id contains s,k,-).
        assert_eq!(
            scrub_secret_like("Invalid value for task-id parameter"),
            "Invalid value for task-id parameter"
        );
        assert_eq!(scrub_secret_like("risk-averse disk-full"), "risk-averse disk-full");
        // key=sk-... (preceded by '=') is a token boundary and IS redacted.
        assert_eq!(scrub_secret_like("key=sk-abc def"), "key=sk-<redacted> def");
    }

    #[test]
    fn truncation_detectors_flag_max_tokens() {
        // A2: both providers' "hit the cap" signals surface as an explicit error.
        assert!(openai_check_truncation(
            &json!({"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}})
        )
        .is_err());
        assert!(openai_check_truncation(&json!({"status": "completed"})).is_ok());
        assert!(
            anthropic_check_truncation(&json!({"stop_reason": "max_tokens"})).is_err()
        );
        assert!(anthropic_check_truncation(&json!({"stop_reason": "end_turn"})).is_ok());
    }

    #[test]
    fn output_config_unsupported_only_on_capability_400() {
        // M4: the capability-fallback heuristic fires only on a 400 that names the feature.
        assert!(anthropic_output_config_unsupported(&AiSdkError::Protocol(
            "anthropic error status=400 type=invalid_request_error message=output_config not supported".into()
        )));
        assert!(!anthropic_output_config_unsupported(&AiSdkError::Protocol(
            "anthropic error status=429 type=rate_limit_error message=slow down".into()
        )));
        assert!(!anthropic_output_config_unsupported(&AiSdkError::Protocol(
            "anthropic error status=400 type=invalid_request_error message=bad max_tokens".into()
        )));
    }
    use crate::function_calling::FunctionToolResult;

    #[test]
    fn ai_provider_wire_form_is_openai_and_matches_display() {
        // Contract: the serde wire form MUST be "openai" — the same string as
        // as_str()/Display/FromStr, the `providers.openai` key, and the spoke
        // hive.yaml the orchestrator generates via Display. A bare
        // rename_all=snake_case ("open_ai") broke fresh installs (packaged
        // hive.yaml.example says `openai`) and every generated spoke yaml —
        // caught by the vault cold-boot VM E2E 2026-07-21.
        let cfg: HiveAiConfig =
            serde_yaml::from_str("default_provider: openai\n").expect("parse openai");
        assert_eq!(cfg.default_provider, AiProvider::OpenAi);
        // Read-only alias for anything written while "open_ai" was the wire form.
        let cfg: HiveAiConfig =
            serde_yaml::from_str("default_provider: open_ai\n").expect("parse open_ai alias");
        assert_eq!(cfg.default_provider, AiProvider::OpenAi);
        let cfg: HiveAiConfig =
            serde_yaml::from_str("default_provider: anthropic\n").expect("parse anthropic");
        assert_eq!(cfg.default_provider, AiProvider::Anthropic);
        // Serialize == Display == as_str for both variants (roundtrip stability).
        for provider in [AiProvider::OpenAi, AiProvider::Anthropic] {
            let wire = serde_json::to_value(provider).expect("serialize");
            assert_eq!(wire, json!(provider.as_str()));
            assert_eq!(provider.to_string(), provider.as_str());
            assert_eq!(
                provider.as_str().parse::<AiProvider>().expect("FromStr"),
                provider
            );
        }
    }

    fn spawn_single_response_server(
        response_body: Value,
    ) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut header_end = None;
            let mut content_length = 0usize;

            loop {
                let n = stream.read(&mut chunk).expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);

                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                        let end = pos + 4;
                        header_end = Some(end);
                        let headers = String::from_utf8_lossy(&buf[..end]);
                        for line in headers.lines() {
                            let lower = line.to_ascii_lowercase();
                            if let Some(value) = lower.strip_prefix("content-length:") {
                                content_length =
                                    value.trim().parse::<usize>().expect("content length");
                            }
                        }
                    }
                }

                if let Some(end) = header_end {
                    if buf.len() >= end + content_length {
                        break;
                    }
                }
            }

            let header_end = header_end.expect("header end");
            let body_bytes = &buf[header_end..header_end + content_length];
            let body_value: Value = serde_json::from_slice(body_bytes).expect("json request body");
            tx.send(body_value).expect("send captured body");

            let response_json = serde_json::to_vec(&response_body).expect("serialize response");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_json.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response head");
            stream
                .write_all(&response_json)
                .expect("write response body");
        });

        (format!("http://{}", addr), rx, handle)
    }

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
    fn user_content_parts_are_forwarded_verbatim() {
        let item = loop_item_to_openai_input(FunctionLoopItem::UserContentParts {
            content: vec![
                ModelContentPart::Text {
                    text: "hola".to_string(),
                },
                ModelContentPart::Image {
                    media_type: "image/jpeg".to_string(),
                    data_base64: "AAA".to_string(),
                    detail: None,
                },
            ],
        });
        assert_eq!(item["role"], "user");
        assert_eq!(item["content"][0]["type"], "input_text");
        assert_eq!(item["content"][1]["type"], "input_image");
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

    #[test]
    fn build_openai_input_items_uses_text_fallback_without_parts() {
        let req = LlmRequest {
            model: "gpt-4.1-mini".to_string(),
            system: Some("sys".to_string()),
            input: "hola".to_string(),
            input_parts: None,
            output_schema: None,
            max_output_tokens: None,
            model_settings: None,
        };
        let input = build_openai_input_items(&req);
        assert_eq!(input.len(), 2);
        assert_eq!(input[1]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["content"][0]["text"], "hola");
    }

    #[test]
    fn build_openai_input_items_prefers_structured_parts() {
        let req = LlmRequest {
            model: "gpt-4.1-mini".to_string(),
            system: None,
            input: "fallback".to_string(),
            input_parts: Some(vec![
                ModelContentPart::Text {
                    text: "texto".to_string(),
                },
                ModelContentPart::Image {
                    media_type: "image/png".to_string(),
                    data_base64: "AAA".to_string(),
                    detail: None,
                },
            ]),
            output_schema: None,
            max_output_tokens: None,
            model_settings: None,
        };
        let input = build_openai_input_items(&req);
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][1]["type"], "input_image");
    }

    #[test]
    fn build_openai_response_format_uses_json_schema_shape() {
        let spec = OutputSchemaSpec::new(
            "final_output",
            json!({
                "type":"object",
                "properties":{"ok":{"type":"boolean"}},
                "required":["ok"],
                "additionalProperties": false
            }),
            true,
        )
        .expect("schema should be valid");
        let format = build_openai_response_format(&spec).expect("format should build");
        assert_eq!(format["type"], "json_schema");
        assert_eq!(format["name"], "final_output");
        assert_eq!(format["strict"], true);
        assert_eq!(format["schema"]["type"], "object");
    }

    #[test]
    fn validate_structured_output_accepts_valid_object() {
        let spec = OutputSchemaSpec::new(
            "final_output",
            json!({
                "type":"object",
                "properties":{
                    "success":{"type":"boolean"},
                    "human_message":{"type":"string"},
                    "error_code":{"type":"string", "enum":["missing_data","unknown"]}
                },
                "required":["success","human_message"],
                "additionalProperties": false
            }),
            true,
        )
        .expect("schema should be valid");
        let out = validate_structured_output(
            r#"{"success":true,"human_message":"ok","error_code":"unknown"}"#,
            &spec,
        )
        .expect("output should validate");
        assert_eq!(
            serde_json::from_str::<Value>(&out).expect("json"),
            json!({"success":true,"human_message":"ok","error_code":"unknown"})
        );
    }

    #[test]
    fn output_schema_new_rejects_non_object_schema() {
        let err = OutputSchemaSpec::new("final_output", json!("bad"), true)
            .expect_err("schema should fail");
        assert!(matches!(err, AiSdkError::InvalidResponseContract { .. }));
    }

    #[test]
    fn validate_structured_output_rejects_unexpected_property() {
        let spec = OutputSchemaSpec::new(
            "final_output",
            json!({
                "type":"object",
                "properties":{"ok":{"type":"boolean"}},
                "required":["ok"],
                "additionalProperties": false
            }),
            true,
        )
        .expect("schema should be valid");
        let err = validate_structured_output(r#"{"ok":true,"extra":"bad"}"#, &spec)
            .expect_err("output should fail");
        assert!(matches!(err, AiSdkError::InvalidStructuredOutput { .. }));
    }

    #[test]
    fn validate_structured_output_rejects_invalid_json() {
        let spec = OutputSchemaSpec::new(
            "final_output",
            json!({
                "type":"object",
                "properties":{"ok":{"type":"boolean"}},
                "required":["ok"],
                "additionalProperties": false
            }),
            true,
        )
        .expect("schema should be valid");
        let err = validate_structured_output("not json", &spec).expect_err("output should fail");
        assert!(matches!(err, AiSdkError::InvalidStructuredOutput { .. }));
    }

    #[test]
    fn validate_structured_output_rejects_optional_null_value() {
        let spec = OutputSchemaSpec::new(
            "final_output",
            json!({
                "type":"object",
                "properties":{
                    "success":{"type":"boolean"},
                    "human_message":{"type":"string"},
                    "error_code":{"type":"string"}
                },
                "required":["success","human_message"],
                "additionalProperties": false
            }),
            true,
        )
        .expect("schema should be valid");
        let err = validate_structured_output(
            r#"{"success":true,"human_message":"ok","error_code":null}"#,
            &spec,
        )
        .expect_err("optional null should fail");
        assert!(matches!(err, AiSdkError::InvalidStructuredOutput { .. }));
    }

    #[test]
    fn validate_structured_output_rejects_enum_mismatch() {
        let spec = OutputSchemaSpec::new(
            "final_output",
            json!({
                "type":"object",
                "properties":{
                    "status":{"type":"string","enum":["ok","error"]}
                },
                "required":["status"],
                "additionalProperties": false
            }),
            true,
        )
        .expect("schema should be valid");
        let err = validate_structured_output(r#"{"status":"weird"}"#, &spec)
            .expect_err("enum mismatch should fail");
        assert!(matches!(err, AiSdkError::InvalidStructuredOutput { .. }));
    }

    #[test]
    fn build_output_schema_fallback_instruction_mentions_fields_and_required() {
        let spec = OutputSchemaSpec::new(
            "final_output",
            json!({
                "type":"object",
                "properties":{
                    "success":{"type":"boolean"},
                    "human_message":{"type":"string"},
                    "error_code":{"type":"string","enum":["missing_data","unknown"]}
                },
                "required":["success","human_message"],
                "additionalProperties": false
            }),
            true,
        )
        .expect("schema should be valid");
        let instruction =
            build_output_schema_fallback_instruction(&spec).expect("instruction should build");
        assert!(instruction.contains("success:boolean"));
        assert!(instruction.contains("human_message:string"));
        assert!(instruction.contains("Required fields: success, human_message"));
        assert!(instruction.contains("error_code:string(missing_data|unknown)"));
        assert!(instruction.contains("Do not include markdown fences or extra text"));
        assert!(instruction.contains("Do not include properties outside this schema"));
    }

    #[tokio::test]
    async fn openai_responses_client_sends_json_schema_when_output_schema_present() {
        let (base_url, body_rx, handle) = spawn_single_response_server(json!({
            "output_text": "{\"ok\":true}"
        }));
        let client = OpenAiResponsesClient::new("test-key").with_base_url(base_url);
        let request = LlmRequest {
            model: "gpt-4.1-mini".to_string(),
            system: Some("sys".to_string()),
            input: "hola".to_string(),
            input_parts: None,
            output_schema: Some(
                OutputSchemaSpec::new(
                    "final_output",
                    json!({
                        "type":"object",
                        "properties":{"ok":{"type":"boolean"}},
                        "required":["ok"],
                        "additionalProperties": false
                    }),
                    true,
                )
                .expect("schema"),
            ),
            max_output_tokens: Some(64),
            model_settings: None,
        };

        let response = client
            .generate(request)
            .await
            .expect("generate should succeed");
        assert_eq!(response.content, "{\"ok\":true}");

        let body = body_rx.recv().expect("captured body");
        assert_eq!(body["model"], "gpt-4.1-mini");
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["name"], "final_output");
        assert_eq!(body["text"]["format"]["strict"], true);
        assert_eq!(
            body["text"]["format"]["schema"]["properties"]["ok"]["type"],
            "boolean"
        );
        handle.join().expect("server thread");
    }

    #[tokio::test]
    async fn openai_responses_client_omits_json_schema_when_output_schema_absent() {
        let (base_url, body_rx, handle) = spawn_single_response_server(json!({
            "output_text": "plain text"
        }));
        let client = OpenAiResponsesClient::new("test-key").with_base_url(base_url);
        let request = LlmRequest {
            model: "gpt-4.1-mini".to_string(),
            system: Some("sys".to_string()),
            input: "hola".to_string(),
            input_parts: None,
            output_schema: None,
            max_output_tokens: Some(64),
            model_settings: None,
        };

        let response = client
            .generate(request)
            .await
            .expect("generate should succeed");
        assert_eq!(response.content, "plain text");

        let body = body_rx.recv().expect("captured body");
        assert!(body.get("text").is_none());
        handle.join().expect("server thread");
    }

    #[test]
    fn hive_ai_fallback_is_openai_gpt_5_5() {
        let engine = HiveAiConfig::fallback();
        assert_eq!(engine.provider, AiProvider::OpenAi);
        assert_eq!(engine.model, "gpt-5.5");
    }

    #[test]
    fn ai_provider_parsing_is_strict() {
        assert_eq!("openai".parse::<AiProvider>().unwrap(), AiProvider::OpenAi);
        assert_eq!(
            "anthropic".parse::<AiProvider>().unwrap(),
            AiProvider::Anthropic
        );
        assert!("gemini".parse::<AiProvider>().is_err());
    }

    #[test]
    fn hive_ai_selects_only_the_default_provider_model() {
        let config: HiveAiConfig = serde_yaml::from_str(
            r#"
default_provider: anthropic
providers:
  openai:
    model: gpt-5.5
  anthropic:
    model: claude-sonnet-4-5
"#,
        )
        .expect("valid hive AI config");
        let engine = config.effective().expect("effective engine");
        assert_eq!(engine.provider, AiProvider::Anthropic);
        assert_eq!(engine.model, "claude-sonnet-4-5");
    }

    #[test]
    fn anthropic_retry_policy_matches_official_status_contract() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(anthropic_should_retry(408, &headers));
        assert!(anthropic_should_retry(409, &headers));
        assert!(anthropic_should_retry(429, &headers));
        assert!(anthropic_should_retry(500, &headers));
        assert!(!anthropic_should_retry(400, &headers));
    }

    #[tokio::test]
    async fn anthropic_messages_client_uses_messages_shape() {
        let (base_url, body_rx, handle) = spawn_single_response_server(json!({
            "content": [{"type":"text", "text":"hola"}],
            "usage": {"input_tokens": 3, "output_tokens": 2}
        }));
        let client = AnthropicMessagesClient::new("test-key").with_base_url(base_url);
        let response = client
            .generate(LlmRequest {
                model: "claude-sonnet-4-5".to_string(),
                system: Some("sys".to_string()),
                input: "entrada".to_string(),
                input_parts: None,
                output_schema: None,
                max_output_tokens: Some(64),
                model_settings: None,
            })
            .await
            .expect("generate should succeed");
        assert_eq!(response.content, "hola");
        let body = body_rx.recv().expect("captured body");
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["system"], "sys");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        handle.join().expect("server thread");
    }
}
