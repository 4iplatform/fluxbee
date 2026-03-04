#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub routing: Routing,
    pub meta: Meta,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
    pub src: String,
    pub dst: Option<String>,
    pub ttl: u32,
    pub trace_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    #[serde(rename = "type")]
    pub ty: String,

    #[serde(default)]
    pub msg: Option<String>,

    #[serde(default)]
    pub target: Option<String>,

    #[serde(default)]
    pub action: Option<String>,

    #[serde(default)]
    pub src_ilk: Option<String>,

    #[serde(default)]
    pub dst_ilk: Option<String>,

    #[serde(default = "default_context")]
    pub context: Value,
}

fn default_context() -> Value {
    Value::Object(serde_json::Map::new())
}
