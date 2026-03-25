#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoContext {
    pub channel: String,
    pub entrypoint: PartyRef,
    pub sender: PartyRef,
    pub conversation: ConversationRef,
    pub message: MessageRef,
    pub reply_target: ReplyTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartyRef {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRef {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRef {
    pub id: String,
    #[serde(default)]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyTarget {
    pub kind: String,
    pub address: String,
    #[serde(default)]
    pub params: Value,
}

pub fn wrap_in_meta_context(io: &IoContext) -> Value {
    serde_json::json!({ "io": io })
}

pub fn extract_io_context(meta_context: &Value) -> Option<IoContext> {
    let io = meta_context.get("io")?.clone();
    serde_json::from_value(io).ok()
}

pub fn extract_reply_target(meta_context: &Value) -> Option<ReplyTarget> {
    let reply_target = meta_context.get("io")?.get("reply_target")?.clone();
    serde_json::from_value(reply_target).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackPostTarget {
    pub channel_id: String,
    pub thread_ts: Option<String>,
    pub workspace_id: Option<String>,
}

pub fn extract_slack_post_target(meta_context: &Value) -> Option<SlackPostTarget> {
    let rt = extract_reply_target(meta_context)?;
    if rt.kind != "slack_post" {
        return None;
    }
    let thread_ts = rt
        .params
        .get("thread_ts")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let workspace_id = rt
        .params
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(SlackPostTarget {
        channel_id: rt.address,
        thread_ts,
        workspace_id,
    })
}

pub fn slack_inbound_io_context(
    team_id: &str,
    user_id: &str,
    channel_id: &str,
    thread_ts: Option<&str>,
    message_id: &str,
) -> IoContext {
    // AI nodes require a logical thread_id for user messages.
    // For Slack root-channel messages (no thread_ts), use channel_id as fallback thread_id.
    let thread_id = thread_ts
        .map(|s| s.to_string())
        .or_else(|| Some(channel_id.to_string()));
    let params = match thread_ts {
        Some(t) => serde_json::json!({ "thread_ts": t, "workspace_id": team_id }),
        None => serde_json::json!({ "workspace_id": team_id }),
    };

    IoContext {
        channel: "slack".to_string(),
        entrypoint: PartyRef {
            kind: "slack_workspace".to_string(),
            id: team_id.to_string(),
        },
        sender: PartyRef {
            kind: "slack_user".to_string(),
            id: user_id.to_string(),
        },
        conversation: ConversationRef {
            kind: "slack_channel".to_string(),
            id: channel_id.to_string(),
            thread_id,
        },
        message: MessageRef {
            id: message_id.to_string(),
            timestamp: None,
        },
        reply_target: ReplyTarget {
            kind: "slack_post".to_string(),
            address: channel_id.to_string(),
            params,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_reply_target_roundtrip() {
        let io = slack_inbound_io_context("T123", "U456", "C789", Some("171234.567"), "EvABC");
        let meta = wrap_in_meta_context(&io);

        let extracted = extract_reply_target(&meta).unwrap();
        assert_eq!(extracted.kind, "slack_post");
        assert_eq!(extracted.address, "C789");
        assert_eq!(
            extracted.params.get("thread_ts").and_then(|v| v.as_str()),
            Some("171234.567")
        );
    }

    #[test]
    fn extract_slack_post_target_parses() {
        let io = slack_inbound_io_context("T123", "U456", "C789", None, "EvABC");
        let meta = wrap_in_meta_context(&io);
        let t = extract_slack_post_target(&meta).unwrap();
        assert_eq!(
            t,
            SlackPostTarget {
                channel_id: "C789".to_string(),
                thread_ts: None,
                workspace_id: Some("T123".to_string()),
            }
        );
    }

    #[test]
    fn slack_thread_id_falls_back_to_channel_id_when_thread_ts_missing() {
        let io = slack_inbound_io_context("T123", "U456", "C789", None, "EvABC");
        assert_eq!(io.conversation.thread_id.as_deref(), Some("C789"));
        assert_eq!(
            io.reply_target
                .params
                .get("thread_ts")
                .and_then(|v| v.as_str()),
            None
        );
    }
}
