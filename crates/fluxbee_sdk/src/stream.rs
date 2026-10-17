//! The router stream's wire contract (docs/router-stream-v1.md, FINDINGS A-70).
//!
//! Every router keeps a short, best-effort log of the records it copies (turns) and of what its
//! nodes append, and delivers them in acked batches to their sinks over the fluxbee socket. The
//! stream is a node of its router, `RT.<router>.stream@<hive>`. This module is what the router,
//! the sinks and the producers share.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use crate::split::NodeSender;
use crate::NodeError;

/// A node to its own router: append `payload` under `subject`. The router consumes it and routes
/// it to no node.
pub const MSG_STREAM_APPEND: &str = "STREAM_APPEND";
/// A stream to a sink: a batch of records in seq order.
pub const MSG_STREAM_BATCH: &str = "STREAM_BATCH";
/// A sink to the stream that sent a batch: everything up to `up_to_seq` is taken.
pub const MSG_STREAM_ACK: &str = "STREAM_ACK";
/// Anyone to a stream: its counters and backlog.
pub const MSG_STREAM_STATUS: &str = "STREAM_STATUS";
pub const MSG_STREAM_STATUS_RESPONSE: &str = "STREAM_STATUS_RESPONSE";

/// The router's copy of every message (except system, admin and query_response kinds).
pub const SUBJECT_STORAGE_TURNS: &str = "storage.turns";
/// What SY.cognition derives from turns (`storage.cognition.<entity>`).
pub const SUBJECT_STORAGE_COGNITION_PREFIX: &str = "storage.cognition.";
/// SY.cognition's envelopes of one turn, packed (`CognitionEnvelopeBatch`); SY.storage expands
/// them into one row per envelope (docs/router-stream-v1.md §6.2).
pub const SUBJECT_STORAGE_COGNITION_BATCH: &str = "storage.cognition.batch";
/// The agent's router's copy of a message its local agent sent or received, for its local
/// SY.cognition only (§6.2). The record's producer is that agent node.
pub const SUBJECT_COGNITION_TURNS: &str = "cognition.turns";
/// LLM usage records (stage 3).
pub const SUBJECT_STORAGE_USAGE: &str = "storage.usage";

/// The payload of `STREAM_APPEND`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamAppend {
    pub subject: String,
    pub payload: Value,
}

/// One record of a batch. `producer` is the router-stamped name of whoever appended it (the
/// sending node for a turn).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamRecord {
    pub seq: u64,
    pub ts_ms: u64,
    pub subject: String,
    pub producer: String,
    pub payload: Value,
}

/// The payload of `STREAM_BATCH`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamBatch {
    /// The stream that sent it, `RT.<router>.stream@<hive>`.
    pub stream: String,
    /// Drawn again whenever the stream's seq may restart (its state lost, or kept in memory only):
    /// a sink that skips repeats keys its last seq by stream and epoch (§6.2).
    #[serde(default)]
    pub epoch: u64,
    pub records: Vec<StreamRecord>,
}

/// The payload of `STREAM_ACK`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamAck {
    pub up_to_seq: u64,
}

/// A node's `STREAM_APPEND` to its own router: fire and forget, no reply. The router takes it before
/// routing, whatever the destination, and appends it only if this node may append that subject
/// (§2.2).
pub async fn append(sender: &NodeSender, subject: &str, payload: Value) -> Result<(), NodeError> {
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Resolve,
            ttl: 1,
            trace_id: uuid::Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_STREAM_APPEND.to_string()),
            ..Meta::default()
        },
        payload: serde_json::to_value(StreamAppend {
            subject: subject.to_string(),
            payload,
        })
        .unwrap_or(Value::Null),
    };
    sender.send(msg).await
}

/// The stream node of a router: `RT.gateway@worker1` → `RT.gateway.stream@worker1`. A router name
/// without the `RT.` prefix (`JSR_ROUTER_NAME` may lack it) gets one.
pub fn stream_node_name(router_l2_name: &str) -> String {
    let (local, hive) = router_l2_name
        .split_once('@')
        .unwrap_or((router_l2_name, ""));
    let local = local.trim();
    let local = if local
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("RT."))
    {
        local.to_string()
    } else {
        format!("RT.{local}")
    };
    if hive.trim().is_empty() {
        format!("{local}.stream")
    } else {
        format!("{local}.stream@{}", hive.trim())
    }
}

/// Whether a router-stamped name is a router stream's: `RT.<router>.stream@<hive>`. Only the
/// router's own process may take an `RT.` name (D12 of docs/router-stream-v1.md).
pub fn is_stream_node_name(name: &str) -> bool {
    let Some((local, hive)) = name.split_once('@') else {
        return false;
    };
    if hive.trim().is_empty() {
        return false;
    }
    let lower = local.to_ascii_lowercase();
    lower.starts_with("rt.") && lower.ends_with(".stream") && lower.len() > "rt..stream".len()
}

/// The hive of a stream node name (`RT.gateway.stream@worker1` → `worker1`).
pub fn stream_hive(name: &str) -> Option<&str> {
    name.split_once('@')
        .map(|(_, hive)| hive.trim())
        .filter(|hive| !hive.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_router_names_its_stream() {
        assert_eq!(
            stream_node_name("RT.gateway@worker1"),
            "RT.gateway.stream@worker1"
        );
        assert_eq!(
            stream_node_name("RT.switch1@worker1"),
            "RT.switch1.stream@worker1"
        );
        assert_eq!(
            stream_node_name("primary@sandbox"),
            "RT.primary.stream@sandbox"
        );
        assert!(is_stream_node_name(&stream_node_name(
            "RT.gateway@motherbee"
        )));
    }

    #[test]
    fn only_stream_names_pass() {
        assert!(is_stream_node_name("RT.gateway.stream@motherbee"));
        assert!(is_stream_node_name("rt.switch1.stream@worker1"));
        assert!(!is_stream_node_name("RT.gateway@motherbee"));
        assert!(!is_stream_node_name("RT.gateway.stream"));
        assert!(!is_stream_node_name("RT..stream@motherbee"));
        assert!(!is_stream_node_name("SY.storage@motherbee"));
        assert!(!is_stream_node_name("AI.stream@motherbee"));
        assert_eq!(stream_hive("RT.gateway.stream@worker1"), Some("worker1"));
    }

    #[test]
    fn a_batch_round_trips() {
        let batch = StreamBatch {
            stream: "RT.gateway.stream@worker1".to_string(),
            epoch: 42,
            records: vec![StreamRecord {
                seq: 7,
                ts_ms: 1_700_000_000_000,
                subject: SUBJECT_STORAGE_TURNS.to_string(),
                producer: "AI.sales@worker1".to_string(),
                payload: serde_json::json!({"routing": {"trace_id": "t"}}),
            }],
        };
        let value = serde_json::to_value(&batch).unwrap();
        assert_eq!(serde_json::from_value::<StreamBatch>(value).unwrap(), batch);
    }
}
