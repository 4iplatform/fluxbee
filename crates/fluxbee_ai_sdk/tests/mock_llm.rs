use std::sync::Arc;

use fluxbee_ai_sdk::{Agent, LlmResponse, LlmStreamEvent, MockLlmClient, ModelSettings};

#[tokio::test]
async fn mock_llm_returns_deterministic_response_and_captures_request() {
    let mock = MockLlmClient::with_responses(vec![LlmResponse {
        content: "mock-answer".to_string(),
    }]);
    let settings = ModelSettings {
        temperature: Some(0.2),
        top_p: Some(1.0),
        max_output_tokens: Some(128),
    };
    let agent = Agent::with_model_settings(
        "test-agent",
        "gpt-4.1-mini",
        Some("instrucciones".to_string()),
        settings,
        Arc::new(mock.clone()),
    );

    let response = agent.run_text("hello").await.expect("mock llm response");
    assert_eq!(response.content, "mock-answer");

    let requests = mock.take_requests().await;
    assert_eq!(requests.len(), 1);
    let req = &requests[0];
    assert_eq!(req.model, "gpt-4.1-mini");
    assert_eq!(req.system.as_deref(), Some("instrucciones"));
    assert_eq!(req.input, "hello");
    assert_eq!(
        req.model_settings
            .as_ref()
            .and_then(|s| s.max_output_tokens),
        Some(128)
    );
}

#[tokio::test]
async fn mock_llm_errors_when_queue_is_empty() {
    let mock = MockLlmClient::default();
    let agent = Agent::new("test-agent", "gpt-4.1-mini", None, Arc::new(mock));

    let err = agent
        .run_text("hello")
        .await
        .expect_err("expected empty queue error");
    let err_text = err.to_string();
    assert!(err_text.contains("mock llm has no queued response"));
}

#[tokio::test]
async fn mock_llm_stream_returns_configured_events() {
    let mock = MockLlmClient::with_stream_responses(vec![vec![
        LlmStreamEvent::Delta("hello ".to_string()),
        LlmStreamEvent::Delta("world".to_string()),
        LlmStreamEvent::Completed,
    ]]);
    let agent = Agent::new("stream-agent", "gpt-4.1-mini", None, Arc::new(mock.clone()));

    let events = agent
        .run_text_stream("stream input")
        .await
        .expect("stream events");

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::Delta("hello ".to_string()),
            LlmStreamEvent::Delta("world".to_string()),
            LlmStreamEvent::Completed,
        ]
    );

    let requests = mock.take_requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].input, "stream input");
}

#[tokio::test]
async fn mock_llm_stream_falls_back_to_single_response_queue() {
    let mock = MockLlmClient::with_responses(vec![LlmResponse {
        content: "plain answer".to_string(),
    }]);
    let agent = Agent::new("stream-agent", "gpt-4.1-mini", None, Arc::new(mock));

    let events = agent
        .run_text_stream("hello")
        .await
        .expect("stream fallback events");

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::Delta("plain answer".to_string()),
            LlmStreamEvent::Completed,
        ]
    );
}
