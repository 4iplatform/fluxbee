use std::fmt::Display;

use serde_json::{json, Map, Value};

use crate::errors::{AiSdkError, Result};
use crate::llm::{
    extract_json_value, parse_structured_output, warn_unstructured_answer, OutputSchemaSpec,
};

pub use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplyContextOptions {
    pub preserve_final_response_contract: bool,
}

pub fn build_reply_routing(msg: &Message, src_uuid: &str) -> Routing {
    let dst = match &msg.routing.src {
        value if value.trim().is_empty() => Destination::Broadcast,
        value => Destination::Unicast(value.to_string()),
    };

    Routing {
        src: src_uuid.to_string(),
        src_l2_name: None,
        dst,
        ttl: msg.routing.ttl.max(1),
        trace_id: msg.routing.trace_id.clone(),
    }
}

pub fn build_reply_message(msg: &Message, src_uuid: &str, payload: Value) -> Message {
    build_reply_message_with_options(msg, src_uuid, payload, ReplyContextOptions::default())
}

pub fn build_reply_message_with_options(
    msg: &Message,
    src_uuid: &str,
    payload: Value,
    options: ReplyContextOptions,
) -> Message {
    let mut meta = build_reply_meta(&msg.meta, options);
    apply_error_reply_msg_suffix(&mut meta, &payload);
    Message {
        routing: build_reply_routing(msg, src_uuid),
        meta,
        payload,
    }
}

/// Suffix (gap-1) appended to a reply's msg-name when the reply payload is an error, so the reply is
/// distinguishable from a successful answer at the msg-name level.
pub const AI_ERROR_MSG_SUFFIX: &str = ".error";

/// gap-1: when a reply payload is an error (`type == "error"`), stamp a distinct `.error` suffix onto
/// the reply's msg-name. Success and error replies otherwise share the request's msg-name, so a WF
/// event_match awaiting the answer would consume the error AS the answer and complete "successfully"
/// with missing data. With the suffix a WF can event_match `<msg>.error` (or the bare marker) to route
/// AI failures to a failed state. Callers that correlate by thread_id (io.api/io.slack) are unaffected;
/// error detection there still uses payload.type.
fn apply_error_reply_msg_suffix(meta: &mut Meta, payload: &Value) {
    if payload.get("type").and_then(Value::as_str) != Some("error") {
        return;
    }
    // Only suffix a request that HAD a msg-name. A no-name request is already unroutable by msg-name,
    // so fabricating a generic, collision-prone "error" marker is worse than leaving it None.
    if let Some(name) = meta.msg.as_deref() {
        if !name.ends_with(AI_ERROR_MSG_SUFFIX) {
            meta.msg = Some(format!("{name}{AI_ERROR_MSG_SUFFIX}"));
        }
    }
}

/// Builds a reply message where routing.src is intentionally left for runtime assignment.
/// NodeRuntime overwrites routing.src with the connected node UUID before sending.
pub fn build_reply_message_runtime_src(msg: &Message, payload: Value) -> Message {
    build_reply_message_runtime_src_with_options(msg, payload, ReplyContextOptions::default())
}

pub fn build_reply_message_runtime_src_with_options(
    msg: &Message,
    payload: Value,
    options: ReplyContextOptions,
) -> Message {
    let mut meta = build_reply_meta(&msg.meta, options);
    apply_error_reply_msg_suffix(&mut meta, &payload);
    Message {
        routing: build_reply_routing(msg, ""),
        meta,
        payload,
    }
}

/// A reply goes from the agent to whoever spoke (§6.2 of docs/router-stream-v1.md): its `dst_ilk`
/// is the incoming `src_ilk`, and its `src_ilk` is the agent's own, which the runtime stamps when it
/// sends it. It never carries the human's as its own.
fn build_reply_meta(meta: &Meta, options: ReplyContextOptions) -> Meta {
    let mut reply = meta.clone();
    reply.dst_ilk = meta.src_ilk.clone();
    reply.src_ilk = None;
    reply.memory_package = None;
    reply.context = sanitize_reply_context(reply.context, options);
    reply
}

fn sanitize_reply_context(context: Option<Value>, options: ReplyContextOptions) -> Option<Value> {
    let Some(Value::Object(mut obj)) = context else {
        return context;
    };
    obj.remove("thread_id");
    obj.remove("src_ilk");
    obj.remove("response_envelope");
    // The request's WF block and evaluation: a WF puts its own block on every message it sends,
    // and an answer carries only its own evaluation (`ResponseContract::answer`).
    obj.remove("wf");
    obj.remove("evaluation");
    if !options.preserve_final_response_contract {
        obj.remove("final_response_contract");
    }
    Some(Value::Object(obj))
}

/// Puts an answer's evaluation (`ContractAnswer::evaluation`) in the reply's
/// `meta.context.evaluation`, where a WF branches on it.
pub fn set_reply_evaluation(reply: &mut Message, evaluation: Value) {
    let mut context = match reply.meta.context.take() {
        Some(Value::Object(context)) => context,
        _ => serde_json::Map::new(),
    };
    context.insert("evaluation".to_string(), evaluation);
    reply.meta.context = Some(Value::Object(context));
}

pub fn extract_response_envelope(meta: &Meta) -> Result<Option<Value>> {
    extract_context_object_field(meta, "response_envelope")
}

pub fn extract_final_response_contract(meta: &Meta) -> Result<Option<Value>> {
    extract_context_object_field(meta, "final_response_contract")
}

pub fn resolve_response_envelope_contract(meta: &Meta) -> Result<Option<ResponseContract>> {
    extract_response_envelope(meta)?
        .as_ref()
        .map(ResponseContract::from_value)
        .transpose()
}

/// The contract a message asks its answer under (docs/wf-orchestration-v1.md §5.3.6): its WF's
/// menu, `meta.context.wf.menu`, or its `meta.context.response_envelope`. A message with both is
/// `ConflictingContracts`, since a WF asks an agent with its menu; an invalid menu is `InvalidMenu`,
/// and an invalid response contract `InvalidResponseContract`. A `null` menu is none.
pub fn resolve_message_contract(meta: &Meta) -> Result<Option<MessageContract>> {
    let context = meta.context.as_ref();
    let menu = context
        .and_then(|context| context.get("wf"))
        .and_then(|wf| wf.get("menu"))
        .filter(|menu| !menu.is_null());
    let Some(menu) = menu else {
        return Ok(resolve_response_envelope_contract(meta)?.map(MessageContract::Envelope));
    };
    if context.is_some_and(|context| context.get("response_envelope").is_some()) {
        return Err(AiSdkError::ConflictingContracts);
    }
    MenuContract::from_value(menu).map(|menu| Some(MessageContract::Menu(menu)))
}

pub fn resolve_final_response_contract_output_schema(
    meta: &Meta,
) -> Result<Option<OutputSchemaSpec>> {
    extract_final_response_contract(meta)?
        .as_ref()
        .map(response_contract_to_output_schema_spec)
        .transpose()
}

/// A response contract (`json_object_v1`, `response_contract_to_output_schema_spec`) as an agent
/// answers it: the schema of the object its model answers with and, when the contract names one,
/// the `text_field` whose value is the answer's text (docs/wf-orchestration-v1.md §5.2.1).
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseContract {
    pub output_schema: OutputSchemaSpec,
    pub text_field: Option<String>,
}

/// An answer under a response contract (`ResponseContract::answer`) or a WF's menu
/// (`MenuContract::answer`).
#[derive(Debug, Clone, PartialEq)]
pub struct ContractAnswer {
    /// The answer's text: the model's output, the value of the contract's `text_field`, or the
    /// `reply` of a menu's answer.
    pub text: String,
    /// With a `text_field`, the whole object the model answered with; with a menu, the checked
    /// `{reply, option, params}`. The answer carries it in `meta.context.evaluation`.
    pub evaluation: Option<Value>,
}

impl ResponseContract {
    /// The contract `contract` describes; an invalid one is `InvalidResponseContract`.
    pub fn from_value(contract: &Value) -> Result<Self> {
        let output_schema = response_contract_to_output_schema_spec(contract)?;
        let text_field = contract
            .get("text_field")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(Self {
            output_schema,
            text_field,
        })
    }

    /// The answer the model's output `raw` gives under this contract. Without a `text_field`, `raw`
    /// as it is. With one, the object in `raw`, checked against the schema as a structured call
    /// checks it, is the evaluation, and that field's value the text; an output that is not that
    /// object is `InvalidStructuredOutput`.
    pub fn answer(&self, raw: String) -> Result<ContractAnswer> {
        let Some(text_field) = self.text_field.as_deref() else {
            return Ok(ContractAnswer {
                text: raw,
                evaluation: None,
            });
        };
        let evaluation = parse_structured_output(&raw, &self.output_schema)?;
        let text = evaluation
            .get(text_field)
            .and_then(Value::as_str)
            .ok_or_else(|| AiSdkError::InvalidStructuredOutput {
                detail: format!("type_mismatch:{text_field}:string"),
            })?
            .to_string();
        Ok(ContractAnswer {
            text,
            evaluation: Some(evaluation),
        })
    }
}

/// The option every menu has, for an answer that none of the WF's options fits, with its one
/// param, `reason`. A WF's menu cannot name an option so.
const MENU_NONE_OPTION: &str = "none";
const MENU_NONE_REASON: &str = "reason";
/// A menu's limits, wfcel's (docs/wf-orchestration-v1.md §5.3.6). Characters are Unicode code
/// points.
const MENU_HELP_MAX_CHARS: usize = 600;
const MENU_DESCRIPTION_MAX_CHARS: usize = 200;
const MENU_OPTIONS_MAX: usize = 8;
const MENU_PARAMS_MAX: usize = 8;
const MENU_ENUM_MAX: usize = 20;
const MENU_NAME_MAX_CHARS: usize = 32;
/// Why a name of an option or a param is refused.
const MENU_NAME_RULE: &str = "must match ^[a-z][a-z0-9_]{0,31}$";
/// The last line of a menu as the model reads it (`MenuContract::prompt`): its answer's shape.
const MENU_ANSWER_INSTRUCTION: &str = r#"Answer with only one JSON object: {"reply": "<text for the person, may be empty>", "option": "<one name above>", "params": {<that option's params>}}"#;
/// The fields of a menu's answer, `{reply, option, params}`.
const MENU_ANSWER_FIELDS: [&str; 3] = ["reply", "option", "params"];

/// The contract a message asks its answer under (`resolve_message_contract`): a WF's menu, or a
/// caller's response contract, which the IO nodes keep for their own callers.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageContract {
    /// `meta.context.response_envelope`.
    Envelope(ResponseContract),
    /// `meta.context.wf.menu`.
    Menu(MenuContract),
}

impl MessageContract {
    /// What the request log calls it: `envelope` or `menu`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Envelope(_) => "envelope",
            Self::Menu(_) => "menu",
        }
    }

    /// The schema a response contract asks the provider for, on a call without tools, and the
    /// tool loop's instruction in the system prompt. A menu has neither: the WF's block shows the
    /// model the menu and its answer's shape, and the answer is checked afterwards.
    pub fn output_schema(&self) -> Option<&OutputSchemaSpec> {
        match self {
            Self::Envelope(contract) => Some(&contract.output_schema),
            Self::Menu(_) => None,
        }
    }

    /// The WF's menu, which the WF's block shows the model.
    pub fn menu(&self) -> Option<&MenuContract> {
        match self {
            Self::Envelope(_) => None,
            Self::Menu(menu) => Some(menu),
        }
    }

    /// Whether an answer under it is an object, checked, that gives the answer's text and its
    /// evaluation: always under a menu, and under a response contract with a `text_field`.
    pub fn has_evaluation(&self) -> bool {
        match self {
            Self::Envelope(contract) => contract.text_field.is_some(),
            Self::Menu(_) => true,
        }
    }

    /// The answer the model's output `raw` gives under this contract.
    pub fn answer(&self, raw: String) -> Result<ContractAnswer> {
        match self {
            Self::Envelope(contract) => contract.answer(raw),
            Self::Menu(menu) => menu.answer(raw),
        }
    }
}

/// A WF's menu, `meta.context.wf.menu` (docs/wf-orchestration-v1.md §5.3.1): the options an agent
/// chooses one of, each with the params its answer gives, and `none`, which every menu has. The
/// agent answers one object, `{"reply", "option", "params"}` (`MenuContract::answer`): the WF
/// delivers the reply and branches on the option and its params.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuContract {
    /// A fixed text that gives the choice its context.
    pub help: Option<String>,
    /// The menu's options, in its order, then `none`.
    pub options: Vec<MenuOption>,
}

/// One option of a menu.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuOption {
    pub name: String,
    pub description: String,
    /// The params an answer with this option gives, every one, in their names' order.
    pub params: Vec<MenuParam>,
}

/// One param of a menu's option.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuParam {
    pub name: String,
    pub param_type: MenuParamType,
    pub description: Option<String>,
    /// The values a string param may take: its `enum`.
    pub values: Option<Vec<String>>,
}

/// The type of a menu's param.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuParamType {
    String,
    Boolean,
    Integer,
    Number,
}

impl MenuParamType {
    /// Its name in a menu.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "string" => Some(Self::String),
            "boolean" => Some(Self::Boolean),
            "integer" => Some(Self::Integer),
            "number" => Some(Self::Number),
            _ => None,
        }
    }

    /// Whether `value` is of this type: an integer is a JSON integer, a number any number.
    fn holds(self, value: &Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Boolean => value.is_boolean(),
            Self::Integer => value.is_i64() || value.is_u64(),
            Self::Number => value.is_number(),
        }
    }
}

impl MenuContract {
    /// The menu `menu` describes, checked again with wfcel's limits (docs/wf-orchestration-v1.md
    /// §5.3.6): an optional `help` of 1 to 600 characters, and 1 to 8 options. An option has a
    /// unique name, never `none`, a description of 1 to 200 characters, and up to 8 params. A
    /// param is a `string`, `boolean`, `integer` or `number`, with an optional description of up
    /// to 200 characters and, for a string, an optional `enum` of 1 to 20 values. A name matches
    /// `^[a-z][a-z0-9_]{0,31}$`. A field a menu does not have is refused, and a `null` one is
    /// absent. `none`, with its `reason`, goes last. An invalid menu is `InvalidMenu`, naming the
    /// field.
    pub fn from_value(menu: &Value) -> Result<Self> {
        let menu = menu_object(menu, "menu", "a menu", &["help", "options"])?;
        let help = menu_field(menu, "help")
            .map(|help| menu_text(help, "menu.help", 1, MENU_HELP_MAX_CHARS))
            .transpose()?;
        let options = menu_field(menu, "options")
            .ok_or_else(|| invalid_menu("menu.options", "is required"))?
            .as_array()
            .ok_or_else(|| invalid_menu("menu.options", "must be a list"))?;
        if options.is_empty() || options.len() > MENU_OPTIONS_MAX {
            return Err(invalid_menu(
                "menu.options",
                format!(
                    "must hold 1 to {MENU_OPTIONS_MAX} options, not {}",
                    options.len()
                ),
            ));
        }
        let mut parsed: Vec<MenuOption> = Vec::with_capacity(options.len() + 1);
        for (index, option) in options.iter().enumerate() {
            let path = format!("menu.options[{index}]");
            let option = MenuOption::from_value(option, &path)?;
            if parsed.iter().any(|earlier| earlier.name == option.name) {
                return Err(invalid_menu(
                    format!("{path}.name"),
                    "must be unique in its menu",
                ));
            }
            parsed.push(option);
        }
        parsed.push(MenuOption::none());
        Ok(Self {
            help,
            options: parsed,
        })
    }

    /// The menu as the model reads it, at the end of the WF's block (docs/wf-orchestration-v1.md
    /// §5.3.6): its help, each option with its params, `none` last, and the one line that gives
    /// the answer's shape. An option's params go on one line, or one a line when it has more than
    /// one and a description would run into the next param.
    pub fn prompt(&self) -> String {
        let mut lines = Vec::new();
        if let Some(help) = &self.help {
            lines.push(format!("Help: {help}"));
        }
        lines.push("Options (choose exactly one):".to_string());
        for option in &self.options {
            lines.push(format!("- {}: {}", option.name, option.description));
            let params = option
                .params
                .iter()
                .map(MenuParam::prompt)
                .collect::<Vec<_>>();
            let described = option
                .params
                .iter()
                .any(|param| param.description.is_some());
            if params.len() > 1 && described {
                lines.push("  params:".to_string());
                lines.extend(params.iter().map(|param| format!("    {param}")));
            } else if !params.is_empty() {
                lines.push(format!("  params: {}", params.join(", ")));
            }
        }
        lines.push(MENU_ANSWER_INSTRUCTION.to_string());
        lines.join("\n")
    }

    /// The answer the model's output `raw` gives under this menu (docs/wf-orchestration-v1.md
    /// §5.3.6): the object in `raw`, also inside a fence or among other text, as a structured call
    /// takes it. Its `option` is one of the menu's, or `none`; each param of that option is there,
    /// of its type (an `enum`'s value one of its values); its `reply`, maybe empty, is the
    /// answer's text. The evaluation is `{reply, option, params}` with the option's params alone:
    /// a param it does not declare, or a field besides the three, is dropped with a log line. An
    /// answer that is not that object is `InvalidStructuredOutput`: `not_json`,
    /// `unknown_option:<x>`, `missing_param:<p>` or `type_mismatch:<p>:<type>`.
    pub fn answer(&self, raw: String) -> Result<ContractAnswer> {
        let evaluation = self.check_answer(&raw).inspect_err(|err| {
            warn_unstructured_answer("menu", err, &raw);
        })?;
        let text = evaluation
            .get("reply")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(ContractAnswer {
            text,
            evaluation: Some(evaluation),
        })
    }

    fn check_answer(&self, raw: &str) -> Result<Value> {
        let not_json = || invalid_answer("not_json".to_string());
        let answer = extract_json_value(raw).map_err(|_| not_json())?;
        let answer = answer.as_object().ok_or_else(not_json)?;
        let name = answer_string(answer, "option")?;
        let option = self
            .options
            .iter()
            .find(|option| option.name == name)
            .ok_or_else(|| invalid_answer(format!("unknown_option:{name}")))?;
        let no_params = Map::new();
        let given = match answer.get("params") {
            None | Some(Value::Null) => &no_params,
            Some(Value::Object(given)) => given,
            Some(_) => return Err(invalid_answer("type_mismatch:params:object".to_string())),
        };
        let mut params = Map::new();
        for param in &option.params {
            let value = given
                .get(&param.name)
                .ok_or_else(|| invalid_answer(format!("missing_param:{}", param.name)))?;
            param.check(value)?;
            params.insert(param.name.clone(), value.clone());
        }
        let reply = answer_string(answer, "reply")?;

        let dropped_params = given
            .keys()
            .filter(|name| !params.contains_key(*name))
            .collect::<Vec<_>>();
        let dropped_fields = answer
            .keys()
            .filter(|field| !MENU_ANSWER_FIELDS.contains(&field.as_str()))
            .collect::<Vec<_>>();
        if !dropped_params.is_empty() || !dropped_fields.is_empty() {
            tracing::warn!(
                option = %option.name,
                dropped_params = ?dropped_params,
                dropped_fields = ?dropped_fields,
                "a menu's answer has params its option does not declare, or fields besides reply, option and params: dropped"
            );
        }
        Ok(json!({"reply": reply, "option": option.name, "params": params}))
    }
}

impl MenuOption {
    fn from_value(option: &Value, path: &str) -> Result<Self> {
        let option = menu_object(
            option,
            path,
            "an option",
            &["name", "description", "params"],
        )?;
        let name_path = format!("{path}.name");
        let name = menu_field(option, "name")
            .ok_or_else(|| invalid_menu(&name_path, "is required"))?
            .as_str()
            .ok_or_else(|| invalid_menu(&name_path, "must be a string"))?;
        if !is_menu_name(name) {
            return Err(invalid_menu(&name_path, MENU_NAME_RULE));
        }
        if name == MENU_NONE_OPTION {
            return Err(invalid_menu(
                &name_path,
                "must not be none: every menu has it, for an answer that none of the options fits",
            ));
        }
        let description_path = format!("{path}.description");
        let description = menu_field(option, "description")
            .ok_or_else(|| invalid_menu(&description_path, "is required"))
            .and_then(|description| {
                menu_text(
                    description,
                    &description_path,
                    1,
                    MENU_DESCRIPTION_MAX_CHARS,
                )
            })?;
        let params = match menu_field(option, "params") {
            None => Vec::new(),
            Some(params) => {
                let path = format!("{path}.params");
                let params = params
                    .as_object()
                    .ok_or_else(|| invalid_menu(&path, "must be a JSON object"))?;
                if params.len() > MENU_PARAMS_MAX {
                    return Err(invalid_menu(
                        &path,
                        format!(
                            "must hold at most {MENU_PARAMS_MAX} params, not {}",
                            params.len()
                        ),
                    ));
                }
                params
                    .iter()
                    .map(|(name, param)| {
                        MenuParam::from_value(name, param, &format!("{path}.{name}"))
                    })
                    .collect::<Result<_>>()?
            }
        };
        Ok(Self {
            name: name.to_string(),
            description,
            params,
        })
    }

    /// `none`, which every menu has.
    fn none() -> Self {
        Self {
            name: MENU_NONE_OPTION.to_string(),
            description: "None of the options fits.".to_string(),
            params: vec![MenuParam {
                name: MENU_NONE_REASON.to_string(),
                param_type: MenuParamType::String,
                description: Some("Why none of the options fits.".to_string()),
                values: None,
            }],
        }
    }
}

impl MenuParam {
    fn from_value(name: &str, param: &Value, path: &str) -> Result<Self> {
        if !is_menu_name(name) {
            return Err(invalid_menu(path, MENU_NAME_RULE));
        }
        let param = menu_object(param, path, "a param", &["type", "description", "enum"])?;
        let type_path = format!("{path}.type");
        let param_type = menu_field(param, "type")
            .ok_or_else(|| invalid_menu(&type_path, "is required"))?
            .as_str()
            .and_then(MenuParamType::from_name)
            .ok_or_else(|| {
                invalid_menu(&type_path, "must be string, boolean, integer or number")
            })?;
        let description = menu_field(param, "description")
            .map(|description| {
                menu_text(
                    description,
                    &format!("{path}.description"),
                    0,
                    MENU_DESCRIPTION_MAX_CHARS,
                )
            })
            .transpose()?
            .filter(|description| !description.is_empty());
        let values = menu_field(param, "enum")
            .map(|values| {
                let path = format!("{path}.enum");
                if param_type != MenuParamType::String {
                    return Err(invalid_menu(&path, "is for string params only"));
                }
                let values = values
                    .as_array()
                    .ok_or_else(|| invalid_menu(&path, "must be a list"))?;
                if values.is_empty() || values.len() > MENU_ENUM_MAX {
                    return Err(invalid_menu(
                        &path,
                        format!(
                            "must hold 1 to {MENU_ENUM_MAX} values, not {}",
                            values.len()
                        ),
                    ));
                }
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_string)
                            .ok_or_else(|| invalid_menu(&path, "must hold strings only"))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        Ok(Self {
            name: name.to_string(),
            param_type,
            description,
            values,
        })
    }

    /// The param as the model reads it: `quantity (integer)`, with its values and its description
    /// when it has them: `plan (string, one of: "basic", "pro"): The plan they choose.`
    fn prompt(&self) -> String {
        let mut text = format!("{} ({}", self.name, self.param_type.as_str());
        if let Some(values) = &self.values {
            let values = values
                .iter()
                .map(|value| Value::from(value.as_str()).to_string())
                .collect::<Vec<_>>();
            text.push_str(&format!(", one of: {}", values.join(", ")));
        }
        text.push(')');
        if let Some(description) = &self.description {
            text.push_str(&format!(": {description}"));
        }
        text
    }

    /// Whether `value`, this param's in an answer, is of its type and, with an `enum`, one of its
    /// values.
    fn check(&self, value: &Value) -> Result<()> {
        if !self.param_type.holds(value) {
            return Err(invalid_answer(format!(
                "type_mismatch:{}:{}",
                self.name,
                self.param_type.as_str()
            )));
        }
        if let Some(values) = &self.values {
            if !values.iter().any(|known| value.as_str() == Some(known)) {
                return Err(invalid_answer(format!("type_mismatch:{}:enum", self.name)));
            }
        }
        Ok(())
    }
}

/// A name of an option or a param: `^[a-z][a-z0-9_]{0,31}$`.
fn is_menu_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && name.len() <= MENU_NAME_MAX_CHARS
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `value` at `path` as `what`, an object with `fields` alone.
fn menu_object<'a>(
    value: &'a Value,
    path: &str,
    what: &str,
    fields: &[&str],
) -> Result<&'a Map<String, Value>> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_menu(path, "must be a JSON object"))?;
    if let Some(field) = object
        .keys()
        .find(|field| !fields.contains(&field.as_str()))
    {
        return Err(invalid_menu(
            format!("{path}.{field}"),
            format!("is not a field of {what}, which has {}", fields.join(", ")),
        ));
    }
    Ok(object)
}

/// A menu's field `name`; a `null` one is absent.
fn menu_field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    object.get(name).filter(|value| !value.is_null())
}

/// `value` at `path` as a text of `min` to `max` characters.
fn menu_text(value: &Value, path: &str, min: usize, max: usize) -> Result<String> {
    let text = value
        .as_str()
        .ok_or_else(|| invalid_menu(path, "must be a string"))?;
    let chars = text.chars().count();
    if chars < min || chars > max {
        let limit = if min == 0 {
            format!("at most {max}")
        } else {
            format!("{min} to {max}")
        };
        return Err(invalid_menu(
            path,
            format!("must be {limit} characters, not {chars}"),
        ));
    }
    Ok(text.to_string())
}

fn invalid_menu(path: impl Display, message: impl Display) -> AiSdkError {
    AiSdkError::InvalidMenu {
        detail: format!("{path}: {message}"),
    }
}

/// A menu's answer's field `field`, a string: `missing_param:<field>` without it,
/// `type_mismatch:<field>:string` when it is not one.
fn answer_string<'a>(answer: &'a Map<String, Value>, field: &str) -> Result<&'a str> {
    answer
        .get(field)
        .ok_or_else(|| invalid_answer(format!("missing_param:{field}")))?
        .as_str()
        .ok_or_else(|| invalid_answer(format!("type_mismatch:{field}:string")))
}

fn invalid_answer(detail: String) -> AiSdkError {
    AiSdkError::InvalidStructuredOutput { detail }
}

pub fn response_contract_to_output_schema_spec(contract: &Value) -> Result<OutputSchemaSpec> {
    let obj = contract
        .as_object()
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "response contract must be a JSON object".to_string(),
        })?;

    let kind = obj.get("kind").and_then(Value::as_str).ok_or_else(|| {
        AiSdkError::InvalidResponseContract {
            detail: "response contract.kind is required".to_string(),
        }
    })?;
    if kind != "json_object_v1" {
        return Err(AiSdkError::InvalidResponseContract {
            detail: format!("unsupported response contract kind: {kind}"),
        });
    }

    let strict = obj.get("strict").and_then(Value::as_bool).unwrap_or(false);
    let required = obj
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "response contract.required is required".to_string(),
        })?;
    if required.is_empty() {
        return Err(AiSdkError::InvalidResponseContract {
            detail: "response contract.required must not be empty".to_string(),
        });
    }

    let properties = obj
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "response contract.properties is required".to_string(),
        })?;
    if properties.is_empty() {
        return Err(AiSdkError::InvalidResponseContract {
            detail: "response contract.properties must not be empty".to_string(),
        });
    }

    let mut schema_required = Vec::with_capacity(required.len());
    let mut schema_properties = serde_json::Map::with_capacity(properties.len());

    for field in required {
        let field_name = field
            .as_str()
            .ok_or_else(|| AiSdkError::InvalidResponseContract {
                detail: "response contract.required entries must be strings".to_string(),
            })?;
        if field_name.trim().is_empty() {
            return Err(AiSdkError::InvalidResponseContract {
                detail: "response contract.required must not contain empty names".to_string(),
            });
        }
        if !properties.contains_key(field_name) {
            return Err(AiSdkError::InvalidResponseContract {
                detail: format!(
                    "required field '{field_name}' is missing from response contract.properties"
                ),
            });
        }
        schema_required.push(Value::String(field_name.to_string()));
    }

    for (field_name, field_schema) in properties {
        let field_obj =
            field_schema
                .as_object()
                .ok_or_else(|| AiSdkError::InvalidResponseContract {
                    detail: format!("response contract.properties.{field_name} must be an object"),
                })?;
        let field_type = field_obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| AiSdkError::InvalidResponseContract {
                detail: format!("response contract.properties.{field_name}.type is required"),
            })?;
        match field_type {
            "string" | "boolean" | "integer" | "number" => {}
            other => {
                return Err(AiSdkError::InvalidResponseContract {
                    detail: format!(
                        "unsupported type '{other}' for response contract property '{field_name}'"
                    ),
                })
            }
        }

        let mut translated = serde_json::Map::new();
        translated.insert("type".to_string(), Value::String(field_type.to_string()));

        if let Some(enum_value) = field_obj.get("enum") {
            let enum_items =
                enum_value
                    .as_array()
                    .ok_or_else(|| AiSdkError::InvalidResponseContract {
                        detail: format!(
                            "response contract.properties.{field_name}.enum must be an array"
                        ),
                    })?;
            if field_type != "string" {
                return Err(AiSdkError::InvalidResponseContract {
                    detail: format!("enum is only supported for string property '{field_name}'"),
                });
            }
            if !enum_items.iter().all(|item| item.as_str().is_some()) {
                return Err(AiSdkError::InvalidResponseContract {
                    detail: format!(
                        "response contract.properties.{field_name}.enum entries must be strings"
                    ),
                });
            }
            translated.insert("enum".to_string(), Value::Array(enum_items.clone()));
        }

        schema_properties.insert(field_name.to_string(), Value::Object(translated));
    }

    // The property whose value is the answer's text (`ResponseContract::answer`): one every answer
    // has, a string.
    if let Some(text_field) = obj.get("text_field") {
        let text_field =
            text_field
                .as_str()
                .ok_or_else(|| AiSdkError::InvalidResponseContract {
                    detail: "response contract.text_field must be a string".to_string(),
                })?;
        if !required
            .iter()
            .any(|field| field.as_str() == Some(text_field))
        {
            return Err(AiSdkError::InvalidResponseContract {
                detail: format!(
                    "response contract.text_field '{text_field}' must be listed in response contract.required"
                ),
            });
        }
        let field_type = schema_properties
            .get(text_field)
            .and_then(|schema| schema.get("type"))
            .and_then(Value::as_str);
        if field_type != Some("string") {
            return Err(AiSdkError::InvalidResponseContract {
                detail: format!(
                    "response contract.text_field '{text_field}' must be a property of type string"
                ),
            });
        }
    }

    OutputSchemaSpec::new(
        "final_output",
        Value::Object(serde_json::Map::from_iter([
            ("type".to_string(), Value::String("object".to_string())),
            ("properties".to_string(), Value::Object(schema_properties)),
            ("required".to_string(), Value::Array(schema_required)),
            ("additionalProperties".to_string(), Value::Bool(false)),
        ])),
        strict,
    )
}

fn extract_context_object_field(meta: &Meta, field: &str) -> Result<Option<Value>> {
    let Some(context) = meta.context.as_ref() else {
        return Ok(None);
    };
    let obj = context
        .as_object()
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "meta.context must be a JSON object".to_string(),
        })?;
    match obj.get(field) {
        None => Ok(None),
        Some(value) if value.is_object() => Ok(Some(value.clone())),
        Some(_) => Err(AiSdkError::InvalidResponseContract {
            detail: format!("meta.context.{field} must be a JSON object"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extract_response_envelope_returns_none_when_context_missing() {
        let meta = Meta::default();
        assert_eq!(extract_response_envelope(&meta).expect("ok"), None);
    }

    #[test]
    fn error_reply_gets_distinct_msg_suffix_success_unchanged() {
        let request = Message {
            routing: Routing {
                src: "caller".to_string(),
                src_l2_name: None,
                dst: Destination::Resolve,
                ttl: 8,
                trace_id: "t1".to_string(),
            },
            meta: Meta {
                msg: Some("ask".to_string()),
                ..Meta::default()
            },
            payload: json!({}),
        };
        // gap-1: an error reply is distinguishable at the msg-name level.
        let err_reply = build_reply_message(
            &request,
            "uuid",
            json!({"type":"error","error":{"code":"x"}}),
        );
        assert_eq!(err_reply.meta.msg.as_deref(), Some("ask.error"));
        // a successful reply keeps the request msg-name so the WF's await still matches it.
        let ok_reply = build_reply_message(&request, "uuid", json!({"type":"text","text":"hi"}));
        assert_eq!(ok_reply.meta.msg.as_deref(), Some("ask"));
        // idempotent: re-replying an already-suffixed name does not double it.
        let mut m = err_reply.meta.clone();
        apply_error_reply_msg_suffix(&mut m, &json!({"type":"error"}));
        assert_eq!(m.msg.as_deref(), Some("ask.error"));
    }

    #[test]
    fn extract_response_envelope_rejects_non_object_context() {
        let meta = Meta {
            context: Some(json!("bad")),
            ..Meta::default()
        };
        assert!(matches!(
            extract_response_envelope(&meta),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    #[test]
    fn extract_response_envelope_rejects_non_object_value() {
        let meta = Meta {
            context: Some(json!({"response_envelope":"bad"})),
            ..Meta::default()
        };
        assert!(matches!(
            extract_response_envelope(&meta),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    #[test]
    fn extract_final_response_contract_returns_object() {
        let value = json!({"kind":"json_object_v1","properties":{"ok":{"type":"boolean"}},"required":["ok"]});
        let meta = Meta {
            context: Some(json!({"final_response_contract": value})),
            ..Meta::default()
        };
        assert_eq!(
            extract_final_response_contract(&meta).expect("ok"),
            Some(
                json!({"kind":"json_object_v1","properties":{"ok":{"type":"boolean"}},"required":["ok"]})
            )
        );
    }

    #[test]
    fn sanitize_reply_context_removes_envelope_and_contract_by_default() {
        let sanitized = sanitize_reply_context(
            Some(json!({
                "response_envelope": {"kind":"json_object_v1"},
                "final_response_contract": {"kind":"json_object_v1"},
                "reply_target": {"kind":"demo"},
            })),
            ReplyContextOptions::default(),
        )
        .expect("context should stay object");
        assert!(sanitized.get("response_envelope").is_none());
        assert!(sanitized.get("final_response_contract").is_none());
        assert!(sanitized.get("reply_target").is_some());
    }

    #[test]
    fn sanitize_reply_context_can_preserve_final_response_contract_explicitly() {
        let sanitized = sanitize_reply_context(
            Some(json!({
                "response_envelope": {"kind":"json_object_v1"},
                "final_response_contract": {"kind":"json_object_v1"},
                "reply_target": {"kind":"demo"},
            })),
            ReplyContextOptions {
                preserve_final_response_contract: true,
            },
        )
        .expect("context should stay object");
        assert!(sanitized.get("response_envelope").is_none());
        assert!(sanitized.get("final_response_contract").is_some());
        assert!(sanitized.get("reply_target").is_some());
    }

    #[test]
    fn build_reply_message_runtime_src_can_preserve_final_response_contract_explicitly() {
        let request = Message {
            routing: Routing {
                src: "IO.api@motherbee".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("AI.demo@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-123".to_string(),
            },
            meta: Meta {
                context: Some(json!({
                    "response_envelope": {"kind":"json_object_v1"},
                    "final_response_contract": {"kind":"json_object_v1"},
                    "reply_target": {"kind":"demo"},
                })),
                ..Meta::default()
            },
            payload: json!({}),
        };

        let reply = build_reply_message_runtime_src_with_options(
            &request,
            json!({"ok":true}),
            ReplyContextOptions {
                preserve_final_response_contract: true,
            },
        );

        let context = reply.meta.context.expect("reply context");
        assert!(context.get("response_envelope").is_none());
        assert!(context.get("final_response_contract").is_some());
        assert!(context.get("reply_target").is_some());
    }

    #[test]
    fn response_contract_to_output_schema_spec_translates_json_object_v1() {
        let spec = response_contract_to_output_schema_spec(&json!({
            "kind":"json_object_v1",
            "strict": true,
            "required":["success","human_message"],
            "properties":{
                "success":{"type":"boolean"},
                "human_message":{"type":"string"},
                "error_code":{"type":"string","enum":["missing_data","unknown"]}
            }
        }))
        .expect("contract should translate");
        assert_eq!(spec.name(), "final_output");
        assert!(spec.strict());
        assert_eq!(spec.json_schema()["type"], "object");
        assert_eq!(
            spec.json_schema()["required"],
            json!(["success", "human_message"])
        );
        assert_eq!(
            spec.json_schema()["properties"]["error_code"]["enum"],
            json!(["missing_data", "unknown"])
        );
        assert_eq!(spec.json_schema()["additionalProperties"], false);
    }

    #[test]
    fn response_contract_to_output_schema_spec_rejects_unsupported_kind() {
        assert!(matches!(
            response_contract_to_output_schema_spec(&json!({
                "kind":"array_v1",
                "required":["ok"],
                "properties":{"ok":{"type":"boolean"}}
            })),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    #[test]
    fn response_contract_to_output_schema_spec_rejects_missing_property_for_required() {
        assert!(matches!(
            response_contract_to_output_schema_spec(&json!({
                "kind":"json_object_v1",
                "required":["ok"],
                "properties":{"other":{"type":"boolean"}}
            })),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    #[test]
    fn resolve_response_envelope_contract_reads_and_translates() {
        let meta = Meta {
            context: Some(json!({
                "response_envelope":{
                    "kind":"json_object_v1",
                    "required":["ok"],
                    "properties":{"ok":{"type":"boolean"}}
                }
            })),
            ..Meta::default()
        };
        let contract = resolve_response_envelope_contract(&meta)
            .expect("ok")
            .expect("contract should exist");
        assert_eq!(
            contract.output_schema.json_schema()["properties"]["ok"]["type"],
            "boolean"
        );
        assert_eq!(contract.text_field, None);
    }

    #[test]
    fn resolve_final_response_contract_output_schema_reads_and_translates() {
        let meta = Meta {
            context: Some(json!({
                "final_response_contract":{
                    "kind":"json_object_v1",
                    "required":["status"],
                    "properties":{"status":{"type":"string","enum":["ok","error"]}}
                }
            })),
            ..Meta::default()
        };
        let spec = resolve_final_response_contract_output_schema(&meta)
            .expect("ok")
            .expect("schema should exist");
        assert_eq!(spec.json_schema()["properties"]["status"]["type"], "string");
        assert_eq!(
            spec.json_schema()["properties"]["status"]["enum"],
            json!(["ok", "error"])
        );
    }

    #[test]
    fn response_contract_to_output_schema_spec_rejects_unsupported_property_type() {
        assert!(matches!(
            response_contract_to_output_schema_spec(&json!({
                "kind":"json_object_v1",
                "required":["items"],
                "properties":{"items":{"type":"array"}}
            })),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    #[test]
    fn response_contract_to_output_schema_spec_rejects_non_string_enum_values() {
        assert!(matches!(
            response_contract_to_output_schema_spec(&json!({
                "kind":"json_object_v1",
                "required":["status"],
                "properties":{"status":{"type":"string","enum":["ok", 1]}}
            })),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    /// A WF's block and an evaluation are the request's: an answer carries neither back, only its
    /// own evaluation.
    #[test]
    fn a_reply_never_carries_the_requests_wf_block_or_evaluation() {
        let request = Message {
            routing: Routing {
                src: "WF.sales.invoice@worker1".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("AI.sales@worker1".to_string()),
                ttl: 16,
                trace_id: "trace-wf".to_string(),
            },
            meta: Meta {
                context: Some(json!({
                    "wf": {
                        "trail": ["IO.api@motherbee", "WF.sales.invoice@worker1"],
                        "vars": {"order": {"quantity": 10}}
                    },
                    "evaluation": {"area": "invoice"},
                    "io": {"channel": "api"},
                })),
                ..Meta::default()
            },
            payload: json!({}),
        };

        let mut reply =
            build_reply_message_runtime_src(&request, json!({"type":"text","content":"hola"}));
        assert_eq!(reply.meta.context, Some(json!({"io": {"channel": "api"}})));

        // Its own evaluation goes beside the rest of its context.
        set_reply_evaluation(&mut reply, json!({"area": "sales"}));
        assert_eq!(
            reply.meta.context,
            Some(json!({"io": {"channel": "api"}, "evaluation": {"area": "sales"}}))
        );
        let mut bare = build_reply_message_runtime_src(
            &Message {
                meta: Meta::default(),
                ..request
            },
            json!({"type":"text","content":"hola"}),
        );
        set_reply_evaluation(&mut bare, json!({"area": "sales"}));
        assert_eq!(
            bare.meta.context,
            Some(json!({"evaluation": {"area": "sales"}}))
        );
    }

    /// A contract whose answer has a text, `reply`, an `area`, a `quantity` and an optional
    /// `note`, with `text_field` as given.
    fn text_field_contract(text_field: Value) -> Value {
        json!({
            "kind": "json_object_v1",
            "required": ["reply", "area", "quantity"],
            "properties": {
                "reply": {"type": "string"},
                "area": {"type": "string", "enum": ["sales", "invoice"]},
                "quantity": {"type": "integer"},
                "note": {"type": "string"}
            },
            "text_field": text_field
        })
    }

    #[test]
    fn a_contracts_text_field_is_a_required_string_property() {
        let contract = ResponseContract::from_value(&text_field_contract(json!("reply")))
            .expect("a valid contract");
        assert_eq!(contract.text_field.as_deref(), Some("reply"));
        // The model's schema has the properties alone.
        assert_eq!(
            contract.output_schema,
            response_contract_to_output_schema_spec(&text_field_contract(json!("reply")))
                .expect("its schema")
        );
        assert!(contract
            .output_schema
            .json_schema()
            .get("text_field")
            .is_none());

        for (text_field, detail) in [
            (json!(7), "response contract.text_field must be a string"),
            (json!(null), "response contract.text_field must be a string"),
            (
                json!("note"),
                "response contract.text_field 'note' must be listed in response contract.required",
            ),
            (
                json!("missing"),
                "response contract.text_field 'missing' must be listed in response contract.required",
            ),
            (
                json!("quantity"),
                "response contract.text_field 'quantity' must be a property of type string",
            ),
        ] {
            let contract = text_field_contract(text_field.clone());
            for err in [
                ResponseContract::from_value(&contract).expect_err("an invalid text_field"),
                response_contract_to_output_schema_spec(&contract)
                    .expect_err("an invalid text_field"),
            ] {
                assert!(
                    matches!(&err, AiSdkError::InvalidResponseContract { detail: got } if got == detail),
                    "text_field {text_field}: {err}"
                );
            }
        }
    }

    #[test]
    fn an_answer_under_a_text_field_is_that_field_with_the_whole_object() {
        let contract =
            ResponseContract::from_value(&text_field_contract(json!("reply"))).expect("contract");
        let object =
            json!({"reply": "Te paso con facturación.", "area": "invoice", "quantity": 10});

        let answer = contract.answer(object.to_string()).expect("an answer");
        assert_eq!(
            answer,
            ContractAnswer {
                text: "Te paso con facturación.".to_string(),
                evaluation: Some(object.clone()),
            }
        );
        // In a fence or among other text, as a structured call takes it.
        let fenced = contract
            .answer(format!("Listo:\n```json\n{object}\n```"))
            .expect("an answer");
        assert_eq!(fenced, answer);
        // A blank text is still the answer.
        let blank = contract
            .answer(r#"{"reply": "", "area": "invoice", "quantity": 1}"#.to_string())
            .expect("an answer");
        assert_eq!(blank.text, "");

        // Without a text_field, the output as it is, unchecked.
        let mut plain = text_field_contract(json!(null));
        plain
            .as_object_mut()
            .expect("an object")
            .remove("text_field");
        let plain = ResponseContract::from_value(&plain).expect("contract");
        assert_eq!(plain.text_field, None);
        assert_eq!(
            plain.answer("not json".to_string()).expect("an answer"),
            ContractAnswer {
                text: "not json".to_string(),
                evaluation: None,
            }
        );
    }

    #[test]
    fn an_output_that_is_not_the_contracts_object_is_invalid_structured_output() {
        let contract =
            ResponseContract::from_value(&text_field_contract(json!("reply"))).expect("contract");
        for raw in [
            "",
            "Te paso con facturación.",
            r#"["Te paso con facturación."]"#,
            r#"{"area": "invoice", "quantity": 10}"#,
            r#"{"reply": 7, "area": "invoice", "quantity": 10}"#,
            r#"{"reply": "ok", "area": "refunds", "quantity": 10}"#,
            r#"{"reply": "ok", "area": "invoice", "quantity": 10, "extra": true}"#,
        ] {
            let err = contract
                .answer(raw.to_string())
                .expect_err("not the contract's object");
            assert!(
                matches!(err, AiSdkError::InvalidStructuredOutput { .. }),
                "{raw}: {err}"
            );
        }
    }

    /// Sales' menu of docs/wf-orchestration-v1.md §5.3.1, as a WF writes it.
    fn sales_menu() -> Value {
        json!({
            "help": "The person is buying licenses. Once they accept the quote, invoicing issues the invoice; a discount above 10% needs a supervisor's approval.",
            "options": [
                {"name": "answer", "description": "You answer the person yourself."},
                {"name": "invoice", "description": "They accepted the quote and ask for the invoice.",
                 "params": {"product": {"type": "string"}, "quantity": {"type": "integer"}, "unit_price": {"type": "number"}}},
                {"name": "consult", "description": "A supervisor must approve something before you answer.",
                 "params": {"question": {"type": "string", "description": "What you ask, with the data to decide."}}}
            ]
        })
    }

    fn sales_menu_contract() -> MenuContract {
        MenuContract::from_value(&sales_menu()).expect("a valid menu")
    }

    /// `sales_menu` with `change` made to it.
    fn sales_menu_with(change: impl FnOnce(&mut Value)) -> Value {
        let mut menu = sales_menu();
        change(&mut menu);
        menu
    }

    /// Why `menu` is refused.
    fn menu_refusal(menu: &Value) -> String {
        match MenuContract::from_value(menu) {
            Err(AiSdkError::InvalidMenu { detail }) => detail,
            other => panic!("{menu}: {other:?}"),
        }
    }

    fn param(name: &str, param_type: MenuParamType, description: Option<&str>) -> MenuParam {
        MenuParam {
            name: name.to_string(),
            param_type,
            description: description.map(str::to_string),
            values: None,
        }
    }

    /// A menu is its options as written, its params in their names' order, then `none`.
    #[test]
    fn a_menu_is_its_options_then_none() {
        let menu = sales_menu_contract();
        assert_eq!(
            menu.help.as_deref(),
            sales_menu()["help"].as_str(),
            "the help as written"
        );
        let option = |name: &str, description: &str, params: Vec<MenuParam>| MenuOption {
            name: name.to_string(),
            description: description.to_string(),
            params,
        };
        assert_eq!(
            menu.options,
            vec![
                option("answer", "You answer the person yourself.", Vec::new()),
                option(
                    "invoice",
                    "They accepted the quote and ask for the invoice.",
                    vec![
                        param("product", MenuParamType::String, None),
                        param("quantity", MenuParamType::Integer, None),
                        param("unit_price", MenuParamType::Number, None),
                    ]
                ),
                option(
                    "consult",
                    "A supervisor must approve something before you answer.",
                    vec![param(
                        "question",
                        MenuParamType::String,
                        Some("What you ask, with the data to decide.")
                    )]
                ),
                option(
                    "none",
                    "None of the options fits.",
                    vec![param(
                        "reason",
                        MenuParamType::String,
                        Some("Why none of the options fits.")
                    )]
                ),
            ]
        );
    }

    /// The menu as the model reads it, as docs/wf-orchestration-v1.md §5.3.6 shows it.
    #[test]
    fn a_menu_reads_as_the_design_shows_it() {
        assert_eq!(
            sales_menu_contract().prompt(),
            "Help: The person is buying licenses. Once they accept the quote, invoicing issues the invoice; a discount above 10% needs a supervisor's approval.\n\
             Options (choose exactly one):\n\
             - answer: You answer the person yourself.\n\
             - invoice: They accepted the quote and ask for the invoice.\n\
             \x20 params: product (string), quantity (integer), unit_price (number)\n\
             - consult: A supervisor must approve something before you answer.\n\
             \x20 params: question (string): What you ask, with the data to decide.\n\
             - none: None of the options fits.\n\
             \x20 params: reason (string): Why none of the options fits.\n\
             Answer with only one JSON object: {\"reply\": \"<text for the person, may be empty>\", \"option\": \"<one name above>\", \"params\": {<that option's params>}}"
        );

        // Without help; an enum's values; params with descriptions, one a line.
        let menu = MenuContract::from_value(&json!({"options": [
            {"name": "quote", "description": "Quote a plan.", "params": {
                "plan": {"type": "string", "enum": ["basic", "pro"], "description": "The plan, as they say it."},
                "seats": {"type": "integer"}
            }},
            {"name": "renew", "description": "Renew a plan.", "params": {
                "plan": {"type": "string", "enum": ["basic", "pro"]},
                "annual": {"type": "boolean"}
            }}
        ]}))
        .expect("a valid menu");
        assert_eq!(
            menu.prompt(),
            "Options (choose exactly one):\n\
             - quote: Quote a plan.\n\
             \x20 params:\n\
             \x20   plan (string, one of: \"basic\", \"pro\"): The plan, as they say it.\n\
             \x20   seats (integer)\n\
             - renew: Renew a plan.\n\
             \x20 params: annual (boolean), plan (string, one of: \"basic\", \"pro\")\n\
             - none: None of the options fits.\n\
             \x20 params: reason (string): Why none of the options fits.\n\
             Answer with only one JSON object: {\"reply\": \"<text for the person, may be empty>\", \"option\": \"<one name above>\", \"params\": {<that option's params>}}"
        );
    }

    /// ai.generic checks a menu again with wfcel's limits, and names what it refuses.
    #[test]
    fn a_menu_is_checked_with_wfcels_limits() {
        let long = |c: &str, n: usize| c.repeat(n);
        let nine_options = (0..9)
            .map(|i| json!({"name": format!("option_{i}"), "description": "One."}))
            .collect::<Vec<_>>();
        let nine_params = (0..9)
            .map(|i| (format!("p{i}"), json!({"type": "string"})))
            .collect::<Map<String, Value>>();
        let many_values = (0..21).map(|i| format!("v{i}")).collect::<Vec<_>>();
        for (menu, detail) in [
            (json!("answer"), "menu: must be a JSON object"),
            (
                sales_menu_with(|m| m["title"] = json!("Sales")),
                "menu.title: is not a field of a menu, which has help, options",
            ),
            (
                sales_menu_with(|m| m["help"] = json!("")),
                "menu.help: must be 1 to 600 characters, not 0",
            ),
            (
                sales_menu_with(|m| m["help"] = json!(long("ñ", 601))),
                "menu.help: must be 1 to 600 characters, not 601",
            ),
            (
                sales_menu_with(|m| m["help"] = json!({"$ref": "vars.help"})),
                "menu.help: must be a string",
            ),
            (json!({"help": "Sales."}), "menu.options: is required"),
            (
                sales_menu_with(|m| m["options"] = json!({})),
                "menu.options: must be a list",
            ),
            (
                sales_menu_with(|m| m["options"] = json!([])),
                "menu.options: must hold 1 to 8 options, not 0",
            ),
            (
                sales_menu_with(|m| m["options"] = json!(nine_options)),
                "menu.options: must hold 1 to 8 options, not 9",
            ),
            (
                sales_menu_with(|m| m["options"][0] = json!("answer")),
                "menu.options[0]: must be a JSON object",
            ),
            (
                sales_menu_with(|m| m["options"][0]["label"] = json!("Answer")),
                "menu.options[0].label: is not a field of an option, which has name, description, params",
            ),
            (
                sales_menu_with(|m| m["options"][0] = json!({"description": "One."})),
                "menu.options[0].name: is required",
            ),
            (
                sales_menu_with(|m| m["options"][0]["name"] = json!(7)),
                "menu.options[0].name: must be a string",
            ),
            (
                sales_menu_with(|m| m["options"][2]["name"] = json!("invoice")),
                "menu.options[2].name: must be unique in its menu",
            ),
            (
                sales_menu_with(|m| m["options"][0]["name"] = json!("none")),
                "menu.options[0].name: must not be none: every menu has it, for an answer that none of the options fits",
            ),
            (
                sales_menu_with(|m| {
                    m["options"][0] = json!({"name": "answer"});
                }),
                "menu.options[0].description: is required",
            ),
            (
                sales_menu_with(|m| m["options"][0]["description"] = json!("")),
                "menu.options[0].description: must be 1 to 200 characters, not 0",
            ),
            (
                sales_menu_with(|m| m["options"][0]["description"] = json!(long("é", 201))),
                "menu.options[0].description: must be 1 to 200 characters, not 201",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"] = json!([])),
                "menu.options[1].params: must be a JSON object",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"] = json!(nine_params)),
                "menu.options[1].params: must hold at most 8 params, not 9",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["Product"] = json!({"type": "string"})),
                "menu.options[1].params.Product: must match ^[a-z][a-z0-9_]{0,31}$",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["product"] = json!("string")),
                "menu.options[1].params.product: must be a JSON object",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["product"] = json!({"description": "The product."})),
                "menu.options[1].params.product.type: is required",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["product"]["type"] = json!("array")),
                "menu.options[1].params.product.type: must be string, boolean, integer or number",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["product"]["default"] = json!("Pro")),
                "menu.options[1].params.product.default: is not a field of a param, which has type, description, enum",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["product"]["description"] = json!(long("x", 201))),
                "menu.options[1].params.product.description: must be at most 200 characters, not 201",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["quantity"]["enum"] = json!(["10"])),
                "menu.options[1].params.quantity.enum: is for string params only",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["product"]["enum"] = json!("Pro")),
                "menu.options[1].params.product.enum: must be a list",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["product"]["enum"] = json!([])),
                "menu.options[1].params.product.enum: must hold 1 to 20 values, not 0",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["product"]["enum"] = json!(many_values)),
                "menu.options[1].params.product.enum: must hold 1 to 20 values, not 21",
            ),
            (
                sales_menu_with(|m| m["options"][1]["params"]["product"]["enum"] = json!(["Pro", 1])),
                "menu.options[1].params.product.enum: must hold strings only",
            ),
        ] {
            assert_eq!(menu_refusal(&menu), detail, "{menu}");
        }

        // A name: a lowercase letter, then lowercase letters, digits and _, 32 at most.
        for name in [
            "Answer",
            "1st",
            "_answer",
            "ask-boss",
            "ask boss",
            "",
            "pregúntale",
        ] {
            let menu = sales_menu_with(|m| m["options"][0]["name"] = json!(name));
            assert_eq!(
                menu_refusal(&menu),
                "menu.options[0].name: must match ^[a-z][a-z0-9_]{0,31}$",
                "{name}"
            );
        }
        let longest = format!("a{}", long("b", 31));
        for name in ["a", "ask_2nd", longest.as_str()] {
            let menu = sales_menu_with(|m| m["options"][0]["name"] = json!(name));
            assert!(MenuContract::from_value(&menu).is_ok(), "{name}");
        }
        let too_long = sales_menu_with(|m| m["options"][0]["name"] = json!(format!("{longest}c")));
        assert_eq!(
            menu_refusal(&too_long),
            "menu.options[0].name: must match ^[a-z][a-z0-9_]{0,31}$"
        );

        // At the limits, counted in characters, not bytes.
        let eight_options = (0..8)
            .map(|i| json!({"name": format!("option_{i}"), "description": long("é", 200)}))
            .collect::<Vec<_>>();
        let eight_params = (0..8)
            .map(|i| {
                (
                    format!("p{i}"),
                    json!({"type": "string", "description": long("é", 200)}),
                )
            })
            .collect::<Map<String, Value>>();
        let twenty_values = (0..20).map(|i| format!("v{i}")).collect::<Vec<_>>();
        let menu = MenuContract::from_value(&json!({
            "help": long("ñ", 600),
            "options": eight_options,
        }))
        .expect("8 options, a help of 600 characters");
        assert_eq!(menu.options.len(), 9, "and none");
        let menu = MenuContract::from_value(&sales_menu_with(|m| {
            m["options"][1]["params"] = json!(eight_params);
            m["options"][2]["params"]["question"]["enum"] = json!(twenty_values);
        }))
        .expect("8 params, 20 values");
        assert_eq!(menu.options[1].params.len(), 8);
        assert_eq!(
            menu.options[2].params[0].values.as_ref().map(Vec::len),
            Some(20)
        );

        // A null field is an absent one, and an empty description none.
        let menu = MenuContract::from_value(&json!({
            "help": null,
            "options": [
                {"name": "answer", "description": "You answer.", "params": null},
                {"name": "consult", "description": "Ask.", "params": {
                    "question": {"type": "string", "description": null, "enum": null},
                    "urgent": {"type": "boolean", "description": ""}
                }}
            ]
        }))
        .expect("a valid menu");
        assert_eq!(menu.help, None);
        assert_eq!(menu.options[0].params, Vec::new());
        assert_eq!(
            menu.options[1].params,
            vec![
                param("question", MenuParamType::String, None),
                param("urgent", MenuParamType::Boolean, None),
            ]
        );
    }

    #[test]
    fn a_menus_answer_is_its_reply_and_the_checked_object() {
        let menu = sales_menu_contract();
        let invoice = json!({
            "reply": "Te paso la factura.",
            "option": "invoice",
            "params": {"product": "Pro", "quantity": 10, "unit_price": 49.5}
        });
        let answer = menu.answer(invoice.to_string()).expect("an answer");
        assert_eq!(
            answer,
            ContractAnswer {
                text: "Te paso la factura.".to_string(),
                evaluation: Some(invoice.clone()),
            }
        );
        // In a fence, or followed by text, as a structured call takes it.
        for raw in [
            format!("```json\n{invoice}\n```"),
            format!("```json\n{invoice}\n```\n¿Algo más?"),
            format!("{invoice}\nListo."),
            format!("Elijo invoice: {invoice}"),
        ] {
            assert_eq!(menu.answer(raw.clone()).expect(&raw), answer, "{raw}");
        }

        // An option without params: none given, empty or null; a blank reply is still the text.
        for raw in [
            r#"{"reply": "", "option": "answer"}"#,
            r#"{"reply": "", "option": "answer", "params": {}}"#,
            r#"{"reply": "", "option": "answer", "params": null}"#,
        ] {
            assert_eq!(
                menu.answer(raw.to_string()).expect(raw),
                ContractAnswer {
                    text: String::new(),
                    evaluation: Some(json!({"reply": "", "option": "answer", "params": {}})),
                },
                "{raw}"
            );
        }

        // none, with its reason.
        let none = json!({"reply": "Lo consulto.", "option": "none", "params": {"reason": "They ask for a refund."}});
        assert_eq!(
            menu.answer(none.to_string()).expect("an answer").evaluation,
            Some(none)
        );

        // A param the option does not declare, and a field besides the three, are dropped.
        let answer = menu
            .answer(
                json!({
                    "reply": "Lo consulto.",
                    "option": "consult",
                    "params": {"question": "¿20% para 10 Pro?", "discount": 20},
                    "confidence": 0.9
                })
                .to_string(),
            )
            .expect("an answer");
        assert_eq!(
            answer.evaluation,
            Some(json!({
                "reply": "Lo consulto.",
                "option": "consult",
                "params": {"question": "¿20% para 10 Pro?"}
            }))
        );

        // An integer is any JSON integer; a number, any number; an enum's value, one of its own.
        let menu = MenuContract::from_value(&json!({"options": [
            {"name": "quote", "description": "Quote a plan.", "params": {
                "plan": {"type": "string", "enum": ["basic", "pro"]},
                "seats": {"type": "integer"},
                "budget": {"type": "number"},
                "annual": {"type": "boolean"}
            }}
        ]}))
        .expect("a valid menu");
        for params in [
            json!({"plan": "pro", "seats": u64::MAX, "budget": 100, "annual": true}),
            json!({"plan": "basic", "seats": -3, "budget": 99.9, "annual": false}),
        ] {
            let object = json!({"reply": "", "option": "quote", "params": params});
            assert_eq!(
                menu.answer(object.to_string())
                    .expect("an answer")
                    .evaluation,
                Some(object)
            );
        }
    }

    #[test]
    fn an_answer_off_the_menu_is_invalid_structured_output() {
        let menu = sales_menu_contract();
        let invoice = |params: Value| {
            json!({"reply": "Listo.", "option": "invoice", "params": params}).to_string()
        };
        for (raw, detail) in [
            (String::new(), "not_json"),
            ("Te paso con facturación.".to_string(), "not_json"),
            (r#"["invoice"]"#.to_string(), "not_json"),
            (
                r#"{"reply": "Listo.", "option": "invoice""#.to_string(),
                "not_json",
            ),
            ("```json\n```".to_string(), "not_json"),
            (
                r#"{"reply": "Listo.", "option": "refund"}"#.to_string(),
                "unknown_option:refund",
            ),
            (
                r#"{"reply": "Listo.", "option": "Invoice"}"#.to_string(),
                "unknown_option:Invoice",
            ),
            (r#"{"reply": "Listo."}"#.to_string(), "missing_param:option"),
            (
                r#"{"reply": "Listo.", "option": 2}"#.to_string(),
                "type_mismatch:option:string",
            ),
            (
                r#"{"reply": "Listo.", "option": "invoice"}"#.to_string(),
                "missing_param:product",
            ),
            (
                invoice(json!({"product": "Pro", "quantity": 10})),
                "missing_param:unit_price",
            ),
            (
                invoice(json!({"product": null, "quantity": 10, "unit_price": 49})),
                "type_mismatch:product:string",
            ),
            (
                invoice(json!({"product": "Pro", "quantity": "10", "unit_price": 49})),
                "type_mismatch:quantity:integer",
            ),
            (
                invoice(json!({"product": "Pro", "quantity": 10.5, "unit_price": 49})),
                "type_mismatch:quantity:integer",
            ),
            (
                invoice(json!({"product": "Pro", "quantity": 10.0, "unit_price": 49})),
                "type_mismatch:quantity:integer",
            ),
            (
                invoice(json!({"product": "Pro", "quantity": 10, "unit_price": "49"})),
                "type_mismatch:unit_price:number",
            ),
            (
                invoice(json!(["Pro", 10, 49])),
                "type_mismatch:params:object",
            ),
            (
                r#"{"reply": "", "option": "none"}"#.to_string(),
                "missing_param:reason",
            ),
            (
                r#"{"reply": "", "option": "none", "params": {"reason": 7}}"#.to_string(),
                "type_mismatch:reason:string",
            ),
            (r#"{"option": "answer"}"#.to_string(), "missing_param:reply"),
            (
                r#"{"reply": null, "option": "answer"}"#.to_string(),
                "type_mismatch:reply:string",
            ),
            (
                r#"{"reply": 7, "option": "answer"}"#.to_string(),
                "type_mismatch:reply:string",
            ),
        ] {
            match menu.answer(raw.clone()) {
                Err(AiSdkError::InvalidStructuredOutput { detail: got }) => {
                    assert_eq!(got, detail, "{raw}")
                }
                other => panic!("{raw}: {other:?}"),
            }
        }

        let menu = MenuContract::from_value(&json!({"options": [
            {"name": "quote", "description": "Quote a plan.", "params": {
                "plan": {"type": "string", "enum": ["basic", "pro"]},
                "annual": {"type": "boolean"}
            }}
        ]}))
        .expect("a valid menu");
        for (params, detail) in [
            (
                json!({"plan": "gold", "annual": true}),
                "type_mismatch:plan:enum",
            ),
            (
                json!({"plan": 1, "annual": true}),
                "type_mismatch:plan:string",
            ),
            (
                json!({"plan": "pro", "annual": "yes"}),
                "type_mismatch:annual:boolean",
            ),
        ] {
            let raw = json!({"reply": "", "option": "quote", "params": params}).to_string();
            assert!(
                matches!(
                    menu.answer(raw.clone()),
                    Err(AiSdkError::InvalidStructuredOutput { detail: got }) if got == detail
                ),
                "{raw}"
            );
        }
    }

    /// A message asks its answer under a WF's menu or a response contract, never both.
    #[test]
    fn a_message_asks_under_one_contract_at_most() {
        let meta = |context: Value| Meta {
            context: Some(context),
            ..Meta::default()
        };
        let envelope = text_field_contract(json!("reply"));

        assert_eq!(
            resolve_message_contract(&Meta::default()).expect("ok"),
            None
        );
        for context in [
            json!({}),
            json!({"wf": {"trail": ["WF.sales.invoice@worker1"]}}),
            json!({"wf": {"trail": ["WF.sales.invoice@worker1"], "menu": null}}),
            json!({"wf": "WF.sales.invoice@worker1"}),
        ] {
            assert_eq!(
                resolve_message_contract(&meta(context.clone())).expect("ok"),
                None,
                "{context}"
            );
        }

        let contract = resolve_message_contract(&meta(json!({"response_envelope": envelope})))
            .expect("ok")
            .expect("a contract");
        assert_eq!(
            contract,
            MessageContract::Envelope(ResponseContract::from_value(&envelope).expect("contract"))
        );
        assert_eq!(contract.kind(), "envelope");
        assert!(contract.output_schema().is_some());
        assert!(contract.has_evaluation());
        assert_eq!(contract.menu(), None);

        let contract = resolve_message_contract(&meta(json!({
            "wf": {"trail": ["WF.sales.invoice@worker1"], "menu": sales_menu()}
        })))
        .expect("ok")
        .expect("a contract");
        assert_eq!(contract, MessageContract::Menu(sales_menu_contract()));
        assert_eq!(contract.kind(), "menu");
        // No schema for the provider and no instruction for the system prompt: the WF's block
        // carries the menu.
        assert_eq!(contract.output_schema(), None);
        assert!(contract.has_evaluation());
        assert_eq!(contract.menu(), Some(&sales_menu_contract()));

        // Both, whatever each holds.
        for (envelope, menu) in [
            (envelope.clone(), sales_menu()),
            (json!("bad"), json!({"options": []})),
        ] {
            assert!(
                matches!(
                    resolve_message_contract(&meta(json!({
                        "response_envelope": envelope,
                        "wf": {"menu": menu}
                    }))),
                    Err(AiSdkError::ConflictingContracts)
                ),
                "{envelope} {menu}"
            );
        }

        assert!(matches!(
            resolve_message_contract(&meta(json!({"wf": {"menu": {"options": []}}}))),
            Err(AiSdkError::InvalidMenu { detail }) if detail == "menu.options: must hold 1 to 8 options, not 0"
        ));
        assert!(matches!(
            resolve_message_contract(&meta(json!({"response_envelope": "bad"}))),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
        assert!(matches!(
            resolve_message_contract(&Meta {
                context: Some(json!("bad")),
                ..Meta::default()
            }),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }
}
