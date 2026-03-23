use std::sync::Arc;

use crate::errors::Result;
use crate::function_calling::{
    FunctionTool, FunctionToolDefinition, FunctionToolProvider, FunctionToolRegistry,
};
use crate::llm::{LlmClient, LlmRequest, LlmResponse, LlmStreamEvent, ModelSettings};

pub struct Agent {
    name: String,
    model: String,
    instructions: Option<String>,
    model_settings: ModelSettings,
    llm: Arc<dyn LlmClient>,
    tools: FunctionToolRegistry,
}

impl Agent {
    pub fn new(
        name: impl Into<String>,
        model: impl Into<String>,
        instructions: Option<String>,
        llm: Arc<dyn LlmClient>,
    ) -> Self {
        Self::with_model_settings(name, model, instructions, ModelSettings::default(), llm)
    }

    pub fn with_model_settings(
        name: impl Into<String>,
        model: impl Into<String>,
        instructions: Option<String>,
        model_settings: ModelSettings,
        llm: Arc<dyn LlmClient>,
    ) -> Self {
        Self {
            name: name.into(),
            model: model.into(),
            instructions,
            model_settings,
            llm,
            tools: FunctionToolRegistry::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn run_text(&self, input: impl Into<String>) -> Result<LlmResponse> {
        self.llm.generate(self.build_request(input.into())).await
    }

    pub async fn run_text_stream(&self, input: impl Into<String>) -> Result<Vec<LlmStreamEvent>> {
        self.llm
            .generate_stream(self.build_request(input.into()))
            .await
    }

    pub fn register_tool(&mut self, tool: Arc<dyn FunctionTool>) -> Result<()> {
        self.tools.register(tool)
    }

    pub fn register_tools_from(&mut self, provider: &dyn FunctionToolProvider) -> Result<()> {
        provider.register_tools(&mut self.tools)
    }

    pub fn tool_definitions(&self) -> Vec<FunctionToolDefinition> {
        self.tools.definitions()
    }

    pub fn tool_registry(&self) -> &FunctionToolRegistry {
        &self.tools
    }

    fn build_request(&self, input: String) -> LlmRequest {
        LlmRequest {
            model: self.model.clone(),
            system: self.instructions.clone(),
            input,
            max_output_tokens: None,
            model_settings: Some(self.model_settings.clone()),
        }
    }
}
