use std::sync::Arc;

use async_trait::async_trait;
use fluxbee_ai_sdk::{
    Agent, FunctionTool, FunctionToolDefinition, FunctionToolProvider, FunctionToolRegistry,
    MockLlmClient,
};
use serde_json::{json, Value};

struct EchoTool;

#[async_trait]
impl FunctionTool for EchoTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "echo_tool".to_string(),
            description: "Echo test tool".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        Ok(arguments)
    }
}

struct ProviderWithEchoTool;

impl FunctionToolProvider for ProviderWithEchoTool {
    fn register_tools(&self, registry: &mut FunctionToolRegistry) -> fluxbee_ai_sdk::Result<()> {
        registry.register(Arc::new(EchoTool))
    }
}

#[test]
fn agent_register_tool_exposes_definitions_and_rejects_duplicates() {
    let mut agent = Agent::new("agent", "gpt-4.1-mini", None, Arc::new(MockLlmClient::default()));
    agent
        .register_tool(Arc::new(EchoTool))
        .expect("first tool registration should succeed");

    let defs = agent.tool_definitions();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "echo_tool");

    let err = agent
        .register_tool(Arc::new(EchoTool))
        .expect_err("duplicate registration must fail");
    assert!(err.to_string().contains("duplicate tool"));
}

#[test]
fn agent_register_tools_from_provider() {
    let mut agent = Agent::new("agent", "gpt-4.1-mini", None, Arc::new(MockLlmClient::default()));
    let provider = ProviderWithEchoTool;

    agent
        .register_tools_from(&provider)
        .expect("provider registration should succeed");

    let defs = agent.tool_definitions();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "echo_tool");
}
