use std::time::Duration;

use fluxbee_ai_sdk::errors::AiSdkError;
use fluxbee_ai_sdk::{create_llm_client, AiAccount, LlmClient, LlmRequest, ModelSettings};
use serde::Deserialize;
use serde_json::json;
use tokio::time;

use crate::episodes::{EpisodeProposal, EPISODE_LENSES, EPISODE_STATES};
use crate::CognitionSemanticTaggerConfig;

/// The summaries are a few sentences of JSON, an episode's paragraph among them; the cap only bounds
/// a runaway answer. A model that reasons spends part of it before it answers (A-69).
const NARRATIVE_SUMMARIZER_MAX_OUTPUT_TOKENS: u32 = 1536;

#[derive(Debug, Clone)]
pub(super) struct NarrativeSummarizerAiInput<'a> {
    pub(super) api_key: &'a str,
    /// Who pays for the call: the communication's agent's tenant (docs/router-stream-v1.md §7.3).
    pub(super) account: &'a AiAccount,
    pub(super) config: &'a CognitionSemanticTaggerConfig,
    pub(super) thread_id: &'a str,
    /// Who spoke the turn (docs/cognition-hands-v1.md §10.3): `person Ana Pérez (ilk:…)`, or `the
    /// agent, answering person Ana Pérez (ilk:…)`. The memory names people by it.
    pub(super) speaker: Option<&'a str>,
    pub(super) context_label: &'a str,
    pub(super) context_tags: &'a [String],
    pub(super) reason_label: &'a str,
    pub(super) canonical_signals: &'a [String],
    pub(super) extra_signals: &'a [String],
    pub(super) previous_memory_summary: Option<&'a str>,
    /// The affects the turn's moment shows, each with its feeling's title; the narrator writes one
    /// text for that moment. Empty when it shows none.
    pub(super) feeling_affects: Vec<&'a str>,
    pub(super) feeling_titles: Vec<&'a str>,
    pub(super) previous_feeling_summary: Option<&'a str>,
    pub(super) previous_feeling_reason: Option<&'a str>,
    /// Whom the turn is to, as `speaker` names a party (docs/cognition-hands-v1.md §12.4); none
    /// when no one else is known yet.
    pub(super) counterpart: Option<&'a str>,
    /// The communication's two leading contexts: an episode rests on one of them.
    pub(super) leading_contexts: &'a [String],
    /// How many live episodes of each lens the turn's people already have: what is habitual for
    /// them makes no episode.
    pub(super) episode_habits: &'a [(&'static str, u32)],
}

#[derive(Debug, Clone, Deserialize)]
struct RawNarrativeSummaries {
    memory_summary: String,
    #[serde(default)]
    feeling_summary: Option<String>,
    #[serde(default)]
    feeling_reason: Option<String>,
    /// Read apart (`episode_of_answer`): an episode that does not read is dropped, the memory kept.
    #[serde(default)]
    episode: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub(super) struct NarrativeSummaries {
    pub(super) memory_summary: String,
    /// Present exactly when the input carried a feeling candidate: the parser rejects an
    /// answer that leaves it out, and drops one the input did not ask for.
    pub(super) feeling: Option<FeelingNarrative>,
    /// The episode the narrator proposes, if any; the gate decides (docs/cognition-hands-v1.md
    /// §12.4).
    pub(super) episode: Option<EpisodeProposal>,
}

#[derive(Debug, Clone)]
pub(super) struct FeelingNarrative {
    pub(super) summary: String,
    pub(super) reason: String,
}

pub(super) async fn run_narrative_summarizer_ai(
    input: NarrativeSummarizerAiInput<'_>,
) -> Result<NarrativeSummaries, AiSdkError> {
    let request = LlmRequest {
        model: input.config.model.clone(),
        system: Some(build_system_prompt(!input.feeling_affects.is_empty())),
        input: build_user_prompt(&input),
        input_parts: None,
        max_output_tokens: Some(NARRATIVE_SUMMARIZER_MAX_OUTPUT_TOKENS),
        // No temperature: the current models of both providers reject it (A-69).
        model_settings: Some(ModelSettings {
            temperature: None,
            top_p: None,
            max_output_tokens: Some(NARRATIVE_SUMMARIZER_MAX_OUTPUT_TOKENS),
        }),
        output_schema: None,
    };

    let client = create_llm_client(
        input.config.provider,
        input.api_key,
        None,
        input.account.clone(),
    );
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

    parse_narrative_summaries(&response.content, !input.feeling_affects.is_empty())
}

fn build_system_prompt(include_feeling: bool) -> String {
    let feeling_rules = if include_feeling {
        "- feeling_summary: 1-2 sentences describing the speaker's concrete moment of pressure or affect, naming the speaker; one text for all the affects the feeling_candidate lists.\n\
         - feeling_reason: one short sentence explaining why this feeling is being registered.\n"
            .to_string()
    } else {
        "- feeling_summary: null.\n- feeling_reason: null.\n".to_string()
    };
    format!(
        concat!(
            "You are the internal narrative summarizer for SY.cognition.\n",
            "Return exactly one JSON object and nothing else.\n",
            "Schema:\n",
            "{{\"memory_summary\":string,\"feeling_summary\":string|null,\"feeling_reason\":string|null,\"episode\":null|{{\"lens\":string,\"title\":string,\"summary\":string,\"state\":string,\"base_intensity\":number,\"novelty\":number,\"evidence_signals\":[string],\"evidence_strength\":number,\"evidence_contexts\":[string]}}}}\n",
            "Rules:\n",
            "- memory_summary: concise narrative synthesis of recurring thread pattern, 1-2 sentences.\n",
            "- speaker is who spoke this turn. Name people as the speaker names them; when the thread has several people, the memory_summary says who said or sought what.\n",
            "- in everything you write (memory_summary, feeling_summary, episode), the agent is only the AI agent: never call a person the agent, even one who answers for a company (billing, support), or whom the speaker calls an agent.\n",
            "- counterpart is whom this turn was written to: the agent, for a turn to it; the person it answered, for the agent's own answer; or, for a turn between people that the agent took no part in, the other person or the node they are reached through.\n",
            "- preserve continuity with previous summaries when provided.\n",
            "- use only the supplied evidence. Do not invent facts, names, or timeline details.\n",
            "- mention context and dominant reason naturally.\n",
            "- keep tone neutral, factual, and useful for future retrieval.\n",
            "{feeling_rules}",
            "- episode: null, or one social event between the people of this turn worth remembering, public like the news: what happened between speaker and counterpart, never what someone feels. Only for a break in the pattern: what is routine for these people is null (episode_habits counts the live episodes of each lens they already have). Conservative: when in doubt, null.\n",
            "  - lens: one of {lenses}: friction is conflict or hostility that can harm the bond; discomfort is tension, uncertainty, delay or silence; appreciation is recognition or genuine thanks; shame is being judged, exposed or rejected.\n",
            "  - title: a few words. summary: a paragraph of two to four sentences, impersonal: what happened, between whom (named as speaker and counterpart name them), and how it stood; no feelings, no private details. A turn between people is not the agent's, and its episode names only them.\n",
            "  - state: one of {states}. base_intensity: 0-10. novelty: -2 to 2, how far it breaks the pattern. evidence_signals: the signals it rests on. evidence_strength: 0-10. evidence_contexts: the labels among leading_contexts it rests on.\n",
            "- no markdown, no code fences, no extra keys.\n"
        ),
        feeling_rules = feeling_rules,
        lenses = EPISODE_LENSES.join(", "),
        states = EPISODE_STATES.join(", ")
    )
}

fn build_user_prompt(input: &NarrativeSummarizerAiInput<'_>) -> String {
    let body = json!({
        "thread_id": input.thread_id,
        "speaker": input.speaker,
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
            "feeling_summary": input.previous_feeling_summary,
            "feeling_reason": input.previous_feeling_reason,
        },
        "feeling_candidate": {
            "affects": input.feeling_affects,
            "titles": input.feeling_titles,
        },
        "counterpart": input.counterpart,
        "leading_contexts": input.leading_contexts,
        "episode_habits": input
            .episode_habits
            .iter()
            .map(|(lens, count)| ((*lens).to_string(), json!(count)))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    });
    serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string())
}

fn parse_narrative_summaries(
    raw: &str,
    expect_feeling: bool,
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

    let episode = episode_of_answer(parsed.episode);
    if !expect_feeling {
        return Ok(NarrativeSummaries {
            memory_summary,
            feeling: None,
            episode,
        });
    }
    let summary = normalize_sentence(parsed.feeling_summary.as_deref().unwrap_or_default());
    let reason = normalize_sentence(parsed.feeling_reason.as_deref().unwrap_or_default());
    if summary.is_empty() || reason.is_empty() {
        return Err(AiSdkError::Protocol(
            "narrative summarizer returned incomplete feeling narrative".into(),
        ));
    }

    Ok(NarrativeSummaries {
        memory_summary,
        feeling: Some(FeelingNarrative { summary, reason }),
        episode,
    })
}

/// The episode an answer proposes: none for `null`, and none, with a warning, for one that does not
/// read as an episode. The memory and the feeling stand either way.
fn episode_of_answer(value: Option<serde_json::Value>) -> Option<EpisodeProposal> {
    let value = value.filter(|value| !value.is_null())?;
    match serde_json::from_value::<EpisodeProposal>(value) {
        Ok(proposal) => Some(proposal),
        Err(err) => {
            tracing::warn!(error = %err, "sy.cognition narrator proposed an episode that does not read; it is dropped");
            None
        }
    }
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
        expect_feeling: bool,
        raw_output: String,
        memory_summary_contains: Vec<String>,
        feeling_summary_contains: Vec<String>,
        feeling_reason_contains: Vec<String>,
    }

    fn load_golden_corpus() -> GoldenCorpus {
        serde_json::from_str(include_str!("golden_corpus.json"))
            .expect("golden corpus should parse")
    }

    #[test]
    fn parses_memory_only_narrative() {
        let raw = r#"{"memory_summary":"Recurring billing friction with pressure for resolution.","feeling_summary":null,"feeling_reason":null}"#;
        let parsed = parse_narrative_summaries(raw, false).expect("parse");
        assert_eq!(
            parsed.memory_summary,
            "Recurring billing friction with pressure for resolution."
        );
        assert!(parsed.feeling.is_none());
    }

    #[test]
    fn rejects_missing_feeling_fields_when_feeling_expected() {
        let raw = r#"{"memory_summary":"Recurring billing friction.","feeling_summary":"Pressure escalated.","feeling_reason":null}"#;
        let err = parse_narrative_summaries(raw, true).expect_err("should fail");
        assert!(err.to_string().contains("incomplete feeling narrative"));
    }

    /// An episode the narrator proposes comes back for the gate (docs/cognition-hands-v1.md
    /// §12.4); `null` is none, and one that does not read is dropped with the memory kept.
    #[test]
    fn reads_the_episode_the_narrator_proposes() {
        let raw = r#"{"memory_summary":"Ana and Juan on the credit note.","feeling_summary":null,"feeling_reason":null,"episode":{"lens":"friction","title":"Argument over the credit note","summary":"Ana and Juan argued over the delay. It is pending.","state":"pending","base_intensity":8,"novelty":1,"evidence_signals":["escalation"],"evidence_strength":9,"evidence_contexts":["credit note"]}}"#;
        let episode = parse_narrative_summaries(raw, false)
            .expect("parse")
            .episode
            .expect("an episode");
        assert_eq!(episode.lens, "friction");
        assert_eq!(episode.state, "pending");
        assert_eq!(episode.base_intensity, 8.0);
        assert_eq!(episode.evidence_contexts, ["credit note"]);

        let none = r#"{"memory_summary":"Billing.","feeling_summary":null,"feeling_reason":null,"episode":null}"#;
        assert!(parse_narrative_summaries(none, false)
            .expect("parse")
            .episode
            .is_none());
        let unreadable = r#"{"memory_summary":"Billing.","feeling_summary":null,"feeling_reason":null,"episode":{"lens":["friction"]}}"#;
        let parsed = parse_narrative_summaries(unreadable, false).expect("the memory stands");
        assert_eq!(parsed.memory_summary, "Billing.");
        assert!(parsed.episode.is_none());
    }

    /// The narrator reads whom the turn is to, the leading contexts and the habits, and the
    /// episode's rules name every lens and state.
    #[test]
    fn the_prompt_carries_what_an_episode_needs() {
        let system = build_system_prompt(false);
        for word in EPISODE_LENSES.iter().chain(EPISODE_STATES.iter()) {
            assert!(system.contains(word), "{word}");
        }
        assert!(system.contains("never what someone feels"));
        assert!(system.contains("counterpart is whom this turn was written to"));
        assert!(system.contains("the person it answered, for the agent's own answer"));
        assert!(system.contains("in everything you write (memory_summary, feeling_summary, episode), the agent is only the AI agent"));
        assert!(system.contains("never call a person the agent"));
        let account = AiAccount::charge(
            fluxbee_ai_sdk::UsageRecorder::disabled(),
            "SY.cognition@worker1",
            None,
            fluxbee_ai_sdk::TenantSource::Agent,
        );
        let config = crate::CognitionSemanticTaggerConfig::default();
        let leading = vec!["credit note".to_string(), "billing".to_string()];
        let habits = [
            ("friction", 2u32),
            ("discomfort", 0),
            ("appreciation", 0),
            ("shame", 0),
        ];
        let prompt = build_user_prompt(&NarrativeSummarizerAiInput {
            api_key: "",
            account: &account,
            config: &config,
            thread_id: "thread:1",
            speaker: Some("person Ana (ilk:ana)"),
            context_label: "credit note",
            context_tags: &[],
            reason_label: "complaint",
            canonical_signals: &[],
            extra_signals: &[],
            previous_memory_summary: None,
            feeling_affects: Vec::new(),
            feeling_titles: Vec::new(),
            previous_feeling_summary: None,
            previous_feeling_reason: None,
            counterpart: Some("person Juan (ilk:juan)"),
            leading_contexts: &leading,
            episode_habits: &habits,
        });
        let body: serde_json::Value = serde_json::from_str(&prompt).expect("json");
        assert_eq!(body["counterpart"], "person Juan (ilk:juan)");
        assert_eq!(body["leading_contexts"][0], "credit note");
        assert_eq!(body["episode_habits"]["friction"], 2);
    }

    #[test]
    fn golden_narrative_corpus_matches_minimum_expected_content() {
        let corpus = load_golden_corpus();
        for case in corpus.narrative_cases {
            let parsed =
                parse_narrative_summaries(&case.raw_output, case.expect_feeling).expect("parse");
            for expected in case.memory_summary_contains {
                assert!(
                    parsed.memory_summary.contains(&expected),
                    "narrative case {} missing memory fragment {}",
                    case.name,
                    expected
                );
            }
            for expected in case.feeling_summary_contains {
                assert!(
                    parsed
                        .feeling
                        .as_ref()
                        .is_some_and(|feeling| feeling.summary.contains(&expected)),
                    "narrative case {} missing feeling summary fragment {}",
                    case.name,
                    expected
                );
            }
            for expected in case.feeling_reason_contains {
                assert!(
                    parsed
                        .feeling
                        .as_ref()
                        .is_some_and(|feeling| feeling.reason.contains(&expected)),
                    "narrative case {} missing feeling reason fragment {}",
                    case.name,
                    expected
                );
            }
        }
    }
}
