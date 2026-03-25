use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImmediateRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImmediateInteractionKind {
    Text,
    ToolResult,
    ToolError,
    OperationSummary,
    SystemNote,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImmediateConversationMemory {
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub summary: Option<ConversationSummary>,
    #[serde(default)]
    pub recent_interactions: Vec<ImmediateInteraction>,
    #[serde(default)]
    pub active_operations: Vec<ImmediateOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConversationSummary {
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub current_focus: Option<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub confirmed_facts: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImmediateInteraction {
    pub role: ImmediateRole,
    pub kind: ImmediateInteractionKind,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImmediateOperation {
    pub operation_id: String,
    #[serde(default)]
    pub resource_scope: Option<String>,
    #[serde(default)]
    pub origin_thread_id: Option<String>,
    #[serde(default)]
    pub origin_session_id: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
    pub action: String,
    #[serde(default)]
    pub target: Option<String>,
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub created_at_ms: Option<u64>,
    #[serde(default)]
    pub updated_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FunctionRunInput {
    pub current_user_message: String,
    #[serde(default)]
    pub immediate_memory: Option<ImmediateConversationMemory>,
}

impl FunctionRunInput {
    pub fn new(current_user_message: impl Into<String>) -> Self {
        Self {
            current_user_message: current_user_message.into(),
            immediate_memory: None,
        }
    }
}
