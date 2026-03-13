use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::function_calling::{FunctionTool, FunctionToolDefinition, FunctionToolProvider, FunctionToolRegistry};
use crate::thread_state::ThreadStateStore;
use crate::{AiSdkError, Result};

#[derive(Clone)]
pub struct ThreadStateGetTool {
    store: Arc<dyn ThreadStateStore>,
}

impl ThreadStateGetTool {
    pub fn new(store: Arc<dyn ThreadStateStore>) -> Self {
        Self { store }
    }
}

#[derive(Clone)]
pub struct ThreadStatePutTool {
    store: Arc<dyn ThreadStateStore>,
}

impl ThreadStatePutTool {
    pub fn new(store: Arc<dyn ThreadStateStore>) -> Self {
        Self { store }
    }
}

#[derive(Clone)]
pub struct ThreadStateDeleteTool {
    store: Arc<dyn ThreadStateStore>,
}

impl ThreadStateDeleteTool {
    pub fn new(store: Arc<dyn ThreadStateStore>) -> Self {
        Self { store }
    }
}

#[derive(Default, Clone)]
pub struct ThreadStateToolsProvider {
    get_tool: Option<ThreadStateGetTool>,
    put_tool: Option<ThreadStatePutTool>,
    delete_tool: Option<ThreadStateDeleteTool>,
}

impl ThreadStateToolsProvider {
    pub fn with_get(store: Arc<dyn ThreadStateStore>) -> Self {
        Self {
            get_tool: Some(ThreadStateGetTool::new(store)),
            put_tool: None,
            delete_tool: None,
        }
    }

    pub fn with_get_put(store: Arc<dyn ThreadStateStore>) -> Self {
        Self {
            get_tool: Some(ThreadStateGetTool::new(store.clone())),
            put_tool: Some(ThreadStatePutTool::new(store)),
            delete_tool: None,
        }
    }

    pub fn with_get_put_delete(store: Arc<dyn ThreadStateStore>) -> Self {
        Self {
            get_tool: Some(ThreadStateGetTool::new(store.clone())),
            put_tool: Some(ThreadStatePutTool::new(store.clone())),
            delete_tool: Some(ThreadStateDeleteTool::new(store)),
        }
    }
}

impl FunctionToolProvider for ThreadStateToolsProvider {
    fn register_tools(&self, registry: &mut FunctionToolRegistry) -> Result<()> {
        if let Some(tool) = &self.get_tool {
            registry.register(Arc::new(tool.clone()))?;
        }
        if let Some(tool) = &self.put_tool {
            registry.register(Arc::new(tool.clone()))?;
        }
        if let Some(tool) = &self.delete_tool {
            registry.register(Arc::new(tool.clone()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ThreadStateGetArgs {
    thread_id: String,
}

#[derive(Debug, Deserialize)]
struct ThreadStatePutArgs {
    thread_id: String,
    data: Value,
    #[serde(default)]
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ThreadStateDeleteArgs {
    thread_id: String,
}

#[async_trait]
impl FunctionTool for ThreadStateGetTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "thread_state_get".to_string(),
            description: "Get thread-scoped state by thread_id".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string", "minLength": 1 }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> Result<Value> {
        let started = Instant::now();
        let args: ThreadStateGetArgs = serde_json::from_value(arguments).map_err(|err| {
            AiSdkError::Protocol(format!(
                "thread_state_get: invalid arguments (expected {{thread_id:string}}): {err}"
            ))
        })?;
        let thread_id = args.thread_id.trim();
        if thread_id.is_empty() {
            return Err(AiSdkError::Protocol(
                "thread_state_get: thread_id must not be empty".to_string(),
            ));
        }

        let found = self.store.get(thread_id).await.map_err(|err| {
            warn!(
                op = "thread_state_get",
                thread_id = %thread_id,
                status = "error",
                latency_ms = started.elapsed().as_millis() as u64,
                error = %err,
                "thread state operation failed"
            );
            err
        })?;
        let payload = match found {
            Some(record) => json!({
                "found": true,
                "thread_id": record.thread_id,
                "data": record.data,
                "updated_at": record.updated_at,
                "ttl_seconds": record.ttl_seconds
            }),
            None => json!({
                "found": false,
                "thread_id": thread_id
            }),
        };
        info!(
            op = "thread_state_get",
            thread_id = %thread_id,
            status = "ok",
            found = payload.get("found").and_then(|v| v.as_bool()).unwrap_or(false),
            latency_ms = started.elapsed().as_millis() as u64,
            "thread state operation completed"
        );
        Ok(payload)
    }
}

#[async_trait]
impl FunctionTool for ThreadStatePutTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "thread_state_put".to_string(),
            description: "Create or overwrite thread-scoped state by thread_id".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string", "minLength": 1 },
                    "data": {},
                    "ttl_seconds": { "type": "integer", "minimum": 1 }
                },
                "required": ["thread_id", "data"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> Result<Value> {
        let started = Instant::now();
        let args: ThreadStatePutArgs = serde_json::from_value(arguments).map_err(|err| {
            AiSdkError::Protocol(format!(
                "thread_state_put: invalid arguments (expected {{thread_id:string,data:any,ttl_seconds?:u64}}): {err}"
            ))
        })?;
        let thread_id = args.thread_id.trim();
        if thread_id.is_empty() {
            return Err(AiSdkError::Protocol(
                "thread_state_put: thread_id must not be empty".to_string(),
            ));
        }

        self.store
            .put(thread_id, args.data, args.ttl_seconds)
            .await
            .map_err(|err| {
                warn!(
                    op = "thread_state_put",
                    thread_id = %thread_id,
                    status = "error",
                    latency_ms = started.elapsed().as_millis() as u64,
                    error = %err,
                    "thread state operation failed"
                );
                err
            })?;
        info!(
            op = "thread_state_put",
            thread_id = %thread_id,
            status = "ok",
            latency_ms = started.elapsed().as_millis() as u64,
            "thread state operation completed"
        );
        Ok(json!({
            "ok": true,
            "thread_id": thread_id
        }))
    }
}

#[async_trait]
impl FunctionTool for ThreadStateDeleteTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "thread_state_delete".to_string(),
            description: "Delete thread-scoped state by thread_id".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string", "minLength": 1 }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> Result<Value> {
        let started = Instant::now();
        let args: ThreadStateDeleteArgs = serde_json::from_value(arguments).map_err(|err| {
            AiSdkError::Protocol(format!(
                "thread_state_delete: invalid arguments (expected {{thread_id:string}}): {err}"
            ))
        })?;
        let thread_id = args.thread_id.trim();
        if thread_id.is_empty() {
            return Err(AiSdkError::Protocol(
                "thread_state_delete: thread_id must not be empty".to_string(),
            ));
        }

        self.store.delete(thread_id).await.map_err(|err| {
            warn!(
                op = "thread_state_delete",
                thread_id = %thread_id,
                status = "error",
                latency_ms = started.elapsed().as_millis() as u64,
                error = %err,
                "thread state operation failed"
            );
            err
        })?;
        info!(
            op = "thread_state_delete",
            thread_id = %thread_id,
            status = "ok",
            latency_ms = started.elapsed().as_millis() as u64,
            "thread state operation completed"
        );
        Ok(json!({
            "ok": true,
            "thread_id": thread_id
        }))
    }
}
