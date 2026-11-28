//! A Digital Executive's capabilities, as its conscious uses them
//! (docs/digital-executive-v1-stage-3.md §5, §6, §6.1).
//!
//! - **Which capabilities an ED has** is identity's (`capabilities_of`): the ILKs of the identity
//!   SHM whose `work_role` is `capability` and that work for the ED, or for nobody in the tenant of
//!   the conscious's own node (a capability of every ED of that tenant). Each one's `handler_node`
//!   is its door, a WF.
//! - **`ask_capability`** (`AskCapabilityTool`), the conscious's base tool, lists them with each
//!   door's `WF_HELP` (`CapabilityDirectory`, which keeps the help until the identity region's
//!   sequence moves), and sends a door a `CAPABILITY_REQUEST` within the turn, carrying the turn's
//!   conversation (`CapabilityConversation`), which the model never writes. Its result is the
//!   door's `CAPABILITY_REQUEST_RESPONSE`.
//! - **A result that comes later** is a `user` message from the door carrying
//!   `meta.context.capability_result`. The runtime takes it only from one of the ED's capabilities
//!   (`screen_capability_result`); the turn runs in the person's conversation, and its answer goes
//!   to the IO node the conversation names, never back to the door (`address_to_conversation`).
//!   From any other sender the field is taken out and the message runs as any other.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use fluxbee_sdk::identity::{list_ilks_from_hive_id, IdentityIlkOption, IdentityIlkSnapshot};
use fluxbee_sdk::protocol::{Destination, Message};
use fluxbee_sdk::{RouterDispatcher, RpcError, SystemRpcRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::function_calling::{AgentTool, ToolCard, ToolErrorCode};
use crate::{AiSdkError, Result};

/// The tool's name.
pub const ASK_CAPABILITY_TOOL: &str = "ask_capability";

/// The `work_role` of a capability's door in the identity SHM (docs/digital-executive-v1-stage-3.md
/// §2).
pub const CAPABILITY_WORK_ROLE: &str = "capability";

/// A WF's help (`handleWFHelp` in go/nodes/wf/wf-generic/node/dispatch.go).
pub const MSG_WF_HELP: &str = "WF_HELP";
pub const MSG_WF_HELP_RESPONSE: &str = "WF_HELP_RESPONSE";

/// The request to a capability's door, and its answer within the turn (stage 3 §6).
pub const MSG_CAPABILITY_REQUEST: &str = "CAPABILITY_REQUEST";
pub const MSG_CAPABILITY_REQUEST_RESPONSE: &str = "CAPABILITY_REQUEST_RESPONSE";

/// How long `ask_capability` waits for a door's answer (the operator, stage 3 §11).
pub const CAPABILITY_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long it waits for a door's help.
pub const CAPABILITY_HELP_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a result that comes later travels: `meta.context.capability_result` (stage 3 §6.1).
pub const CAPABILITY_RESULT_KEY: &str = "capability_result";

/// How much of a result's JSON the model reads (`capability_result_prompt_block`), as a WF's vars.
pub const CAPABILITY_RESULT_PROMPT_MAX_BYTES: usize = 8 * 1024;

/// What `ask_capability` names its failures with (its card's `errors`).
const NO_CAPABILITIES: &str = "no_capabilities";
const UNKNOWN_CAPABILITY: &str = "unknown_capability";
const NO_CONVERSATION: &str = "no_conversation";
const CAPABILITY_UNAVAILABLE: &str = "capability_unavailable";
const INVALID_CAPABILITY_ANSWER: &str = "invalid_capability_answer";

/// One capability of an ED: its door, by the node name identity gives the door's ILK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDoor {
    /// The door's node name (`WF.billing.ed4qm7k2xa@worker1`): the name the model asks it by.
    pub node_name: String,
    /// The door's ILK.
    pub ilk_id: String,
    /// A capability of every ED of its tenant (its ILK works for nobody), not of this ED alone.
    pub for_every_ed: bool,
}

/// The capabilities of the ED `ed`, for a conscious whose node is of `tenant` (stage 3 §2): the
/// active ILKs of `ilks` whose `work_role` is `capability` and that work for `ed`, or for nobody
/// and are of `tenant`, each with a node name. In the order of their names, each once.
pub fn capabilities_of(
    ilks: &[IdentityIlkOption],
    ed: &str,
    tenant: Option<&str>,
) -> Vec<CapabilityDoor> {
    let ed = ed.trim();
    let mut doors = ilks
        .iter()
        .filter(|ilk| ilk.work_role.as_deref() == Some(CAPABILITY_WORK_ROLE))
        .filter_map(|ilk| {
            let for_every_ed = match ilk.works_for.as_deref() {
                Some(person) if person.eq_ignore_ascii_case(ed) => false,
                Some(_) => return None,
                None if tenant
                    .is_some_and(|tenant| tenant.eq_ignore_ascii_case(&ilk.tenant_id)) =>
                {
                    true
                }
                None => return None,
            };
            let node_name = ilk
                .handler_node
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())?;
            Some(CapabilityDoor {
                node_name: node_name.to_string(),
                ilk_id: ilk.ilk_id.clone(),
                for_every_ed,
            })
        })
        .collect::<Vec<_>>();
    doors.sort_by(|a, b| a.node_name.cmp(&b.node_name));
    doors.dedup_by(|a, b| a.node_name == b.node_name);
    doors
}

/// The tenant of the node's own ILK, `self_ilk`, in `ilks`.
fn own_tenant<'a>(ilks: &'a [IdentityIlkOption], self_ilk: Option<&str>) -> Option<&'a str> {
    let self_ilk = self_ilk.map(str::trim).filter(|ilk| !ilk.is_empty())?;
    ilks.iter()
        .find(|ilk| ilk.ilk_id.eq_ignore_ascii_case(self_ilk))
        .map(|ilk| ilk.tenant_id.as_str())
}

/// Reads the identity of a node's hive: its SHM, or a test's.
pub(crate) type IdentityRead =
    Arc<dyn Fn() -> std::result::Result<IdentityIlkSnapshot, String> + Send + Sync>;

/// The identity SHM of the hive `node_name` runs on (`<name>@<hive>`), every active ILK of it.
pub(crate) fn identity_of_node(node_name: &str) -> IdentityRead {
    let node_name = node_name.to_string();
    Arc::new(move || {
        let hive = node_name
            .rsplit_once('@')
            .map(|(_, hive)| hive.trim())
            .filter(|hive| !hive.is_empty())
            .ok_or_else(|| format!("the node's name {node_name:?} names no hive"))?;
        list_ilks_from_hive_id(hive)
            .map_err(|err| format!("the identity SHM of hive {hive} cannot be read: {err}"))
    })
}

/// The conversation a request carries (stage 3 §6.1): the person's thread, the person's ILK, the
/// IO node it came through and that IO's `reply_target`. A door keeps it, and a result that comes
/// later carries it back; the conscious's answer to that result goes where it says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityConversation {
    pub thread_id: String,
    #[serde(default)]
    pub person_ilk: Option<String>,
    pub io_node: String,
    #[serde(default)]
    pub reply_target: Option<Value>,
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

impl CapabilityConversation {
    /// The conversation of the turn `msg`, never the model's: on a capability's result (one the
    /// runtime let through, `screen_capability_result`), the conversation it carries; on any other
    /// message, the message's own: its thread (`meta.thread_id`), its sender (`meta.src_ilk`), the
    /// node it came through (`routing.src_l2_name`, as the router stamped it) and
    /// `meta.context.io.reply_target`. None without a thread or a node.
    pub fn of_turn(msg: &Message) -> Option<Self> {
        if let Some(result) = capability_result_of(msg) {
            return Some(result.conversation);
        }
        Some(Self {
            thread_id: trimmed(msg.meta.thread_id.as_deref())?,
            person_ilk: trimmed(msg.meta.src_ilk.as_deref()),
            io_node: trimmed(msg.routing.src_l2_name.as_deref())?,
            reply_target: msg
                .meta
                .context
                .as_ref()
                .and_then(|context| context.pointer("/io/reply_target"))
                .filter(|target| !target.is_null())
                .cloned(),
        })
    }
}

/// A result that comes later (stage 3 §6.1), as `meta.context.capability_result` carries it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CapabilityResult {
    pub request_id: String,
    /// The door that sent it: the runtime writes the sender's name here as it takes it.
    #[serde(default)]
    pub capability: String,
    pub status: String,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    pub conversation: CapabilityConversation,
}

/// The capability's result `msg` carries, a `user` message; None for any other. On the AI
/// runtime, a handler sees one only from one of its ED's capabilities: the runtime takes the field
/// out of any other (`screen_capability_result`).
pub fn capability_result_of(msg: &Message) -> Option<CapabilityResult> {
    if !msg.meta.msg_type.eq_ignore_ascii_case("user") {
        return None;
    }
    let value = msg.meta.context.as_ref()?.get(CAPABILITY_RESULT_KEY)?;
    parse_capability_result(value).ok()
}

/// A result as the contract says it: a request's id, a status, and a conversation with a thread and
/// an IO node (`IO.<name>@<hive>`). The answer to it goes to that node, so it is never another kind
/// of node: never a WF, never the door.
fn parse_capability_result(value: &Value) -> std::result::Result<CapabilityResult, String> {
    let result: CapabilityResult = serde_json::from_value(value.clone())
        .map_err(|err| format!("capability_result is not a result: {err}"))?;
    if result.request_id.trim().is_empty() || result.status.trim().is_empty() {
        return Err("capability_result names no request_id or no status".to_string());
    }
    let conversation = &result.conversation;
    if conversation.thread_id.trim().is_empty() {
        return Err("capability_result's conversation names no thread".to_string());
    }
    let io_node = conversation.io_node.trim();
    if !io_node.starts_with("IO.") || !io_node.contains('@') {
        return Err(format!(
            "capability_result's conversation names {io_node:?}, not an IO node"
        ));
    }
    Ok(result)
}

/// What the runtime does with a message that carries a capability's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResultScreen {
    /// No result in the message: it runs as any other.
    Absent,
    /// From one of the ED's capabilities: the turn runs in the person's conversation, and its
    /// answer goes to the conversation's IO node.
    Accepted {
        capability: String,
        request_id: String,
    },
    /// From any other sender, or while the agent speaks as no confirmed ED: the field is taken
    /// out, and the message runs as any other, as before stage 3.
    Ignored(String),
    /// From one of the ED's capabilities but not as the contract says, or from a sender identity
    /// could not be read for: never run and never answered, since a capability's door never gets
    /// an answer from the conscious (stage 3 §6.1).
    Dropped(String),
}

/// Screens `msg`, a message the runtime is about to run, for a capability's result (stage 3
/// §6.1): accepted only from one of the capabilities (`capabilities_of`) of `ed`, the person the
/// agent speaks as while identity confirms it, as identity says now (`identity`), whose sender
/// (`routing.src_l2_name`) is the door. An accepted result names its sender as its `capability`,
/// and its conversation's thread is the turn's. Any other loses the field.
pub(crate) fn screen_capability_result(
    msg: &mut Message,
    ed: Option<&str>,
    self_ilk: Option<&str>,
    identity: impl FnOnce() -> std::result::Result<IdentityIlkSnapshot, String>,
) -> ResultScreen {
    if !msg.meta.msg_type.eq_ignore_ascii_case("user") {
        return ResultScreen::Absent;
    }
    let Some(context) = msg.meta.context.as_mut().and_then(Value::as_object_mut) else {
        return ResultScreen::Absent;
    };
    if !context.contains_key(CAPABILITY_RESULT_KEY) {
        return ResultScreen::Absent;
    }
    let ignore = |context: &mut Map<String, Value>, reason: String| {
        context.remove(CAPABILITY_RESULT_KEY);
        ResultScreen::Ignored(reason)
    };
    let Some(ed) = ed.map(str::trim).filter(|ed| !ed.is_empty()) else {
        return ignore(
            context,
            "this agent speaks as no ED that identity confirms".to_string(),
        );
    };
    let Some(sender) = trimmed(msg.routing.src_l2_name.as_deref()) else {
        return ignore(context, "the message names no sender node".to_string());
    };
    let snapshot = match identity() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            return ResultScreen::Dropped(format!(
                "whether {sender} is one of the ED's capabilities cannot be read: {err}"
            ))
        }
    };
    let tenant = own_tenant(&snapshot.ilks, self_ilk);
    let is_door = capabilities_of(&snapshot.ilks, ed, tenant)
        .iter()
        .any(|door| door.node_name == sender);
    if !is_door {
        return ignore(
            context,
            format!("{sender} is not one of the capabilities of {ed}"),
        );
    }
    let mut result = match parse_capability_result(&context[CAPABILITY_RESULT_KEY]) {
        Ok(result) => result,
        Err(reason) => return ResultScreen::Dropped(format!("from {sender}: {reason}")),
    };
    result.capability = sender.clone();
    if let Some(Value::Object(field)) = context.get_mut(CAPABILITY_RESULT_KEY) {
        field.insert("capability".to_string(), json!(sender));
    }
    msg.meta.thread_id = Some(result.conversation.thread_id.clone());
    ResultScreen::Accepted {
        capability: sender,
        request_id: result.request_id,
    }
}

/// The conversation the answer to `msg` goes to: the one its result carries, when `msg` is a
/// capability's result the runtime let through; None for any other message.
pub(crate) fn result_conversation(msg: &Message) -> Option<CapabilityConversation> {
    capability_result_of(msg).map(|result| result.conversation)
}

/// Sends `reply`, the answer to a capability's result, into `conversation` (stage 3 §6.1): to its
/// IO node, for its person (`dst_ilk`), in its thread, with its `reply_target` as the IO reads it
/// (`meta.context.io.reply_target`), and without the result. Its `src_ilk` stays for the runtime to
/// stamp: the ED while identity confirms it.
pub(crate) fn address_to_conversation(reply: &mut Message, conversation: &CapabilityConversation) {
    reply.routing.dst = Destination::Unicast(conversation.io_node.clone());
    reply.meta.dst_ilk = conversation.person_ilk.clone();
    reply.meta.thread_id = Some(conversation.thread_id.clone());
    let mut context = match reply.meta.context.take() {
        Some(Value::Object(context)) => context,
        _ => Map::new(),
    };
    context.remove(CAPABILITY_RESULT_KEY);
    match &conversation.reply_target {
        Some(target) => {
            context.insert("io".to_string(), json!({ "reply_target": target }));
        }
        None => {
            context.remove("io");
        }
    }
    reply.meta.context = Some(Value::Object(context));
}

/// A capability's result as the model reads it, ahead of the message (`meta.context.
/// capability_result`, one the runtime let through): whose it is, that it answers an earlier
/// request of this conversation, and its status, result or error; the result's JSON cut at
/// `CAPABILITY_RESULT_PROMPT_MAX_BYTES` with a line that says so. None for any other message.
pub fn capability_result_prompt_block(msg: &Message) -> Option<String> {
    let result = capability_result_of(msg)?;
    let mut lines = vec![
        "Capability result:".to_string(),
        format!(
            "Your capability {} answers your earlier request {} of this conversation, which it \
             said would come later. Your reply goes to the person of this conversation, not to \
             the capability.",
            result.capability, result.request_id
        ),
        format!("Status: {}", result.status),
    ];
    if let Some(value) = result.result.as_ref().filter(|value| !value.is_null()) {
        let mut text = value.to_string();
        let len = text.len();
        if len > CAPABILITY_RESULT_PROMPT_MAX_BYTES {
            let mut cut = CAPABILITY_RESULT_PROMPT_MAX_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str(&format!(
                "\n[cut at {CAPABILITY_RESULT_PROMPT_MAX_BYTES} of {len} bytes]"
            ));
        }
        lines.push(format!("Result: {text}"));
    }
    if result.error_code.is_some() || result.message.is_some() {
        lines.push(format!(
            "Error: {}{}",
            result.error_code.as_deref().unwrap_or("error"),
            result
                .message
                .as_deref()
                .map(|message| format!(": {}", message.replace('\n', " ")))
                .unwrap_or_default()
        ));
    }
    Some(lines.join("\n"))
}

/// What `ask_capability` keeps across turns: the node's link to its router, its name and its own
/// ILK, and each door's help, kept until the identity region's sequence moves or the ED changes.
/// A help that could not be read is asked again at the next listing.
pub struct CapabilityDirectory {
    router: Arc<RouterDispatcher>,
    node_name: String,
    self_ilk: Option<String>,
    identity: IdentityRead,
    help_timeout: Duration,
    request_timeout: Duration,
    cache: Mutex<HelpCache>,
}

#[derive(Default)]
struct HelpCache {
    /// The identity region's sequence and the ED the help was read for.
    seq: Option<u64>,
    ed: String,
    /// Each door's help, by its node name, as the model reads it.
    help: HashMap<String, Map<String, Value>>,
}

impl CapabilityDirectory {
    /// The directory of the node `node_name` (`AI.ed4qm7k2xa@worker1`), whose own ILK is
    /// `self_ilk`: it reads the identity SHM of the node's hive and asks over `router`.
    pub fn new(
        router: Arc<RouterDispatcher>,
        node_name: impl Into<String>,
        self_ilk: Option<String>,
    ) -> Self {
        let node_name = node_name.into();
        Self {
            router,
            identity: identity_of_node(&node_name),
            node_name,
            self_ilk,
            help_timeout: CAPABILITY_HELP_TIMEOUT,
            request_timeout: CAPABILITY_REQUEST_TIMEOUT,
            cache: Mutex::default(),
        }
    }

    /// The ED's capabilities now, and the identity region's sequence they were read at.
    fn doors(&self, ed: &str) -> std::result::Result<(u64, Vec<CapabilityDoor>), String> {
        let snapshot = (self.identity)()?;
        let tenant = own_tenant(&snapshot.ilks, self.self_ilk.as_deref());
        Ok((snapshot.seq, capabilities_of(&snapshot.ilks, ed, tenant)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HelpCache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every capability of `ed`, each with its help (its description, input schema and actions),
    /// or why its help could not be read.
    async fn listing(&self, ed: &str) -> std::result::Result<Vec<Value>, String> {
        let (seq, doors) = self.doors(ed)?;
        let (mut known, missing) = {
            let mut cache = self.lock();
            if cache.seq != Some(seq) || cache.ed != ed {
                *cache = HelpCache {
                    seq: Some(seq),
                    ed: ed.to_string(),
                    help: HashMap::new(),
                };
            }
            let known: HashMap<String, std::result::Result<Map<String, Value>, String>> = doors
                .iter()
                .filter_map(|door| {
                    cache
                        .help
                        .get(&door.node_name)
                        .map(|help| (door.node_name.clone(), Ok(help.clone())))
                })
                .collect();
            let missing = doors
                .iter()
                .filter(|door| !known.contains_key(&door.node_name))
                .map(|door| door.node_name.clone())
                .collect::<Vec<_>>();
            (known, missing)
        };
        let mut asks = JoinSet::new();
        for door in missing {
            let router = self.router.clone();
            let timeout = self.help_timeout;
            asks.spawn(async move {
                let help = ask_help(&router, &door, timeout).await;
                (door, help)
            });
        }
        let mut read = Vec::new();
        while let Some(asked) = asks.join_next().await {
            let Ok((door, help)) = asked else {
                continue;
            };
            if let Err(reason) = &help {
                tracing::warn!(
                    node_name = %self.node_name,
                    capability = %door,
                    reason = %reason,
                    "ask_capability: a capability's help could not be read; asked again next time"
                );
            }
            if let Ok(help) = &help {
                read.push((door.clone(), help.clone()));
            }
            known.insert(door, help);
        }
        {
            let mut cache = self.lock();
            if cache.seq == Some(seq) && cache.ed == ed {
                cache.help.extend(read);
            }
        }
        Ok(doors
            .iter()
            .map(|door| {
                let mut entry = Map::new();
                entry.insert("capability".to_string(), json!(door.node_name));
                match known.remove(&door.node_name) {
                    Some(Ok(help)) => entry.extend(help),
                    Some(Err(reason)) => {
                        entry.insert("help_error".to_string(), json!(reason));
                    }
                    None => {
                        entry.insert("help_error".to_string(), json!("not asked"));
                    }
                }
                Value::Object(entry)
            })
            .collect())
    }

    /// Sends `door` a `CAPABILITY_REQUEST` and waits for its answer, as the tool gives it.
    async fn request(&self, door: &str, payload: Value, request_id: &str) -> Value {
        let started = Instant::now();
        let answer = self
            .router
            .send_system_rpc(SystemRpcRequest {
                target: door,
                request_msg: MSG_CAPABILITY_REQUEST,
                response_msg: MSG_CAPABILITY_REQUEST_RESPONSE,
                payload,
                timeout: self.request_timeout,
            })
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match answer {
            Ok(msg) => {
                let answer = capability_answer(door, request_id, msg.payload);
                let field = |key: &str| answer.get(key).and_then(Value::as_str).unwrap_or("");
                let (status, error_code) = (field("status"), field("error_code"));
                tracing::info!(
                    node_name = %self.node_name,
                    capability = %door,
                    request_id = %request_id,
                    trace_id = %msg.routing.trace_id,
                    status = %status,
                    error_code = %error_code,
                    elapsed_ms,
                    "ask_capability: the capability answered"
                );
                answer
            }
            Err(err) => {
                tracing::warn!(
                    node_name = %self.node_name,
                    capability = %door,
                    request_id = %request_id,
                    error = %err,
                    elapsed_ms,
                    "ask_capability: the capability did not answer"
                );
                unavailable(door, request_id, &err, self.request_timeout)
            }
        }
    }
}

/// A door's help (`WF_HELP_RESPONSE`), as the model reads it: its workflow type, description,
/// input schema and, when it names them, its actions.
async fn ask_help(
    router: &RouterDispatcher,
    door: &str,
    timeout: Duration,
) -> std::result::Result<Map<String, Value>, String> {
    let answer = router
        .send_system_rpc(SystemRpcRequest {
            target: door,
            request_msg: MSG_WF_HELP,
            response_msg: MSG_WF_HELP_RESPONSE,
            payload: json!({}),
            timeout,
        })
        .await
        .map_err(|err| format!("it did not answer its help: {err}"))?;
    let payload = answer.payload;
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        let code = payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("not ok");
        let detail = payload.get("detail").and_then(Value::as_str).unwrap_or("");
        return Err(format!("its help answered {code}: {detail}"));
    }
    let mut help = Map::new();
    for key in ["workflow_type", "description", "input_schema", "actions"] {
        if let Some(value) = payload.get(key).filter(|value| !value.is_null()) {
            help.insert(key.to_string(), value.clone());
        }
    }
    Ok(help)
}

/// The tool's result for a door's `CAPABILITY_REQUEST_RESPONSE` payload: the payload as the door
/// answered it, naming the capability (stage 3 §6). A WF engine's own refusal (`{ok: false, error,
/// detail}`, before its definition runs) reads as an error of the same shape; an answer with no
/// status is `invalid_capability_answer`.
fn capability_answer(door: &str, request_id: &str, payload: Value) -> Value {
    match payload {
        Value::Object(mut answer)
            if answer
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| !status.trim().is_empty()) =>
        {
            answer.insert("capability".to_string(), json!(door));
            answer
                .entry("request_id".to_string())
                .or_insert_with(|| json!(request_id));
            Value::Object(answer)
        }
        Value::Object(answer) if answer.get("ok") == Some(&Value::Bool(false)) => json!({
            "status": "error",
            "request_id": request_id,
            "capability": door,
            "error_code": answer.get("error").and_then(Value::as_str).unwrap_or("error"),
            "message": answer.get("detail").and_then(Value::as_str).unwrap_or(""),
        }),
        _ => tool_error(
            INVALID_CAPABILITY_ANSWER,
            &format!("{door} answered without a status; its answer is not used"),
            false,
            Some((door, request_id)),
        ),
    }
}

/// The tool's error for a request that got no answer.
fn unavailable(door: &str, request_id: &str, err: &RpcError, timeout: Duration) -> Value {
    let (message, retryable) = match err {
        RpcError::Timeout { .. } => (
            format!(
                "{door} did not answer within {} s. It may still be working on the request: do \
                 not ask again now; tell the person you could not get the answer yet",
                timeout.as_secs()
            ),
            false,
        ),
        RpcError::NotTaken { retryable, .. } => {
            (format!("{door} did not take the request"), *retryable)
        }
        RpcError::Unreachable { .. } | RpcError::TtlExceeded { .. } | RpcError::Disconnected => {
            (format!("{door} cannot be reached now"), true)
        }
        other => (format!("{door} could not be asked: {other}"), false),
    };
    tool_error(
        CAPABILITY_UNAVAILABLE,
        &message,
        retryable,
        Some((door, request_id)),
    )
}

/// A failure of `ask_capability`, in the shape every tool's has, naming the capability and the
/// request when there is one.
fn tool_error(code: &str, message: &str, retryable: bool, request: Option<(&str, &str)>) -> Value {
    let mut error = json!({
        "status": "error",
        "error_code": code,
        "message": format!("{ASK_CAPABILITY_TOOL}: {message}"),
        "retryable": retryable
    });
    if let Some((door, request_id)) = request {
        error["capability"] = json!(door);
        error["request_id"] = json!(request_id);
    }
    error
}

/// `ask_capability` for one turn: the directory the node keeps, the ED the agent speaks as when the
/// turn starts (None while identity does not confirm it), and the turn's conversation.
pub struct AskCapabilityTool {
    directory: Arc<CapabilityDirectory>,
    ed: Option<String>,
    conversation: Option<CapabilityConversation>,
}

impl AskCapabilityTool {
    pub fn new(
        directory: Arc<CapabilityDirectory>,
        ed: Option<String>,
        conversation: Option<CapabilityConversation>,
    ) -> Self {
        Self {
            directory,
            ed: ed
                .map(|ed| ed.trim().to_string())
                .filter(|ed| !ed.is_empty()),
            conversation,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AskCapabilityArgs {
    #[serde(default)]
    capability: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

#[async_trait]
impl AgentTool for AskCapabilityTool {
    fn card() -> ToolCard {
        ToolCard {
            name: ASK_CAPABILITY_TOOL.to_string(),
            summary: "Ask one of your executive's capabilities for data or an action, and get its \
                answer."
                .to_string(),
            when_to_use: "When the person asks for something a system behind you holds or does \
                (an invoice, an order, a record): call it without `capability` to list your \
                capabilities, what each does and its input; then call it with one of them and \
                that input."
                .to_string(),
            limits: [
                "Only the capabilities of the executive you speak as, while identity confirms \
                 it.",
                "It waits up to 60 s. `pending` means the result comes later, as a new message \
                 in this conversation: tell the person you will let them know.",
                "The request carries this conversation by itself: `input` holds only what the \
                 capability's `input_schema` asks.",
            ]
            .map(String::from)
            .into(),
            present_when: "the agent's config names the person it speaks as (`speaks_as`), a \
                Digital Executive's conscious; it lists or asks a capability only while identity \
                confirms that person"
                .to_string(),
            input: json!({
                "type": "object",
                "properties": {
                    "capability": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The capability, by the name the list gives it: its \
                            door's node name, such as WF.billing.ed4qm7k2xa@worker1. Leave it out \
                            to list your capabilities."
                    },
                    "input": {
                        "type": "object",
                        "description": "What the capability takes, by its input_schema. Only \
                            with `capability`; {} when absent."
                    }
                },
                "additionalProperties": false
            }),
            output: json!({
                "description": "Without `capability`: `{status: \"ok\", capabilities: \
                    [{capability, description, input_schema, actions?}]}`. With one: its \
                    answer, `{status: \"done\" | \"pending\" | \"error\", request_id, \
                    capability, result?, error_code?, message?}`.",
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "status": { "const": "ok" },
                            "capabilities": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "capability": { "type": "string" },
                                        "workflow_type": { "type": "string" },
                                        "description": { "type": "string" },
                                        "input_schema": {
                                            "description": "What its `input` takes, as a JSON \
                                                schema."
                                        },
                                        "actions": {
                                            "description": "Its actions and what each does, \
                                                when it names them."
                                        },
                                        "help_error": {
                                            "type": "string",
                                            "description": "Why its help could not be read now."
                                        }
                                    },
                                    "required": ["capability"]
                                }
                            }
                        },
                        "required": ["status", "capabilities"]
                    },
                    {
                        "type": "object",
                        "properties": {
                            "status": { "enum": ["done", "pending", "error"] },
                            "request_id": { "type": "string" },
                            "capability": { "type": "string" },
                            "result": { "description": "What the capability answered." },
                            "error_code": { "type": "string" },
                            "message": { "type": "string" }
                        },
                        "required": ["status", "request_id", "capability"]
                    },
                    {
                        "type": "object",
                        "properties": {
                            "status": { "const": "error" },
                            "error_code": {
                                "enum": [
                                    NO_CAPABILITIES,
                                    UNKNOWN_CAPABILITY,
                                    NO_CONVERSATION,
                                    CAPABILITY_UNAVAILABLE,
                                    INVALID_CAPABILITY_ANSWER
                                ]
                            },
                            "message": { "type": "string" },
                            "retryable": { "type": "boolean" }
                        },
                        "required": ["status", "error_code", "message", "retryable"]
                    }
                ]
            }),
            errors: vec![
                ToolErrorCode::new(
                    NO_CAPABILITIES,
                    "You speak as no executive that identity confirms now, so you have no \
                     capabilities: answer without them.",
                ),
                ToolErrorCode::new(
                    UNKNOWN_CAPABILITY,
                    "No capability of yours has that name; the message names the ones you have.",
                ),
                ToolErrorCode::new(
                    NO_CONVERSATION,
                    "The current message names no thread or no node it came through, which a \
                     request carries: nothing was asked.",
                ),
                ToolErrorCode::new(
                    CAPABILITY_UNAVAILABLE,
                    "The capability did not answer: it cannot be reached, did not take the \
                     request, or took more than 60 s. A request that timed out may still run: \
                     do not ask again at once. `retryable` says whether asking again may help.",
                ),
                ToolErrorCode::new(
                    INVALID_CAPABILITY_ANSWER,
                    "The capability answered without a status: its answer is not used.",
                ),
                ToolErrorCode::new(
                    "INVALID_INPUT",
                    "The capability's own: `input` does not follow its input_schema. Fix it and \
                     ask again.",
                ),
                ToolErrorCode::new(
                    "UNAUTHORIZED",
                    "The capability's own: it does not take requests from you.",
                ),
            ],
        }
    }

    async fn call(&self, arguments: Value) -> Result<Value> {
        let args: AskCapabilityArgs = serde_json::from_value(arguments).map_err(|err| {
            AiSdkError::Protocol(format!("{ASK_CAPABILITY_TOOL}: invalid arguments: {err}"))
        })?;
        let Some(ed) = self.ed.as_deref() else {
            return Ok(tool_error(
                NO_CAPABILITIES,
                "this agent speaks as no executive that identity confirms, so it has no \
                 capabilities",
                false,
                None,
            ));
        };
        let directory = &self.directory;
        let Some(name) = trimmed(args.capability.as_deref()) else {
            if args.input.is_some() {
                return Err(AiSdkError::Protocol(format!(
                    "{ASK_CAPABILITY_TOOL}: `input` goes with `capability`; without it the tool \
                     lists the capabilities"
                )));
            }
            return Ok(match directory.listing(ed).await {
                Ok(capabilities) => json!({"status": "ok", "capabilities": capabilities}),
                Err(reason) => tool_error(CAPABILITY_UNAVAILABLE, &reason, true, None),
            });
        };
        let input = match args.input {
            None => json!({}),
            Some(input @ Value::Object(_)) => input,
            Some(_) => {
                return Err(AiSdkError::Protocol(format!(
                    "{ASK_CAPABILITY_TOOL}: `input` must be an object"
                )))
            }
        };
        let doors = match directory.doors(ed) {
            Ok((_, doors)) => doors,
            Err(reason) => return Ok(tool_error(CAPABILITY_UNAVAILABLE, &reason, true, None)),
        };
        let Some(door) = doors
            .iter()
            .find(|door| door.node_name.eq_ignore_ascii_case(&name))
        else {
            let names = doors
                .iter()
                .map(|door| door.node_name.as_str())
                .collect::<Vec<_>>();
            let have = if names.is_empty() {
                "you have none".to_string()
            } else {
                format!("yours: {}", names.join(", "))
            };
            return Ok(tool_error(
                UNKNOWN_CAPABILITY,
                &format!("no capability named {name:?}; {have}"),
                false,
                None,
            ));
        };
        let Some(conversation) = self.conversation.as_ref() else {
            return Ok(tool_error(
                NO_CONVERSATION,
                "the current message names no thread or no node it came through",
                false,
                None,
            ));
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        let payload = json!({
            "request_id": request_id,
            "ed_ilk": ed,
            "input": input,
            "conversation": conversation,
        });
        tracing::info!(
            node_name = %directory.node_name,
            capability = %door.node_name,
            request_id = %request_id,
            thread_id = %conversation.thread_id,
            io_node = %conversation.io_node,
            "ask_capability: asking a capability"
        );
        Ok(directory
            .request(&door.node_name, payload, &request_id)
            .await)
    }
}

#[cfg(test)]
mod tests {
    use fluxbee_sdk::protocol::{Meta, Routing};
    use fluxbee_sdk::{
        OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcherTestHarness,
    };

    use super::*;

    const ED: &str = "ilk:7a1f0c3e-1111-4c2b-9d3e-0123456789ab";
    const OTHER_ED: &str = "ilk:7a1f0c3e-2222-4c2b-9d3e-0123456789ab";
    const CONSCIOUS: &str = "ilk:7a1f0c3e-3333-4c2b-9d3e-0123456789ab";
    const ACME: &str = "tnt:00000000-0000-4000-8000-00000000000a";
    const OTHER_TENANT: &str = "tnt:00000000-0000-4000-8000-00000000000b";
    const DOOR: &str = "WF.billing.ed4qm7k2xa@worker1";
    const DOOR_FOR_ALL: &str = "WF.calendar.acme@worker1";

    fn ilk(
        ilk_id: &str,
        node: Option<&str>,
        tenant: &str,
        works_for: Option<&str>,
        work_role: Option<&str>,
    ) -> IdentityIlkOption {
        IdentityIlkOption {
            ilk_id: ilk_id.to_string(),
            tenant_id: tenant.to_string(),
            display_name: node.map(str::to_string),
            handler_node: node.map(str::to_string),
            registration_status: "complete".to_string(),
            ilk_type: "agent".to_string(),
            role_hash: None,
            skill_hashes: Vec::new(),
            handbook_hashes: Vec::new(),
            personality_hash: None,
            is_virtual: false,
            works_for: works_for.map(str::to_string),
            work_role: work_role.map(str::to_string),
            document_hash: None,
        }
    }

    /// The ED's conscious, its two capabilities (its own and its tenant's), and what is not one of
    /// them: another ED's capability, another tenant's capability for all, a connector, a
    /// capability with no node.
    fn identity(seq: u64) -> IdentityIlkSnapshot {
        IdentityIlkSnapshot {
            seq,
            ilks: vec![
                ilk(
                    CONSCIOUS,
                    Some("AI.ed4qm7k2xa@worker1"),
                    ACME,
                    Some(ED),
                    Some("conscious"),
                ),
                ilk(
                    "ilk:00000000-0000-4000-8000-000000000001",
                    Some(DOOR),
                    ACME,
                    Some(ED),
                    Some("capability"),
                ),
                ilk(
                    "ilk:00000000-0000-4000-8000-000000000002",
                    Some(DOOR_FOR_ALL),
                    ACME,
                    None,
                    Some("capability"),
                ),
                ilk(
                    "ilk:00000000-0000-4000-8000-000000000003",
                    Some("WF.billing.other@worker1"),
                    ACME,
                    Some(OTHER_ED),
                    Some("capability"),
                ),
                ilk(
                    "ilk:00000000-0000-4000-8000-000000000004",
                    Some("WF.calendar.beta@worker1"),
                    OTHER_TENANT,
                    None,
                    Some("capability"),
                ),
                ilk(
                    "ilk:00000000-0000-4000-8000-000000000005",
                    Some("IO.crm.ed4qm7k2xa@worker1"),
                    ACME,
                    Some(ED),
                    Some("connector"),
                ),
                ilk(
                    "ilk:00000000-0000-4000-8000-000000000006",
                    None,
                    ACME,
                    Some(ED),
                    Some("capability"),
                ),
            ],
        }
    }

    #[test]
    fn an_eds_capabilities_are_its_own_and_its_tenants_for_every_ed() {
        let snapshot = identity(1);
        let doors = capabilities_of(&snapshot.ilks, ED, Some(ACME));
        assert_eq!(
            doors,
            vec![
                CapabilityDoor {
                    node_name: DOOR.to_string(),
                    ilk_id: "ilk:00000000-0000-4000-8000-000000000001".to_string(),
                    for_every_ed: false,
                },
                CapabilityDoor {
                    node_name: DOOR_FOR_ALL.to_string(),
                    ilk_id: "ilk:00000000-0000-4000-8000-000000000002".to_string(),
                    for_every_ed: true,
                },
            ]
        );
        // Without the node's tenant, only the ED's own; another ED has its own.
        let names = |doors: Vec<CapabilityDoor>| {
            doors
                .into_iter()
                .map(|door| door.node_name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(capabilities_of(&snapshot.ilks, ED, None)), [DOOR]);
        assert_eq!(
            names(capabilities_of(&snapshot.ilks, OTHER_ED, Some(ACME))),
            ["WF.billing.other@worker1", DOOR_FOR_ALL]
        );
        assert_eq!(own_tenant(&snapshot.ilks, Some(CONSCIOUS)), Some(ACME));
        assert_eq!(
            own_tenant(&snapshot.ilks, Some(&CONSCIOUS.to_ascii_uppercase())),
            Some(ACME),
            "the spelling of the node's own ILK does not matter"
        );
        assert_eq!(own_tenant(&snapshot.ilks, None), None);
    }

    fn person_message() -> Message {
        Message {
            routing: Routing {
                src: "uuid-of-the-io".to_string(),
                src_l2_name: Some("IO.wapp.ed4qm7k2xa@worker1".to_string()),
                dst: Destination::Unicast("uuid-of-the-conscious".to_string()),
                ttl: 16,
                trace_id: "trace-person".to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                src_ilk: Some("ilk:00000000-0000-4000-8000-0000000000aa".to_string()),
                dst_ilk: Some(ED.to_string()),
                thread_id: Some("thread-ana".to_string()),
                context: Some(json!({
                    "io": {
                        "channel": "whatsapp",
                        "reply_target": {"kind": "wapp_post", "address": "5491100000000"}
                    }
                })),
                ..Meta::default()
            },
            payload: json!({"type": "text", "content": "how is invoice A-1032?"}),
        }
    }

    fn conversation() -> CapabilityConversation {
        CapabilityConversation {
            thread_id: "thread-ana".to_string(),
            person_ilk: Some("ilk:00000000-0000-4000-8000-0000000000aa".to_string()),
            io_node: "IO.wapp.ed4qm7k2xa@worker1".to_string(),
            reply_target: Some(json!({"kind": "wapp_post", "address": "5491100000000"})),
        }
    }

    fn result_message(sender: &str, result: Value) -> Message {
        Message {
            routing: Routing {
                src: "uuid-of-the-door".to_string(),
                src_l2_name: Some(sender.to_string()),
                dst: Destination::Unicast("uuid-of-the-conscious".to_string()),
                ttl: 16,
                trace_id: "trace-notify".to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                msg: Some("capability_result".to_string()),
                src_ilk: Some("ilk:00000000-0000-4000-8000-000000000001".to_string()),
                thread_id: Some("thread-ana".to_string()),
                context: Some(json!({ CAPABILITY_RESULT_KEY: result })),
                ..Meta::default()
            },
            payload: json!({"type": "text", "content": "Invoice A-1032 is paid."}),
        }
    }

    fn done_result() -> Value {
        json!({
            "request_id": "req-1",
            "capability": DOOR,
            "status": "done",
            "result": {"state": "paid", "date": "2026-11-12"},
            "conversation": conversation(),
        })
    }

    #[test]
    fn a_turns_conversation_is_the_messages_or_the_results_never_the_models() {
        assert_eq!(
            CapabilityConversation::of_turn(&person_message()),
            Some(conversation())
        );
        // Without a reply target or a person: null on the wire, as the contract says.
        let mut bare = person_message();
        bare.meta.context = None;
        bare.meta.src_ilk = None;
        let bare = CapabilityConversation::of_turn(&bare).expect("a conversation");
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({"thread_id": "thread-ana", "person_ilk": null,
                   "io_node": "IO.wapp.ed4qm7k2xa@worker1", "reply_target": null})
        );
        // No thread, or no node it came through: none.
        let mut threadless = person_message();
        threadless.meta.thread_id = Some(" ".to_string());
        assert_eq!(CapabilityConversation::of_turn(&threadless), None);
        let mut nodeless = person_message();
        nodeless.routing.src_l2_name = None;
        assert_eq!(CapabilityConversation::of_turn(&nodeless), None);
        // A result's turn is in the result's conversation, not the door's.
        assert_eq!(
            CapabilityConversation::of_turn(&result_message(DOOR, done_result())),
            Some(conversation())
        );
    }

    fn screen(msg: &mut Message, ed: Option<&str>) -> ResultScreen {
        screen_capability_result(msg, ed, Some(CONSCIOUS), || Ok(identity(7)))
    }

    #[test]
    fn a_result_is_taken_only_from_one_of_the_eds_capabilities() {
        // From the ED's own door, and from its tenant's door for every ED.
        for door in [DOOR, DOOR_FOR_ALL] {
            let mut msg = result_message(door, done_result());
            msg.meta.thread_id = Some("another-thread".to_string());
            assert_eq!(
                screen(&mut msg, Some(ED)),
                ResultScreen::Accepted {
                    capability: door.to_string(),
                    request_id: "req-1".to_string()
                }
            );
            // The sender is the capability, and the conversation's thread the turn's.
            let result = capability_result_of(&msg).expect("kept");
            assert_eq!(result.capability, door);
            assert_eq!(msg.meta.thread_id.as_deref(), Some("thread-ana"));
            assert_eq!(result_conversation(&msg), Some(conversation()));
        }

        // From any other sender, or with no ED confirmed: the field goes, and the message runs.
        for (sender, ed) in [
            ("WF.billing.other@worker1", Some(ED)),
            ("WF.calendar.beta@worker1", Some(ED)),
            ("IO.crm.ed4qm7k2xa@worker1", Some(ED)),
            ("AI.someone@worker1", Some(ED)),
            (DOOR, None),
            (DOOR, Some("  ")),
        ] {
            let mut msg = result_message(sender, done_result());
            assert!(
                matches!(screen(&mut msg, ed), ResultScreen::Ignored(_)),
                "{sender} {ed:?}"
            );
            assert!(msg.meta.context.as_ref().unwrap()[CAPABILITY_RESULT_KEY].is_null());
            assert_eq!(capability_result_of(&msg), None);
            assert_eq!(result_conversation(&msg), None);
        }
        let mut senderless = result_message(DOOR, done_result());
        senderless.routing.src_l2_name = None;
        assert!(matches!(
            screen(&mut senderless, Some(ED)),
            ResultScreen::Ignored(_)
        ));

        // From one of them, but not as the contract says, or unreadable identity: never run.
        let mut odd = done_result();
        odd["conversation"]["io_node"] = json!(DOOR);
        let mut no_thread = done_result();
        no_thread["conversation"]["thread_id"] = json!("");
        let mut no_request = done_result();
        no_request["request_id"] = json!(" ");
        for result in [odd, no_thread, no_request, json!("done")] {
            let mut msg = result_message(DOOR, result.clone());
            assert!(
                matches!(screen(&mut msg, Some(ED)), ResultScreen::Dropped(_)),
                "{result}"
            );
        }
        let mut unread = result_message(DOOR, done_result());
        assert!(matches!(
            screen_capability_result(&mut unread, Some(ED), Some(CONSCIOUS), || Err(
                "no region".to_string()
            )),
            ResultScreen::Dropped(reason) if reason.contains("no region")
        ));

        // A message without the field, or that is not a user message, is not screened.
        let mut plain = person_message();
        assert_eq!(screen(&mut plain, Some(ED)), ResultScreen::Absent);
        let mut control = result_message(DOOR, done_result());
        control.meta.msg_type = "system".to_string();
        assert_eq!(screen(&mut control, Some(ED)), ResultScreen::Absent);
        assert_eq!(capability_result_of(&control), None);
    }

    #[test]
    fn the_answer_to_a_result_goes_to_the_persons_io_never_to_the_door() {
        let msg = result_message(DOOR, done_result());
        let mut reply = crate::message::build_reply_message_runtime_src(
            &msg,
            json!({"type": "text", "content": "Your invoice is paid."}),
        );
        assert!(matches!(
            &reply.routing.dst,
            Destination::Unicast(dst) if dst == "uuid-of-the-door"
        ));
        address_to_conversation(&mut reply, &conversation());
        assert!(matches!(
            &reply.routing.dst,
            Destination::Unicast(dst) if dst == "IO.wapp.ed4qm7k2xa@worker1"
        ));
        assert_eq!(
            reply.meta.dst_ilk.as_deref(),
            Some("ilk:00000000-0000-4000-8000-0000000000aa")
        );
        assert_eq!(reply.meta.thread_id.as_deref(), Some("thread-ana"));
        assert_eq!(reply.meta.src_ilk, None, "the runtime stamps the ED");
        assert_eq!(
            reply.meta.context,
            Some(json!({"io": {"reply_target": {"kind": "wapp_post",
                                                "address": "5491100000000"}}}))
        );
        // No reply target: no io block of the door's either.
        let mut reply = crate::message::build_reply_message_runtime_src(&msg, json!({}));
        reply.meta.context = Some(json!({"io": {"reply_target": "the door's"}, "trace": []}));
        let mut no_target = conversation();
        no_target.reply_target = None;
        address_to_conversation(&mut reply, &no_target);
        assert_eq!(reply.meta.context, Some(json!({"trace": []})));
    }

    #[test]
    fn the_model_reads_the_result_ahead_of_the_message() {
        let mut msg = result_message(DOOR, done_result());
        assert_eq!(
            screen(&mut msg, Some(ED)),
            ResultScreen::Accepted {
                capability: DOOR.to_string(),
                request_id: "req-1".to_string()
            }
        );
        let block = capability_result_prompt_block(&msg).expect("a block");
        assert!(block.starts_with("Capability result:\n"), "{block}");
        assert!(block.contains(&format!(
            "Your capability {DOOR} answers your earlier request req-1"
        )));
        assert!(block.contains("Status: done"));
        let result_line = block
            .lines()
            .find(|line| line.starts_with("Result: {"))
            .expect("the result's line");
        assert!(result_line.contains(r#""state":"paid""#), "{block}");
        assert!(result_line.contains(r#""date":"2026-11-12""#), "{block}");
        assert!(!block.contains("Error:"));

        let mut failed = done_result();
        failed["status"] = json!("error");
        failed["result"] = Value::Null;
        failed["error_code"] = json!("CRM_DOWN");
        failed["message"] = json!("the CRM\ndid not answer");
        let block = capability_result_prompt_block(&result_message(DOOR, failed)).unwrap();
        assert!(block.contains("Status: error"));
        assert!(!block.contains("Result:"));
        assert!(block.contains("Error: CRM_DOWN: the CRM did not answer"));

        let mut large = done_result();
        large["result"] = json!("é".repeat(CAPABILITY_RESULT_PROMPT_MAX_BYTES));
        let block = capability_result_prompt_block(&result_message(DOOR, large)).unwrap();
        assert!(block.contains(&format!("[cut at {CAPABILITY_RESULT_PROMPT_MAX_BYTES} of")));

        assert_eq!(capability_result_prompt_block(&person_message()), None);
    }

    #[test]
    fn the_card_says_all_it_has_to() {
        let card = AskCapabilityTool::card();
        assert_eq!(card.validate(), Ok(()));
        assert_eq!(card.name, ASK_CAPABILITY_TOOL);
    }

    fn profile() -> OperationalRouteProfile {
        OperationalRouteProfile::builder()
            .command_channel("incoming")
            .post_pending_rule(RouteMatch::Any, RouteTarget::Command("incoming"))
            .build()
            .expect("profile")
    }

    /// A directory over a test router, whose identity is `identity`'s, at a sequence the test moves.
    fn directory(
        router: Arc<RouterDispatcher>,
        seq: Arc<std::sync::atomic::AtomicU64>,
    ) -> Arc<CapabilityDirectory> {
        let mut directory =
            CapabilityDirectory::new(router, "AI.ed4qm7k2xa@worker1", Some(CONSCIOUS.to_string()));
        directory.identity =
            Arc::new(move || Ok(identity(seq.load(std::sync::atomic::Ordering::SeqCst))));
        directory.help_timeout = Duration::from_secs(2);
        directory.request_timeout = Duration::from_millis(300);
        Arc::new(directory)
    }

    fn answer_to(request: &Message, msg: &str, payload: Value) -> Message {
        Message {
            routing: Routing {
                src: "uuid-of-the-door".to_string(),
                src_l2_name: request.routing.dst_name(),
                dst: Destination::Unicast(request.routing.src.clone()),
                ttl: 16,
                trace_id: request.routing.trace_id.clone(),
            },
            meta: Meta {
                msg_type: "system".to_string(),
                msg: Some(msg.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    trait DstName {
        fn dst_name(&self) -> Option<String>;
    }

    impl DstName for Routing {
        fn dst_name(&self) -> Option<String> {
            match &self.dst {
                Destination::Unicast(name) => Some(name.clone()),
                _ => None,
            }
        }
    }

    fn help_of(workflow_type: &str) -> Value {
        json!({
            "ok": true,
            "workflow_type": workflow_type,
            "description": format!("{workflow_type}: what it does"),
            "states": ["start", "done"],
            "terminal_states": ["done"],
            "input_schema": {"type": "object", "properties": {"number": {"type": "string"}}},
            "idle_timeout": "1h",
            "actions": null
        })
    }

    #[tokio::test]
    async fn without_a_confirmed_ed_there_are_no_capabilities() {
        let (router, mut harness) =
            RouterDispatcherTestHarness::new("AI.ed4qm7k2xa@worker1", profile());
        let seq = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let tool = AskCapabilityTool::new(directory(router, seq), None, Some(conversation()));
        for arguments in [json!({}), json!({"capability": DOOR, "input": {}})] {
            let out = AgentTool::call(&tool, arguments).await.expect("a result");
            assert_eq!(out["status"], json!("error"));
            assert_eq!(out["error_code"], json!(NO_CAPABILITIES));
        }
        assert!(harness
            .next_outgoing_within(Duration::from_millis(50))
            .await
            .is_none());
    }

    /// The list reads each door's help once while the identity region stays, asks again a help that
    /// failed, and asks every one again once the region moves.
    #[tokio::test]
    async fn the_list_keeps_each_help_until_identity_moves() {
        let (router, mut harness) =
            RouterDispatcherTestHarness::new("AI.ed4qm7k2xa@worker1", profile());
        let seq = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let directory = directory(router, seq.clone());
        let tool = Arc::new(AskCapabilityTool::new(
            directory.clone(),
            Some(ED.to_string()),
            Some(conversation()),
        ));

        // The first list asks both doors; the tenant's one refuses.
        let call = {
            let tool = tool.clone();
            tokio::spawn(async move { AgentTool::call(tool.as_ref(), json!({})).await })
        };
        for _ in 0..2 {
            let ask = harness.next_outgoing().await.expect("a help request");
            assert_eq!(ask.meta.msg_type, "system");
            assert_eq!(ask.meta.msg.as_deref(), Some(MSG_WF_HELP));
            let door = ask.routing.dst_name().expect("a door");
            let payload = if door == DOOR {
                help_of("billing")
            } else {
                json!({"ok": false, "error": "UNAUTHORIZED", "detail": "not yours"})
            };
            harness
                .inject(answer_to(&ask, MSG_WF_HELP_RESPONSE, payload))
                .await
                .unwrap();
        }
        let listed = call.await.unwrap().expect("a list");
        assert_eq!(
            listed,
            json!({"status": "ok", "capabilities": [
                {"capability": DOOR, "workflow_type": "billing",
                 "description": "billing: what it does",
                 "input_schema": {"type": "object", "properties": {"number": {"type": "string"}}}},
                {"capability": DOOR_FOR_ALL,
                 "help_error": "its help answered UNAUTHORIZED: not yours"}
            ]})
        );

        // The same region: the help kept, the one that failed asked again.
        let call = {
            let tool = tool.clone();
            tokio::spawn(async move { AgentTool::call(tool.as_ref(), json!({})).await })
        };
        let ask = harness.next_outgoing().await.expect("the failed one again");
        assert_eq!(ask.routing.dst_name().as_deref(), Some(DOOR_FOR_ALL));
        harness
            .inject(answer_to(&ask, MSG_WF_HELP_RESPONSE, help_of("calendar")))
            .await
            .unwrap();
        let listed = call.await.unwrap().expect("a list");
        assert_eq!(
            listed["capabilities"][1]["workflow_type"],
            json!("calendar")
        );

        // Both kept now: nothing is asked.
        let listed = AgentTool::call(tool.as_ref(), json!({}))
            .await
            .expect("a list");
        assert_eq!(listed["capabilities"][0]["workflow_type"], json!("billing"));
        assert!(harness
            .next_outgoing_within(Duration::from_millis(50))
            .await
            .is_none());

        // The region moves: both are asked again.
        seq.store(2, std::sync::atomic::Ordering::SeqCst);
        let call = {
            let tool = tool.clone();
            tokio::spawn(async move { AgentTool::call(tool.as_ref(), json!({})).await })
        };
        for _ in 0..2 {
            let ask = harness.next_outgoing().await.expect("asked again");
            harness
                .inject(answer_to(&ask, MSG_WF_HELP_RESPONSE, help_of("again")))
                .await
                .unwrap();
        }
        let listed = call.await.unwrap().expect("a list");
        assert_eq!(listed["capabilities"][0]["workflow_type"], json!("again"));
    }

    /// The request carries the ED, the model's input and the turn's conversation, and the tool's
    /// result is the door's answer naming the capability.
    #[tokio::test]
    async fn a_request_carries_the_turns_conversation_and_returns_the_doors_answer() {
        let (router, mut harness) =
            RouterDispatcherTestHarness::new("AI.ed4qm7k2xa@worker1", profile());
        let seq = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let tool = Arc::new(AskCapabilityTool::new(
            directory(router, seq),
            Some(ED.to_string()),
            Some(conversation()),
        ));
        for (answer, expected) in [
            (
                json!({"status": "done", "result": {"state": "paid"}}),
                json!({"status": "done", "result": {"state": "paid"}}),
            ),
            (json!({"status": "pending"}), json!({"status": "pending"})),
            (
                json!({"status": "error", "error_code": "INVALID_INPUT", "message": "number"}),
                json!({"status": "error", "error_code": "INVALID_INPUT", "message": "number"}),
            ),
            // A WF engine's own refusal, before its definition runs.
            (
                json!({"ok": false, "error": "UNAUTHORIZED", "detail": "not a conscious"}),
                json!({"status": "error", "error_code": "UNAUTHORIZED",
                       "message": "not a conscious"}),
            ),
        ] {
            let call = {
                let tool = tool.clone();
                tokio::spawn(async move {
                    AgentTool::call(
                        tool.as_ref(),
                        json!({"capability": DOOR.to_ascii_lowercase(),
                               "input": {"number": "A-1032"}}),
                    )
                    .await
                })
            };
            let request = harness.next_outgoing().await.expect("a request");
            assert_eq!(request.meta.msg_type, "system");
            assert_eq!(request.meta.msg.as_deref(), Some(MSG_CAPABILITY_REQUEST));
            assert_eq!(request.routing.dst_name().as_deref(), Some(DOOR));
            let payload = &request.payload;
            let request_id = payload["request_id"].as_str().expect("an id").to_string();
            assert!(uuid::Uuid::parse_str(&request_id).is_ok());
            assert_eq!(payload["ed_ilk"], json!(ED));
            assert_eq!(payload["input"], json!({"number": "A-1032"}));
            assert_eq!(
                payload["conversation"],
                serde_json::to_value(conversation()).unwrap()
            );
            harness
                .inject(answer_to(&request, MSG_CAPABILITY_REQUEST_RESPONSE, {
                    let mut answer = answer.clone();
                    if answer.get("status").is_some() {
                        answer["request_id"] = json!(request_id);
                    }
                    answer
                }))
                .await
                .unwrap();
            let mut expected = expected;
            expected["request_id"] = json!(request_id);
            expected["capability"] = json!(DOOR);
            assert_eq!(call.await.unwrap().expect("a result"), expected);
        }

        // No answer within the bound: the tool's error, which says not to ask again at once.
        let out = AgentTool::call(tool.as_ref(), json!({"capability": DOOR}))
            .await
            .expect("a result");
        let request = harness.next_outgoing().await.expect("a request");
        assert_eq!(request.payload["input"], json!({}));
        assert_eq!(out["status"], json!("error"));
        assert_eq!(out["error_code"], json!(CAPABILITY_UNAVAILABLE));
        assert_eq!(out["retryable"], json!(false));
        assert_eq!(out["capability"], json!(DOOR));
    }

    #[tokio::test]
    async fn a_request_names_a_capability_of_the_ed_and_goes_with_a_conversation() {
        let (router, mut harness) =
            RouterDispatcherTestHarness::new("AI.ed4qm7k2xa@worker1", profile());
        let seq = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let directory = directory(router, seq);
        let tool = AskCapabilityTool::new(
            directory.clone(),
            Some(ED.to_string()),
            Some(conversation()),
        );
        for name in ["WF.billing.other@worker1", "billing"] {
            let out = AgentTool::call(&tool, json!({"capability": name, "input": {}}))
                .await
                .expect("a result");
            assert_eq!(out["error_code"], json!(UNKNOWN_CAPABILITY), "{name}");
            let message = out["message"].as_str().unwrap();
            assert!(
                message.contains(DOOR) && message.contains(DOOR_FOR_ALL),
                "{message}"
            );
        }
        let tool = AskCapabilityTool::new(directory, Some(ED.to_string()), None);
        let out = AgentTool::call(&tool, json!({"capability": DOOR, "input": {}}))
            .await
            .expect("a result");
        assert_eq!(out["error_code"], json!(NO_CONVERSATION));
        // The model's arguments: an object input, and input only with a capability.
        for arguments in [
            json!({"capability": DOOR, "input": "A-1032"}),
            json!({"input": {"number": "A-1032"}}),
            json!({"capability": DOOR, "ed_ilk": OTHER_ED}),
        ] {
            assert!(
                AgentTool::call(&tool, arguments.clone()).await.is_err(),
                "{arguments}"
            );
        }
        assert!(harness
            .next_outgoing_within(Duration::from_millis(50))
            .await
            .is_none());
    }
}
