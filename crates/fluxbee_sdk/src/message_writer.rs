//! Who wrote a message's text (docs/cognition-hands-v1.md): `meta.context.writer`, the writer's ILK
//! and name. Whoever writes the text names it: an IO node the person, an agent itself, a WF itself
//! for its own text; a forward keeps it. As it sends, a node fills a missing name from the identity
//! memory of its hive (`fill_name`, called by `NodeSender::send_traced`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::identity;
use crate::protocol::{is_solution_kind, Message, Meta};

/// The writer's key in `meta.context`.
pub const WRITER_KEY: &str = "writer";

/// How long a node keeps what the identity memory said of an ILK: a person who registers in the
/// middle of a conversation gets their name in their messages within it.
const ILK_FACTS_TTL: Duration = Duration::from_secs(60);
/// The most ILKs a node keeps facts of; past it, it forgets them all and reads again.
const ILK_FACTS_MAX: usize = 4096;

/// Who wrote a message's text: their ILK, and their name. A node with no ILK of its own names only
/// its name; a person the identity has no name for, only their ILK.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Writer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Writer {
    /// A writer with `ilk` and `name`, each kept when it is not blank.
    pub fn new(ilk: Option<&str>, name: Option<&str>) -> Self {
        let kept = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        };
        Self {
            ilk: kept(ilk),
            name: kept(name),
        }
    }
}

/// What the identity memory says of an ILK: its type (`human`, `agent` or `system`) and its display
/// name, a registered person's name or a node's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IlkFacts {
    pub ilk_type: String,
    pub name: Option<String>,
}

impl IlkFacts {
    /// The kind of party the ILK is, as an agent reads it: `person` for a human ILK, `workflow` for
    /// an agent ILK whose name is a WF node's, `agent` for any other agent ILK, `system` for a
    /// system ILK. None for a type the identity does not name.
    pub fn kind(&self) -> Option<&'static str> {
        match self.ilk_type.as_str() {
            "human" => Some("person"),
            "agent" => {
                let workflow = self
                    .name
                    .as_deref()
                    .and_then(|name| name.get(..3))
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("WF."));
                Some(if workflow { "workflow" } else { "agent" })
            }
            "system" => Some("system"),
            _ => None,
        }
    }

    /// Whether the ILK is a person's.
    pub fn is_person(&self) -> bool {
        self.ilk_type == "human"
    }
}

/// The writer a context names, when it names one with an ILK or a name.
pub fn writer_of(context: Option<&Value>) -> Option<Writer> {
    let writer: Writer = serde_json::from_value(context?.get(WRITER_KEY)?.clone()).ok()?;
    let writer = Writer::new(writer.ilk.as_deref(), writer.name.as_deref());
    (writer.ilk.is_some() || writer.name.is_some()).then_some(writer)
}

/// Names `writer` as the writer of `meta`'s text, in place of any other, making the context when it
/// is missing. A context that is not an object is a node's own and stays as it is.
pub fn set_writer(meta: &mut Meta, writer: Writer) {
    let context = meta
        .context
        .get_or_insert_with(|| Value::Object(Map::new()));
    let Some(context) = context.as_object_mut() else {
        return;
    };
    if let Ok(writer) = serde_json::to_value(writer) {
        context.insert(WRITER_KEY.to_string(), writer);
    }
}

/// Fills the name of `msg`'s writer from the identity memory of `hive_id`, when the writer names an
/// ILK and no name: a solution message only, and only when the identity has a name for it.
pub fn fill_name(msg: &mut Message, hive_id: &str) {
    fill_name_with(msg, |ilk| ilk_facts(hive_id, ilk));
}

/// `fill_name`, with `facts` saying what the identity knows of an ILK.
pub fn fill_name_with(msg: &mut Message, facts: impl Fn(&str) -> Option<IlkFacts>) {
    if !is_solution_kind(&msg.meta.msg_type) {
        return;
    }
    let Some(writer) = writer_of(msg.meta.context.as_ref()) else {
        return;
    };
    if writer.name.is_some() {
        return;
    }
    let Some(name) = writer
        .ilk
        .as_deref()
        .and_then(&facts)
        .and_then(|facts| facts.name)
    else {
        return;
    };
    set_writer(
        &mut msg.meta,
        Writer::new(writer.ilk.as_deref(), Some(&name)),
    );
}

/// What the identity memory of `hive_id` says of `ilk`, as this node read it within the last
/// `ILK_FACTS_TTL`. None when the identity has no such active ILK, or cannot be read.
pub fn ilk_facts(hive_id: &str, ilk: &str) -> Option<IlkFacts> {
    let (hive_id, ilk) = (hive_id.trim(), ilk.trim());
    if hive_id.is_empty() || ilk.is_empty() {
        return None;
    }
    static KNOWN: OnceLock<Mutex<HashMap<(String, String), (Instant, Option<IlkFacts>)>>> =
        OnceLock::new();
    let known = KNOWN.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (hive_id.to_string(), ilk.to_ascii_lowercase());
    if let Ok(known) = known.lock() {
        if let Some((read_at, facts)) = known.get(&key) {
            if read_at.elapsed() < ILK_FACTS_TTL {
                return facts.clone();
            }
        }
    }
    let facts = identity::find_ilk_by_id_from_hive_id(hive_id, ilk)
        .ok()
        .flatten()
        .map(|option| IlkFacts {
            ilk_type: option.ilk_type,
            name: option.display_name,
        });
    if let Ok(mut known) = known.lock() {
        if known.len() >= ILK_FACTS_MAX {
            known.clear();
        }
        known.insert(key, (Instant::now(), facts.clone()));
    }
    facts
}

/// The hive of a node's full name (`AI.sales@worker1`), or none.
pub fn hive_of(node_name: &str) -> Option<&str> {
    node_name
        .rsplit_once('@')
        .map(|(_, hive)| hive.trim())
        .filter(|hive| !hive.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Destination, Routing};
    use serde_json::json;

    fn message(msg_type: &str, context: Option<Value>) -> Message {
        Message {
            routing: Routing {
                src: "uuid".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("AI.sales@worker1".to_string()),
                ttl: 16,
                trace_id: "trace".to_string(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                context,
                ..Meta::default()
            },
            payload: json!({}),
        }
    }

    fn facts(ilk_type: &str, name: Option<&str>) -> IlkFacts {
        IlkFacts {
            ilk_type: ilk_type.to_string(),
            name: name.map(ToString::to_string),
        }
    }

    #[test]
    fn a_writer_is_set_read_and_replaced_beside_the_other_keys() {
        let mut msg = message(
            "user",
            Some(json!({"trace": [], "io": {"channel": "slack"}})),
        );
        set_writer(
            &mut msg.meta,
            Writer::new(Some(" ilk:ana "), Some("Ana Pérez")),
        );
        assert_eq!(
            writer_of(msg.meta.context.as_ref()),
            Some(Writer::new(Some("ilk:ana"), Some("Ana Pérez")))
        );
        set_writer(&mut msg.meta, Writer::new(Some("ilk:wf"), None));
        let context = msg.meta.context.as_ref().unwrap();
        assert_eq!(context["writer"], json!({"ilk": "ilk:wf"}));
        assert_eq!(context["io"], json!({"channel": "slack"}));
        assert!(context["trace"].is_array());

        // A writer with neither an ILK nor a name, or one that does not read as a writer, is none.
        assert_eq!(writer_of(Some(&json!({"writer": {"ilk": " "}}))), None);
        assert_eq!(writer_of(Some(&json!({"writer": "ilk:ana"}))), None);
        assert_eq!(writer_of(None), None);

        // A context that is not an object is the node's own.
        let mut own = message("user", Some(json!("text")));
        set_writer(&mut own.meta, Writer::new(Some("ilk:ana"), None));
        assert_eq!(own.meta.context, Some(json!("text")));
    }

    #[test]
    fn a_missing_name_is_filled_from_the_identity() {
        let known = |ilk: &str| match ilk {
            "ilk:ana" => Some(facts("human", Some("Ana Pérez"))),
            "ilk:temp" => Some(facts("human", None)),
            _ => None,
        };
        let mut msg = message("user", Some(json!({"writer": {"ilk": "ilk:ana"}})));
        fill_name_with(&mut msg, known);
        assert_eq!(
            writer_of(msg.meta.context.as_ref()),
            Some(Writer::new(Some("ilk:ana"), Some("Ana Pérez")))
        );

        // A name the message has stays; a person with no name in the identity and an ILK the
        // identity does not know keep theirs as they are.
        for context in [
            json!({"writer": {"ilk": "ilk:ana", "name": "Ana"}}),
            json!({"writer": {"ilk": "ilk:temp"}}),
            json!({"writer": {"ilk": "ilk:unknown"}}),
            json!({"writer": {"name": "WF.soporte@worker1"}}),
        ] {
            let mut msg = message("user", Some(context.clone()));
            fill_name_with(&mut msg, known);
            assert_eq!(msg.meta.context, Some(context));
        }

        // A system message is not the solution's: nothing is read for it.
        let mut system = message("system", Some(json!({"writer": {"ilk": "ilk:ana"}})));
        fill_name_with(&mut system, |_| {
            panic!("no identity read for a system message")
        });
    }

    #[test]
    fn the_kind_of_an_ilk() {
        assert_eq!(facts("human", Some("Ana Pérez")).kind(), Some("person"));
        assert_eq!(
            facts("agent", Some("AI.sales@worker1")).kind(),
            Some("agent")
        );
        assert_eq!(
            facts("agent", Some("WF.soporte@worker1")).kind(),
            Some("workflow")
        );
        assert_eq!(
            facts("agent", Some("wf.soporte@worker1")).kind(),
            Some("workflow")
        );
        assert_eq!(facts("agent", None).kind(), Some("agent"));
        assert_eq!(
            facts("system", Some("SY.frontdesk.gov@motherbee")).kind(),
            Some("system")
        );
        assert_eq!(facts("unknown", None).kind(), None);
        assert!(facts("human", None).is_person());
        assert!(!facts("agent", None).is_person());
        assert_eq!(hive_of("AI.sales@worker1"), Some("worker1"));
        assert_eq!(hive_of("AI.sales"), None);
    }
}
