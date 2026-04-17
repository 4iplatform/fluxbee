use serde::{Deserialize, Serialize};

use crate::errors::Result;
use crate::immediate_memory::{
    ConversationSummary, ImmediateInteraction, ImmediateInteractionKind, ImmediateOperation,
    ImmediateRole,
};
use crate::llm::{LlmClient, LlmRequest, ModelSettings};

const SUMMARY_REFRESH_SYSTEM_PROMPT: &str = r#"You refresh immediate conversation summaries for operational AI nodes.

Return JSON only.
Do not include markdown fences.
Do not invent facts.
Keep the summary short and operational.

Return an object with exactly these fields:
- goal: string or null
- current_focus: string or null
- decisions: array of strings
- confirmed_facts: array of strings
- open_questions: array of strings"#;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SummaryRefreshInput {
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub existing_summary: Option<ConversationSummary>,
    #[serde(default)]
    pub recent_interactions: Vec<ImmediateInteraction>,
    #[serde(default)]
    pub active_operations: Vec<ImmediateOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryRefreshConfig {
    pub model: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub model_settings: Option<ModelSettings>,
}

pub async fn refresh_conversation_summary(
    llm: &dyn LlmClient,
    config: SummaryRefreshConfig,
    input: SummaryRefreshInput,
) -> Result<ConversationSummary> {
    let request = LlmRequest {
        model: config.model,
        system: Some(compose_system_prompt(config.system_prompt.as_deref())),
        input: build_summary_refresh_input(input),
        input_parts: None,
        output_schema: None,
        max_output_tokens: None,
        model_settings: config.model_settings,
    };
    let response = llm.generate(request).await?;
    parse_summary_response(&response.content)
}

fn compose_system_prompt(extra_prompt: Option<&str>) -> String {
    match extra_prompt.filter(|value| !value.trim().is_empty()) {
        Some(extra_prompt) => {
            format!("{extra_prompt}\n\n{SUMMARY_REFRESH_SYSTEM_PROMPT}")
        }
        None => SUMMARY_REFRESH_SYSTEM_PROMPT.to_string(),
    }
}

fn build_summary_refresh_input(input: SummaryRefreshInput) -> String {
    let mut lines = vec![
        "Refresh the immediate conversation summary using the latest short-horizon context."
            .to_string(),
    ];

    if let Some(thread_id) = input.thread_id.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("thread_id: {thread_id}"));
    }
    if let Some(scope_id) = input.scope_id.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("scope_id: {scope_id}"));
    }
    if let Some(existing) = input.existing_summary {
        lines.push("Existing summary:".to_string());
        append_summary_lines(&mut lines, &existing);
    }

    lines.push("Recent interactions:".to_string());
    if input.recent_interactions.is_empty() {
        lines.push("- none".to_string());
    } else {
        for interaction in input.recent_interactions.into_iter().take(12) {
            lines.push(format!(
                "- role={} kind={} content={}",
                render_role(&interaction.role),
                render_interaction_kind(&interaction.kind),
                sanitize_line(&interaction.content, 800)
            ));
        }
    }

    lines.push("Active operations:".to_string());
    if input.active_operations.is_empty() {
        lines.push("- none".to_string());
    } else {
        for operation in input.active_operations.into_iter().take(8) {
            let mut parts = vec![
                format!("operation_id={}", operation.operation_id),
                format!("action={}", sanitize_line(&operation.action, 120)),
                format!("status={}", sanitize_line(&operation.status, 80)),
                format!("summary={}", sanitize_line(&operation.summary, 220)),
            ];
            if let Some(target) = operation.target.filter(|value| !value.trim().is_empty()) {
                parts.push(format!("target={}", sanitize_line(&target, 120)));
            }
            if let Some(resource_scope) = operation
                .resource_scope
                .filter(|value| !value.trim().is_empty())
            {
                parts.push(format!(
                    "resource_scope={}",
                    sanitize_line(&resource_scope, 120)
                ));
            }
            lines.push(format!("- {}", parts.join(" | ")));
        }
    }

    lines.push(
        "Return only the JSON object for the refreshed summary. Keep arrays short and factual."
            .to_string(),
    );
    lines.join("\n")
}

fn append_summary_lines(lines: &mut Vec<String>, summary: &ConversationSummary) {
    if let Some(goal) = summary
        .goal
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("goal: {}", sanitize_line(goal, 220)));
    }
    if let Some(focus) = summary
        .current_focus
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("current_focus: {}", sanitize_line(focus, 220)));
    }
    append_string_list(lines, "decisions", &summary.decisions);
    append_string_list(lines, "confirmed_facts", &summary.confirmed_facts);
    append_string_list(lines, "open_questions", &summary.open_questions);
}

fn append_string_list(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    lines.push(format!("{label}:"));
    for value in values.iter().take(6) {
        if !value.trim().is_empty() {
            lines.push(format!("- {}", sanitize_line(value, 220)));
        }
    }
}

fn render_role(role: &ImmediateRole) -> &'static str {
    match role {
        ImmediateRole::User => "user",
        ImmediateRole::Assistant => "assistant",
        ImmediateRole::System => "system",
    }
}

fn render_interaction_kind(kind: &ImmediateInteractionKind) -> &'static str {
    match kind {
        ImmediateInteractionKind::Text => "text",
        ImmediateInteractionKind::ToolResult => "tool_result",
        ImmediateInteractionKind::ToolError => "tool_error",
        ImmediateInteractionKind::OperationSummary => "operation_summary",
        ImmediateInteractionKind::SystemNote => "system_note",
    }
}

fn parse_summary_response(raw: &str) -> Result<ConversationSummary> {
    let candidate = extract_json_candidate(raw);
    let parsed: ConversationSummary = serde_json::from_str(&candidate)?;
    Ok(normalize_summary(parsed))
}

fn extract_json_candidate(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(stripped) = trimmed.strip_prefix("```") {
        let stripped = stripped.trim_start_matches(|c| c != '\n');
        let stripped = stripped.strip_prefix('\n').unwrap_or(stripped);
        let stripped = stripped.strip_suffix("```").unwrap_or(stripped);
        return stripped.trim().to_string();
    }

    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return trimmed.to_string();
    }

    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        return trimmed[start..=end].to_string();
    }

    trimmed.to_string()
}

fn normalize_summary(summary: ConversationSummary) -> ConversationSummary {
    ConversationSummary {
        goal: summary.goal.map(|value| sanitize_line(&value, 240)),
        current_focus: summary
            .current_focus
            .map(|value| sanitize_line(&value, 240)),
        decisions: summary
            .decisions
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .map(|value| sanitize_line(&value, 200))
            .take(6)
            .collect(),
        confirmed_facts: summary
            .confirmed_facts
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .map(|value| sanitize_line(&value, 200))
            .take(6)
            .collect(),
        open_questions: summary
            .open_questions
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .map(|value| sanitize_line(&value, 200))
            .take(6)
            .collect(),
    }
}

fn sanitize_line(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut out = compact
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}
