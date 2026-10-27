use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::function_calling::{AgentTool, FunctionToolProvider, FunctionToolRegistry, ToolCard};
use crate::thread_state::ThreadStateStore;
use crate::{AiSdkError, Result};

/// When a message gets the state tools: they are bound to the record of whoever sent it.
const STATE_TOOLS_PRESENT_WHEN: &str =
    "the message names its sender (`src_ilk`), and the node's state store opened at start";

/// Every state tool works on the record of the message's sender, whatever key the model passes,
/// so it never reaches another person's state.
const STATE_RECORD_LIMIT: &str = "One record per person: every call works on the record of \
    whoever sent the current message, shared by all their conversations with this agent. \
    `state_key` is required, but it does not choose the record.";

const STATE_PLACE_LIMIT: &str = "Only this agent sees the record: it is kept on its node's disk.";

const STATE_KEY_INPUT: &str = "Any non-empty label. The record is always the sender's.";

const STATE_KEY_OUTPUT: &str = "The record's key: the sender's ILK.";

/// `thread_state_get`: reads the record of the message's sender.
#[derive(Clone)]
pub struct ThreadStateGetTool {
    store: Arc<dyn ThreadStateStore>,
    /// The record every call reads: in an AI runtime, the sender's `src_ilk`.
    state_key: String,
}

impl ThreadStateGetTool {
    pub fn new(store: Arc<dyn ThreadStateStore>, state_key: impl Into<String>) -> Self {
        Self {
            store,
            state_key: state_key.into(),
        }
    }
}

/// `thread_state_put`: replaces the record of the message's sender.
#[derive(Clone)]
pub struct ThreadStatePutTool {
    store: Arc<dyn ThreadStateStore>,
    /// The record every call writes: in an AI runtime, the sender's `src_ilk`.
    state_key: String,
}

impl ThreadStatePutTool {
    pub fn new(store: Arc<dyn ThreadStateStore>, state_key: impl Into<String>) -> Self {
        Self {
            store,
            state_key: state_key.into(),
        }
    }
}

/// `thread_state_delete`: deletes the record of the message's sender.
#[derive(Clone)]
pub struct ThreadStateDeleteTool {
    store: Arc<dyn ThreadStateStore>,
    /// The record every call deletes: in an AI runtime, the sender's `src_ilk`.
    state_key: String,
}

impl ThreadStateDeleteTool {
    pub fn new(store: Arc<dyn ThreadStateStore>, state_key: impl Into<String>) -> Self {
        Self {
            store,
            state_key: state_key.into(),
        }
    }
}

/// The three state tools over one record: in an AI runtime, the message sender's (`src_ilk`).
#[derive(Clone)]
pub struct ThreadStateToolsProvider {
    get_tool: ThreadStateGetTool,
    put_tool: ThreadStatePutTool,
    delete_tool: ThreadStateDeleteTool,
}

impl ThreadStateToolsProvider {
    pub fn new(store: Arc<dyn ThreadStateStore>, state_key: impl Into<String>) -> Self {
        let state_key = state_key.into();
        Self {
            get_tool: ThreadStateGetTool::new(store.clone(), state_key.clone()),
            put_tool: ThreadStatePutTool::new(store.clone(), state_key.clone()),
            delete_tool: ThreadStateDeleteTool::new(store, state_key),
        }
    }
}

impl FunctionToolProvider for ThreadStateToolsProvider {
    fn register_tools(&self, registry: &mut FunctionToolRegistry) -> Result<()> {
        registry.register(Arc::new(self.get_tool.clone()))?;
        registry.register(Arc::new(self.put_tool.clone()))?;
        registry.register(Arc::new(self.delete_tool.clone()))?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ThreadStateGetArgs {
    #[serde(default)]
    state_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ThreadStatePutArgs {
    #[serde(default)]
    state_key: Option<String>,
    data: Value,
    #[serde(default)]
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ThreadStateDeleteArgs {
    #[serde(default)]
    state_key: Option<String>,
}

fn reject_legacy_thread_id_alias(tool_name: &str, arguments: &Value) -> Result<()> {
    if arguments
        .as_object()
        .is_some_and(|map| map.contains_key("thread_id"))
    {
        return Err(AiSdkError::Protocol(format!(
            "{tool_name}: legacy alias 'thread_id' is not supported; use 'state_key'"
        )));
    }
    Ok(())
}

/// The input schema asks for a key and a call without one is refused, but the record is always the
/// tool's own: the model never chooses whose state it reaches.
fn require_requested_state_key(state_key: Option<String>) -> Result<()> {
    state_key
        .filter(|value| !value.trim().is_empty())
        .map(|_| ())
        .ok_or_else(|| AiSdkError::Protocol("state key must not be empty".to_string()))
}

fn state_key_only_input() -> Value {
    json!({
        "type": "object",
        "properties": {
            "state_key": { "type": "string", "minLength": 1, "description": STATE_KEY_INPUT }
        },
        "required": ["state_key"],
        "additionalProperties": false
    })
}

fn state_saved_output(description: &str) -> Value {
    json!({
        "description": description,
        "type": "object",
        "properties": {
            "ok": { "const": true },
            "state_key": { "type": "string", "description": STATE_KEY_OUTPUT }
        },
        "required": ["ok", "state_key"]
    })
}

#[async_trait]
impl AgentTool for ThreadStateGetTool {
    fn card() -> ToolCard {
        ToolCard {
            name: "thread_state_get".to_string(),
            summary: "Read what this agent saved about the person it is talking to.".to_string(),
            when_to_use: "At the start of a turn that continues earlier work with this person: \
                the step reached, the data already collected, a choice they made."
                .to_string(),
            limits: [STATE_RECORD_LIMIT, STATE_PLACE_LIMIT]
                .map(String::from)
                .into(),
            present_when: STATE_TOOLS_PRESENT_WHEN.to_string(),
            input: state_key_only_input(),
            output: json!({
                "description": "`{found: true, state_key, data, updated_at, ttl_seconds}` with \
                    the saved record, or `{found: false, state_key}` when there is none.",
                "type": "object",
                "properties": {
                    "found": { "type": "boolean" },
                    "state_key": { "type": "string", "description": STATE_KEY_OUTPUT },
                    "data": { "description": "What `thread_state_put` saved." },
                    "updated_at": {
                        "type": "string",
                        "description": "When it was saved (RFC 3339)."
                    },
                    "ttl_seconds": {
                        "type": ["integer", "null"],
                        "description": "The ttl_seconds of the last put. Nothing expires the record."
                    }
                },
                "required": ["found", "state_key"]
            }),
            errors: Vec::new(),
        }
    }

    async fn call(&self, arguments: Value) -> Result<Value> {
        let started = Instant::now();
        reject_legacy_thread_id_alias("thread_state_get", &arguments)?;
        let args: ThreadStateGetArgs = serde_json::from_value(arguments).map_err(|err| {
            AiSdkError::Protocol(format!(
                "thread_state_get: invalid arguments (expected {{state_key:string}}): {err}"
            ))
        })?;
        require_requested_state_key(args.state_key)?;
        let state_key = self.state_key.trim();

        let found = self.store.get(state_key).await.map_err(|err| {
            warn!(
                op = "thread_state_get",
                state_key = %state_key,
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
                "state_key": record.thread_id,
                "data": record.data,
                "updated_at": record.updated_at,
                "ttl_seconds": record.ttl_seconds
            }),
            None => json!({
                "found": false,
                "state_key": state_key
            }),
        };
        info!(
            op = "thread_state_get",
            state_key = %state_key,
            status = "ok",
            found = payload.get("found").and_then(|v| v.as_bool()).unwrap_or(false),
            latency_ms = started.elapsed().as_millis() as u64,
            "thread state operation completed"
        );
        Ok(payload)
    }
}

#[async_trait]
impl AgentTool for ThreadStatePutTool {
    fn card() -> ToolCard {
        ToolCard {
            name: "thread_state_put".to_string(),
            summary: "Save what this agent needs to remember about the person it is talking to."
                .to_string(),
            when_to_use: "As soon as something worth keeping changes (a step reached, data \
                collected, a decision), before you answer."
                .to_string(),
            limits: [
                STATE_RECORD_LIMIT,
                "A put replaces the whole record: keep everything in one object. Read it with \
                 `thread_state_get`, change it, and put all of it back.",
                STATE_PLACE_LIMIT,
                // It names no other tool: the frontdesk registers get and put alone
                // (agent-tools-v1 T10).
                "`ttl_seconds` is stored with the record, but nothing expires it.",
            ]
            .map(String::from)
            .into(),
            present_when: STATE_TOOLS_PRESENT_WHEN.to_string(),
            input: json!({
                "type": "object",
                "properties": {
                    "state_key": { "type": "string", "minLength": 1, "description": STATE_KEY_INPUT },
                    "data": {
                        "description": "What to keep, as one JSON value: it replaces the record."
                    },
                    "ttl_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Kept with the record and returned by \
                            `thread_state_get`; nothing expires it."
                    }
                },
                "required": ["state_key", "data"],
                "additionalProperties": false
            }),
            output: state_saved_output("`{ok: true, state_key}` once the record is saved."),
            errors: Vec::new(),
        }
    }

    async fn call(&self, arguments: Value) -> Result<Value> {
        let started = Instant::now();
        reject_legacy_thread_id_alias("thread_state_put", &arguments)?;
        let args: ThreadStatePutArgs = serde_json::from_value(arguments).map_err(|err| {
            AiSdkError::Protocol(format!(
                "thread_state_put: invalid arguments (expected {{state_key:string,data:any,ttl_seconds?:u64}}): {err}"
            ))
        })?;
        require_requested_state_key(args.state_key)?;
        let state_key = self.state_key.trim();

        self.store
            .put(state_key, args.data, args.ttl_seconds)
            .await
            .map_err(|err| {
                warn!(
                    op = "thread_state_put",
                    state_key = %state_key,
                    status = "error",
                    latency_ms = started.elapsed().as_millis() as u64,
                    error = %err,
                    "thread state operation failed"
                );
                err
            })?;
        info!(
            op = "thread_state_put",
            state_key = %state_key,
            status = "ok",
            latency_ms = started.elapsed().as_millis() as u64,
            "thread state operation completed"
        );
        Ok(json!({
            "ok": true,
            "state_key": state_key
        }))
    }
}

#[async_trait]
impl AgentTool for ThreadStateDeleteTool {
    fn card() -> ToolCard {
        ToolCard {
            name: "thread_state_delete".to_string(),
            summary: "Delete what this agent saved about the person it is talking to.".to_string(),
            when_to_use: "When the work with this person is over and nothing should carry into \
                their next conversation, or when they ask you to forget it."
                .to_string(),
            limits: [
                "It deletes the record of whoever sent the current message, whatever \
                 `state_key` says.",
                "Deleting a record that does not exist succeeds.",
            ]
            .map(String::from)
            .into(),
            present_when: STATE_TOOLS_PRESENT_WHEN.to_string(),
            input: state_key_only_input(),
            output: state_saved_output("`{ok: true, state_key}` once the record is gone."),
            errors: Vec::new(),
        }
    }

    async fn call(&self, arguments: Value) -> Result<Value> {
        let started = Instant::now();
        reject_legacy_thread_id_alias("thread_state_delete", &arguments)?;
        let args: ThreadStateDeleteArgs = serde_json::from_value(arguments).map_err(|err| {
            AiSdkError::Protocol(format!(
                "thread_state_delete: invalid arguments (expected {{state_key:string}}): {err}"
            ))
        })?;
        require_requested_state_key(args.state_key)?;
        let state_key = self.state_key.trim();

        self.store.delete(state_key).await.map_err(|err| {
            warn!(
                op = "thread_state_delete",
                state_key = %state_key,
                status = "error",
                latency_ms = started.elapsed().as_millis() as u64,
                error = %err,
                "thread state operation failed"
            );
            err
        })?;
        info!(
            op = "thread_state_delete",
            state_key = %state_key,
            status = "ok",
            latency_ms = started.elapsed().as_millis() as u64,
            "thread state operation completed"
        );
        Ok(json!({
            "ok": true,
            "state_key": state_key
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_tools_cards_are_complete() {
        for card in [
            ThreadStateGetTool::card(),
            ThreadStatePutTool::card(),
            ThreadStateDeleteTool::card(),
        ] {
            assert_eq!(card.validate(), Ok(()), "{}", card.name);
        }
    }
}
