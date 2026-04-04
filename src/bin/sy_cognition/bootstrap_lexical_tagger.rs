use std::collections::HashSet;

use crate::{COGNITION_MAX_REASON_SIGNALS, COGNITION_MAX_TAGS, COGNITION_REASON_CANONICAL_SIGNALS};

// Transitional bootstrap tagger kept isolated until the AI-backed semantic
// pipeline replaces it. This is not the target long-term architecture.
#[derive(Debug, Clone, Default)]
pub(super) struct DeterministicTaggerOutput {
    pub(super) tags: Vec<String>,
    pub(super) reason_signals_canonical: Vec<String>,
    pub(super) reason_signals_extra: Vec<String>,
}

pub(super) fn run_bootstrap_lexical_tagger(text: &str) -> DeterministicTaggerOutput {
    let normalized = normalize_text(text);
    if normalized.is_empty() {
        return DeterministicTaggerOutput::default();
    }
    let tokens = tokenize(&normalized);
    let mut tags = Vec::new();
    let mut canonical = Vec::new();
    let mut extra = Vec::new();

    add_tag_if_matches(
        &mut tags,
        &tokens,
        &[
            "billing", "refund", "invoice", "payment", "charge", "charged",
        ],
        "billing",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &["account", "login", "password", "access", "user", "profile"],
        "account",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &[
            "support", "issue", "problem", "broken", "error", "bug", "failed", "failing",
        ],
        "support",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &["order", "shipping", "delivery", "package", "shipment"],
        "orders",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &["identity", "tenant", "ilk", "channel", "provision"],
        "identity",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &[
            "policy",
            "permission",
            "role",
            "access",
            "authorize",
            "approval",
        ],
        "policy",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &["meeting", "schedule", "calendar", "call", "appointment"],
        "scheduling",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &[
            "deploy",
            "release",
            "runtime",
            "node",
            "orchestrator",
            "service",
        ],
        "operations",
    );

    if matches_any_phrase(
        &normalized,
        &[
            "please",
            "can you",
            "could you",
            "need you",
            "i need",
            "i want",
        ],
    ) || tokens
        .iter()
        .any(|token| matches!(token.as_str(), "please" | "need" | "want"))
    {
        canonical.push("request".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "fix",
            "solve",
            "resolve",
            "refund",
            "help me",
            "not working",
            "working again",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "fix" | "solve" | "resolve" | "refund" | "help"
        )
    }) {
        canonical.push("resolve".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "why",
            "wrong",
            "broken",
            "still broken",
            "doesnt work",
            "unacceptable",
            "complaint",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "wrong" | "broken" | "complaint" | "bad" | "still"
        )
    }) {
        canonical.push("challenge".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "status",
            "update",
            "explain",
            "information",
            "details",
            "clarify",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "status" | "update" | "explain" | "details" | "clarify" | "info"
        )
    }) {
        canonical.push("inform".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "confirm",
            "verify",
            "is it correct",
            "is that correct",
            "okay?",
        ],
    ) || tokens
        .iter()
        .any(|token| matches!(token.as_str(), "confirm" | "verify" | "correct" | "ok"))
    {
        canonical.push("confirm".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &["security", "fraud", "risk", "protect", "block", "suspend"],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "security" | "fraud" | "risk" | "protect" | "block" | "suspend"
        )
    }) {
        canonical.push("protect".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "hello",
            "thanks",
            "thank you",
            "appreciate",
            "glad",
            "connect",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "hello" | "thanks" | "thank" | "appreciate" | "glad" | "connect"
        )
    }) {
        canonical.push("connect".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "cancel",
            "stop",
            "nevermind",
            "never mind",
            "close this",
            "no longer",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "cancel" | "stop" | "nevermind" | "close" | "abandon"
        )
    }) {
        canonical.push("abandon".to_string());
    }

    if matches_any_phrase(&normalized, &["urgent", "asap", "immediately", "right now"]) {
        extra.push("urgency".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "frustrated",
            "angry",
            "broken",
            "still broken",
            "again",
            "unacceptable",
        ],
    ) {
        extra.push("frustration".to_string());
    }
    if matches_any_phrase(&normalized, &["thanks", "thank you", "appreciate"]) {
        extra.push("gratitude".to_string());
    }
    if matches_any_phrase(&normalized, &["why", "how", "confused", "dont understand"]) {
        extra.push("confusion".to_string());
    }
    if matches_any_phrase(&normalized, &["lawyer", "legal", "complaint", "escalate"]) {
        extra.push("escalation".to_string());
    }

    tags = dedup_limit(tags, COGNITION_MAX_TAGS);
    canonical = dedup_limit(
        canonical
            .into_iter()
            .filter(|value| {
                COGNITION_REASON_CANONICAL_SIGNALS
                    .iter()
                    .any(|allowed| allowed == value)
            })
            .collect(),
        COGNITION_MAX_REASON_SIGNALS,
    );
    extra = dedup_limit(extra, COGNITION_MAX_REASON_SIGNALS);

    if tags.is_empty() {
        tags = fallback_tags(&tokens, COGNITION_MAX_TAGS);
    }

    DeterministicTaggerOutput {
        tags,
        reason_signals_canonical: canonical,
        reason_signals_extra: extra,
    }
}

fn normalize_text(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
}

fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn matches_any_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| text.contains(phrase))
}

fn add_tag_if_matches(tags: &mut Vec<String>, tokens: &[String], lexicon: &[&str], label: &str) {
    if tokens
        .iter()
        .any(|token| lexicon.iter().any(|candidate| candidate == token))
    {
        tags.push(label.to_string());
    }
}

fn dedup_limit(values: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        if value.trim().is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        out.push(value);
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn fallback_tags(tokens: &[String], limit: usize) -> Vec<String> {
    let stopwords: HashSet<&str> = [
        "a", "an", "and", "are", "as", "at", "be", "but", "by", "do", "for", "from", "how", "i",
        "if", "in", "is", "it", "me", "my", "of", "on", "or", "our", "please", "so", "that", "the",
        "this", "to", "we", "what", "why", "you", "your",
    ]
    .into_iter()
    .collect();
    dedup_limit(
        tokens
            .iter()
            .filter(|token| token.len() >= 4 && !stopwords.contains(token.as_str()))
            .cloned()
            .collect(),
        limit,
    )
}
