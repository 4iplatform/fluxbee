use std::collections::HashSet;
use std::time::Duration;

use fluxbee_ai_sdk::errors::AiSdkError;
use fluxbee_ai_sdk::{LlmClient, LlmRequest, ModelSettings, OpenAiResponsesClient};
use serde::Deserialize;
use serde_json::json;
use tokio::time;

use crate::{
    CognitionSemanticTaggerConfig, SemanticTaggerOutput, COGNITION_REASON_CANONICAL_SIGNALS,
};

#[derive(Debug, Clone)]
pub(super) struct SemanticTaggerAiInput<'a> {
    pub(super) api_key: &'a str,
    pub(super) text: &'a str,
    pub(super) src_ilk: Option<&'a str>,
    pub(super) dst_ilk: Option<&'a str>,
    pub(super) ich: Option<&'a str>,
    pub(super) config: &'a CognitionSemanticTaggerConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct RawSemanticTaggerResponse {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    reason_signals_canonical: Vec<String>,
    #[serde(default)]
    reason_signals_extra: Vec<String>,
}

pub(super) async fn run_semantic_tagger_ai(
    input: SemanticTaggerAiInput<'_>,
) -> Result<SemanticTaggerOutput, AiSdkError> {
    if !input.config.provider.trim().eq_ignore_ascii_case("openai") {
        return Err(AiSdkError::Protocol(format!(
            "unsupported semantic_tagger provider={}",
            input.config.provider
        )));
    }

    if input.text.trim().is_empty() {
        return Ok(SemanticTaggerOutput::default());
    }

    let request = LlmRequest {
        model: input.config.model.clone(),
        system: Some(build_system_prompt(input.config)),
        input: build_user_prompt(&input),
        input_parts: None,
        max_output_tokens: Some(256),
        model_settings: Some(ModelSettings {
            temperature: Some(0.1),
            top_p: None,
            max_output_tokens: Some(256),
        }),
    };

    let client = OpenAiResponsesClient::new(input.api_key);
    let response = time::timeout(
        Duration::from_millis(input.config.timeout_ms),
        LlmClient::generate(&client, request),
    )
    .await
    .map_err(|_| {
        AiSdkError::Timeout(format!(
            "semantic tagger timed out after {} ms",
            input.config.timeout_ms
        ))
    })??;

    parse_semantic_tagger_response(
        &response.content,
        input.config.max_tags,
        input.config.max_reason_signals,
    )
}

fn build_system_prompt(config: &CognitionSemanticTaggerConfig) -> String {
    format!(
        concat!(
            "You are AI.tagger inside SY.cognition.\n",
            "Return exactly one JSON object and nothing else.\n",
            "Schema:\n",
            "{{\"tags\":[string],\"reason_signals_canonical\":[string],\"reason_signals_extra\":[string]}}\n",
            "Rules:\n",
            "- tags: concise lowercase semantic labels, max {max_tags}.\n",
            "- reason_signals_canonical: only from this closed set: {canonical}.\n",
            "- reason_signals_canonical max {max_reason_signals}.\n",
            "- reason_signals_extra: concise lowercase narrative hints, max {max_reason_signals}.\n",
            "- Do not include markdown, comments, code fences, or extra keys.\n",
            "- Use only evidence present in the input.\n"
        ),
        max_tags = config.max_tags,
        max_reason_signals = config.max_reason_signals,
        canonical = serde_json::to_string(&COGNITION_REASON_CANONICAL_SIGNALS.to_vec())
            .unwrap_or_else(|_| "[]".to_string()),
    )
}

fn build_user_prompt(input: &SemanticTaggerAiInput<'_>) -> String {
    let body = json!({
        "text": input.text,
        "word_count": input.text.split_whitespace().count(),
        "max_tags": input.config.max_tags,
        "max_reason_signals": input.config.max_reason_signals,
        "canonical_signals": COGNITION_REASON_CANONICAL_SIGNALS,
        "identity_hints": {
            "src_ilk": input.src_ilk,
            "dst_ilk": input.dst_ilk,
            "ich": input.ich,
        }
    });
    serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string())
}

fn parse_semantic_tagger_response(
    raw: &str,
    max_tags: usize,
    max_reason_signals: usize,
) -> Result<SemanticTaggerOutput, AiSdkError> {
    let json_slice = extract_json_object(raw).ok_or_else(|| {
        AiSdkError::Protocol("semantic tagger response missing JSON object".into())
    })?;
    let parsed: RawSemanticTaggerResponse = serde_json::from_str(json_slice)?;
    Ok(SemanticTaggerOutput {
        tags: normalize_labels(parsed.tags, max_tags, None),
        reason_signals_canonical: normalize_labels(
            parsed.reason_signals_canonical,
            max_reason_signals,
            Some(&COGNITION_REASON_CANONICAL_SIGNALS),
        ),
        reason_signals_extra: normalize_labels(
            parsed.reason_signals_extra,
            max_reason_signals,
            None,
        ),
    })
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }

    let fenced = trimmed.strip_prefix("```")?;
    let fenced = fenced.strip_prefix("json").unwrap_or(fenced);
    let fenced = fenced.trim();
    let fenced = fenced.strip_suffix("```").unwrap_or(fenced).trim();
    if fenced.starts_with('{') && fenced.ends_with('}') {
        return Some(fenced);
    }

    None
}

fn normalize_labels(values: Vec<String>, limit: usize, allowed: Option<&[&str]>) -> Vec<String> {
    let allowed_set: Option<HashSet<&str>> =
        allowed.map(|items| items.iter().copied().collect::<HashSet<&str>>());
    let mut seen = HashSet::<String>::new();
    let mut out = Vec::new();

    for value in values {
        let normalized = value.trim().to_lowercase();
        if normalized.is_empty() {
            continue;
        }
        if let Some(allowed_set) = allowed_set.as_ref() {
            if !allowed_set.contains(normalized.as_str()) {
                continue;
            }
        }
        if seen.insert(normalized.clone()) {
            out.push(normalized);
        }
        if out.len() >= limit {
            break;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_json_and_filters_unknown_canonical_signals() {
        let raw = r#"{
            "tags": ["Billing", "Refund", "Billing"],
            "reason_signals_canonical": ["request", "invalid", "resolve"],
            "reason_signals_extra": ["Urgency", "frustration"]
        }"#;

        let parsed = parse_semantic_tagger_response(raw, 4, 4).expect("should parse");
        assert_eq!(parsed.tags, vec!["billing", "refund"]);
        assert_eq!(parsed.reason_signals_canonical, vec!["request", "resolve"]);
        assert_eq!(parsed.reason_signals_extra, vec!["urgency", "frustration"]);
    }

    #[test]
    fn parses_json_inside_code_fence() {
        let raw = "```json\n{\"tags\":[\"support\"],\"reason_signals_canonical\":[\"request\"],\"reason_signals_extra\":[]}\n```";
        let parsed = parse_semantic_tagger_response(raw, 4, 4).expect("should parse");
        assert_eq!(parsed.tags, vec!["support"]);
        assert_eq!(parsed.reason_signals_canonical, vec!["request"]);
    }
}
