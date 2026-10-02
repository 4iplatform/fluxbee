use std::time::Duration;

use fluxbee_ai_sdk::errors::AiSdkError;
use fluxbee_ai_sdk::{create_llm_client, LlmClient, LlmRequest, ModelSettings};
use serde::Deserialize;
use serde_json::json;
use tokio::time;

use crate::CognitionSemanticTaggerConfig;

#[derive(Debug, Clone)]
pub(super) struct NarrativeSummarizerAiInput<'a> {
    pub(super) api_key: &'a str,
    pub(super) config: &'a CognitionSemanticTaggerConfig,
    pub(super) thread_id: &'a str,
    pub(super) context_label: &'a str,
    pub(super) context_tags: &'a [String],
    pub(super) reason_label: &'a str,
    pub(super) canonical_signals: &'a [String],
    pub(super) extra_signals: &'a [String],
    pub(super) previous_memory_summary: Option<&'a str>,
    pub(super) episode_affect_id: Option<&'a str>,
    pub(super) episode_title: Option<&'a str>,
    pub(super) previous_episode_summary: Option<&'a str>,
    pub(super) previous_episode_reason: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawNarrativeSummaries {
    memory_summary: String,
    #[serde(default)]
    episode_summary: Option<String>,
    #[serde(default)]
    episode_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct NarrativeSummaries {
    pub(super) memory_summary: String,
    /// Present exactly when the input carried an episode candidate: the parser rejects an
    /// answer that leaves it out, and drops one the input did not ask for.
    pub(super) episode: Option<EpisodeNarrative>,
}

#[derive(Debug, Clone)]
pub(super) struct EpisodeNarrative {
    pub(super) summary: String,
    pub(super) reason: String,
}

pub(super) async fn run_narrative_summarizer_ai(
    input: NarrativeSummarizerAiInput<'_>,
) -> Result<NarrativeSummaries, AiSdkError> {
    let request = LlmRequest {
        model: input.config.model.clone(),
        system: Some(build_system_prompt(input.episode_affect_id.is_some())),
        input: build_user_prompt(&input),
        input_parts: None,
        max_output_tokens: Some(220),
        model_settings: Some(ModelSettings {
            temperature: Some(0.2),
            top_p: None,
            max_output_tokens: Some(220),
        }),
        output_schema: None,
    };

    let client = create_llm_client(input.config.provider, input.api_key, None);
    let response = time::timeout(
        Duration::from_millis(input.config.timeout_ms),
        LlmClient::generate(client.as_ref(), request),
    )
    .await
    .map_err(|_| {
        AiSdkError::Timeout(format!(
            "narrative summarizer timed out after {} ms",
            input.config.timeout_ms
        ))
    })??;

    parse_narrative_summaries(&response.content, input.episode_affect_id.is_some())
}

fn build_system_prompt(include_episode: bool) -> String {
    let episode_rules = if include_episode {
        "- episode_summary: 1-2 sentences describing the concrete moment of pressure or affect.\n\
         - episode_reason: one short sentence explaining why this episode is being registered.\n"
            .to_string()
    } else {
        "- episode_summary: null.\n- episode_reason: null.\n".to_string()
    };
    format!(
        concat!(
            "You are the internal narrative summarizer for SY.cognition.\n",
            "Return exactly one JSON object and nothing else.\n",
            "Schema:\n",
            "{{\"memory_summary\":string,\"episode_summary\":string|null,\"episode_reason\":string|null}}\n",
            "Rules:\n",
            "- memory_summary: concise narrative synthesis of recurring thread pattern, 1-2 sentences.\n",
            "- preserve continuity with previous summaries when provided.\n",
            "- use only the supplied evidence. Do not invent facts, names, or timeline details.\n",
            "- mention context and dominant reason naturally.\n",
            "- keep tone neutral, factual, and useful for future retrieval.\n",
            "{episode_rules}",
            "- no markdown, no code fences, no extra keys.\n"
        ),
        episode_rules = episode_rules
    )
}

fn build_user_prompt(input: &NarrativeSummarizerAiInput<'_>) -> String {
    let body = json!({
        "thread_id": input.thread_id,
        "context": {
            "label": input.context_label,
            "tags": input.context_tags,
        },
        "reason": {
            "label": input.reason_label,
            "canonical_signals": input.canonical_signals,
            "extra_signals": input.extra_signals,
        },
        "previous": {
            "memory_summary": input.previous_memory_summary,
            "episode_summary": input.previous_episode_summary,
            "episode_reason": input.previous_episode_reason,
        },
        "episode_candidate": {
            "affect_id": input.episode_affect_id,
            "title": input.episode_title,
        }
    });
    serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string())
}

fn parse_narrative_summaries(
    raw: &str,
    expect_episode: bool,
) -> Result<NarrativeSummaries, AiSdkError> {
    let json_slice = extract_json_object(raw).ok_or_else(|| {
        AiSdkError::Protocol("narrative summarizer response missing JSON object".into())
    })?;
    let parsed: RawNarrativeSummaries = serde_json::from_str(json_slice)?;
    let memory_summary = normalize_sentence(&parsed.memory_summary);
    if memory_summary.is_empty() {
        return Err(AiSdkError::Protocol(
            "narrative summarizer returned empty memory_summary".into(),
        ));
    }

    if !expect_episode {
        return Ok(NarrativeSummaries {
            memory_summary,
            episode: None,
        });
    }
    let summary = normalize_sentence(parsed.episode_summary.as_deref().unwrap_or_default());
    let reason = normalize_sentence(parsed.episode_reason.as_deref().unwrap_or_default());
    if summary.is_empty() || reason.is_empty() {
        return Err(AiSdkError::Protocol(
            "narrative summarizer returned incomplete episode narrative".into(),
        ));
    }

    Ok(NarrativeSummaries {
        memory_summary,
        episode: Some(EpisodeNarrative { summary, reason }),
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

fn normalize_sentence(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct GoldenCorpus {
        narrative_cases: Vec<NarrativeCase>,
    }

    #[derive(Debug, Deserialize)]
    struct NarrativeCase {
        name: String,
        expect_episode: bool,
        raw_output: String,
        memory_summary_contains: Vec<String>,
        episode_summary_contains: Vec<String>,
        episode_reason_contains: Vec<String>,
    }

    fn load_golden_corpus() -> GoldenCorpus {
        serde_json::from_str(include_str!("golden_corpus.json"))
            .expect("golden corpus should parse")
    }

    #[test]
    fn parses_memory_only_narrative() {
        let raw = r#"{"memory_summary":"Recurring billing friction with pressure for resolution.","episode_summary":null,"episode_reason":null}"#;
        let parsed = parse_narrative_summaries(raw, false).expect("parse");
        assert_eq!(
            parsed.memory_summary,
            "Recurring billing friction with pressure for resolution."
        );
        assert!(parsed.episode.is_none());
    }

    #[test]
    fn rejects_missing_episode_fields_when_episode_expected() {
        let raw = r#"{"memory_summary":"Recurring billing friction.","episode_summary":"Pressure escalated.","episode_reason":null}"#;
        let err = parse_narrative_summaries(raw, true).expect_err("should fail");
        assert!(err.to_string().contains("incomplete episode narrative"));
    }

    #[test]
    fn golden_narrative_corpus_matches_minimum_expected_content() {
        let corpus = load_golden_corpus();
        for case in corpus.narrative_cases {
            let parsed =
                parse_narrative_summaries(&case.raw_output, case.expect_episode).expect("parse");
            for expected in case.memory_summary_contains {
                assert!(
                    parsed.memory_summary.contains(&expected),
                    "narrative case {} missing memory fragment {}",
                    case.name,
                    expected
                );
            }
            for expected in case.episode_summary_contains {
                assert!(
                    parsed
                        .episode
                        .as_ref()
                        .is_some_and(|episode| episode.summary.contains(&expected)),
                    "narrative case {} missing episode summary fragment {}",
                    case.name,
                    expected
                );
            }
            for expected in case.episode_reason_contains {
                assert!(
                    parsed
                        .episode
                        .as_ref()
                        .is_some_and(|episode| episode.reason.contains(&expected)),
                    "narrative case {} missing episode reason fragment {}",
                    case.name,
                    expected
                );
            }
        }
    }
}
