use fluxbee_ai_sdk::{
    refresh_conversation_summary, ConversationSummary, ImmediateInteraction,
    ImmediateInteractionKind, ImmediateOperation, ImmediateRole, LlmResponse, MockLlmClient,
    ModelSettings, SummaryRefreshConfig, SummaryRefreshInput,
};

#[tokio::test]
async fn summary_refresh_helper_builds_prompt_and_parses_json() {
    let mock = MockLlmClient::with_responses(vec![LlmResponse {
        content: r#"{
            "goal":"Provision worker-220",
            "current_focus":"Check whether add_hive completed after timeout",
            "decisions":["Mutations require explicit CONFIRM"],
            "confirmed_facts":["worker-220 creation was requested"],
            "open_questions":["Whether the hive already exists"]
        }"#
        .to_string(),
    }]);

    let summary = refresh_conversation_summary(
        &mock,
        SummaryRefreshConfig {
            model: "gpt-4.1-mini".to_string(),
            system_prompt: Some("Node-specific context.".to_string()),
            model_settings: Some(ModelSettings {
                temperature: Some(0.1),
                top_p: Some(1.0),
                max_output_tokens: Some(256),
            }),
        },
        SummaryRefreshInput {
            thread_id: Some("thread:abc".to_string()),
            scope_id: Some("scope:ops".to_string()),
            existing_summary: Some(ConversationSummary {
                goal: Some("Old goal".to_string()),
                current_focus: None,
                decisions: vec![],
                confirmed_facts: vec![],
                open_questions: vec![],
            }),
            recent_interactions: vec![
                ImmediateInteraction {
                    role: ImmediateRole::User,
                    kind: ImmediateInteractionKind::Text,
                    content: "create hive worker-220".to_string(),
                },
                ImmediateInteraction {
                    role: ImmediateRole::System,
                    kind: ImmediateInteractionKind::ToolError,
                    content: "add_hive -> timeout_unknown".to_string(),
                },
            ],
            active_operations: vec![ImmediateOperation {
                operation_id: "op-1".to_string(),
                resource_scope: Some("hive:worker-220".to_string()),
                origin_thread_id: Some("thread:abc".to_string()),
                origin_session_id: Some("session-1".to_string()),
                scope_id: None,
                action: "add_hive".to_string(),
                target: Some("worker-220".to_string()),
                status: "timeout_unknown".to_string(),
                summary: "Inspect before retrying".to_string(),
                created_at_ms: Some(1),
                updated_at_ms: Some(2),
            }],
        },
    )
    .await
    .expect("summary refresh should succeed");

    assert_eq!(summary.goal.as_deref(), Some("Provision worker-220"));
    assert_eq!(
        summary.current_focus.as_deref(),
        Some("Check whether add_hive completed after timeout")
    );
    assert_eq!(summary.decisions.len(), 1);

    let requests = mock.take_requests().await;
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.model, "gpt-4.1-mini");
    assert!(request
        .system
        .as_deref()
        .unwrap_or_default()
        .contains("Return JSON only."));
    assert!(request.input.contains("thread_id: thread:abc"));
    assert!(request.input.contains("Recent interactions:"));
    assert!(request.input.contains("Active operations:"));
}

#[tokio::test]
async fn summary_refresh_helper_accepts_json_inside_code_fence() {
    let mock = MockLlmClient::with_responses(vec![LlmResponse {
        content: "```json\n{\"goal\":null,\"current_focus\":\"focus\",\"decisions\":[],\"confirmed_facts\":[],\"open_questions\":[]}\n```".to_string(),
    }]);

    let summary = refresh_conversation_summary(
        &mock,
        SummaryRefreshConfig {
            model: "gpt-4.1-mini".to_string(),
            system_prompt: None,
            model_settings: None,
        },
        SummaryRefreshInput::default(),
    )
    .await
    .expect("code fence json should parse");

    assert_eq!(summary.current_focus.as_deref(), Some("focus"));
}
