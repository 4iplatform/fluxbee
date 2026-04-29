use std::sync::Arc;

use async_trait::async_trait;
use fluxbee_ai_sdk::{
    ConversationSummary, FunctionCallingConfig, FunctionCallingModel, FunctionCallingRunner,
    FunctionLoopItem, FunctionModelTurnRequest, FunctionModelTurnResponse, FunctionRunInput,
    FunctionToolRegistry, ImmediateConversationMemory, ImmediateInteraction,
    ImmediateInteractionKind, ImmediateOperation, ImmediateRole,
};
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct CaptureModel {
    requests: Arc<Mutex<Vec<FunctionModelTurnRequest>>>,
}

#[async_trait]
impl FunctionCallingModel for CaptureModel {
    async fn run_turn(
        &self,
        request: FunctionModelTurnRequest,
    ) -> fluxbee_ai_sdk::Result<FunctionModelTurnResponse> {
        self.requests.lock().await.push(request);
        Ok(FunctionModelTurnResponse {
            assistant_text: Some("ok".to_string()),
            tool_calls: Vec::new(),
            tokens_used: 0,
        })
    }
}

#[tokio::test]
async fn runner_serializes_immediate_memory_before_current_turn() {
    let model = CaptureModel::default();
    let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
    let tools = FunctionToolRegistry::new();
    let input = FunctionRunInput {
        current_user_message: "current user message".to_string(),
        current_user_parts: None,
        immediate_memory: Some(ImmediateConversationMemory {
            thread_id: Some("thread:abc".to_string()),
            scope_id: Some("scope:demo".to_string()),
            summary: Some(ConversationSummary {
                goal: Some("test immediate memory".to_string()),
                current_focus: Some("sdk serialization".to_string()),
                decisions: vec!["keep context".to_string()],
                confirmed_facts: vec!["operator asked before".to_string()],
                open_questions: vec!["whether active ops stay hot".to_string()],
            }),
            recent_interactions: vec![
                ImmediateInteraction {
                    role: ImmediateRole::User,
                    kind: ImmediateInteractionKind::Text,
                    content: "previous user".to_string(),
                },
                ImmediateInteraction {
                    role: ImmediateRole::Assistant,
                    kind: ImmediateInteractionKind::Text,
                    content: "previous assistant".to_string(),
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
                summary: "inspect before retrying".to_string(),
                created_at_ms: Some(1),
                updated_at_ms: Some(2),
            }],
        }),
    };

    let result = runner
        .run_with_input(&model, &tools, input)
        .await
        .expect("runner should succeed");
    assert_eq!(result.final_assistant_text.as_deref(), Some("ok"));

    let requests = model.requests.lock().await;
    let request = requests.first().expect("captured request");
    assert_eq!(request.items.len(), 5);
    match &request.items[0] {
        FunctionLoopItem::SystemText { content } => {
            assert!(content.contains("Immediate conversation summary:"));
            assert!(content.contains("thread_id: thread:abc"));
        }
        other => panic!("unexpected first item: {other:?}"),
    }
    match &request.items[1] {
        FunctionLoopItem::UserText { content } => assert_eq!(content, "previous user"),
        other => panic!("unexpected second item: {other:?}"),
    }
    match &request.items[2] {
        FunctionLoopItem::AssistantText { content } => assert_eq!(content, "previous assistant"),
        other => panic!("unexpected third item: {other:?}"),
    }
    match &request.items[3] {
        FunctionLoopItem::SystemText { content } => {
            assert!(content.contains("Active operations:"));
            assert!(content.contains("action=add_hive"));
            assert!(content.contains("status=timeout_unknown"));
        }
        other => panic!("unexpected fourth item: {other:?}"),
    }
    match &request.items[4] {
        FunctionLoopItem::UserText { content } => assert_eq!(content, "current user message"),
        other => panic!("unexpected fifth item: {other:?}"),
    }
}
