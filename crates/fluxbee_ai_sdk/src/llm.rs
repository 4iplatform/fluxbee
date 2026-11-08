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

use crate::errors::{AiSdkError, ProviderError, Result};
use crate::function_calling::{
    FunctionCallingModel, FunctionLoopItem, FunctionModelTurnRequest, FunctionModelTurnResponse,
    FunctionToolCall,
};
use crate::text_payload::ModelContentPart;
use crate::usage::AiAccount;
use fluxbee_sdk::UsageKind;

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
        "Return only valid JSON. Do not include markdown fences or extra text. The top-level value must be an object with exactly these fields: {fields}. Required fields: {required_fields}. Every field you include holds a value of its type, never null. Do not include properties outside this schema."
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
/// Private to the SDK: every call goes through the factories, with its `AiAccount` (§7.2).
pub(crate) struct OpenAiResponsesClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    account: AiAccount,
}

impl OpenAiResponsesClient {
    pub(crate) fn new(api_key: impl Into<String>, account: AiAccount) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: "https://api.openai.com/v1/responses".to_string(),
            api_key: api_key.into(),
            account,
        }
    }

    pub(crate) fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub(crate) fn function_model(
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

    /// One request, asked again by the rules Anthropic's are (`provider_should_retry`), its
    /// status and headers read before its body: a proxy's 502 with an HTML body keeps its status
    /// (FINDINGS A-184). Each attempt is a call of its own for the usage records (§7.2): only the
    /// one that answers 2xx is recorded, and a retry's wait is no call.
    async fn send(&self, body: &Value, model: &str, kind: UsageKind) -> Result<Value> {
        let auth = format!("Bearer {}", self.api_key);
        for attempt in 0..PROVIDER_MAX_ATTEMPTS {
            let mut call = self.account.begin(AiProvider::OpenAi, model, kind);
            let response = match self
                .http
                .post(&self.base_url)
                .header(AUTHORIZATION, &auth)
                .header(CONTENT_TYPE, "application/json")
                .json(body)
                .send()
                .await
            {
                Ok(response) => response,
                Err(err) if attempt + 1 < PROVIDER_MAX_ATTEMPTS => {
                    call.failed();
                    let delay = provider_retry_delay(attempt, None);
                    // Logged before the wait: a caller with a shorter deadline (cognition's 8 s)
                    // may drop the call during it.
                    tracing::warn!(attempt = attempt + 1, delay_ms = delay.as_millis() as u64, error = %err, "retrying OpenAI transport error");
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(err) => {
                    call.failed();
                    return Err(err.into());
                }
            };
            let status = response.status();
            let headers = response.headers().clone();
            let request_id = header_text(&headers, "x-request-id");
            if !status.is_success()
                && attempt + 1 < PROVIDER_MAX_ATTEMPTS
                && provider_should_retry(status.as_u16(), &headers)
            {
                call.failed();
                let delay = provider_retry_delay(attempt, provider_retry_after(&headers));
                tracing::warn!(attempt = attempt + 1, delay_ms = delay.as_millis() as u64, status = %status, request_id = ?request_id, "retrying OpenAI response");
                tokio::time::sleep(delay).await;
                continue;
            }
            if status.is_success() {
                return match response.json::<Value>().await {
                    Ok(value) => {
                        call.answered(Some(&value));
                        Ok(value)
                    }
                    Err(err) => {
                        call.answered(None);
                        Err(err.into())
                    }
                };
            }
            // Not to be asked again, or the last attempt: a body that is no JSON leaves the status.
            call.failed();
            let value: Value = response.json().await.unwrap_or(Value::Null);
            return Err(openai_status_error(status.as_u16(), &value, request_id).into());
        }
        Err(AiSdkError::Protocol(
            "OpenAI retry loop exhausted unexpectedly".to_string(),
        ))
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

        let value = self
            .send(&body, &request.model, UsageKind::Generate)
            .await?;
        openai_check_truncation(&value)?;
        openai_check_filter(&value)?;

        let Some(text) = openai_response_text(&value) else {
            if openai_refused(&value) {
                return Err(AiSdkError::Refused {
                    provider: AiProvider::OpenAi,
                    reason: "refusal",
                });
            }
            return Err(AiSdkError::NoAnswer {
                provider: AiProvider::OpenAi,
                reason: openai_no_text_reason(&value),
            });
        };

        let text = match &request.output_schema {
            Some(output_schema) => validate_structured_output(&text, output_schema)?,
            None => text,
        };

        Ok(LlmResponse { content: text })
    }
}

/// The answer of a Responses payload: `output_text` when the server sends it (the official SDKs
/// compute it client-side), else the first text of the first `message` item of `output`. A model
/// that reasons puts a `reasoning` item first, so the first item is not the answer: reading it
/// left SY.cognition's tagger with "missing text output" on gpt-5.5 (A-69). A blank text is none:
/// it is nothing to send (FINDINGS A-184).
fn openai_response_text(value: &Value) -> Option<String> {
    let written = |text: &&str| !text.trim().is_empty();
    if let Some(text) = value
        .get("output_text")
        .and_then(Value::as_str)
        .filter(written)
    {
        return Some(text.to_string());
    }
    value
        .get("output")
        .and_then(Value::as_array)?
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("message"))?
        .get("content")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|part| part.get("text").and_then(Value::as_str))
        .filter(written)
        .map(ToString::to_string)
}

/// Why a Responses payload has no answer, for its error: its `status` and the reason an incomplete
/// one gives, so an empty answer reads apart from a refusal, which is its own (FINDINGS A-162).
fn openai_no_text_reason(value: &Value) -> String {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let reason = value
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
        .map(|reason| format!(", reason={reason}"))
        .unwrap_or_default();
    format!("status={status}{reason}")
}

/// Whether a Responses payload holds a message's refusal part: the model declined (FINDINGS A-184).
fn openai_refused(value: &Value) -> bool {
    value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .any(|part| part.get("type").and_then(Value::as_str) == Some("refusal"))
}

/// OpenAI's content filter cut the answer (`status: incomplete`, reason `content_filter`): the text
/// before the cut is no answer to give, nor any call one to run. It is a refusal on both paths,
/// whatever the answer holds, as Anthropic's `stop_reason: refusal` is (FINDINGS A-184).
fn openai_check_filter(value: &Value) -> Result<()> {
    let filtered = value.get("status").and_then(Value::as_str) == Some("incomplete")
        && value
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
            == Some("content_filter");
    if filtered {
        return Err(AiSdkError::Refused {
            provider: AiProvider::OpenAi,
            reason: "content_filter",
        });
    }
    Ok(())
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
    let parsed = parse_structured_output(raw, output_schema)?;
    serde_json::to_string(&parsed).map_err(AiSdkError::from)
}

/// The object a model's `raw` answer holds, checked against `output_schema`: its JSON, also inside
/// a fence or among other text (`extract_json_candidate`). An answer without that object is
/// `InvalidStructuredOutput`.
pub(crate) fn parse_structured_output(
    raw: &str,
    output_schema: &OutputSchemaSpec,
) -> Result<Value> {
    let parsed = extract_json_value(raw).and_then(|parsed| {
        validate_value_against_schema(&parsed, output_schema.json_schema())?;
        Ok(parsed)
    });
    if let Err(err) = &parsed {
        warn_unstructured_answer(output_schema.name(), err, raw);
    }
    parsed
}

/// The JSON value a model's `raw` answer holds, also inside a fence or among other text
/// (`extract_json_candidate`); an answer without one is `InvalidStructuredOutput`.
pub(crate) fn extract_json_value(raw: &str) -> Result<Value> {
    let candidate = extract_json_candidate(raw)?;
    serde_json::from_str(&candidate).map_err(|err| AiSdkError::InvalidStructuredOutput {
        detail: format!("json_parse_error: {err}"),
    })
}

/// The log of an answer that is not the object its contract, `schema`, asks for: what the model
/// wrote, to tune the prompt that led to it, since the error says only why.
pub(crate) fn warn_unstructured_answer(schema: &str, err: &AiSdkError, raw: &str) {
    tracing::warn!(
        schema = %schema,
        error = %err,
        output_len = raw.len(),
        output_preview = %raw.chars().take(200).collect::<String>(),
        "a model's answer is not the object its schema asks for"
    );
}

fn extract_json_candidate(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AiSdkError::InvalidStructuredOutput {
            detail: "empty_response".to_string(),
        });
    }

    if let Some(fenced) = trimmed.strip_prefix("```") {
        let (opening_line, body) = fenced.split_once('\n').unwrap_or((fenced, ""));
        // The object starts after the opening fence's language tag, on that line
        // (```json {"reply": ...}```) or the next one, and ends where its JSON ends: the closing
        // fence, and any text or fenced block after it, is the model's (FINDINGS A-131). Without a
        // whole object, it ends at the last closing fence.
        let tag_len = opening_line
            .find(|c: char| c.is_whitespace() || c == '{')
            .unwrap_or(opening_line.len());
        if let Some(object) = [&fenced[tag_len..], body]
            .into_iter()
            .find_map(|start| leading_json_object(start.trim_start()))
        {
            return Ok(object.to_string());
        }
        let body = body.rfind("```").map_or(body, |end| &body[..end]);
        let inner = body.trim();
        if inner.is_empty() {
            return Err(AiSdkError::InvalidStructuredOutput {
                detail: "empty_json_fence".to_string(),
            });
        }
        return Ok(inner.to_string());
    }

    // Among other text, the object is the first whole one, whatever text is around it: a `{` that
    // starts none (`Elijo {consult}: {...}`) is the text's.
    if let Some(object) = trimmed
        .match_indices('{')
        .find_map(|(start, _)| leading_json_object(&trimmed[start..]))
    {
        return Ok(object.to_string());
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

/// The JSON object `text` starts with, without what follows it, when it starts with a whole one:
/// an object, never another value.
fn leading_json_object(text: &str) -> Option<&str> {
    let mut values = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    match values.next() {
        Some(Ok(Value::Object(_))) => Some(&text[..values.byte_offset()]),
        _ => None,
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

pub(crate) struct OpenAiFunctionCallingModel {
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
        // The run's answer schema goes next to the tools, as the call without tools sends it, on
        // every turn, as the tools do: any turn without a call is the final answer.
        if let Some(output_schema) = &request.output_schema {
            body["text"] = json!({
                "format": build_openai_response_format(output_schema)?,
            });
        }

        let mut asked_without_format = false;
        let value = loop {
            match self
                .client
                .send(&body, &self.model, UsageKind::FunctionTurn)
                .await
            {
                Ok(value) => break value,
                // A schema OpenAI cannot hold, a strict one with an optional property or one a
                // model without `json_schema` refuses, comes back as a 400 that names
                // `text.format`: the turn is asked again without it, and the schema reaches the
                // model only as its caller's instruction, as on every tool turn before FINDINGS
                // A-150 and as Anthropic's turn does with `output_config`.
                Err(err)
                    if !asked_without_format
                        && body.get("text").is_some()
                        && openai_text_format_refused(&err) =>
                {
                    asked_without_format = true;
                    if let Some(fields) = body.as_object_mut() {
                        fields.remove("text");
                    }
                    tracing::warn!(
                        "openai model refused text.format on a tool turn; retrying without it"
                    );
                }
                Err(err) => return Err(err),
            }
        };
        // A turn cut at the token limit can end inside a tool call's arguments: it is an error,
        // as on the plain path and on Anthropic's turns, never a turn to act on. So is one the
        // content filter cut.
        openai_check_truncation(&value)?;
        openai_check_filter(&value)?;

        let response_id = value
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let assistant_text = openai_response_text(&value);
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
            }
        }

        // A turn with neither a call nor text because the model declined is its refusal, as on the
        // plain path, never a turn its caller asks again without tools.
        if tool_calls.is_empty() && assistant_text.is_none() && openai_refused(&value) {
            return Err(AiSdkError::Refused {
                provider: AiProvider::OpenAi,
                reason: "refusal",
            });
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
/// How many times a call to either provider is tried, by the rules both advise
/// (`provider_should_retry`, `provider_retry_delay`).
const PROVIDER_MAX_ATTEMPTS: usize = 3;

/// Private to the SDK: every call goes through the factories, with its `AiAccount` (§7.2).
#[derive(Clone)]
pub(crate) struct AnthropicMessagesClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    account: AiAccount,
}

impl AnthropicMessagesClient {
    pub(crate) fn new(api_key: impl Into<String>, account: AiAccount) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: ANTHROPIC_MESSAGES_URL.to_string(),
            account,
        }
    }

    pub(crate) fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub(crate) fn function_model(
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

    /// One request, retried as Anthropic advises. Each attempt is a call of its own for the usage
    /// records (§7.2): only the one that answers 2xx is recorded, and a retry's wait is no call.
    async fn send(&self, body: &Value, kind: UsageKind) -> Result<Value> {
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for attempt in 0..PROVIDER_MAX_ATTEMPTS {
            let mut call = self.account.begin(AiProvider::Anthropic, &model, kind);
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
                Err(err) if attempt + 1 < PROVIDER_MAX_ATTEMPTS => {
                    call.failed();
                    let delay = provider_retry_delay(attempt, None);
                    // Logged before the wait: a caller with a shorter deadline (cognition's 8 s)
                    // may drop the call during it.
                    tracing::warn!(attempt = attempt + 1, delay_ms = delay.as_millis() as u64, error = %err, "retrying Anthropic transport error");
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(err) => {
                    call.failed();
                    return Err(err.into());
                }
            };
            let status = response.status();
            let headers = response.headers().clone();
            let request_id = header_text(&headers, "request-id");
            // Decide retry from STATUS + headers BEFORE touching the body (audit M1): a
            // proxy/LB 5xx or 429 often carries an HTML or empty body, and parsing it first
            // (`response.json().await?`) aborted the whole retry loop on a decode error exactly
            // when a retry was warranted.
            if !status.is_success()
                && attempt + 1 < PROVIDER_MAX_ATTEMPTS
                && provider_should_retry(status.as_u16(), &headers)
            {
                call.failed();
                let delay = provider_retry_delay(attempt, provider_retry_after(&headers));
                tracing::warn!(attempt = attempt + 1, delay_ms = delay.as_millis() as u64, status = %status, request_id = ?request_id, "retrying Anthropic response");
                tokio::time::sleep(delay).await;
                continue;
            }
            if status.is_success() {
                return match response.json::<Value>().await {
                    Ok(value) => {
                        call.answered(Some(&value));
                        Ok(value)
                    }
                    Err(err) => {
                        call.answered(None);
                        Err(err.into())
                    }
                };
            }
            // Non-retryable (or last attempt): parse tolerantly so a non-JSON error body
            // surfaces the status/request-id instead of a masking decode error.
            call.failed();
            let value: Value = response.json().await.unwrap_or(Value::Null);
            return Err(provider_status_error(
                AiProvider::Anthropic,
                status.as_u16(),
                &value,
                request_id,
            )
            .into());
        }
        Err(AiSdkError::Protocol(
            "Anthropic retry loop exhausted unexpectedly".to_string(),
        ))
    }
}

/// Whether a provider's error status is worth asking again: its `x-should-retry` when it sends one,
/// else 408, 409, 429 and any 5xx, as both providers' own SDKs do.
fn provider_should_retry(status: u16, headers: &reqwest::header::HeaderMap) -> bool {
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

/// The wait a provider asks for before the next attempt (`retry-after-ms`, `retry-after`), at most
/// 60 s.
fn provider_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
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

/// The wait before the next attempt: the provider's, else 0.5 s doubling, with jitter.
fn provider_retry_delay(attempt: usize, server_delay: Option<Duration>) -> Duration {
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

/// A provider's header as text, when it sends one that is not empty.
fn header_text(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// An error status of either provider, from its body's `error.type`, `error.param` and
/// `error.message`, never the body itself (audit B6): OpenAI's 401 body echoes a masked form of
/// the key ("Incorrect API key provided: sk-…"), so the message is scrubbed of it too.
fn provider_status_error(
    provider: AiProvider,
    status: u16,
    body: &Value,
    request_id: Option<String>,
) -> ProviderError {
    let text = |pointer: &str| body.pointer(pointer).and_then(Value::as_str);
    ProviderError {
        provider,
        status,
        error_type: text("/error/type").unwrap_or("unknown_error").to_string(),
        param: text("/error/param")
            .filter(|param| !param.is_empty())
            .map(ToString::to_string),
        request_id,
        message: scrub_secret_like(text("/error/message").unwrap_or("request failed")),
    }
}

/// An OpenAI error status (`provider_status_error`).
fn openai_status_error(status: u16, body: &Value, request_id: Option<String>) -> ProviderError {
    provider_status_error(AiProvider::OpenAi, status, body, request_id)
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
        return Err(truncated_answer_error(
            AiProvider::OpenAi,
            "max_output_tokens",
        ));
    }
    Ok(())
}

/// The error of an answer cut at its token limit, the same on both providers so a caller can tell
/// it apart (`AiSdkError::is_truncated_answer`).
fn truncated_answer_error(provider: AiProvider, limit: &'static str) -> AiSdkError {
    AiSdkError::Truncated { provider, limit }
}

/// An OpenAI 400 that refuses the `text.format` a tool turn sent: its `param` names it.
fn openai_text_format_refused(err: &AiSdkError) -> bool {
    matches!(
        err,
        AiSdkError::Provider(err)
            if err.provider == AiProvider::OpenAi
                && err.status == 400
                && err
                    .param
                    .as_deref()
                    .is_some_and(|param| param.starts_with("text.format"))
    )
}

/// Heuristic: an Anthropic 400 that indicates the model does not support the `output_config`
/// structured-outputs feature (audit M4), so `generate` can retry once with a prompt-instruction
/// fallback instead of surfacing the raw 400, and a tool turn once without it. Conservative — only
/// a 400 whose message names the feature.
fn anthropic_output_config_unsupported(err: &AiSdkError) -> bool {
    let AiSdkError::Provider(err) = err else {
        return false;
    };
    if err.provider != AiProvider::Anthropic || err.status != 400 {
        return false;
    }
    let lower = err.message.to_ascii_lowercase();
    lower.contains("output_config") || lower.contains("json_schema") || lower.contains("structured")
}

/// The `output_config` that asks Anthropic for `output_schema`'s object (structured outputs), the
/// same on the plain path and on a tool turn. It holds the model's text to the schema; tool calls
/// and thinking stay free, so a tool turn can still call its tools.
fn anthropic_output_config(output_schema: &OutputSchemaSpec) -> Result<Value> {
    output_schema.validate()?;
    Ok(json!({
        "format": {
            "type": "json_schema",
            "schema": output_schema.json_schema(),
        }
    }))
}

/// Detect a silently-truncated Anthropic completion (audit A2): stop_reason="max_tokens" means
/// the output was cut at the (now 4096) cap. Fail loudly so a caller needing more raises
/// model_settings.max_output_tokens rather than parsing a truncated tool_use/structured output.
fn anthropic_check_truncation(value: &Value) -> Result<()> {
    if value.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
        return Err(truncated_answer_error(AiProvider::Anthropic, "max_tokens"));
    }
    Ok(())
}

/// `stop_reason: refusal`: the model declined, and whatever text came before it is no answer to
/// give, nor any tool call one to run (FINDINGS A-184).
fn anthropic_check_refusal(value: &Value) -> Result<()> {
    if value.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        return Err(AiSdkError::Refused {
            provider: AiProvider::Anthropic,
            reason: "stop_reason=refusal",
        });
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
            body.insert(
                "output_config".to_string(),
                anthropic_output_config(output_schema)?,
            );
        }
        // Capability fallback (audit B5/M4): output_config is sent optimistically; a model that
        // does not support structured outputs answers 400. Instead of surfacing that raw 400,
        // retry ONCE without output_config, steering the schema via a system instruction — the
        // local validate_structured_output below still enforces the contract on the result.
        let value = match self
            .send(&Value::Object(body.clone()), UsageKind::Generate)
            .await
        {
            Ok(value) => value,
            Err(err)
                if request.output_schema.is_some() && anthropic_output_config_unsupported(&err) =>
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
                self.send(&Value::Object(body), UsageKind::Generate).await?
            }
            Err(err) => return Err(err),
        };
        anthropic_check_truncation(&value)?;
        anthropic_check_refusal(&value)?;
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
    if text.trim().is_empty() {
        // Nothing to send, and why, so an empty answer reads apart from a refusal (FINDINGS
        // A-162, A-184).
        let stop_reason = value
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("none");
        return Err(AiSdkError::NoAnswer {
            provider: AiProvider::Anthropic,
            reason: format!("stop_reason={stop_reason}"),
        });
    }
    Ok(text)
}

#[derive(Clone)]
pub(crate) struct AnthropicFunctionCallingModel {
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
        let mut tools = request
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
        // A cache breakpoint on the last tool: the definitions are the same on every turn, in name
        // order (`FunctionToolRegistry`), so Anthropic reads them from its prompt cache instead of
        // billing them again. Without tools there is no breakpoint and no `tools`.
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = anthropic_cache_breakpoint();
        }
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), Value::String(self.model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));
        if !tools.is_empty() {
            body.insert("tools".to_string(), Value::Array(tools));
        }
        body.insert(
            "max_tokens".to_string(),
            Value::from(
                self.model_settings
                    .max_output_tokens
                    .unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS),
            ),
        );
        if !system.is_empty() {
            body.insert("system".to_string(), Value::Array(system));
        }
        if let Some(temperature) = self.model_settings.temperature {
            body.insert("temperature".to_string(), Value::from(temperature));
        }
        if let Some(top_p) = self.model_settings.top_p {
            body.insert("top_p".to_string(), Value::from(top_p));
        }
        // The run's answer schema goes next to the tools on every turn, as the tools do: any turn
        // without a call is the final answer. It is the same on every turn of a run, so the turns
        // still read the prompt cache, which a different `output_config` would invalidate.
        if let Some(output_schema) = &request.output_schema {
            body.insert(
                "output_config".to_string(),
                anthropic_output_config(output_schema)?,
            );
        }
        let mut body = Value::Object(body);
        let value = match self.client.send(&body, UsageKind::FunctionTurn).await {
            Ok(value) => value,
            // A model without structured outputs refuses `output_config` (audit M4): the turn is
            // asked again without it, and the schema reaches the model only as its caller's
            // instruction, as on every tool turn before FINDINGS A-150.
            Err(err)
                if request.output_schema.is_some() && anthropic_output_config_unsupported(&err) =>
            {
                if let Some(body) = body.as_object_mut() {
                    body.remove("output_config");
                }
                tracing::warn!(
                    "anthropic model rejected output_config on a tool turn; retrying without it"
                );
                self.client.send(&body, UsageKind::FunctionTurn).await?
            }
            Err(err) => return Err(err),
        };
        anthropic_check_truncation(&value)?;
        anthropic_check_refusal(&value)?;
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
        // The whole prompt and the answer, as OpenAI's `total_tokens` counts them: Anthropic's
        // `input_tokens` is only the part after the last cache breakpoint, and Archi's token
        // budget reads this sum.
        let tokens_used = value
            .get("usage")
            .map(|usage| {
                [
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                    "output_tokens",
                ]
                .iter()
                .map(|field| usage.get(field).and_then(Value::as_u64).unwrap_or(0))
                .fold(0, u64::saturating_add)
            })
            .unwrap_or(0) as u32;
        // A blank text is none, as on OpenAI's turn: nothing to send (FINDINGS A-184).
        let assistant_text = assistant_text.join("");
        Ok(FunctionModelTurnResponse {
            assistant_text: (!assistant_text.trim().is_empty()).then_some(assistant_text),
            tool_calls,
            tokens_used,
        })
    }
}

/// Anthropic's prompt-cache breakpoint with its default 5-minute life: the prompt up to and
/// including the block that carries it, in the order `tools`, `system`, `messages`, is cached and
/// read back by the next request that starts with the same bytes. A prompt shorter than the
/// model's minimum is sent uncached, without an error.
fn anthropic_cache_breakpoint() -> Value {
    json!({"type": "ephemeral"})
}

/// The `system` blocks and the `messages` of a tool turn.
///
/// The configured system prompt (an agent's definition, a SY node's own prompt) is the first block
/// and a cache breakpoint: after the tools, it is what every turn of the node repeats. The run's
/// own system text (its immediate memory: the summary, the active operations) changes from message
/// to message, so it follows in a block of its own, after the breakpoint. A blank one is left out:
/// Anthropic refuses an empty text block.
fn build_anthropic_function_messages(
    configured_system: &Option<String>,
    items: &[FunctionLoopItem],
) -> Result<(Vec<Value>, Vec<Value>)> {
    let mut run_system = Vec::new();
    let mut messages = Vec::new();
    let mut index = 0usize;
    while index < items.len() {
        match &items[index] {
            FunctionLoopItem::SystemText { content } => run_system.push(content.clone()),
            FunctionLoopItem::UserText { content } => messages.push(json!({
                "role": "user",
                "content": [{"type":"text", "text":content}],
            })),
            FunctionLoopItem::UserContentParts { content } => messages.push(json!({
                "role": "user",
                "content": content.iter().map(model_part_to_anthropic).collect::<Result<Vec<_>>>()?,
            })),
            // A blank one is left out, as a blank system block is: Anthropic refuses it, and a
            // history that keeps one would fail every later turn.
            FunctionLoopItem::AssistantText { content } if content.trim().is_empty() => {}
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
                        && blocks
                            .iter()
                            .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                });
        if role == Some("assistant") || orphan_tool_result {
            messages.remove(0);
        } else {
            break;
        }
    }
    let mut system = Vec::new();
    if let Some(configured) = configured_system
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        system.push(json!({
            "type": "text",
            "text": configured,
            "cache_control": anthropic_cache_breakpoint(),
        }));
    }
    let run_system = run_system
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();
    if !run_system.is_empty() {
        system.push(json!({"type": "text", "text": run_system.join("\n\n")}));
    }
    Ok((system, messages))
}

/// The longest one model call may take: one `generate` or one tool turn, the provider's own retries
/// included (Anthropic's `send`). Past it the call fails with `AiSdkError::Timeout` and its usage
/// record says `abandoned`, so a stuck connection costs one bounded call, never a worker. It is
/// there for stuck connections, not slow answers: a reasoning model with no `max_output_tokens`
/// can rightly take minutes. A node whose calls rightly take longer (Archi's pipeline roles) gives
/// its own limit (`create_function_calling_model_with_call_timeout`); the HTTP client sets none.
pub const MODEL_CALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounds each call of the client it wraps; the factories wrap every client they hand out.
struct WithCallTimeout<C> {
    inner: C,
    limit: Duration,
}

impl<C> WithCallTimeout<C> {
    fn new(inner: C, limit: Duration) -> Self {
        Self { inner, limit }
    }

    async fn bounded<T>(&self, call: impl std::future::Future<Output = Result<T>>) -> Result<T> {
        tokio::time::timeout(self.limit, call).await.map_err(|_| {
            AiSdkError::Timeout(format!(
                "model call took longer than {} s",
                self.limit.as_secs()
            ))
        })?
    }
}

#[async_trait]
impl<C: LlmClient> LlmClient for WithCallTimeout<C> {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse> {
        self.bounded(self.inner.generate(request)).await
    }
}

#[async_trait]
impl<M: FunctionCallingModel> FunctionCallingModel for WithCallTimeout<M> {
    async fn run_turn(
        &self,
        request: FunctionModelTurnRequest,
    ) -> Result<FunctionModelTurnResponse> {
        self.bounded(self.inner.run_turn(request)).await
    }
}

/// The plain-path client. Every model call of the system goes through this or
/// `create_function_calling_model`, with the account that says who spends and who pays (§7.2):
/// each answer leaves one usage record. Each call is bounded by `MODEL_CALL_TIMEOUT`.
pub fn create_llm_client(
    provider: AiProvider,
    api_key: impl Into<String>,
    base_url: Option<String>,
    account: AiAccount,
) -> Arc<dyn LlmClient> {
    let api_key = api_key.into();
    match provider {
        AiProvider::OpenAi => {
            let mut client = OpenAiResponsesClient::new(api_key, account);
            if let Some(base_url) = base_url {
                client = client.with_base_url(base_url);
            }
            Arc::new(WithCallTimeout::new(client, MODEL_CALL_TIMEOUT))
        }
        AiProvider::Anthropic => {
            let mut client = AnthropicMessagesClient::new(api_key, account);
            if let Some(base_url) = base_url {
                client = client.with_base_url(base_url);
            }
            Arc::new(WithCallTimeout::new(client, MODEL_CALL_TIMEOUT))
        }
    }
}

/// The tool-loop client, each turn bounded by `MODEL_CALL_TIMEOUT`; see `create_llm_client`.
pub fn create_function_calling_model(
    provider: AiProvider,
    api_key: impl Into<String>,
    base_url: Option<String>,
    model: impl Into<String>,
    system: Option<String>,
    model_settings: ModelSettings,
    account: AiAccount,
) -> Arc<dyn FunctionCallingModel> {
    create_function_calling_model_with_call_timeout(
        provider,
        api_key,
        base_url,
        model,
        system,
        model_settings,
        account,
        MODEL_CALL_TIMEOUT,
    )
}

/// `create_function_calling_model` with each turn bounded by `call_timeout` instead of
/// `MODEL_CALL_TIMEOUT`, for a node whose calls rightly take longer.
pub fn create_function_calling_model_with_call_timeout(
    provider: AiProvider,
    api_key: impl Into<String>,
    base_url: Option<String>,
    model: impl Into<String>,
    system: Option<String>,
    model_settings: ModelSettings,
    account: AiAccount,
    call_timeout: Duration,
) -> Arc<dyn FunctionCallingModel> {
    let api_key = api_key.into();
    let model = model.into();
    match provider {
        AiProvider::OpenAi => {
            let mut client = OpenAiResponsesClient::new(api_key, account);
            if let Some(base_url) = base_url {
                client = client.with_base_url(base_url);
            }
            Arc::new(WithCallTimeout::new(
                client.function_model(model, system, model_settings),
                call_timeout,
            ))
        }
        AiProvider::Anthropic => {
            let mut client = AnthropicMessagesClient::new(api_key, account);
            if let Some(base_url) = base_url {
                client = client.with_base_url(base_url);
            }
            Arc::new(WithCallTimeout::new(
                client.function_model(model, system, model_settings),
                call_timeout,
            ))
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
        // B6: the error carries only what the body names, never the body itself (OpenAI's 401
        // body echoes a masked key), and the message is scrubbed of the key.
        let body = json!({"error": {"type": "invalid_request_error", "message": "Incorrect API key provided: sk-abc...xyz"}});
        let err = openai_status_error(401, &body, Some("req_1".to_string()));
        assert_eq!(err.kind(), crate::errors::ProviderErrorKind::Auth);
        assert_eq!(err.error_type, "invalid_request_error");
        assert_eq!(err.param, None);
        assert!(!err.message.contains("sk-abc...xyz")); // the masked-key echo must NOT be present
        let text = AiSdkError::from(err).to_string();
        assert_eq!(
            text,
            "openai error status=401 type=invalid_request_error request_id=req_1 \
             message=Incorrect API key provided: sk-<redacted>"
        );
        assert!(!text.contains("body="));
    }

    /// A status names its kind, whichever provider sent it; a body that names nothing, as a
    /// proxy's page, still leaves the status.
    #[test]
    fn a_provider_error_status_names_its_kind() {
        use crate::errors::ProviderErrorKind as Kind;
        for (status, kind) in [
            (400, Kind::InvalidRequest),
            (404, Kind::InvalidRequest),
            (422, Kind::InvalidRequest),
            (401, Kind::Auth),
            (403, Kind::Auth),
            (408, Kind::Timeout),
            (409, Kind::Other),
            (429, Kind::RateLimited),
            (500, Kind::Unavailable),
            (529, Kind::Unavailable),
        ] {
            let err = provider_status_error(AiProvider::Anthropic, status, &Value::Null, None);
            assert_eq!(err.kind(), kind, "{status}");
            assert_eq!(err.error_type, "unknown_error");
        }
    }

    #[test]
    fn scrub_secret_like_redacts_keys_but_not_ordinary_words() {
        assert_eq!(
            scrub_secret_like("Incorrect API key provided: sk-abc...xyz please"),
            "Incorrect API key provided: sk-<redacted> please"
        );
        assert_eq!(
            scrub_secret_like("token sk-live-REALKEY"),
            "token sk-<redacted>"
        );
        // "sk-" inside ordinary words must be preserved (task-id contains s,k,-).
        assert_eq!(
            scrub_secret_like("Invalid value for task-id parameter"),
            "Invalid value for task-id parameter"
        );
        assert_eq!(
            scrub_secret_like("risk-averse disk-full"),
            "risk-averse disk-full"
        );
        // key=sk-... (preceded by '=') is a token boundary and IS redacted.
        assert_eq!(scrub_secret_like("key=sk-abc def"), "key=sk-<redacted> def");
    }

    /// An answer with no text says why: Anthropic's `stop_reason`, and the Responses payload's
    /// `status` with a refusal named, so a refusal and an empty answer read apart (FINDINGS A-162).
    #[test]
    fn an_answer_with_no_text_says_why() {
        let err = anthropic_response_text(&json!({
            "content": [{"type": "thinking", "thinking": "..."}], "stop_reason": "refusal"
        }))
        .expect_err("no text");
        assert_eq!(
            err.to_string(),
            "anthropic answered with no text (stop_reason=refusal)"
        );
        assert!(anthropic_response_text(&json!({"content": []}))
            .expect_err("no text")
            .to_string()
            .ends_with("(stop_reason=none)"));
        assert_eq!(
            anthropic_response_text(&json!({
                "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn"
            }))
            .expect("text"),
            "ok"
        );
        assert_eq!(
            openai_no_text_reason(&json!({"status": "completed", "output": []})),
            "status=completed"
        );
        assert_eq!(
            openai_no_text_reason(&json!({
                "status": "incomplete", "incomplete_details": {"reason": "content_filter"}
            })),
            "status=incomplete, reason=content_filter"
        );
        // A refusal, or the filter's cut, is its own error (FINDINGS A-184).
        assert!(openai_refused(&json!({"status": "completed", "output": [
            {"type": "message", "content": [{"type": "refusal", "refusal": "I can't help with that."}]}
        ]})));
        assert!(!openai_refused(
            &json!({"status": "completed", "output": []})
        ));
        assert!(matches!(
            openai_check_filter(&json!({
                "status": "incomplete", "incomplete_details": {"reason": "content_filter"}
            })),
            Err(AiSdkError::Refused {
                reason: "content_filter",
                ..
            })
        ));
        assert!(openai_check_filter(&json!({
            "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}
        }))
        .is_ok());
    }

    #[test]
    fn truncation_detectors_flag_max_tokens() {
        // A2: both providers' "hit the cap" signals surface as an explicit error.
        assert!(openai_check_truncation(
            &json!({"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}})
        )
        .is_err());
        assert!(openai_check_truncation(&json!({"status": "completed"})).is_ok());
        assert!(anthropic_check_truncation(&json!({"stop_reason": "max_tokens"})).is_err());
        assert!(anthropic_check_truncation(&json!({"stop_reason": "end_turn"})).is_ok());
        // A caller tells the cut apart from any other protocol error, on both providers.
        for err in [
            openai_check_truncation(&json!({
                "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}
            }))
            .expect_err("openai cut"),
            anthropic_check_truncation(&json!({"stop_reason": "max_tokens"}))
                .expect_err("anthropic cut"),
        ] {
            assert!(err.is_truncated_answer(), "{err}");
        }
        assert!(
            !AiSdkError::from(openai_status_error(400, &json!({}), None)).is_truncated_answer()
        );
    }

    #[test]
    fn output_config_unsupported_only_on_capability_400() {
        // M4: the capability-fallback heuristic fires only on a 400 that names the feature.
        let anthropic = |status, message: &str| {
            AiSdkError::from(provider_status_error(
                AiProvider::Anthropic,
                status,
                &json!({"error": {"type": "invalid_request_error", "message": message}}),
                None,
            ))
        };
        assert!(anthropic_output_config_unsupported(&anthropic(
            400,
            "output_config not supported"
        )));
        assert!(!anthropic_output_config_unsupported(&anthropic(
            429,
            "slow down"
        )));
        assert!(!anthropic_output_config_unsupported(&anthropic(
            400,
            "bad max_tokens"
        )));
        assert!(!anthropic_output_config_unsupported(&AiSdkError::from(
            openai_status_error(
                400,
                &json!({"error": {"message": "output_config not supported"}}),
                None
            )
        )));
    }
    use crate::function_calling::{FunctionToolDefinition, FunctionToolResult};

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

    fn test_account() -> AiAccount {
        AiAccount::root(crate::usage::UsageRecorder::disabled(), "SY.test@motherbee")
    }

    fn spawn_single_response_server(
        response_body: Value,
    ) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
        spawn_response_server("200 OK", response_body)
    }

    fn spawn_response_server(
        status_line: &'static str,
        response_body: Value,
    ) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
        spawn_responses_server(vec![(status_line, response_body)])
    }

    /// A server that answers one request per connection with each of `responses` in turn, and
    /// hands over each request's body. Each answer closes its connection, so the client's next
    /// request opens the next one.
    fn spawn_responses_server(
        responses: Vec<(&'static str, Value)>,
    ) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
        spawn_raw_responses_server(
            responses
                .into_iter()
                .map(|(status_line, body)| RawAnswer {
                    status_line,
                    headers: "Content-Type: application/json\r\n",
                    body: serde_json::to_vec(&body).expect("serialize response"),
                })
                .collect(),
        )
    }

    /// One answer of `spawn_raw_responses_server`: its status line, its header lines (each ending
    /// in CRLF) and its body, which need not be JSON.
    struct RawAnswer {
        status_line: &'static str,
        headers: &'static str,
        body: Vec<u8>,
    }

    /// `spawn_responses_server` with each answer's headers and body as given.
    fn spawn_raw_responses_server(
        responses: Vec<RawAnswer>,
    ) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            for answer in responses {
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
                let body_value: Value =
                    serde_json::from_slice(body_bytes).expect("json request body");
                tx.send(body_value).expect("send captured body");

                let response = format!(
                    "HTTP/1.1 {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    answer.status_line,
                    answer.headers,
                    answer.body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response head");
                stream.write_all(&answer.body).expect("write response body");
            }
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
        assert!(instruction.contains("never null"));
        assert!(instruction.contains("Do not include properties outside this schema"));
    }

    /// The object a model wrote in a fence is that object, with a line for the person after the
    /// fence or without one.
    #[test]
    fn a_fenced_object_followed_by_text_is_that_object() {
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
        for raw in [
            "```json\n{\"ok\":true}\n```\n¿Algo más?",
            "```json\n{\"ok\":true}\n```",
            "```\n{\"ok\":true}\n```\n",
            // FINDINGS A-131: another fenced block after the object's.
            "```json\n{\"ok\":true}\n```\nPor ejemplo:\n```\nplan pro\n```",
            // Text with braces after an object that is not fenced.
            "Listo: {\"ok\":true} y {nada más}",
        ] {
            assert_eq!(
                parse_structured_output(raw, &spec).expect(raw),
                json!({"ok": true})
            );
        }
    }

    /// A fence in the object's own text stays in it: the object ends where its JSON ends.
    #[test]
    fn a_fence_inside_the_objects_text_stays_in_it() {
        let raw = "```json\n{\"reply\":\"Mirá:\\n```\\nplan pro\\n```\"}\n```\nListo.";
        assert_eq!(
            extract_json_value(raw).expect(raw),
            json!({"reply": "Mirá:\n```\nplan pro\n```"})
        );
    }

    /// Among other text, the object is the first whole one, from whichever `{` it starts at: a `{`
    /// that starts none is the text's. Text with no object is still an error.
    #[test]
    fn a_brace_that_starts_no_object_is_the_texts() {
        let object =
            json!({"reply": "Lo consulto.", "option": "consult", "params": {"question": "20%?"}});
        for raw in [
            format!("Elijo {{consult}}: {object}"),
            format!("{{nada}} y {{\"a\": }} antes de {object} y {{después}}"),
            format!("Elijo {{consult}}:\n```json\n{object}\n```"),
        ] {
            assert_eq!(extract_json_value(&raw).expect(&raw), object, "{raw}");
        }
        assert!(matches!(
            extract_json_value("Elijo {consult} y {nada}"),
            Err(AiSdkError::InvalidStructuredOutput { .. })
        ));
    }

    /// The opening fence's line may hold the object after its language tag, or with none, and the
    /// object may go on over the next lines. A tag with more words leaves the object to the next
    /// line, as before.
    #[test]
    fn an_object_on_the_opening_fences_line_is_that_object() {
        for raw in [
            "```json {\"ok\":true}```",
            "```json {\"ok\":true}\n```\n¿Algo más?",
            "```{\"ok\":true}```",
            "```JSON{\"ok\":\ntrue}\n```",
            "```json title=\"answer\"\n{\"ok\":true}\n```",
        ] {
            assert_eq!(
                extract_json_value(raw).expect(raw),
                json!({"ok": true}),
                "{raw}"
            );
        }
    }

    /// A blank assistant turn never reaches Anthropic, which refuses an empty text block: a history
    /// that kept one would fail every later turn.
    #[test]
    fn a_blank_assistant_turn_is_left_out_of_an_anthropic_request() {
        let items = vec![
            FunctionLoopItem::UserText {
                content: "hola".to_string(),
            },
            FunctionLoopItem::AssistantText {
                content: " ".to_string(),
            },
            FunctionLoopItem::UserText {
                content: "dale".to_string(),
            },
        ];
        let (_, messages) = build_anthropic_function_messages(&None, &items).expect("messages");
        assert_eq!(
            messages,
            vec![
                json!({"role":"user","content":[{"type":"text","text":"hola"}]}),
                json!({"role":"user","content":[{"type":"text","text":"dale"}]}),
            ]
        );
    }

    #[tokio::test]
    async fn openai_responses_client_sends_json_schema_when_output_schema_present() {
        let (base_url, body_rx, handle) = spawn_single_response_server(json!({
            "output_text": "{\"ok\":true}"
        }));
        let client = OpenAiResponsesClient::new("test-key", test_account()).with_base_url(base_url);
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
        let client = OpenAiResponsesClient::new("test-key", test_account()).with_base_url(base_url);
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

    #[tokio::test]
    async fn openai_responses_client_reads_the_message_after_a_reasoning_item() {
        let (base_url, _body_rx, handle) = spawn_single_response_server(json!({
            "status": "completed",
            "output": [
                {"type": "reasoning", "id": "rs_1", "summary": []},
                {"type": "message", "id": "msg_1", "role": "assistant",
                 "content": [{"type": "output_text", "text": "{\"tags\":[\"billing\"]}", "annotations": []}]}
            ]
        }));
        let client = OpenAiResponsesClient::new("test-key", test_account()).with_base_url(base_url);
        let request = LlmRequest {
            model: "gpt-5.5".to_string(),
            system: Some("sys".to_string()),
            input: "hola".to_string(),
            input_parts: None,
            output_schema: None,
            max_output_tokens: Some(1024),
            model_settings: None,
        };

        let response = client
            .generate(request)
            .await
            .expect("generate should succeed");
        assert_eq!(response.content, "{\"tags\":[\"billing\"]}");
        handle.join().expect("server thread");
    }

    #[tokio::test]
    async fn a_2xx_answer_leaves_one_usage_record_and_an_error_none() {
        let (recorder, mut records) = crate::usage::UsageRecorder::for_test();
        let account = AiAccount::charge(
            recorder,
            "AI.sales@motherbee",
            Some("tnt:22222222-2222-4222-8222-222222222222".to_string()),
            fluxbee_sdk::TenantSource::Node,
        )
        .with_thread(Some("thread-1".to_string()));
        let request = || LlmRequest {
            model: "gpt-5.5".to_string(),
            system: None,
            input: "hola".to_string(),
            input_parts: None,
            output_schema: None,
            max_output_tokens: None,
            model_settings: None,
        };

        let (base_url, _body_rx, handle) = spawn_single_response_server(json!({
            "id": "resp_1",
            "model": "gpt-5.5-2026-04-01",
            "service_tier": "default",
            "status": "completed",
            "output_text": "hola",
            "usage": {"input_tokens": 20, "input_tokens_details": {"cached_tokens": 5},
                      "output_tokens": 9, "output_tokens_details": {"reasoning_tokens": 4}}
        }));
        let client =
            OpenAiResponsesClient::new("test-key", account.clone()).with_base_url(base_url);
        client
            .generate(request())
            .await
            .expect("generate should succeed");
        handle.join().expect("server thread");
        let record = records.try_recv().expect("one usage record");
        assert_eq!(record.provider_response_id.as_deref(), Some("resp_1"));
        assert_eq!(record.served_model.as_deref(), Some("gpt-5.5-2026-04-01"));
        assert_eq!(record.service_tier.as_deref(), Some("default"));
        assert_eq!(record.kind, UsageKind::Generate);
        assert_eq!(record.outcome, fluxbee_sdk::UsageOutcome::Ok);
        assert_eq!(
            (
                record.uncached_input,
                record.cache_read,
                record.output,
                record.reasoning
            ),
            (Some(15), Some(5), Some(9), Some(4))
        );
        assert_eq!(record.thread_id.as_deref(), Some("thread-1"));

        let (base_url, _body_rx, handle) = spawn_response_server(
            "400 Bad Request",
            json!({"error": {"type": "invalid_request_error", "message": "bad"}}),
        );
        let client = OpenAiResponsesClient::new("test-key", account).with_base_url(base_url);
        assert!(client.generate(request()).await.is_err());
        handle.join().expect("server thread");
        assert!(
            records.try_recv().is_err(),
            "a non-2xx answer leaves no record"
        );
    }

    /// A tool turn cut at the token limit can end inside a tool call's arguments: it is an error,
    /// never a call to run, and its usage record still says `truncated`.
    #[tokio::test]
    async fn an_openai_tool_turn_cut_at_the_limit_is_an_error() {
        let (recorder, mut records) = crate::usage::UsageRecorder::for_test();
        let account = AiAccount::root(recorder, "AI.sales@motherbee");
        let (base_url, _body_rx, handle) = spawn_single_response_server(json!({
            "id": "resp_cut",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{
                "type": "function_call",
                "call_id": "call_1",
                "name": "publish_html_page",
                "arguments": "{\"filename\":\"page.html\",\"content\":\"<html><bo"
            }],
            "usage": {"input_tokens": 12, "output_tokens": 64}
        }));
        let model = OpenAiResponsesClient::new("test-key", account)
            .with_base_url(base_url)
            .function_model("gpt-5.5", None, ModelSettings::default());

        let err = model
            .run_turn(FunctionModelTurnRequest {
                items: vec![FunctionLoopItem::UserText {
                    content: "publish a page".to_string(),
                }],
                tools: Vec::new(),
                output_schema: None,
            })
            .await
            .expect_err("a cut turn is an error");
        handle.join().expect("server thread");

        assert!(err.is_truncated_answer(), "{err}");
        let record = records.try_recv().expect("the cut answer still cost");
        assert_eq!(record.kind, UsageKind::FunctionTurn);
        assert_eq!(record.outcome, fluxbee_sdk::UsageOutcome::Truncated);
    }

    /// A run's answer schema, as a response contract gives it.
    fn answer_schema() -> OutputSchemaSpec {
        OutputSchemaSpec::new(
            "final_output",
            json!({
                "type": "object",
                "properties": {
                    "reply": {"type": "string"},
                    "area": {"type": "string", "enum": ["sales", "invoice"]}
                },
                "required": ["reply", "area"],
                "additionalProperties": false
            }),
            true,
        )
        .expect("schema")
    }

    /// A tool turn under the run's answer schema asks OpenAI for it next to the tools, as the call
    /// without tools does (FINDINGS A-150): on the first turn, and on one that carries the tools'
    /// results after `previous_response_id`. Without a schema there is no `text`.
    #[tokio::test]
    async fn an_openai_tool_turn_asks_for_the_answer_schema_next_to_its_tools() {
        let ask = FunctionLoopItem::UserText {
            content: "the report, please".to_string(),
        };
        let call = FunctionToolCall {
            call_id: "call_1".to_string(),
            response_id: Some("resp_1".to_string()),
            name: "thread_state_get".to_string(),
            arguments: json!({}),
        };
        let after_the_call = vec![
            ask.clone(),
            FunctionLoopItem::AssistantToolCalls {
                content: None,
                calls: vec![call.clone()],
            },
            FunctionLoopItem::ToolResult {
                result: FunctionToolResult {
                    call_id: call.call_id,
                    response_id: call.response_id,
                    name: call.name,
                    arguments: call.arguments,
                    output: json!({"status": "ok"}),
                    is_error: false,
                },
            },
        ];
        let text = json!({
            "format": build_openai_response_format(&answer_schema()).expect("format")
        });
        for (items, output_schema, sent_text, previous_response_id) in [
            (vec![ask.clone()], Some(answer_schema()), Some(&text), None),
            (
                after_the_call,
                Some(answer_schema()),
                Some(&text),
                Some("resp_1"),
            ),
            (vec![ask], None, None, None),
        ] {
            let (base_url, body_rx, handle) = spawn_single_response_server(json!({
                "id": "resp_2",
                "status": "completed",
                "output_text": "{\"reply\":\"listo\",\"area\":\"sales\"}"
            }));
            OpenAiResponsesClient::new("test-key", test_account())
                .with_base_url(base_url)
                .function_model("gpt-5.5", None, ModelSettings::default())
                .run_turn(FunctionModelTurnRequest {
                    items,
                    tools: vec![tool_definition("thread_state_get")],
                    output_schema,
                })
                .await
                .expect("tool turn");
            handle.join().expect("server thread");
            let body = body_rx.recv().expect("captured body");
            assert_eq!(body["tools"][0]["name"], "thread_state_get");
            assert_eq!(body.get("text"), sent_text);
            assert_eq!(
                body.get("previous_response_id").and_then(Value::as_str),
                previous_response_id
            );
        }
    }

    struct SlowModel(Duration);

    #[async_trait]
    impl LlmClient for SlowModel {
        async fn generate(&self, _request: LlmRequest) -> Result<LlmResponse> {
            tokio::time::sleep(self.0).await;
            Ok(LlmResponse {
                content: "late".to_string(),
            })
        }
    }

    #[async_trait]
    impl FunctionCallingModel for SlowModel {
        async fn run_turn(
            &self,
            _request: FunctionModelTurnRequest,
        ) -> Result<FunctionModelTurnResponse> {
            tokio::time::sleep(self.0).await;
            Ok(FunctionModelTurnResponse {
                assistant_text: Some("late".to_string()),
                tool_calls: Vec::new(),
                tokens_used: 0,
            })
        }
    }

    /// Each call is bounded by the limit its client was made with: `MODEL_CALL_TIMEOUT`, or the one
    /// a node gives (`create_function_calling_model_with_call_timeout`).
    #[tokio::test]
    async fn a_model_call_past_its_limit_is_a_timeout() {
        let bounded = |delay| WithCallTimeout::new(SlowModel(delay), Duration::from_millis(50));
        let request = || LlmRequest {
            model: "gpt-5.5".to_string(),
            system: None,
            input: "hola".to_string(),
            input_parts: None,
            output_schema: None,
            max_output_tokens: None,
            model_settings: None,
        };
        let turn = || FunctionModelTurnRequest {
            items: Vec::new(),
            tools: Vec::new(),
            output_schema: None,
        };

        let slow = bounded(Duration::from_secs(5));
        assert!(matches!(
            LlmClient::generate(&slow, request()).await,
            Err(AiSdkError::Timeout(_))
        ));
        assert!(matches!(
            slow.run_turn(turn()).await,
            Err(AiSdkError::Timeout(_))
        ));

        let fast = bounded(Duration::from_millis(1));
        assert_eq!(
            LlmClient::generate(&fast, request())
                .await
                .expect("in time")
                .content,
            "late"
        );
        assert!(fast.run_turn(turn()).await.is_ok());
    }

    #[test]
    fn a_responses_payload_without_a_message_has_no_text() {
        assert_eq!(
            openai_response_text(&json!({"output": [{"type": "reasoning", "summary": []}]})),
            None
        );
        assert_eq!(
            openai_response_text(&json!({"output_text": "plain"})).as_deref(),
            Some("plain")
        );
        // A blank text is none (FINDINGS A-184).
        assert_eq!(openai_response_text(&json!({"output_text": " \n "})), None);
        assert_eq!(
            openai_response_text(&json!({"output": [
                {"type": "message", "content": [{"type": "output_text", "text": ""}]}
            ]})),
            None
        );
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
    fn provider_retry_policy_matches_official_status_contract() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(provider_should_retry(408, &headers));
        assert!(provider_should_retry(409, &headers));
        assert!(provider_should_retry(429, &headers));
        assert!(provider_should_retry(500, &headers));
        assert!(provider_should_retry(529, &headers));
        assert!(!provider_should_retry(400, &headers));
        assert!(!provider_should_retry(401, &headers));
        // The provider's own word wins either way.
        let mut said = reqwest::header::HeaderMap::new();
        said.insert("x-should-retry", HeaderValue::from_static("false"));
        assert!(!provider_should_retry(429, &said));
        said.insert("x-should-retry", HeaderValue::from_static("true"));
        assert!(provider_should_retry(400, &said));
        // Its wait, at most 60 s.
        let mut wait = reqwest::header::HeaderMap::new();
        wait.insert("retry-after", HeaderValue::from_static("2"));
        assert_eq!(provider_retry_after(&wait), Some(Duration::from_secs(2)));
        wait.insert("retry-after-ms", HeaderValue::from_static("250"));
        assert_eq!(
            provider_retry_after(&wait),
            Some(Duration::from_millis(250))
        );
        wait.insert("retry-after-ms", HeaderValue::from_static("600000"));
        assert_eq!(provider_retry_after(&wait), Some(Duration::from_secs(60)));
    }

    #[tokio::test]
    async fn anthropic_messages_client_uses_messages_shape() {
        let (base_url, body_rx, handle) = spawn_single_response_server(json!({
            "content": [{"type":"text", "text":"hola"}],
            "usage": {"input_tokens": 3, "output_tokens": 2}
        }));
        let client =
            AnthropicMessagesClient::new("test-key", test_account()).with_base_url(base_url);
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

    /// The body of one Anthropic tool turn, as the API receives it, and the turn.
    async fn anthropic_tool_turn(
        system: Option<&str>,
        items: Vec<FunctionLoopItem>,
        tools: Vec<FunctionToolDefinition>,
        output_schema: Option<OutputSchemaSpec>,
    ) -> (Value, FunctionModelTurnResponse) {
        let (base_url, body_rx, handle) = spawn_single_response_server(json!({
            "content": [{"type": "text", "text": "listo"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 3,
                "cache_creation_input_tokens": 50,
                "cache_read_input_tokens": 4000,
                "output_tokens": 2
            }
        }));
        let model = AnthropicMessagesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .function_model(
                "claude-sonnet-5-5",
                system.map(str::to_string),
                ModelSettings::default(),
            );
        let turn = model
            .run_turn(FunctionModelTurnRequest {
                items,
                tools,
                output_schema,
            })
            .await
            .expect("tool turn");
        assert_eq!(turn.assistant_text.as_deref(), Some("listo"));
        handle.join().expect("server thread");
        (body_rx.recv().expect("captured body"), turn)
    }

    fn tool_definition(name: &str) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: name.to_string(),
            description: format!("The {name} tool."),
            parameters_json_schema: json!({"type": "object", "properties": {}}),
        }
    }

    /// Prompt caching (Anthropic's Messages API): a breakpoint on the last tool caches every tool
    /// definition, and one on the configured system prompt caches the tools and that prompt. The
    /// run's own system text, which changes per message, goes after both, unmarked. The turn's
    /// tokens count the cached prompt too.
    #[tokio::test]
    async fn an_anthropic_tool_turn_marks_the_tools_and_the_system_prompt_for_the_cache() {
        let summary = "Immediate conversation summary:\ngoal: a report";
        let (body, turn) = anthropic_tool_turn(
            Some("You are AI.sales."),
            vec![
                FunctionLoopItem::SystemText {
                    content: summary.to_string(),
                },
                FunctionLoopItem::UserText {
                    content: "the report, please".to_string(),
                },
            ],
            vec![
                tool_definition("generate_csv_artifact"),
                tool_definition("thread_state_get"),
            ],
            None,
        )
        .await;
        let breakpoint = json!({"type": "ephemeral"});
        assert_eq!(body["tools"][0].get("cache_control"), None);
        assert_eq!(body["tools"][1]["name"], "thread_state_get");
        assert_eq!(body["tools"][1]["cache_control"], breakpoint);
        assert_eq!(
            body["system"],
            json!([
                {"type": "text", "text": "You are AI.sales.", "cache_control": breakpoint},
                {"type": "text", "text": summary}
            ])
        );
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(
            body.to_string().matches("cache_control").count(),
            2,
            "of the 4 breakpoints a request may have"
        );
        assert_eq!(turn.tokens_used, 3 + 50 + 4000 + 2);
    }

    /// A turn without tools sends no `tools` and marks nothing there; without a configured prompt,
    /// the run's own system text goes alone and unmarked, and with neither there is no `system`.
    #[tokio::test]
    async fn an_anthropic_tool_turn_without_tools_or_prompt_stays_a_plain_request() {
        let ask = || FunctionLoopItem::UserText {
            content: "hola".to_string(),
        };
        let (body, _) = anthropic_tool_turn(
            None,
            vec![
                FunctionLoopItem::SystemText {
                    content: "Active operations:\n- op-1".to_string(),
                },
                ask(),
            ],
            Vec::new(),
            None,
        )
        .await;
        assert_eq!(body.get("tools"), None);
        assert_eq!(
            body["system"],
            json!([{"type": "text", "text": "Active operations:\n- op-1"}])
        );
        assert!(!body.to_string().contains("cache_control"));

        let (body, _) = anthropic_tool_turn(Some("  "), vec![ask()], Vec::new(), None).await;
        assert_eq!(body.get("system"), None);
        assert_eq!(body.get("tools"), None);
        assert_eq!(body["messages"][0]["content"][0]["text"], "hola");
    }

    /// A tool turn under the run's answer schema asks Anthropic for it next to the tools, in the
    /// `output_config` the call without tools sends (FINDINGS A-150): structured outputs hold the
    /// model's text, never its tool calls. Without a schema there is no `output_config`.
    #[tokio::test]
    async fn an_anthropic_tool_turn_asks_for_the_answer_schema_next_to_its_tools() {
        let ask = || {
            vec![FunctionLoopItem::UserText {
                content: "the report, please".to_string(),
            }]
        };
        let tools = || vec![tool_definition("thread_state_get")];
        let (body, _) = anthropic_tool_turn(
            Some("You are AI.sales."),
            ask(),
            tools(),
            Some(answer_schema()),
        )
        .await;
        assert_eq!(body["tools"][0]["name"], "thread_state_get");
        assert_eq!(
            body["output_config"],
            json!({"format": {"type": "json_schema", "schema": answer_schema().json_schema()}})
        );

        let (base_url, body_rx, handle) = spawn_single_response_server(json!({
            "content": [{"type": "text", "text": "{\"reply\":\"listo\",\"area\":\"sales\"}"}],
            "stop_reason": "end_turn"
        }));
        AnthropicMessagesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .generate(LlmRequest {
                model: "claude-sonnet-5-5".to_string(),
                system: None,
                input: "the report, please".to_string(),
                input_parts: None,
                output_schema: Some(answer_schema()),
                max_output_tokens: None,
                model_settings: None,
            })
            .await
            .expect("generate should succeed");
        handle.join().expect("server thread");
        assert_eq!(
            body_rx.recv().expect("captured body")["output_config"],
            body["output_config"]
        );

        let (body, _) = anthropic_tool_turn(Some("You are AI.sales."), ask(), tools(), None).await;
        assert_eq!(body.get("output_config"), None);
    }

    /// A strict schema with an optional property is one OpenAI refuses, naming `text.format`: the
    /// turn is asked again without it, the caller's instruction all that steers the answer.
    #[tokio::test]
    async fn an_openai_tool_turn_is_asked_again_without_a_schema_it_refuses() {
        let (base_url, bodies, handle) = spawn_responses_server(vec![
            (
                "400 Bad Request",
                json!({"error": {
                    "type": "invalid_request_error",
                    "param": "text.format.schema",
                    "message": "Invalid schema: 'required' must list every key in properties."
                }}),
            ),
            (
                "200 OK",
                json!({"id": "resp_2", "status": "completed", "output_text": "listo"}),
            ),
        ]);
        let turn = OpenAiResponsesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .function_model("gpt-5.5", None, ModelSettings::default())
            .run_turn(FunctionModelTurnRequest {
                items: vec![FunctionLoopItem::UserText {
                    content: "the report, please".to_string(),
                }],
                tools: vec![tool_definition("thread_state_get")],
                output_schema: Some(answer_schema()),
            })
            .await
            .expect("the turn, asked again");
        handle.join().expect("server thread");
        assert_eq!(turn.assistant_text.as_deref(), Some("listo"));

        let mut refused = bodies.recv().expect("the refused request");
        let asked_again = bodies.recv().expect("the request asked again");
        assert!(refused
            .as_object_mut()
            .and_then(|body| body.remove("text"))
            .is_some());
        assert_eq!(asked_again, refused);
        let openai = |status, param: &str| {
            AiSdkError::from(openai_status_error(
                status,
                &json!({"error": {"param": param}}),
                None,
            ))
        };
        assert!(openai_text_format_refused(&openai(
            400,
            "text.format.schema"
        )));
        assert!(!openai_text_format_refused(&openai(400, "input")));
        assert!(!openai_text_format_refused(&openai(429, "text.format")));
    }

    /// A model without structured outputs refuses `output_config` with a 400 that names it: the
    /// tool turn is asked once more without it, the rest of its request as it was, and the
    /// caller's instruction is all that steers the answer, as before FINDINGS A-150.
    #[tokio::test]
    async fn an_anthropic_tool_turn_is_asked_again_without_a_schema_its_model_refuses() {
        let (base_url, bodies, handle) = spawn_responses_server(vec![
            (
                "400 Bad Request",
                json!({"type": "error", "error": {
                    "type": "invalid_request_error",
                    "message": "output_config.format: this model does not support structured outputs"
                }}),
            ),
            (
                "200 OK",
                json!({"content": [{"type": "text", "text": "listo"}], "stop_reason": "end_turn"}),
            ),
        ]);
        let turn = AnthropicMessagesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .function_model(
                "claude-3-haiku-20240307",
                Some("You are AI.sales.".to_string()),
                ModelSettings::default(),
            )
            .run_turn(FunctionModelTurnRequest {
                items: vec![FunctionLoopItem::UserText {
                    content: "the report, please".to_string(),
                }],
                tools: vec![tool_definition("thread_state_get")],
                output_schema: Some(answer_schema()),
            })
            .await
            .expect("the turn, asked again");
        handle.join().expect("server thread");
        assert_eq!(turn.assistant_text.as_deref(), Some("listo"));

        let mut refused = bodies.recv().expect("the refused request");
        let asked_again = bodies.recv().expect("the request asked again");
        assert!(refused
            .as_object_mut()
            .and_then(|body| body.remove("output_config"))
            .is_some());
        assert_eq!(asked_again, refused);
    }

    fn plain_request() -> LlmRequest {
        LlmRequest {
            model: "gpt-5.5".to_string(),
            system: None,
            input: "hola".to_string(),
            input_parts: None,
            output_schema: None,
            max_output_tokens: None,
            model_settings: None,
        }
    }

    fn plain_turn() -> FunctionModelTurnRequest {
        FunctionModelTurnRequest {
            items: vec![FunctionLoopItem::UserText {
                content: "hola".to_string(),
            }],
            tools: vec![tool_definition("thread_state_get")],
            output_schema: None,
        }
    }

    fn json_answer(status_line: &'static str, headers: &'static str, body: Value) -> RawAnswer {
        RawAnswer {
            status_line,
            headers,
            body: serde_json::to_vec(&body).expect("serialize response"),
        }
    }

    /// OpenAI's two paths ask again on a status worth it, as Anthropic's do, after the wait the
    /// provider names (FINDINGS A-184); only the answer that succeeds is a usage record.
    #[tokio::test]
    async fn an_openai_call_is_asked_again_on_a_status_worth_it() {
        let (recorder, mut records) = crate::usage::UsageRecorder::for_test();
        let account = AiAccount::root(recorder, "AI.sales@motherbee");
        let (base_url, bodies, handle) = spawn_raw_responses_server(vec![
            json_answer(
                "429 Too Many Requests",
                "Content-Type: application/json\r\nretry-after-ms: 1\r\n",
                json!({"error": {"type": "rate_limit_error", "message": "slow down"}}),
            ),
            json_answer(
                "200 OK",
                "Content-Type: application/json\r\n",
                json!({"id": "resp_1", "status": "completed", "output_text": "hola"}),
            ),
        ]);
        let answer = OpenAiResponsesClient::new("test-key", account.clone())
            .with_base_url(base_url)
            .generate(plain_request())
            .await
            .expect("the answer, asked again");
        handle.join().expect("server thread");
        assert_eq!(answer.content, "hola");
        assert_eq!(bodies.try_iter().count(), 2);
        assert!(records.try_recv().is_ok(), "the answer is a record");
        assert!(records.try_recv().is_err(), "the 429 is none");

        let (base_url, bodies, handle) = spawn_raw_responses_server(vec![
            json_answer(
                "503 Service Unavailable",
                "Content-Type: application/json\r\nretry-after: 0\r\n",
                json!({"error": {"type": "server_error", "message": "try again"}}),
            ),
            json_answer(
                "200 OK",
                "Content-Type: application/json\r\n",
                json!({"id": "resp_2", "status": "completed", "output_text": "listo"}),
            ),
        ]);
        let turn = OpenAiResponsesClient::new("test-key", account)
            .with_base_url(base_url)
            .function_model("gpt-5.5", None, ModelSettings::default())
            .run_turn(plain_turn())
            .await
            .expect("the turn, asked again");
        handle.join().expect("server thread");
        assert_eq!(turn.assistant_text.as_deref(), Some("listo"));
        assert_eq!(bodies.try_iter().count(), 2);
    }

    /// The status decides before the body is read: a proxy's 502 with an HTML page is asked
    /// again, and the last one's error keeps its status, where the body read first lost it in a
    /// decoding error (FINDINGS A-184). A 400 is asked once, and its error names its field.
    #[tokio::test]
    async fn an_openai_error_keeps_its_status_whatever_its_body() {
        let page = || RawAnswer {
            status_line: "502 Bad Gateway",
            headers: "Content-Type: text/html\r\nretry-after-ms: 1\r\nx-request-id: req_502\r\n",
            body: b"<html><body>502 Bad Gateway</body></html>".to_vec(),
        };
        let (base_url, bodies, handle) = spawn_raw_responses_server(vec![page(), page(), page()]);
        let err = OpenAiResponsesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .generate(plain_request())
            .await
            .expect_err("three 502s");
        handle.join().expect("server thread");
        assert_eq!(bodies.try_iter().count(), PROVIDER_MAX_ATTEMPTS);
        let AiSdkError::Provider(err) = err else {
            panic!("a provider error: {err:?}");
        };
        assert_eq!(
            (err.provider, err.status, err.kind()),
            (
                AiProvider::OpenAi,
                502,
                crate::errors::ProviderErrorKind::Unavailable
            )
        );
        assert_eq!(err.error_type, "unknown_error");
        assert_eq!(err.request_id.as_deref(), Some("req_502"));

        let (base_url, bodies, handle) = spawn_raw_responses_server(vec![json_answer(
            "400 Bad Request",
            "Content-Type: application/json\r\n",
            json!({"error": {
                "type": "invalid_request_error",
                "param": "input[0].content[1].file_data",
                "message": "Invalid value"
            }}),
        )]);
        let err = OpenAiResponsesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .function_model("gpt-5.5", None, ModelSettings::default())
            .run_turn(plain_turn())
            .await
            .expect_err("a 400");
        handle.join().expect("server thread");
        assert_eq!(bodies.try_iter().count(), 1, "a 400 is asked once");
        let AiSdkError::Provider(err) = err else {
            panic!("a provider error: {err:?}");
        };
        assert_eq!(err.status, 400);
        assert_eq!(err.param.as_deref(), Some("input[0].content[1].file_data"));
    }

    /// Anthropic's error status is typed the same way, with its `request-id`.
    #[tokio::test]
    async fn an_anthropic_error_status_is_typed_with_its_request_id() {
        let (base_url, _bodies, handle) = spawn_raw_responses_server(vec![json_answer(
            "401 Unauthorized",
            "Content-Type: application/json\r\nrequest-id: req_401\r\n",
            json!({"type": "error", "error": {
                "type": "authentication_error", "message": "invalid x-api-key"
            }}),
        )]);
        let err = AnthropicMessagesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .generate(plain_request())
            .await
            .expect_err("a 401");
        handle.join().expect("server thread");
        let AiSdkError::Provider(err) = err else {
            panic!("a provider error: {err:?}");
        };
        assert_eq!(
            (err.provider, err.status, err.kind()),
            (
                AiProvider::Anthropic,
                401,
                crate::errors::ProviderErrorKind::Auth
            )
        );
        assert_eq!(err.error_type, "authentication_error");
        assert_eq!(err.request_id.as_deref(), Some("req_401"));
        assert_eq!(
            err.to_string(),
            "anthropic error status=401 type=authentication_error request_id=req_401 \
             message=invalid x-api-key"
        );
    }

    /// A refusal is the model's answer, its own error on both providers and both paths, never an
    /// empty turn its caller asks again without tools (FINDINGS A-184, 0.1.112's gate).
    #[tokio::test]
    async fn a_refusal_is_its_own_error_on_both_providers() {
        let anthropic_refusal = || {
            json!({
                "content": [{"type": "text", "text": "I"}],
                "stop_reason": "refusal"
            })
        };
        let (base_url, _bodies, handle) = spawn_single_response_server(anthropic_refusal());
        let err = AnthropicMessagesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .generate(plain_request())
            .await
            .expect_err("a refusal");
        handle.join().expect("server thread");
        assert!(
            matches!(
                err,
                AiSdkError::Refused {
                    provider: AiProvider::Anthropic,
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(err.is_provider_failure());

        let (base_url, _bodies, handle) = spawn_single_response_server(anthropic_refusal());
        let err = AnthropicMessagesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .function_model("claude-sonnet-5-5", None, ModelSettings::default())
            .run_turn(plain_turn())
            .await
            .expect_err("a refusal on a tool turn");
        handle.join().expect("server thread");
        assert!(matches!(err, AiSdkError::Refused { .. }), "{err:?}");

        let openai_refusal = || {
            json!({"id": "resp_r", "status": "completed", "output": [{
                "type": "message", "role": "assistant",
                "content": [{"type": "refusal", "refusal": "I can't help with that."}]
            }]})
        };
        let (base_url, _bodies, handle) = spawn_single_response_server(openai_refusal());
        let err = OpenAiResponsesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .generate(plain_request())
            .await
            .expect_err("a refusal");
        handle.join().expect("server thread");
        assert!(
            matches!(
                err,
                AiSdkError::Refused {
                    provider: AiProvider::OpenAi,
                    reason: "refusal"
                }
            ),
            "{err:?}"
        );

        let (base_url, _bodies, handle) = spawn_single_response_server(openai_refusal());
        let err = OpenAiResponsesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .function_model("gpt-5.5", None, ModelSettings::default())
            .run_turn(plain_turn())
            .await
            .expect_err("a refusal on a tool turn");
        handle.join().expect("server thread");
        assert!(matches!(err, AiSdkError::Refused { .. }), "{err:?}");

        // The content filter's cut is a refusal on both paths, whatever text or call came before
        // it: that text is never sent, nor that call run.
        let filtered = |output: Value| {
            json!({
                "id": "resp_f", "status": "incomplete",
                "incomplete_details": {"reason": "content_filter"}, "output": output
            })
        };
        for output in [
            json!([]),
            json!([{"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "Here is how to"}]}]),
        ] {
            let (base_url, _bodies, handle) = spawn_single_response_server(filtered(output));
            let err = OpenAiResponsesClient::new("test-key", test_account())
                .with_base_url(base_url)
                .generate(plain_request())
                .await
                .expect_err("the filter's cut");
            handle.join().expect("server thread");
            assert!(
                matches!(
                    err,
                    AiSdkError::Refused {
                        reason: "content_filter",
                        ..
                    }
                ),
                "{err:?}"
            );
        }
        let (base_url, _bodies, handle) = spawn_single_response_server(filtered(json!([
            {"type": "message", "role": "assistant",
             "content": [{"type": "output_text", "text": "Let me look"}]},
            {"type": "function_call", "call_id": "call_1", "name": "thread_state_get",
             "arguments": "{}"}
        ])));
        let err = OpenAiResponsesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .function_model("gpt-5.5", None, ModelSettings::default())
            .run_turn(plain_turn())
            .await
            .expect_err("the filter's cut on a tool turn");
        handle.join().expect("server thread");
        assert!(
            matches!(
                err,
                AiSdkError::Refused {
                    reason: "content_filter",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// An answer with nothing to send, no text or only blanks and no refusal, is its own error on
    /// both providers' plain path, and a tool turn's blank text is none (FINDINGS A-184): a person
    /// gets the fixed text for it, as for a missing key, never an empty reply.
    #[tokio::test]
    async fn an_answer_with_nothing_to_send_is_its_own_error() {
        for (answer, reason) in [
            (
                json!({"id": "resp_e", "status": "completed", "output": []}),
                "status=completed",
            ),
            (
                json!({"id": "resp_b", "status": "completed", "output_text": " \n"}),
                "status=completed",
            ),
        ] {
            let (base_url, _bodies, handle) = spawn_single_response_server(answer);
            let err = OpenAiResponsesClient::new("test-key", test_account())
                .with_base_url(base_url)
                .generate(plain_request())
                .await
                .expect_err("nothing to send");
            handle.join().expect("server thread");
            let AiSdkError::NoAnswer {
                provider: AiProvider::OpenAi,
                reason: said,
            } = &err
            else {
                panic!("no answer: {err:?}");
            };
            assert_eq!(said, reason);
            assert!(err.is_provider_failure());
        }

        let blank = || {
            json!({
                "content": [{"type": "text", "text": "  "}],
                "stop_reason": "end_turn"
            })
        };
        let (base_url, _bodies, handle) = spawn_single_response_server(blank());
        let err = AnthropicMessagesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .generate(plain_request())
            .await
            .expect_err("nothing to send");
        handle.join().expect("server thread");
        assert_eq!(
            err.to_string(),
            "anthropic answered with no text (stop_reason=end_turn)"
        );
        assert!(err.is_provider_failure());

        let (base_url, _bodies, handle) = spawn_single_response_server(blank());
        let turn = AnthropicMessagesClient::new("test-key", test_account())
            .with_base_url(base_url)
            .function_model("claude-sonnet-5-5", None, ModelSettings::default())
            .run_turn(plain_turn())
            .await
            .expect("a turn");
        handle.join().expect("server thread");
        assert_eq!(turn.assistant_text, None);

        // A missing key is the provider's side too; a cut answer is not.
        assert!(AiSdkError::MissingKey {
            provider: AiProvider::OpenAi,
            reason: "SY.vault has no openai secret for this node".to_string(),
        }
        .is_provider_failure());
        assert!(!AiSdkError::Truncated {
            provider: AiProvider::OpenAi,
            limit: "max_output_tokens",
        }
        .is_provider_failure());
    }
}
