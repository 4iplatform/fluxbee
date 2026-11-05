//! The trace of a message's cycle (docs/message-trace-v1.md): `meta.context.trace`, one entry for
//! each node a solution message went through, oldest first. Each node writes its own entry as it
//! sends (`NodeSender::send_traced`): when, from which node, to which, and what it did.

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::protocol::{is_solution_kind, Destination, Message, Meta};

/// The trace's key in `meta.context`.
pub const TRACE_KEY: &str = "trace";
/// The most words an entry's `act` keeps (§3, decision 13).
pub const MAX_ACT_WORDS: usize = 8;
/// The most characters an `act` word keeps (§3, decision 13).
pub const MAX_ACT_WORD_CHARS: usize = 32;

/// A node's entry in a trace: when it sent the message, its own name, the destination by name, and
/// the actions it took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEntry {
    pub t: String,
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub act: Vec<String>,
}

/// What a node adds to the entry its SDK writes as it sends (§5): the destination by name, when the
/// message goes to a UUID (a reply), and the actions it took.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceNote {
    pub to: Option<String>,
    pub act: Vec<String>,
}

/// Writes `from`'s entry on `msg` as it sends it, with `note`: the destination is `note.to`, else
/// the message's when it names a node. A message that is not the solution's (`is_solution_kind`),
/// or a sender with no name, gets none.
pub fn write_entry(msg: &mut Message, from: &str, note: TraceNote) {
    let from = from.trim();
    if from.is_empty() || !is_solution_kind(&msg.meta.msg_type) {
        return;
    }
    let to = note
        .to
        .map(|to| to.trim().to_string())
        .filter(|to| !to.is_empty())
        .or_else(|| destination_name(&msg.routing.dst));
    append(
        &mut msg.meta,
        TraceEntry {
            t: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            from: from.to_string(),
            to,
            act: act_words(note.act),
        },
    );
}

/// Appends `entry` to `meta.context.trace`, making the context and the list when they are missing.
/// A context that is not an object is a node's own and stays as it is, without the entry; a `trace`
/// that is not a list is replaced.
pub fn append(meta: &mut Meta, entry: TraceEntry) {
    let context = meta
        .context
        .get_or_insert_with(|| Value::Object(Map::new()));
    let Some(context) = context.as_object_mut() else {
        return;
    };
    let trace = context
        .entry(TRACE_KEY)
        .or_insert_with(|| Value::Array(Vec::new()));
    if !trace.is_array() {
        *trace = Value::Array(Vec::new());
    }
    if let (Some(list), Ok(entry)) = (trace.as_array_mut(), serde_json::to_value(entry)) {
        list.push(entry);
    }
}

/// The entries of a context's trace, oldest first. One that does not read as an entry is skipped.
pub fn entries(context: Option<&Value>) -> Vec<TraceEntry> {
    context
        .and_then(|context| context.get(TRACE_KEY))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Where a message's cycle started: the node of its trace's first entry.
pub fn origin(context: Option<&Value>) -> Option<String> {
    context?
        .get(TRACE_KEY)?
        .as_array()?
        .first()?
        .get("from")?
        .as_str()
        .map(str::trim)
        .filter(|from| !from.is_empty())
        .map(ToString::to_string)
}

/// A destination's name for an entry: a node's name, or `broadcast`. A UUID is the node's to name
/// (`TraceNote::to`), and a message with no destination gets the router's entry when it picks one.
fn destination_name(dst: &Destination) -> Option<String> {
    match dst {
        Destination::Unicast(value) if Uuid::parse_str(value).is_err() => {
            Some(value.trim().to_string()).filter(|value| !value.is_empty())
        }
        Destination::Broadcast => Some("broadcast".to_string()),
        _ => None,
    }
}

/// `words` as an entry keeps them: each one trimmed and cut at `MAX_ACT_WORD_CHARS`, no blank or
/// repeated one, at most `MAX_ACT_WORDS`.
fn act_words(words: Vec<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for word in words {
        let word: String = word.trim().chars().take(MAX_ACT_WORD_CHARS).collect();
        if word.is_empty() || kept.contains(&word) {
            continue;
        }
        kept.push(word);
        if kept.len() == MAX_ACT_WORDS {
            break;
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Routing;
    use serde_json::json;

    fn message(msg_type: &str, dst: Destination, context: Option<Value>) -> Message {
        Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: None,
                dst,
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                context,
                ..Meta::default()
            },
            payload: json!({"type": "text", "content": "hola"}),
        }
    }

    #[test]
    fn a_solution_message_gets_its_senders_entry_after_the_ones_it_carries() {
        let carried = json!({"io": {"channel": "slack"}, "trace": [
            {"t": "2026-11-04T10:17:03.120Z", "from": "IO.slack@motherbee", "to": "WF.ventas@worker1"}
        ]});
        let mut msg = message(
            "user",
            Destination::Unicast("AI.ventas@worker1".to_string()),
            Some(carried),
        );
        let note = TraceNote {
            to: None,
            act: vec!["forward".to_string()],
        };
        write_entry(&mut msg, "WF.ventas@worker1", note);

        let trace = entries(msg.meta.context.as_ref());
        assert_eq!(trace.len(), 2);
        assert_eq!(trace[0].from, "IO.slack@motherbee");
        let entry = &trace[1];
        assert_eq!(entry.from, "WF.ventas@worker1");
        assert_eq!(entry.to.as_deref(), Some("AI.ventas@worker1"));
        assert_eq!(entry.act, ["forward"]);
        assert!(
            entry.t.ends_with('Z') && entry.t.len() == "2026-11-04T10:17:03.120Z".len(),
            "{}",
            entry.t
        );
        assert_eq!(msg.meta.context.as_ref().unwrap()["io"]["channel"], "slack");
        assert_eq!(
            origin(msg.meta.context.as_ref()).as_deref(),
            Some("IO.slack@motherbee")
        );
    }

    #[test]
    fn a_reply_names_its_requester_and_a_message_with_no_context_gets_one() {
        let requester = Uuid::new_v4().to_string();
        let mut msg = message("user", Destination::Unicast(requester), None);
        let note = TraceNote {
            to: Some("WF.ventas@worker1".to_string()),
            act: vec!["handover".to_string()],
        };
        write_entry(&mut msg, "AI.ventas@worker1", note);

        let trace = entries(msg.meta.context.as_ref());
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].to.as_deref(), Some("WF.ventas@worker1"));
        assert_eq!(trace[0].act, ["handover"]);
    }

    #[test]
    fn a_uuid_or_no_destination_leaves_no_to_and_a_broadcast_says_so() {
        let mut to_uuid = message(
            "user",
            Destination::Unicast(Uuid::new_v4().to_string()),
            None,
        );
        write_entry(&mut to_uuid, "IO.slack@motherbee", TraceNote::default());
        assert_eq!(entries(to_uuid.meta.context.as_ref())[0].to, None);

        let mut unresolved = message("user", Destination::Resolve, None);
        write_entry(&mut unresolved, "IO.slack@motherbee", TraceNote::default());
        assert_eq!(entries(unresolved.meta.context.as_ref())[0].to, None);

        let mut everyone = message("user", Destination::Broadcast, None);
        write_entry(&mut everyone, "WF.ventas@worker1", TraceNote::default());
        assert_eq!(
            entries(everyone.meta.context.as_ref())[0].to.as_deref(),
            Some("broadcast")
        );
    }

    #[test]
    fn system_admin_and_query_responses_carry_no_trace() {
        for kind in ["system", "SYSTEM", "admin", "query_response"] {
            let mut msg = message(
                kind,
                Destination::Unicast("SY.admin@motherbee".into()),
                None,
            );
            write_entry(&mut msg, "SY.timer@worker1", TraceNote::default());
            assert_eq!(msg.meta.context, None, "{kind}");
        }
        let mut nameless = message("user", Destination::Broadcast, None);
        write_entry(&mut nameless, " ", TraceNote::default());
        assert_eq!(nameless.meta.context, None);
    }

    #[test]
    fn act_words_are_trimmed_cut_deduplicated_and_at_most_eight() {
        let long = "x".repeat(40);
        let mut words: Vec<String> = vec![" forward ".into(), "".into(), "forward".into(), long];
        words.extend((1..=10).map(|i| format!("w{i}")));
        let kept = act_words(words);
        assert_eq!(kept.len(), MAX_ACT_WORDS);
        assert_eq!(kept[0], "forward");
        assert_eq!(kept[1].chars().count(), MAX_ACT_WORD_CHARS);
        assert_eq!(kept[2], "w1");
    }

    #[test]
    fn a_context_that_is_not_an_object_is_left_alone_and_a_bad_trace_is_replaced() {
        let mut text = message("user", Destination::Broadcast, Some(json!("free text")));
        write_entry(&mut text, "WF.ventas@worker1", TraceNote::default());
        assert_eq!(text.meta.context, Some(json!("free text")));

        let mut bad = message(
            "user",
            Destination::Broadcast,
            Some(json!({"trace": "not a list"})),
        );
        write_entry(&mut bad, "WF.ventas@worker1", TraceNote::default());
        assert_eq!(entries(bad.meta.context.as_ref()).len(), 1);

        let unreadable = json!({"trace": [{"from": "no time"}, {"t": "x", "from": "A@h"}]});
        assert_eq!(entries(Some(&unreadable)).len(), 1);
        assert_eq!(origin(Some(&unreadable)).as_deref(), Some("no time"));
    }
}
