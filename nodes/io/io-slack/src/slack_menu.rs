//! The WF's block on an outbound post, and the menu for people (docs/wf-orchestration-v1.md
//! §5.4.3 and §5.4.4, built as §5.4.10 piece 4).
//!
//! - **The facts:** a message's `meta.context.wf.vars`, when it is a non-empty object, is a context
//!   line under its text: `key: value` pairs, escaped, each value cut at 200 characters, 10 keys at
//!   most. Facts alone make no post.
//! - **The menu**, `meta.context.wf.menu`, is the same for an agent and a person: an agent answers
//!   it with an object, and here it is buttons. The post gets an actions block, one button per
//!   option, labeled with its name (`_` as a space, the first letter capitalized), and the menu's
//!   `help`, when it has one, is a context line above them. The post is recorded as its thread id's
//!   open menu in the channel (thread_records), and a new one closes the older. Closing a menu, by
//!   a new one or by the mark `end`, updates its post: the same blocks without the buttons, and a
//!   context line "Closed.".
//! - **A click** comes as a Socket Mode `interactive` envelope (`block_actions`), acked at once. An
//!   option without params sends the click message at once. One with params opens a modal, with one
//!   required input per param, which Slack checks: a string is multiline text, an integer or a
//!   number a number input, a boolean a yes/no select, and an enum a select of its values. The
//!   submission (`view_submission`) is acked at once, checked again against the types, and dropped
//!   with a log line when it does not match.
//! - **The click message** is a relayed Slack message from the person who clicked, resolved or
//!   provisioned as for a message, at the post's place and on the menu's thread id, with
//!   `meta.context.evaluation` = `{"option", "params"}`: the shape of an agent's answer, so the WF's
//!   guards read both. Its text is the option's name, then one line per param, `name: value`.
//! - A click on a post with no open menu gets "This is no longer open.", which only the clicker
//!   sees, and nothing goes.
//!
//! The words posted are a person's or an agent's: escaped, and `verbatim`, so Slack turns none of
//! them into a mention, a channel or a link. A label, a hint or a title is plain text, which Slack
//! shows as written. The node's Slack app needs Interactivity on; over Socket Mode it needs no
//! Request URL.

use crate::not_taken::RelayedMessages;
use crate::thread_records::{OpenMenu, SlackThreadRecords, Subject};
use crate::{Config, SlackBindingConfig, SlackClients};
use fluxbee_sdk::{NodeSender, RouterDispatcher};
use io_common::identity::{IdentityProvisioner, IdentityResolver};
use io_common::inbound::{InboundOutcome, InboundProcessor};
use io_common::io_context::{slack_inbound_io_context, IoContext};
use io_common::io_control_plane::IoControlPlaneState;
use io_common::provision::IdentityProvisionConfig;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// The block id of a menu's buttons, which a click names.
const MENU_BLOCK_ID: &str = "wf_menu";
/// The callback id of an option's modal, which its submission names.
const MENU_VIEW_CALLBACK_ID: &str = "wf_menu_params";

/// wfcel's limits on a menu (docs/wf-orchestration-v1.md §5.3.6), checked again here, as the agent
/// does, so what Slack shows stays within its own limits.
const MENU_HELP_MAX_CHARS: usize = 600;
const MENU_DESCRIPTION_MAX_CHARS: usize = 200;
const MENU_OPTIONS_MAX: usize = 8;
const MENU_PARAMS_MAX: usize = 8;
const MENU_ENUM_MAX: usize = 20;

/// Slack's limits: a section's text, a message's blocks, a modal's title and a select option's text.
const SECTION_TEXT_MAX_CHARS: usize = 3_000;
const MESSAGE_BLOCKS_MAX: usize = 50;
const MODAL_TITLE_MAX_CHARS: usize = 24;
const SELECT_OPTION_MAX_CHARS: usize = 75;
/// The blocks a post keeps for what goes under its text: the facts, the help, the buttons, and
/// "Closed." once the menu closes.
const BLOCKS_UNDER_TEXT: usize = 4;

/// The facts line: its keys at most, and the characters of each value.
const FACTS_MAX_KEYS: usize = 10;
const FACT_VALUE_MAX_CHARS: usize = 200;

/// What a click on a post with no open menu gets.
const NO_LONGER_OPEN: &str = "This is no longer open.";
/// The line a closed menu's post gets in place of its buttons.
const CLOSED: &str = "Closed.";

/// A WF's menu, as a person sees it: its help, and its options in its order, each with its params
/// in their names' order.
#[derive(Debug, Clone, PartialEq)]
struct Menu {
    help: Option<String>,
    options: Vec<MenuOption>,
}

/// One option of a menu.
#[derive(Debug, Clone, PartialEq)]
struct MenuOption {
    name: String,
    description: String,
    params: Vec<MenuParam>,
}

/// One param of a menu's option.
#[derive(Debug, Clone, PartialEq)]
struct MenuParam {
    name: String,
    param_type: MenuParamType,
    description: Option<String>,
    /// The values a string param may take: its `enum`.
    values: Option<Vec<String>>,
}

/// The type of a menu's param.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuParamType {
    String,
    Boolean,
    Integer,
    Number,
}

impl MenuParamType {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "string" => Some(Self::String),
            "boolean" => Some(Self::Boolean),
            "integer" => Some(Self::Integer),
            "number" => Some(Self::Number),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
        }
    }
}

impl Menu {
    /// The menu a WF's block carries, checked with wfcel's limits: an optional `help` of 1 to 600
    /// characters, and 1 to 8 options. An option has a unique name, a description of 1 to 200
    /// characters, and up to 8 params. A param is a `string`, `boolean`, `integer` or `number`, with
    /// an optional description of up to 200 characters and, for a string, an optional `enum` of 1
    /// to 20 values, each not empty and of its own. A name matches `^[a-z][a-z0-9_]{0,31}$`. A
    /// field it does not know is left out. An invalid menu names the field.
    fn from_value(menu: &Value) -> Result<Self, String> {
        let menu = menu.as_object().ok_or("menu: must be an object")?;
        let help = optional_text(menu.get("help"), "menu.help", MENU_HELP_MAX_CHARS)?;
        let options = menu
            .get("options")
            .and_then(Value::as_array)
            .ok_or("menu.options: must be a list")?;
        if options.is_empty() || options.len() > MENU_OPTIONS_MAX {
            return Err(format!(
                "menu.options: must hold 1 to {MENU_OPTIONS_MAX} options, not {}",
                options.len()
            ));
        }
        let mut parsed: Vec<MenuOption> = Vec::with_capacity(options.len());
        for (index, option) in options.iter().enumerate() {
            let option = MenuOption::from_value(option, &format!("menu.options[{index}]"))?;
            if parsed.iter().any(|earlier| earlier.name == option.name) {
                return Err(format!(
                    "menu.options[{index}].name: {} is not unique in its menu",
                    option.name
                ));
            }
            parsed.push(option);
        }
        Ok(Self {
            help,
            options: parsed,
        })
    }

    fn option(&self, name: &str) -> Option<&MenuOption> {
        self.options.iter().find(|option| option.name == name)
    }
}

impl MenuOption {
    fn from_value(option: &Value, path: &str) -> Result<Self, String> {
        let option = option
            .as_object()
            .ok_or_else(|| format!("{path}: must be an object"))?;
        let name = menu_name(
            option
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &format!("{path}.name"),
        )?;
        let description = optional_text(
            option.get("description"),
            &format!("{path}.description"),
            MENU_DESCRIPTION_MAX_CHARS,
        )?
        .ok_or_else(|| format!("{path}.description: is required"))?;
        let params = match option.get("params") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Object(params)) if params.len() <= MENU_PARAMS_MAX => params
                .iter()
                .map(|(name, param)| MenuParam::from_value(name, param, &format!("{path}.params")))
                .collect::<Result<Vec<_>, _>>()?,
            Some(Value::Object(_)) => {
                return Err(format!(
                    "{path}.params: must hold {MENU_PARAMS_MAX} params at most"
                ))
            }
            Some(_) => return Err(format!("{path}.params: must be an object")),
        };
        Ok(Self {
            name,
            description,
            params,
        })
    }
}

impl MenuParam {
    fn from_value(name: &str, param: &Value, path: &str) -> Result<Self, String> {
        let path = format!("{path}.{name}");
        let name = menu_name(name, &path)?;
        let param = param
            .as_object()
            .ok_or_else(|| format!("{path}: must be an object"))?;
        let param_type = param
            .get("type")
            .and_then(Value::as_str)
            .and_then(MenuParamType::from_name)
            .ok_or_else(|| format!("{path}.type: must be string, boolean, integer or number"))?;
        let description = optional_text(
            param.get("description"),
            &format!("{path}.description"),
            MENU_DESCRIPTION_MAX_CHARS,
        )?;
        let values = match param.get("enum") {
            None | Some(Value::Null) => None,
            Some(_) if param_type != MenuParamType::String => {
                return Err(format!("{path}.enum: is only for a param of type string"))
            }
            Some(Value::Array(values)) if !values.is_empty() && values.len() <= MENU_ENUM_MAX => {
                let mut taken: Vec<String> = Vec::with_capacity(values.len());
                for value in values {
                    let value = value
                        .as_str()
                        .filter(|value| !value.trim().is_empty())
                        .ok_or_else(|| format!("{path}.enum: each value is a text, not empty"))?;
                    if taken.iter().any(|earlier| earlier == value) {
                        return Err(format!("{path}.enum: {value} is not unique"));
                    }
                    taken.push(value.to_string());
                }
                Some(taken)
            }
            Some(_) => {
                return Err(format!(
                    "{path}.enum: must hold 1 to {MENU_ENUM_MAX} values"
                ))
            }
        };
        Ok(Self {
            name,
            param_type,
            description,
            values,
        })
    }

    /// The param's value in a modal's submission, `state` (its input's entry in
    /// `view.state.values`), as its type says: a string, a JSON integer, a number, a boolean, or one
    /// of its `enum`'s values. Anything else is `type_mismatch:<p>:<type>`.
    fn submitted(&self, state: &Value) -> Result<Value, String> {
        let mismatch = || format!("type_mismatch:{}:{}", self.name, self.param_type.as_str());
        let typed = state.get("value").and_then(Value::as_str);
        let selected = state
            .get("selected_option")
            .and_then(|option| option.get("value"))
            .and_then(Value::as_str);
        match self.param_type {
            MenuParamType::String => match &self.values {
                Some(values) => selected
                    .and_then(|index| index.parse::<usize>().ok())
                    .and_then(|index| values.get(index))
                    .map(|value| Value::String(value.clone())),
                None => typed.map(|value| Value::String(value.to_string())),
            },
            MenuParamType::Integer => typed
                .and_then(|value| value.trim().parse::<i64>().ok())
                .map(Value::from),
            MenuParamType::Number => typed
                .and_then(|value| value.trim().parse::<f64>().ok())
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number),
            MenuParamType::Boolean => match selected {
                Some("true") => Some(Value::Bool(true)),
                Some("false") => Some(Value::Bool(false)),
                _ => None,
            },
        }
        .ok_or_else(mismatch)
    }
}

/// An option's name, or a param's: `^[a-z][a-z0-9_]{0,31}$`.
fn menu_name(name: &str, path: &str) -> Result<String, String> {
    let mut chars = name.chars();
    let valid = name.len() <= 32
        && chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
    if valid {
        Ok(name.to_string())
    } else {
        Err(format!(
            "{path}: must be a lowercase name of 32 characters at most"
        ))
    }
}

/// An optional text of a menu: absent or `null`, or 1 to `max` characters, never blank.
fn optional_text(text: Option<&Value>, path: &str, max: usize) -> Result<Option<String>, String> {
    match text {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() && text.chars().count() <= max => {
            Ok(Some(text.clone()))
        }
        Some(_) => Err(format!(
            "{path}: must be a text of 1 to {max} characters, not blank"
        )),
    }
}

/// What the WF's block, `meta.context.wf`, asks of an outbound message's post: its mark, the facts
/// under its text, and a menu for people.
pub(crate) struct WfPost {
    /// The message's mark (thread_records).
    pub(crate) subject: Subject,
    /// The facts line, a context block.
    facts: Option<Value>,
    menu: Option<PostMenu>,
}

/// A menu a post shows: as the WF sent it, as read, and the thread id its clicks go on.
struct PostMenu {
    raw: Value,
    menu: Menu,
    thread_id: String,
}

impl WfPost {
    /// The WF's block of a message's `meta.context`, for a message on `thread_id`. Its menu shows
    /// no buttons, with a log line, when it does not read; when the message has no thread id for a
    /// click to go on; when the records are closed, since no click could find the menu; and on a
    /// message that ends its subject, which closes its menu.
    pub(crate) fn read(
        context: Option<&Value>,
        thread_id: Option<&str>,
        records_open: bool,
    ) -> Self {
        let subject = Subject::of_context(context);
        let block = context.and_then(|context| context.get("wf"));
        let facts = block
            .and_then(|block| block.get("vars"))
            .and_then(facts_line);
        let menu = block
            .and_then(|block| block.get("menu"))
            .filter(|menu| !menu.is_null())
            .and_then(|raw| {
                let menu = match Menu::from_value(raw) {
                    Ok(menu) => menu,
                    Err(detail) => {
                        tracing::warn!(detail = %detail, "the WF's menu does not read; the post shows no buttons");
                        return None;
                    }
                };
                let thread_id = thread_id.map(str::trim).filter(|id| !id.is_empty());
                let shown_without = match (thread_id, records_open, subject) {
                    (None, _, _) => "the message has no thread id for a click to go on",
                    (_, false, _) => "the thread records are closed, so no click could find it",
                    (_, _, Subject::End) => "the message ends its subject, which closes its menu",
                    (Some(thread_id), true, _) => {
                        return Some(PostMenu {
                            raw: raw.clone(),
                            menu,
                            thread_id: thread_id.to_string(),
                        })
                    }
                };
                tracing::warn!(reason = shown_without, "the WF's menu: the post shows no buttons");
                None
            });
        Self {
            subject,
            facts,
            menu,
        }
    }

    /// Whether the message ends its subject with nothing to post, `payload` having no text, no
    /// `content_ref`, no files and no blocks: it only closes the subject's menu, and posts nothing
    /// (docs/wf-orchestration-v1.md §5.4.2), not even the fallback a message with nothing gets.
    pub(crate) fn only_closes(&self, payload: &Value) -> bool {
        let empty = |key: &str| match payload.get(key) {
            None | Some(Value::Null) => true,
            Some(Value::String(text)) => text.trim().is_empty(),
            Some(Value::Array(items)) => items.is_empty(),
            Some(Value::Object(fields)) => fields.is_empty(),
            Some(_) => false,
        };
        let nothing = payload.is_null()
            || (payload.is_object()
                && ["content", "content_ref", "attachments", "blocks"]
                    .into_iter()
                    .all(empty));
        self.subject == Subject::End && nothing
    }

    /// The blocks of the post of a message whose text is `text`, with `own` its payload's `blocks`.
    /// Without facts or a menu, the payload's blocks as they are, as before the WF's block. Else
    /// its own blocks, or its text as sections (escaped); the facts line under them; and the menu's
    /// help and buttons last. Facts alone make no post: with no text, no blocks of its own and no
    /// menu, the payload's blocks as they are.
    pub(crate) fn blocks(&self, text: &str, own: Option<&Value>) -> Option<Value> {
        if self.facts.is_none() && self.menu.is_none() {
            return own.cloned();
        }
        let mut blocks = match own.and_then(Value::as_array).filter(|own| !own.is_empty()) {
            Some(own) => own.clone(),
            None => text_sections(text),
        };
        if blocks.is_empty() && self.menu.is_none() {
            return own.cloned();
        }
        blocks.extend(self.facts.clone());
        if let Some(menu) = &self.menu {
            blocks.extend(menu.menu.help.as_deref().map(context_line));
            blocks.push(menu_buttons(&menu.menu));
        }
        Some(Value::Array(blocks))
    }

    /// The post of a message went out, at `ts` in `channel_id`, in the Slack thread `thread_ts`
    /// (`None` at the top level), with `blocks`. When it shows a menu, the menu is recorded as its
    /// thread id's open menu in the channel, and the older one is closed. When recording fails, the
    /// post keeps buttons that no click finds: each gets "This is no longer open.".
    pub(crate) async fn menu_posted(
        &self,
        slack: &SlackClients,
        records: Option<&SlackThreadRecords>,
        channel_id: &str,
        ts: &str,
        thread_ts: Option<&str>,
        blocks: &Value,
    ) {
        let (Some(menu), Some(records)) = (&self.menu, records) else {
            return;
        };
        let open = OpenMenu {
            channel_id: channel_id.to_string(),
            ts: ts.to_string(),
            thread_ts: thread_ts.unwrap_or_default().to_string(),
            thread_id: menu.thread_id.clone(),
            menu: menu.raw.clone(),
            blocks: blocks.clone(),
        };
        match records.open_menu(&open, crate::now_epoch_ms()) {
            Ok(older) => {
                tracing::debug!(
                    channel_id = %channel_id,
                    ts = %ts,
                    thread_id = %menu.thread_id,
                    "a menu is open on its post"
                );
                if let Some(older) = older {
                    close_menu_post(slack, &older, "a new menu on its thread id").await;
                }
            }
            Err(err) => tracing::warn!(
                channel_id = %channel_id,
                ts = %ts,
                thread_id = %menu.thread_id,
                error = %err,
                "slack thread records: recording an open menu failed; a click on its post gets \
                 \"This is no longer open.\""
            ),
        }
    }
}

/// The mark `end` ends the subject of `thread_id` in `channel_id`: its open menu, if any, is closed.
pub(crate) async fn close_subject_menu(
    slack: &SlackClients,
    records: Option<&SlackThreadRecords>,
    thread_id: Option<&str>,
    channel_id: &str,
) {
    let thread_id = thread_id.map(str::trim).filter(|id| !id.is_empty());
    let (Some(records), Some(thread_id)) = (records, thread_id) else {
        return;
    };
    match records.take_menu_of_thread(thread_id, channel_id) {
        Ok(Some(menu)) => close_menu_post(slack, &menu, "its subject ended").await,
        Ok(None) => tracing::debug!(
            thread_id = %thread_id,
            "the subject ends with no open menu on its thread id"
        ),
        Err(err) => tracing::warn!(
            thread_id = %thread_id,
            error = %err,
            "slack thread records: closing the subject's menu failed"
        ),
    }
}

/// Updates the post of a menu the records no longer hold: its buttons go, and "Closed." takes
/// their place. When Slack refuses it, the post keeps buttons that no click finds.
async fn close_menu_post(slack: &SlackClients, menu: &OpenMenu, why: &str) {
    match slack
        .update_message(&menu.channel_id, &menu.ts, closed_blocks(&menu.blocks))
        .await
    {
        Ok(()) => tracing::info!(
            channel_id = %menu.channel_id,
            ts = %menu.ts,
            thread_id = %menu.thread_id,
            why,
            "a menu is closed"
        ),
        Err(err) => tracing::warn!(
            channel_id = %menu.channel_id,
            ts = %menu.ts,
            thread_id = %menu.thread_id,
            why,
            error = %err,
            "closing a menu's post failed; its buttons stay, and a click on them gets \"This is \
             no longer open.\""
        ),
    }
}

/// A `mrkdwn` text of a person's or an agent's words: escaped (`crate::escape_slack_text`), and
/// `verbatim`, so Slack turns no `@channel`, `#channel` or URL in them into a mention or a link.
fn words(text: &str) -> Value {
    json!({"type": "mrkdwn", "text": crate::escape_slack_text(text), "verbatim": true})
}

/// A `plain_text` text: Slack shows it as written, with no markup.
fn plain(text: &str) -> Value {
    json!({"type": "plain_text", "text": text})
}

/// A context line of words.
fn context_line(text: &str) -> Value {
    json!({"type": "context", "elements": [words(text)]})
}

/// A text as section blocks, each within Slack's 3,000 characters once escaped and never blank, as
/// many as a message leaves room for under the text; a blank text has none.
fn text_sections(text: &str) -> Vec<Value> {
    let mut sections = Vec::new();
    let mut push = |chunk: &str| {
        if !chunk.trim().is_empty() {
            sections.push(json!({"type": "section", "text": words(chunk)}));
        }
    };
    let mut chunk = String::new();
    let mut chunk_chars = 0;
    for ch in text.chars() {
        let escaped_chars = match ch {
            '&' => 5,
            '<' | '>' => 4,
            _ => 1,
        };
        if chunk_chars + escaped_chars > SECTION_TEXT_MAX_CHARS {
            push(&chunk);
            chunk.clear();
            chunk_chars = 0;
        }
        chunk.push(ch);
        chunk_chars += escaped_chars;
    }
    push(&chunk);
    let room = MESSAGE_BLOCKS_MAX - BLOCKS_UNDER_TEXT;
    if sections.len() > room {
        tracing::warn!(
            sections = sections.len(),
            room,
            "an outbound text is longer than a post's blocks hold; its end is left out of them"
        );
        sections.truncate(room);
    }
    sections
}

/// The facts line of a WF's `vars`, when they are a non-empty object: one `key: value` element per
/// key, 10 keys at most, each value on one line and cut at 200 characters.
fn facts_line(vars: &Value) -> Option<Value> {
    let vars = vars.as_object().filter(|vars| !vars.is_empty())?;
    let elements = vars
        .iter()
        .take(FACTS_MAX_KEYS)
        .map(|(key, value)| {
            let value = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            let one_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
            words(&format!(
                "{key}: {}",
                crate::truncate(&one_line, FACT_VALUE_MAX_CHARS)
            ))
        })
        .collect::<Vec<_>>();
    Some(json!({"type": "context", "elements": elements}))
}

/// A name as a person reads it: `_` as a space, the first letter capitalized.
fn label(name: &str) -> String {
    let spaced = name.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A menu's buttons: one per option, labeled with its name, which the click names.
fn menu_buttons(menu: &Menu) -> Value {
    let buttons = menu
        .options
        .iter()
        .map(|option| {
            json!({
                "type": "button",
                "action_id": option.name,
                "value": option.name,
                "text": plain(&label(&option.name)),
            })
        })
        .collect::<Vec<_>>();
    json!({"type": "actions", "block_id": MENU_BLOCK_ID, "elements": buttons})
}

/// A closed menu's post: its blocks without the buttons, and "Closed." in their place.
fn closed_blocks(blocks: &Value) -> Value {
    let mut kept = blocks
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| {
                    block.get("block_id").and_then(Value::as_str) != Some(MENU_BLOCK_ID)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    kept.push(json!({"type": "context", "elements": [plain(CLOSED)]}));
    Value::Array(kept)
}

/// Which post an option's modal was opened from, and for which option: the modal's
/// `private_metadata`, which its submission brings back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ModalOrigin {
    channel: String,
    ts: String,
    /// The Slack thread the post is in, absent at the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thread_ts: Option<String>,
    option: String,
}

impl ModalOrigin {
    fn of(menu: &OpenMenu, option: &str) -> Self {
        Self {
            channel: menu.channel_id.clone(),
            ts: menu.ts.clone(),
            thread_ts: non_blank(&menu.thread_ts).map(str::to_string),
            option: option.to_string(),
        }
    }
}

/// The modal of an option with params: its description, then one required input per param,
/// labeled with its name and hinted with its description. A string is multiline text, an integer
/// or a number a number input, a boolean a yes/no select, and an enum a select of its values, each
/// by its index.
fn option_modal(origin: &ModalOrigin, option: &MenuOption) -> Value {
    let mut blocks = vec![json!({"type": "context", "elements": [plain(&option.description)]})];
    for param in &option.params {
        let mut input = json!({
            "type": "input",
            "block_id": param.name,
            "label": plain(&label(&param.name)),
            "element": param_input(param),
        });
        if let Some(description) = &param.description {
            input["hint"] = plain(description);
        }
        blocks.push(input);
    }
    json!({
        "type": "modal",
        "callback_id": MENU_VIEW_CALLBACK_ID,
        "private_metadata": serde_json::to_string(origin).unwrap_or_default(),
        "title": plain(&cut(&label(&option.name), MODAL_TITLE_MAX_CHARS)),
        "submit": plain("Send"),
        "close": plain("Cancel"),
        "blocks": blocks,
    })
}

/// A param's input element, whose action id is the param's name.
fn param_input(param: &MenuParam) -> Value {
    let select = |choices: Vec<(String, String)>| {
        let options = choices
            .into_iter()
            .map(|(value, text)| {
                json!({"text": plain(&cut(&text, SELECT_OPTION_MAX_CHARS)), "value": value})
            })
            .collect::<Vec<_>>();
        json!({"type": "static_select", "action_id": param.name, "options": options})
    };
    match (param.param_type, &param.values) {
        (MenuParamType::String, Some(values)) => select(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value.clone()))
                .collect(),
        ),
        (MenuParamType::String, None) => {
            json!({"type": "plain_text_input", "action_id": param.name, "multiline": true})
        }
        (MenuParamType::Integer, _) => {
            json!({"type": "number_input", "action_id": param.name, "is_decimal_allowed": false})
        }
        (MenuParamType::Number, _) => {
            json!({"type": "number_input", "action_id": param.name, "is_decimal_allowed": true})
        }
        (MenuParamType::Boolean, _) => select(vec![
            ("true".to_string(), "Yes".to_string()),
            ("false".to_string(), "No".to_string()),
        ]),
    }
}

/// The params of an option in a modal's submission, `values` (its `view.state.values`), each as
/// its type says. A missing one is `missing_param:<p>`, and a mistyped one
/// `type_mismatch:<p>:<type>`.
fn submitted_params(option: &MenuOption, values: &Value) -> Result<Map<String, Value>, String> {
    let mut params = Map::new();
    for param in &option.params {
        let state = values
            .get(&param.name)
            .and_then(|block| block.get(&param.name))
            .ok_or_else(|| format!("missing_param:{}", param.name))?;
        params.insert(param.name.clone(), param.submitted(state)?);
    }
    Ok(params)
}

/// The text of a click's message: the option's name, then one line per param, `name: value`.
fn click_text(option: &str, params: &Map<String, Value>) -> String {
    let mut text = option.to_string();
    for (name, value) in params {
        let value = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        text.push_str(&format!("\n{name}: {value}"));
    }
    text
}

/// The parts of a click's message: its IO context, from the person who clicked, at the post's
/// place and on the menu's thread id; its payload, a relayed Slack message's with the click's text;
/// and its evaluation, `{"option", "params"}`.
fn click_parts(
    workspace_id: &str,
    menu: &OpenMenu,
    user_id: &str,
    message_id: &str,
    option: &str,
    params: Map<String, Value>,
) -> Option<(IoContext, Value, Value)> {
    let thread_ts = non_blank(&menu.thread_ts);
    let mut io = slack_inbound_io_context(
        workspace_id,
        user_id,
        &menu.channel_id,
        thread_ts,
        message_id,
    );
    io.conversation.thread_id = Some(menu.thread_id.clone());
    let payload = crate::build_slack_inbound_payload_from_parts(
        workspace_id,
        user_id,
        &menu.channel_id,
        thread_ts,
        message_id,
        &click_text(option, &params),
        &[],
    )?;
    Some((io, payload, json!({"option": option, "params": params})))
}

/// A relayed message with the click's evaluation in its `meta.context`.
fn with_evaluation(outcome: InboundOutcome, evaluation: Value) -> InboundOutcome {
    match outcome {
        InboundOutcome::SendNow(mut msg) => {
            if let Some(context) = msg.meta.context.as_mut().and_then(Value::as_object_mut) {
                context.insert("evaluation".to_string(), evaluation);
            }
            InboundOutcome::SendNow(msg)
        }
        other => other,
    }
}

/// A click on a menu's button: what a `block_actions` payload says of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Click {
    team_id: String,
    user_id: String,
    channel_id: String,
    /// The post's `ts`.
    message_ts: String,
    /// The Slack thread the post is in, `None` at the top level.
    post_thread_ts: Option<String>,
    trigger_id: String,
    option: String,
    action_ts: String,
}

impl Click {
    /// The click in a `block_actions` payload, on one of a menu's buttons; or why it is none.
    fn parse(payload: &Value) -> Result<Self, &'static str> {
        let action = payload
            .get("actions")
            .and_then(Value::as_array)
            .and_then(|actions| {
                actions.iter().find(|action| {
                    text_at(action, &["block_id"]) == Some(MENU_BLOCK_ID)
                        && text_at(action, &["type"]) == Some("button")
                })
            })
            .ok_or("not a click on a menu's button")?;
        let message_ts = text_at(payload, &["container", "message_ts"])
            .or_else(|| text_at(payload, &["message", "ts"]))
            .ok_or("no post")?;
        // A post in a Slack thread names its root; a root names itself, or nothing.
        let post_thread_ts = text_at(payload, &["container", "thread_ts"])
            .or_else(|| text_at(payload, &["message", "thread_ts"]))
            .filter(|thread_ts| *thread_ts != message_ts)
            .map(str::to_string);
        Ok(Self {
            team_id: text_at(payload, &["team", "id"])
                .or_else(|| text_at(payload, &["user", "team_id"]))
                .ok_or("no team")?
                .to_string(),
            user_id: text_at(payload, &["user", "id"])
                .ok_or("no user")?
                .to_string(),
            channel_id: text_at(payload, &["container", "channel_id"])
                .or_else(|| text_at(payload, &["channel", "id"]))
                .ok_or("no channel")?
                .to_string(),
            message_ts: message_ts.to_string(),
            post_thread_ts,
            trigger_id: text_at(payload, &["trigger_id"])
                .ok_or("no trigger_id")?
                .to_string(),
            option: text_at(action, &["action_id"])
                .ok_or("no action_id")?
                .to_string(),
            action_ts: text_at(action, &["action_ts"])
                .ok_or("no action_ts")?
                .to_string(),
        })
    }

    /// The click's id as a message's, `message.id` in its IO context: io-common's deduplication
    /// takes a click once.
    fn message_id(&self) -> String {
        format!("click:{}:{}", self.message_ts, self.action_ts)
    }
}

/// The submission of an option's modal: what a `view_submission` payload says of it.
#[derive(Debug, Clone, PartialEq)]
struct Submission {
    team_id: String,
    user_id: String,
    view_id: String,
    origin: ModalOrigin,
    /// `view.state.values`: the inputs, by block and action id.
    values: Value,
}

impl Submission {
    /// The submission in a `view_submission` payload, of a menu's modal; or why it is none.
    fn parse(payload: &Value) -> Result<Self, &'static str> {
        if text_at(payload, &["view", "callback_id"]) != Some(MENU_VIEW_CALLBACK_ID) {
            return Err("not a menu's modal");
        }
        let origin = text_at(payload, &["view", "private_metadata"])
            .and_then(|metadata| serde_json::from_str::<ModalOrigin>(metadata).ok())
            .ok_or("no post in the modal's metadata")?;
        Ok(Self {
            team_id: text_at(payload, &["team", "id"])
                .or_else(|| text_at(payload, &["user", "team_id"]))
                .or_else(|| text_at(payload, &["view", "team_id"]))
                .ok_or("no team")?
                .to_string(),
            user_id: text_at(payload, &["user", "id"])
                .ok_or("no user")?
                .to_string(),
            view_id: text_at(payload, &["view", "id"])
                .ok_or("no view id")?
                .to_string(),
            origin,
            values: payload
                .get("view")
                .and_then(|view| view.get("state"))
                .and_then(|state| state.get("values"))
                .cloned()
                .unwrap_or_else(|| json!({})),
        })
    }

    /// The submission's id as a message's: io-common's deduplication takes a modal once.
    fn message_id(&self) -> String {
        format!("view:{}", self.view_id)
    }
}

/// The text at `path` in `value`, when it is one and not blank.
fn text_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |value, key| value.get(*key))?
        .as_str()
        .and_then(non_blank)
}

fn non_blank(text: &str) -> Option<&str> {
    Some(text.trim()).filter(|text| !text.is_empty())
}

/// `text` cut at `max` characters.
fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// What a click needs (`handle_interactive`): the node's binding and its own ICH, as a message's
/// relay checks them; the Slack client and the records; and what takes a person's message to the
/// mesh, remembered for "not taken" (not_taken).
pub(crate) struct Interactions {
    pub(crate) config: Config,
    pub(crate) control_plane: Arc<RwLock<IoControlPlaneState>>,
    pub(crate) dispatcher: Arc<RouterDispatcher>,
    pub(crate) identity_provision_cfg: IdentityProvisionConfig,
    pub(crate) self_ilk_id: Option<String>,
    pub(crate) self_tenant_id: Option<String>,
    pub(crate) ensured_bindings: Arc<Mutex<HashSet<String>>>,
    pub(crate) slack: Arc<SlackClients>,
    pub(crate) records: Option<Arc<SlackThreadRecords>>,
    pub(crate) identity: Arc<dyn IdentityResolver>,
    pub(crate) provisioner: Arc<dyn IdentityProvisioner>,
    pub(crate) inbound: Arc<Mutex<InboundProcessor>>,
    pub(crate) sender: NodeSender,
    pub(crate) relayed: Arc<RelayedMessages>,
}

/// Takes the payload of a Socket Mode `interactive` envelope, acked already: a click on a menu's
/// button (`block_actions`), or the submission of an option's modal (`view_submission`). Any other
/// is no menu's, and is ignored.
pub(crate) async fn handle_interactive(interactions: Arc<Interactions>, payload: Value) {
    match payload.get("type").and_then(Value::as_str) {
        Some("block_actions") => interactions.on_click(&payload).await,
        Some("view_submission") => interactions.on_submission(&payload).await,
        kind => tracing::debug!(
            kind = kind.unwrap_or_default(),
            "ignoring an interactive payload that is no menu's"
        ),
    }
}

impl Interactions {
    async fn on_click(&self, payload: &Value) {
        let click = match Click::parse(payload) {
            Ok(click) => click,
            Err(reason) => {
                tracing::debug!(reason, "ignoring a block action");
                return;
            }
        };
        let Some(binding) = self.binding_for(&click.team_id, &click.channel_id).await else {
            return;
        };
        let Some((menu, option)) =
            self.open_option(&click.channel_id, &click.message_ts, &click.option)
        else {
            self.no_longer_open(
                &click.user_id,
                &click.channel_id,
                click.post_thread_ts.as_deref(),
            )
            .await;
            return;
        };
        if option.params.is_empty() {
            self.relay_click(
                &binding,
                &menu,
                &click.user_id,
                &click.message_id(),
                &option.name,
                Map::new(),
            )
            .await;
            return;
        }
        let modal = option_modal(&ModalOrigin::of(&menu, &option.name), &option);
        match self.slack.open_view(&click.trigger_id, modal).await {
            Ok(()) => tracing::debug!(
                channel_id = %menu.channel_id,
                ts = %menu.ts,
                option = %option.name,
                "an option's modal is open"
            ),
            Err(err) => tracing::warn!(
                channel_id = %menu.channel_id,
                ts = %menu.ts,
                option = %option.name,
                error = %err,
                "opening an option's modal failed; the click sends nothing"
            ),
        }
    }

    async fn on_submission(&self, payload: &Value) {
        let submission = match Submission::parse(payload) {
            Ok(submission) => submission,
            Err(reason) => {
                tracing::debug!(reason, "ignoring a view submission");
                return;
            }
        };
        let origin = &submission.origin;
        let Some(binding) = self.binding_for(&submission.team_id, &origin.channel).await else {
            return;
        };
        let Some((menu, option)) = self.open_option(&origin.channel, &origin.ts, &origin.option)
        else {
            self.no_longer_open(
                &submission.user_id,
                &origin.channel,
                origin.thread_ts.as_deref(),
            )
            .await;
            return;
        };
        let params = match submitted_params(&option, &submission.values) {
            Ok(params) => params,
            Err(detail) => {
                tracing::warn!(
                    channel_id = %menu.channel_id,
                    ts = %menu.ts,
                    option = %option.name,
                    detail = %detail,
                    "dropping a modal's submission that does not match its option's params"
                );
                return;
            }
        };
        self.relay_click(
            &binding,
            &menu,
            &submission.user_id,
            &submission.message_id(),
            &option.name,
            params,
        )
        .await;
    }

    /// The node's binding, when a click is in its channel of its workspace: else `None`, logged.
    async fn binding_for(&self, team_id: &str, channel_id: &str) -> Option<SlackBindingConfig> {
        let binding = match crate::extract_runtime_slack_binding(
            self.control_plane.read().await.effective_config.as_ref(),
            &self.config,
        ) {
            Ok(binding) => binding,
            Err(error) => {
                tracing::warn!(
                    node_name = %self.config.node_name,
                    error = %error,
                    "dropping a click: missing workspace_id/conversation_id binding"
                );
                return None;
            }
        };
        if team_id != binding.workspace_id || channel_id != binding.conversation_id {
            tracing::debug!(
                node_name = %self.config.node_name,
                configured_workspace_id = %binding.workspace_id,
                configured_conversation_id = %binding.conversation_id,
                click_workspace_id = %team_id,
                click_conversation_id = %channel_id,
                "dropping a click outside configured binding"
            );
            return None;
        }
        Some(binding)
    }

    /// The open menu of the post at `ts` in `channel_id`, and its option `name`: `None`, logged,
    /// when the post has no open menu, when the records cannot say, or when the menu has no such
    /// option.
    fn open_option(
        &self,
        channel_id: &str,
        ts: &str,
        name: &str,
    ) -> Option<(OpenMenu, MenuOption)> {
        let records = self.records.as_deref()?;
        let menu = match records.menu_of_post(channel_id, ts) {
            Ok(Some(menu)) => menu,
            Ok(None) => {
                tracing::info!(
                    channel_id,
                    ts,
                    option = name,
                    "a click on a post with no open menu"
                );
                return None;
            }
            Err(err) => {
                tracing::warn!(
                    channel_id,
                    ts,
                    error = %err,
                    "slack thread records: looking up a post's menu failed; the click is answered \
                     as on a closed menu"
                );
                return None;
            }
        };
        let option = match Menu::from_value(&menu.menu) {
            Ok(read) => read.option(name).cloned(),
            Err(detail) => {
                tracing::warn!(channel_id, ts, detail = %detail, "an open menu does not read");
                None
            }
        };
        if option.is_none() {
            tracing::info!(
                channel_id,
                ts,
                option = name,
                "a click on an option its menu does not have"
            );
        }
        Some((menu, option?))
    }

    /// "This is no longer open.", which only the person who clicked sees, at the post's place.
    async fn no_longer_open(&self, user_id: &str, channel_id: &str, thread_ts: Option<&str>) {
        if let Err(err) = self
            .slack
            .post_ephemeral(channel_id, user_id, NO_LONGER_OPEN, thread_ts)
            .await
        {
            tracing::warn!(
                channel_id,
                user = user_id,
                error = %err,
                "telling a person that a menu is no longer open failed"
            );
        }
    }

    /// The click's message goes to the mesh as a relayed Slack message (the module's doc), once the
    /// binding's own ICH is ensured, as for a message. That waits on SY.identity when the ICH is
    /// not ensured yet, so it comes after a modal opens: its trigger lasts 3 seconds.
    async fn relay_click(
        &self,
        binding: &SlackBindingConfig,
        menu: &OpenMenu,
        user_id: &str,
        message_id: &str,
        option: &str,
        params: Map<String, Value>,
    ) {
        if let Err(err) = crate::ensure_io_slack_binding_ich_registered(
            &self.config.node_name,
            binding,
            &self.dispatcher,
            &self.identity_provision_cfg,
            self.self_ilk_id.as_deref(),
            self.self_tenant_id.as_deref(),
            self.ensured_bindings.clone(),
        )
        .await
        {
            tracing::warn!(
                node_name = %self.config.node_name,
                error = %err,
                "dropping a click: unable to ensure own binding ICH"
            );
            return;
        }
        let Some((io, payload, evaluation)) = click_parts(
            &binding.workspace_id,
            menu,
            user_id,
            message_id,
            option,
            params,
        ) else {
            tracing::warn!(option, "dropping a click whose message does not build");
            return;
        };
        let outcome = self
            .inbound
            .lock()
            .await
            .process_inbound(
                self.identity.as_ref(),
                Some(self.provisioner.as_ref()),
                crate::slack_identity_input(
                    &self.config.node_name,
                    &binding.workspace_id,
                    &binding.conversation_id,
                    user_id,
                ),
                None,
                io,
                payload,
            )
            .await;
        tracing::info!(
            channel_id = %menu.channel_id,
            ts = %menu.ts,
            thread_id = %menu.thread_id,
            option,
            "a click on a menu goes to the mesh"
        );
        crate::dispatch_inbound_outcome(
            self.sender.clone(),
            with_evaluation(outcome, evaluation),
            "menu click",
            &self.relayed,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use io_common::identity::{MockIdentityResolver, ResolveOrCreateInput};
    use io_common::inbound::InboundConfig;

    const WORKSPACE: &str = "T0WORKSPACE";
    const CHANNEL: &str = "C0SUPERVISORS";
    /// The thread id of the person's conversation, which the WF's instance carries.
    const CASE_THREAD: &str = "thread:sha256:ana-conversation";
    const POST_TS: &str = "1793000000.000100";

    /// The known option `handback`, as the WF sends it (docs/wf-orchestration-v1.md §5.4.10).
    fn handback_menu() -> Value {
        json!({"options": [{
            "name": "handback",
            "description": "Hand the conversation back to the agent.",
            "params": {"note": {
                "type": "string",
                "description": "What the agent must know: what was done and agreed."
            }}
        }]})
    }

    /// A menu with a param of each kind, and an option without params.
    fn typed_menu() -> Value {
        json!({
            "help": "The person is buying licenses.",
            "options": [
                {"name": "answer_team", "description": "Your reply goes to the team."},
                {"name": "invoice", "description": "They accept the quote.", "params": {
                    "plan": {"type": "string", "enum": ["basic", "pro"]},
                    "quantity": {"type": "integer", "description": "How many licenses."},
                    "unit_price": {"type": "number"},
                    "urgent": {"type": "boolean"},
                    "comment": {"type": "string"}
                }}
            ]
        })
    }

    fn wf_context(block: Value) -> Value {
        json!({"io": {"channel": "slack"}, "wf": block})
    }

    fn open_menu(menu: Value, thread_ts: &str) -> OpenMenu {
        OpenMenu {
            channel_id: CHANNEL.to_string(),
            ts: POST_TS.to_string(),
            thread_ts: thread_ts.to_string(),
            thread_id: CASE_THREAD.to_string(),
            menu,
            blocks: json!([]),
        }
    }

    fn option_of(menu: &Value, name: &str) -> MenuOption {
        Menu::from_value(menu)
            .expect("menu")
            .option(name)
            .cloned()
            .expect("option")
    }

    /// A click on the "Handback" button of a top-level post, as Slack sends it over Socket Mode.
    fn block_actions_fixture() -> Value {
        json!({
            "type": "block_actions",
            "user": {"id": "U0JUAN", "username": "juan", "name": "juan", "team_id": WORKSPACE},
            "api_app_id": "A0B291N47KL",
            "container": {
                "type": "message",
                "message_ts": POST_TS,
                "channel_id": CHANNEL,
                "is_ephemeral": false
            },
            "trigger_id": "1793000100.1234567890.abcdef",
            "team": {"id": WORKSPACE, "domain": "fluxbeetest"},
            "enterprise": null,
            "is_enterprise_install": false,
            "channel": {"id": CHANNEL, "name": "supervisores"},
            "message": {
                "type": "message",
                "user": "U0BOT",
                "ts": POST_TS,
                "bot_id": "B0BOT",
                "text": "Ana needs a credit note.",
                "blocks": []
            },
            "state": {"values": {}},
            "actions": [{
                "action_id": "handback",
                "block_id": MENU_BLOCK_ID,
                "text": {"type": "plain_text", "text": "Handback", "emoji": true},
                "value": "handback",
                "type": "button",
                "action_ts": "1793000100.000200"
            }]
        })
    }

    /// The submission of the "Handback" modal, as Slack sends it over Socket Mode.
    fn view_submission_fixture(state_values: Value) -> Value {
        json!({
            "type": "view_submission",
            "team": {"id": WORKSPACE, "domain": "fluxbeetest"},
            "user": {"id": "U0JUAN", "username": "juan", "name": "juan", "team_id": WORKSPACE},
            "api_app_id": "A0B291N47KL",
            "trigger_id": "1793000200.1234567890.fedcba",
            "view": {
                "id": "V0MODAL",
                "team_id": WORKSPACE,
                "type": "modal",
                "blocks": [],
                "private_metadata": format!(
                    "{{\"channel\":\"{CHANNEL}\",\"ts\":\"{POST_TS}\",\"option\":\"handback\"}}"
                ),
                "callback_id": MENU_VIEW_CALLBACK_ID,
                "state": {"values": state_values},
                "hash": "1793000200.abc",
                "title": {"type": "plain_text", "text": "Handback"},
                "app_id": "A0B291N47KL"
            },
            "response_urls": [],
            "is_enterprise_install": false,
            "enterprise": null
        })
    }

    /// A menu reads as the WF sends it: the options in its order, each one's params in their names'
    /// order, its help, and the types. One wfcel would refuse does not read, and names its field.
    #[test]
    fn a_menu_reads_as_the_wf_sends_it_and_a_bad_one_names_its_field() {
        let menu = Menu::from_value(&typed_menu()).expect("menu");
        assert_eq!(menu.help.as_deref(), Some("The person is buying licenses."));
        assert_eq!(
            menu.options
                .iter()
                .map(|option| option.name.as_str())
                .collect::<Vec<_>>(),
            vec!["answer_team", "invoice"]
        );
        let invoice = menu.option("invoice").expect("invoice");
        assert_eq!(
            invoice
                .params
                .iter()
                .map(|param| (param.name.as_str(), param.param_type))
                .collect::<Vec<_>>(),
            vec![
                ("comment", MenuParamType::String),
                ("plan", MenuParamType::String),
                ("quantity", MenuParamType::Integer),
                ("unit_price", MenuParamType::Number),
                ("urgent", MenuParamType::Boolean),
            ]
        );
        assert_eq!(
            invoice.params[1].values,
            Some(vec!["basic".to_string(), "pro".to_string()])
        );
        assert_eq!(
            invoice.params[2].description.as_deref(),
            Some("How many licenses.")
        );

        for (menu, field) in [
            (json!({"options": []}), "menu.options"),
            (
                json!({"options": [{"name": "Handback", "description": "x"}]}),
                "menu.options[0].name",
            ),
            (
                json!({"options": [{"name": "handback"}]}),
                "menu.options[0].description",
            ),
            (
                json!({"options": [
                    {"name": "a", "description": "x"},
                    {"name": "a", "description": "y"}
                ]}),
                "menu.options[1].name",
            ),
            (
                json!({"options": [
                    {"name": "a", "description": "x", "params": {"n": {"type": "date"}}}
                ]}),
                "menu.options[0].params.n.type",
            ),
            (
                json!({"options": [{"name": "a", "description": "x",
                    "params": {"n": {"type": "integer", "enum": ["1"]}}}]}),
                "menu.options[0].params.n.enum",
            ),
            (
                json!({"options": [{"name": "a", "description": "x",
                    "params": {"n": {"type": "string", "enum": ["b", "b"]}}}]}),
                "menu.options[0].params.n.enum",
            ),
            (
                json!({"help": " ", "options": [{"name": "a", "description": "x"}]}),
                "menu.help",
            ),
        ] {
            let err = Menu::from_value(&menu).expect_err(field);
            assert!(err.starts_with(&format!("{field}:")), "{field}: {err}");
        }
    }

    /// The case's first post to the team (docs/wf-orchestration-v1.md §5.4.3): sales' summary as
    /// its text, the person's facts under it, the menu's help and the "Handback" button. The words
    /// are escaped and verbatim, so neither the summary nor a fact pings anyone; the label is plain
    /// text.
    #[test]
    fn a_post_with_a_menu_shows_its_text_its_facts_its_help_and_its_buttons() {
        let mut block = json!({
            "trail": ["IO.api.sales@worker1", "WF.sales.invoice@motherbee"],
            "vars": {"person": "Ana <!channel>", "channel": "api"},
            "menu": handback_menu(),
            "subject": "new"
        });
        block["menu"]["help"] = json!("Take the case & hand it back.");
        let post = WfPost::read(Some(&wf_context(block)), Some(CASE_THREAD), true);
        assert_eq!(post.subject, Subject::New);

        let blocks = post
            .blocks("Ana needs a credit note <@U0BOSS>.", None)
            .expect("blocks");
        assert_eq!(
            blocks,
            json!([
                {"type": "section", "text": {
                    "type": "mrkdwn",
                    "text": "Ana needs a credit note &lt;@U0BOSS&gt;.",
                    "verbatim": true
                }},
                {"type": "context", "elements": [
                    {"type": "mrkdwn", "text": "channel: api", "verbatim": true},
                    {"type": "mrkdwn", "text": "person: Ana &lt;!channel&gt;", "verbatim": true}
                ]},
                {"type": "context", "elements": [
                    {"type": "mrkdwn", "text": "Take the case &amp; hand it back.", "verbatim": true}
                ]},
                {"type": "actions", "block_id": MENU_BLOCK_ID, "elements": [{
                    "type": "button",
                    "action_id": "handback",
                    "value": "handback",
                    "text": {"type": "plain_text", "text": "Handback"}
                }]}
            ])
        );

        // A payload's own blocks stay the post's, with the facts and the buttons under them.
        let own = json!([{"type": "section", "text": {"type": "mrkdwn", "text": "*own*"}}]);
        let blocks = post.blocks("", Some(&own)).expect("blocks");
        assert_eq!(blocks[0], own[0]);
        assert_eq!(blocks.as_array().map(Vec::len), Some(4));

        // A menu with no text shows its buttons alone.
        let menu_only = WfPost::read(
            Some(&wf_context(json!({"menu": handback_menu()}))),
            Some(CASE_THREAD),
            true,
        );
        let blocks = menu_only.blocks("  ", None).expect("blocks");
        assert_eq!(blocks.as_array().map(Vec::len), Some(1));
        assert_eq!(blocks[0]["block_id"], MENU_BLOCK_ID);
    }

    /// The facts line: one `key: value` per key, 10 keys at most, each value on one line, cut at
    /// 200 characters and escaped; a value that is no text is written as JSON. Vars that are not a
    /// non-empty object make no line.
    #[test]
    fn the_facts_line_holds_ten_keys_each_value_cut_at_200_characters() {
        let mut vars = Map::new();
        for key in 0..12 {
            vars.insert(format!("k{key:02}"), json!(key));
        }
        vars.insert("k00".to_string(), json!("line one\n  line two & <three>"));
        vars.insert("k01".to_string(), json!("x".repeat(250)));
        vars.insert("k02".to_string(), json!({"since": "10:02", "taken": true}));
        let line = facts_line(&Value::Object(vars)).expect("a line");
        let elements = line["elements"].as_array().expect("elements");
        assert_eq!(elements.len(), FACTS_MAX_KEYS);
        assert_eq!(
            elements[0]["text"],
            "k00: line one line two &amp; &lt;three&gt;"
        );
        assert_eq!(
            elements[1]["text"],
            format!("k01: {}…", "x".repeat(FACT_VALUE_MAX_CHARS))
        );
        assert_eq!(
            elements[2]["text"],
            "k02: {\"since\":\"10:02\",\"taken\":true}"
        );
        assert_eq!(elements[9]["text"], "k09: 9");

        assert_eq!(facts_line(&json!({})), None);
        assert_eq!(facts_line(&json!(["a"])), None);
        assert_eq!(facts_line(&json!("a")), None);
    }

    /// Without facts or a menu, a post is as before the WF's block: its payload's blocks as they
    /// are, or none. Facts alone make no post.
    #[test]
    fn without_facts_or_a_menu_a_post_is_as_before_and_facts_alone_make_none() {
        let plain_post = WfPost::read(Some(&json!({"io": {}})), Some(CASE_THREAD), true);
        assert_eq!(plain_post.subject, Subject::New);
        assert_eq!(plain_post.blocks("hola", None), None);
        let own = json!([{"type": "divider"}]);
        assert_eq!(plain_post.blocks("hola", Some(&own)), Some(own.clone()));
        let no_context = WfPost::read(None, None, true);
        assert_eq!(no_context.blocks("hola", None), None);

        let facts_only = WfPost::read(
            Some(&wf_context(json!({"vars": {"how": "quiet"}}))),
            Some(CASE_THREAD),
            true,
        );
        assert_eq!(facts_only.blocks("", None), None);
        assert_eq!(facts_only.blocks("", Some(&json!([]))), Some(json!([])));
        assert_eq!(
            facts_only
                .blocks("Returned.", None)
                .and_then(|blocks| blocks.as_array().map(Vec::len)),
            Some(2)
        );
    }

    /// A menu shows no buttons when no click could go anywhere: the message has no thread id, the
    /// records are closed, or the message ends its subject (which closes its menu). One that does
    /// not read shows none either. The text still posts, as text.
    #[test]
    fn a_menu_shows_no_buttons_when_no_click_could_go_anywhere() {
        let block = json!({"menu": handback_menu()});
        for (context, thread_id, records_open) in [
            (wf_context(block.clone()), None, true),
            (wf_context(block.clone()), Some("  "), true),
            (wf_context(block.clone()), Some(CASE_THREAD), false),
            (
                wf_context(json!({"menu": handback_menu(), "subject": "end"})),
                Some(CASE_THREAD),
                true,
            ),
            (
                wf_context(json!({"menu": {"options": "handback"}})),
                Some(CASE_THREAD),
                true,
            ),
        ] {
            let post = WfPost::read(Some(&context), thread_id, records_open);
            assert!(post.menu.is_none(), "{context}");
            assert_eq!(post.blocks("text", None), None, "{context}");
        }
    }

    /// `end` with nothing to post only closes the subject: no text, no `content_ref`, no files and
    /// no blocks. Anything else posts, and only `end` only closes.
    #[test]
    fn an_end_with_nothing_to_post_only_closes() {
        let end = WfPost::read(
            Some(&wf_context(
                json!({"subject": "end", "vars": {"how": "handback"}}),
            )),
            Some(CASE_THREAD),
            true,
        );
        assert_eq!(end.subject, Subject::End);
        for payload in [
            json!(null),
            json!({}),
            json!({"type": "text"}),
            json!({"type": "text", "content": ""}),
            json!({"type": "text", "content": "  \n", "attachments": [], "blocks": []}),
        ] {
            assert!(end.only_closes(&payload), "{payload}");
        }
        for payload in [
            json!({"type": "text", "content": "Closed by the team."}),
            json!({"type": "text", "content_ref": {"blob_name": "b"}}),
            json!({"type": "text", "content": "", "attachments": [{"blob_name": "a"}]}),
            json!({"type": "text", "blocks": [{"type": "divider"}]}),
            json!("text"),
        ] {
            assert!(!end.only_closes(&payload), "{payload}");
        }
        let same = WfPost::read(
            Some(&wf_context(json!({"subject": "same"}))),
            Some(CASE_THREAD),
            true,
        );
        assert!(!same.only_closes(&json!({})));
    }

    /// A text longer than a section holds goes in several, each within Slack's 3,000 characters
    /// once escaped, as many as a post leaves room for.
    #[test]
    fn a_long_text_goes_in_sections_within_slacks_limit() {
        // 2,995 characters and an `&`, escaped as 5, fill a section; the `b` opens the next.
        let text = format!("{}&b", "a".repeat(2_995));
        let sections = text_sections(&text);
        assert_eq!(sections.len(), 2);
        assert_eq!(
            sections[0]["text"]["text"],
            format!("{}&amp;", "a".repeat(2_995))
        );
        assert_eq!(sections[1]["text"]["text"], "b");
        // An escape is never split.
        let sections = text_sections(&format!("{}&b", "a".repeat(2_996)));
        assert_eq!(sections[0]["text"]["text"], "a".repeat(2_996));
        assert_eq!(sections[1]["text"]["text"], "&amp;b");
        for section in &sections {
            let chars = section["text"]["text"]
                .as_str()
                .map(|text| text.chars().count());
            assert!(chars <= Some(SECTION_TEXT_MAX_CHARS));
        }
        assert!(text_sections(" \n ").is_empty());
        assert_eq!(
            text_sections(&"a".repeat(SECTION_TEXT_MAX_CHARS * 60)).len(),
            MESSAGE_BLOCKS_MAX - BLOCKS_UNDER_TEXT
        );
    }

    /// A closed menu's post keeps its blocks, its facts included, without the buttons, and
    /// "Closed." takes their place.
    #[test]
    fn a_closed_menu_keeps_its_post_without_the_buttons() {
        let post = WfPost::read(
            Some(&wf_context(
                json!({"vars": {"person": "Ana"}, "menu": handback_menu()}),
            )),
            Some(CASE_THREAD),
            true,
        );
        let blocks = post
            .blocks("Ana needs a credit note.", None)
            .expect("blocks");
        let closed = closed_blocks(&blocks);
        assert_eq!(
            closed,
            json!([
                blocks[0].clone(),
                blocks[1].clone(),
                {"type": "context", "elements": [{"type": "plain_text", "text": "Closed."}]}
            ])
        );
        assert_eq!(
            closed_blocks(&Value::Null),
            json!([{"type": "context", "elements": [{"type": "plain_text", "text": "Closed."}]}])
        );
    }

    #[test]
    fn a_name_reads_with_spaces_and_a_capital() {
        assert_eq!(label("handback"), "Handback");
        assert_eq!(label("answer_team"), "Answer team");
        assert_eq!(label("a"), "A");
        assert_eq!(label(""), "");
    }

    /// An option with params opens a modal with one required input per param, labeled with its
    /// name and hinted with its description: a string is multiline text, an integer or a number a
    /// number input, a boolean a yes/no select, an enum a select of its values. Its metadata names
    /// the post and the option.
    #[test]
    fn an_options_modal_has_one_required_input_per_param() {
        let menu = open_menu(typed_menu(), "1793000000.000050");
        let invoice = option_of(&menu.menu, "invoice");
        let modal = option_modal(&ModalOrigin::of(&menu, "invoice"), &invoice);
        assert_eq!(modal["type"], "modal");
        assert_eq!(modal["callback_id"], MENU_VIEW_CALLBACK_ID);
        assert_eq!(
            modal["title"],
            json!({"type": "plain_text", "text": "Invoice"})
        );
        assert_eq!(modal["submit"]["text"], "Send");
        let origin: ModalOrigin =
            serde_json::from_str(modal["private_metadata"].as_str().expect("metadata"))
                .expect("origin");
        assert_eq!(
            origin,
            ModalOrigin {
                channel: CHANNEL.to_string(),
                ts: POST_TS.to_string(),
                thread_ts: Some("1793000000.000050".to_string()),
                option: "invoice".to_string(),
            }
        );
        let blocks = modal["blocks"].as_array().expect("blocks");
        assert_eq!(
            blocks[0],
            json!({"type": "context", "elements": [
                {"type": "plain_text", "text": "They accept the quote."}
            ]})
        );
        let inputs = &blocks[1..];
        assert_eq!(inputs.len(), 5);
        for input in inputs {
            assert_eq!(input["type"], "input");
            assert_eq!(input.get("optional"), None, "required: {input}");
            assert_eq!(input["element"]["action_id"], input["block_id"]);
        }
        assert_eq!(
            inputs[0]["element"],
            json!({"type": "plain_text_input", "action_id": "comment", "multiline": true})
        );
        assert_eq!(
            inputs[1]["element"]["options"],
            json!([
                {"text": {"type": "plain_text", "text": "basic"}, "value": "0"},
                {"text": {"type": "plain_text", "text": "pro"}, "value": "1"}
            ])
        );
        assert_eq!(inputs[2]["label"]["text"], "Quantity");
        assert_eq!(inputs[2]["hint"]["text"], "How many licenses.");
        assert_eq!(inputs[2]["element"]["is_decimal_allowed"], false);
        assert_eq!(inputs[3]["label"]["text"], "Unit price");
        assert_eq!(inputs[3]["element"]["is_decimal_allowed"], true);
        assert_eq!(
            inputs[4]["element"]["options"],
            json!([
                {"text": {"type": "plain_text", "text": "Yes"}, "value": "true"},
                {"text": {"type": "plain_text", "text": "No"}, "value": "false"}
            ])
        );

        // A post at the top level names no thread, and a long name fits Slack's title.
        let top = open_menu(typed_menu(), "");
        assert_eq!(ModalOrigin::of(&top, "invoice").thread_ts, None);
        let long = MenuOption {
            name: "a_very_long_option_name_of_32_ch".to_string(),
            description: "Long.".to_string(),
            params: Vec::new(),
        };
        let modal = option_modal(&ModalOrigin::of(&top, &long.name), &long);
        assert_eq!(
            modal["title"]["text"]
                .as_str()
                .map(|title| title.chars().count()),
            Some(MODAL_TITLE_MAX_CHARS)
        );
    }

    /// A click on a menu's button reads from Slack's `block_actions`: who, where, which post, its
    /// trigger and the option. A post at the top level has no thread; a post in a thread names its
    /// root. A click on another block is none of a menu's.
    #[test]
    fn a_click_reads_from_block_actions() {
        let click = Click::parse(&block_actions_fixture()).expect("click");
        assert_eq!(
            click,
            Click {
                team_id: WORKSPACE.to_string(),
                user_id: "U0JUAN".to_string(),
                channel_id: CHANNEL.to_string(),
                message_ts: POST_TS.to_string(),
                post_thread_ts: None,
                trigger_id: "1793000100.1234567890.abcdef".to_string(),
                option: "handback".to_string(),
                action_ts: "1793000100.000200".to_string(),
            }
        );
        assert_eq!(
            click.message_id(),
            format!("click:{POST_TS}:1793000100.000200")
        );

        // A root names itself as its thread: still the top level.
        let mut root = block_actions_fixture();
        root["message"]["thread_ts"] = json!(POST_TS);
        assert_eq!(Click::parse(&root).expect("click").post_thread_ts, None);
        // A post in a Slack thread.
        let mut in_thread = block_actions_fixture();
        in_thread["container"]["thread_ts"] = json!("1793000000.000050");
        assert_eq!(
            Click::parse(&in_thread)
                .expect("click")
                .post_thread_ts
                .as_deref(),
            Some("1793000000.000050")
        );

        let mut other_block = block_actions_fixture();
        other_block["actions"][0]["block_id"] = json!("an_agents_block");
        assert_eq!(
            Click::parse(&other_block),
            Err("not a click on a menu's button")
        );
        let mut no_trigger = block_actions_fixture();
        no_trigger["trigger_id"] = json!("");
        assert_eq!(Click::parse(&no_trigger), Err("no trigger_id"));
    }

    /// A modal's submission reads from Slack's `view_submission`: who, the post and the option its
    /// metadata names, and the inputs. A modal that is not a menu's is none.
    #[test]
    fn a_submission_reads_from_view_submission() {
        let values = json!({"note": {"note": {
            "type": "plain_text_input",
            "value": "Emitida la NC 0001-0042 <!here> & enviada."
        }}});
        let submission =
            Submission::parse(&view_submission_fixture(values.clone())).expect("submission");
        assert_eq!(
            submission,
            Submission {
                team_id: WORKSPACE.to_string(),
                user_id: "U0JUAN".to_string(),
                view_id: "V0MODAL".to_string(),
                origin: ModalOrigin {
                    channel: CHANNEL.to_string(),
                    ts: POST_TS.to_string(),
                    thread_ts: None,
                    option: "handback".to_string(),
                },
                values,
            }
        );
        assert_eq!(submission.message_id(), "view:V0MODAL");

        let mut other = view_submission_fixture(json!({}));
        other["view"]["callback_id"] = json!("another_app_modal");
        assert_eq!(Submission::parse(&other), Err("not a menu's modal"));
        let mut no_origin = view_submission_fixture(json!({}));
        no_origin["view"]["private_metadata"] = json!("{}");
        assert_eq!(
            Submission::parse(&no_origin),
            Err("no post in the modal's metadata")
        );
    }

    /// A submission is checked again against the menu's types: a string as written, an integer as
    /// a JSON integer, a number, a boolean, an enum's value. A missing or mistyped input names its
    /// param, and the submission is dropped.
    #[test]
    fn a_submission_is_typed_as_the_menu_says_and_a_mismatch_names_its_param() {
        let invoice = option_of(&typed_menu(), "invoice");
        let state = |changed: Value| -> Value {
            let mut state = json!({
                "comment": {"comment": {"type": "plain_text_input", "value": "Q&A <x>"}},
                "plan": {"plan": {"type": "static_select", "selected_option": {
                    "text": {"type": "plain_text", "text": "pro"}, "value": "1"}}},
                "quantity": {"quantity": {"type": "number_input", "value": "10"}},
                "unit_price": {"unit_price": {"type": "number_input", "value": "12.5"}},
                "urgent": {"urgent": {"type": "static_select", "selected_option": {
                    "text": {"type": "plain_text", "text": "No"}, "value": "false"}}}
            });
            if let (Some(state), Some(changed)) = (state.as_object_mut(), changed.as_object()) {
                for (key, value) in changed {
                    state.insert(key.clone(), value.clone());
                }
            }
            state
        };
        assert_eq!(
            submitted_params(&invoice, &state(json!({}))).map(Value::Object),
            Ok(json!({
                "comment": "Q&A <x>",
                "plan": "pro",
                "quantity": 10,
                "unit_price": 12.5,
                "urgent": false
            }))
        );

        for (changed, detail) in [
            (
                json!({"quantity": {"quantity": {"type": "number_input", "value": "2.5"}}}),
                "type_mismatch:quantity:integer",
            ),
            (
                json!({"unit_price": {"unit_price": {"type": "number_input", "value": "lots"}}}),
                "type_mismatch:unit_price:number",
            ),
            (
                json!({"plan": {"plan": {"type": "static_select",
                    "selected_option": {"value": "7"}}}}),
                "type_mismatch:plan:string",
            ),
            (
                json!({"urgent": {"urgent": {"type": "static_select", "selected_option": null}}}),
                "type_mismatch:urgent:boolean",
            ),
            (
                json!({"comment": {"comment": {"type": "plain_text_input", "value": null}}}),
                "type_mismatch:comment:string",
            ),
            (json!({"quantity": {}}), "missing_param:quantity"),
        ] {
            assert_eq!(
                submitted_params(&invoice, &state(changed.clone())),
                Err(detail.to_string()),
                "{changed}"
            );
        }
    }

    /// The click's message (docs/wf-orchestration-v1.md §5.4.4): a relayed Slack message, from the
    /// person who clicked, as identity resolves them, at the post's place and on the menu's thread
    /// id, with the option and its params as `meta.context.evaluation`, the shape of an agent's
    /// answer. Its text is the option's name, then each param as `name: value`.
    #[tokio::test]
    async fn a_clicks_message_is_a_relayed_slack_message_on_the_menus_thread_id() {
        let menu = open_menu(handback_menu(), "");
        let mut params = Map::new();
        params.insert(
            "note".to_string(),
            json!("Emitida la NC 0001-0042.\nEnviada."),
        );
        let (io, payload, evaluation) = click_parts(
            WORKSPACE,
            &menu,
            "U0JUAN",
            "view:V0MODAL",
            "handback",
            params,
        )
        .expect("parts");

        let mut inbound = InboundProcessor::new("io-slack-uuid", InboundConfig::default());
        let identity = MockIdentityResolver::new();
        let input: ResolveOrCreateInput =
            crate::slack_identity_input("IO.slack.team@motherbee", WORKSPACE, CHANNEL, "U0JUAN");
        assert_eq!(input.external_id, "IO.slack.team@motherbee:U0JUAN");
        let outcome = inbound
            .process_inbound(&identity, None, input, None, io, payload)
            .await;
        let InboundOutcome::SendNow(msg) = with_evaluation(outcome, evaluation) else {
            panic!("the click's message goes");
        };

        assert_eq!(msg.meta.msg_type, "user");
        assert_eq!(msg.meta.thread_id.as_deref(), Some(CASE_THREAD));
        assert_eq!(
            msg.meta.ich.as_deref(),
            Some("slack://T0WORKSPACE:C0SUPERVISORS")
        );
        assert!(msg
            .meta
            .src_ilk
            .as_deref()
            .is_some_and(|ilk| ilk.starts_with("ilk:mock:")));
        let context = msg.meta.context.as_ref().expect("context");
        assert_eq!(
            context["evaluation"],
            json!({"option": "handback",
                   "params": {"note": "Emitida la NC 0001-0042.\nEnviada."}})
        );
        let io = &context["io"];
        assert_eq!(io["channel"], "slack");
        assert_eq!(io["sender"], json!({"kind": "slack_user", "id": "U0JUAN"}));
        assert_eq!(io["conversation"]["id"], CHANNEL);
        assert_eq!(io["conversation"]["thread_id"], CASE_THREAD);
        assert_eq!(io["message"]["id"], "view:V0MODAL");
        // The post is at the top level: so is the click's place.
        assert_eq!(
            io["reply_target"],
            json!({"kind": "slack_post", "address": CHANNEL, "params": {
                "workspace_id": WORKSPACE, "conversation_id": CHANNEL
            }})
        );
        assert_eq!(msg.payload["type"], "text");
        assert_eq!(
            msg.payload["content"],
            "handback\nnote: Emitida la NC 0001-0042.\nEnviada."
        );
        assert_eq!(msg.payload["raw"]["slack"]["user"], "U0JUAN");

        // A click on a post in a Slack thread is at that thread; an option without params is its
        // name alone; params that are no text are written as JSON.
        let in_thread = open_menu(typed_menu(), "1793000000.000050");
        let (io, payload, evaluation) = click_parts(
            WORKSPACE,
            &in_thread,
            "U0JUAN",
            "click:1",
            "answer_team",
            Map::new(),
        )
        .expect("parts");
        assert_eq!(
            io.reply_target.params["thread_ts"].as_str(),
            Some("1793000000.000050")
        );
        assert_eq!(io.conversation.thread_id.as_deref(), Some(CASE_THREAD));
        assert_eq!(payload["content"], "answer_team");
        assert_eq!(evaluation, json!({"option": "answer_team", "params": {}}));
        let mut typed = Map::new();
        typed.insert("quantity".to_string(), json!(10));
        typed.insert("urgent".to_string(), json!(true));
        assert_eq!(
            click_text("invoice", &typed),
            "invoice\nquantity: 10\nurgent: true"
        );
    }

    /// The node's post with a menu is recorded as its thread id's open menu, and a new one takes
    /// its place; `end` takes it. Closing a post calls Slack, which this test has no token for:
    /// the records decide all the same, and a click on the older post finds no menu.
    #[tokio::test]
    async fn a_posted_menu_is_recorded_a_new_one_takes_its_place_and_end_takes_it() {
        let records = SlackThreadRecords::in_memory().expect("records");
        let slack = SlackClients::new().expect("client");
        let post = WfPost::read(
            Some(&wf_context(json!({"menu": handback_menu()}))),
            Some(CASE_THREAD),
            true,
        );
        let blocks = post.blocks("first", None).expect("blocks");
        post.menu_posted(&slack, Some(&records), CHANNEL, POST_TS, None, &blocks)
            .await;
        let first = records
            .menu_of_post(CHANNEL, POST_TS)
            .expect("lookup")
            .expect("open");
        assert_eq!(first.thread_id, CASE_THREAD);
        assert_eq!(first.thread_ts, "");
        assert_eq!(first.menu, handback_menu());
        assert_eq!(first.blocks, blocks);

        post.menu_posted(
            &slack,
            Some(&records),
            CHANNEL,
            "1793000000.000300",
            Some("1793000000.000050"),
            &blocks,
        )
        .await;
        assert_eq!(
            records.menu_of_post(CHANNEL, POST_TS).expect("lookup"),
            None
        );
        let second = records
            .menu_of_post(CHANNEL, "1793000000.000300")
            .expect("lookup")
            .expect("open");
        assert_eq!(second.thread_ts, "1793000000.000050");

        // A post with no menu records none.
        let plain_post = WfPost::read(None, Some(CASE_THREAD), true);
        plain_post
            .menu_posted(
                &slack,
                Some(&records),
                CHANNEL,
                "1793000000.000400",
                None,
                &blocks,
            )
            .await;
        assert_eq!(
            records
                .menu_of_post(CHANNEL, "1793000000.000400")
                .expect("lookup"),
            None
        );

        close_subject_menu(&slack, Some(&records), Some(CASE_THREAD), CHANNEL).await;
        assert_eq!(
            records
                .menu_of_post(CHANNEL, "1793000000.000300")
                .expect("lookup"),
            None
        );
        // Nothing left to close, and nothing to close without records or a thread id.
        close_subject_menu(&slack, Some(&records), Some(CASE_THREAD), CHANNEL).await;
        close_subject_menu(&slack, None, Some(CASE_THREAD), CHANNEL).await;
        close_subject_menu(&slack, Some(&records), None, CHANNEL).await;
    }
}
