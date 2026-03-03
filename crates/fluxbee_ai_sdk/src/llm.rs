use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::errors::{AiSdkError, Result};

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
        let temperature = request
            .model_settings
            .as_ref()
            .and_then(|v| v.temperature);
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
