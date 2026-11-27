//! The source reader (docs/digital-executive-v1-stage-2.md §3, §4): the format every source
//! compiles to, its checks, the templates its configs and documents are written with, and the
//! checks of its blanks' values (§5.1).
//!
//! A source holds:
//! - `blanks`: each with a name and a type that carries its check;
//! - `founds` (the native layer only): the person it founds, its names from the blanks, and its
//!   document;
//! - `nodes`: each with its role, its kind (`AI` or `IO`), its middle name part, its runtime, its
//!   config with the blanks in place, its `work_role`, and, for an agent, its definition's assets;
//! - `vault`: which blank's key goes to which node.
//!
//! Its configs, its document and its assets are templates: an object `{"$blank": "<name>[.<field>]"}`
//! stands for a blank's value (with `"optional": true`, a missing value drops its key), and an
//! object `{"$ref": "<name>"}` for a name or an id SY.deploy makes concrete (`REF_*`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use json_router::agent_assets::{canonical_agent_asset_bytes, sha256_hex};

/// The sources SY.deploy carries, built in (§4: the plain ED's ships with Fluxbee).
pub const NATIVE_SOURCES: &[&str] = &[include_str!("sources/plain-ed-1.json")];

/// The one schema of a source this release reads.
pub const SOURCE_SCHEMA: u64 = 1;

/// A source's sections SY.deploy reads.
const SOURCE_SECTIONS: &[&str] = &[
    "schema",
    "id",
    "version",
    "kind",
    "title",
    "description",
    "blanks",
    "founds",
    "nodes",
    "vault",
];

/// The sections a source may hold one day that SY.deploy cannot build yet (§4, §12): a source
/// with one is refused before any step.
const SECTIONS_NOT_BUILT_YET: &[&str] = &[
    "workflows",
    "wfs",
    "routes",
    "vpns",
    "policy",
    "opa",
    "hives",
    "runtimes",
];

/// The kinds of source the catalog lists (§3.3).
const SOURCE_KINDS: &[&str] = &["ed", "infra", "custom"];

/// The refs every source may name: the grouping's ILK (its person) and short id, the hive and the
/// tenant.
pub const REF_GROUPING_ILK: &str = "grouping.ilk_id";
pub const REF_GROUPING_SHORT_ID: &str = "grouping.short_id";
pub const REF_HIVE: &str = "hive";
pub const REF_TENANT: &str = "tenant_id";

/// A source as read, with its hash: `sha256:` and the SHA-256 of its canonical bytes.
#[derive(Debug, Clone)]
pub struct ParsedSource {
    pub source: Source,
    pub hash: String,
    /// The canonical bytes, what SY.deploy keeps as the source's copy (§3.3).
    pub canonical: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub schema: u64,
    pub id: String,
    pub version: String,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub blanks: Vec<BlankSpec>,
    #[serde(default)]
    pub founds: Option<FoundsSpec>,
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub vault: Vec<VaultSpec>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlankType {
    /// A name: a string of at most `max_bytes` (128 without it), no control characters.
    Name,
    /// A personality: timezone, country_code and primary_language, and optionally region_code,
    /// additional_languages, description, biographical and narrative.
    Personality,
    /// A WhatsApp number: phone_number_id (digits), waba_id, and credential_key, a key of the
    /// blank's `resource_type` in the tenant.
    WhatsappChannel,
    /// A key of the blank's `resource_type` in the tenant.
    VaultKey,
    /// An ILK of the deployment's tenant.
    TenantIlk,
    /// Any JSON value.
    Value,
}

impl BlankType {
    pub fn as_str(self) -> &'static str {
        match self {
            BlankType::Name => "name",
            BlankType::Personality => "personality",
            BlankType::WhatsappChannel => "whatsapp_channel",
            BlankType::VaultKey => "vault_key",
            BlankType::TenantIlk => "tenant_ilk",
            BlankType::Value => "value",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BlankSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub blank_type: BlankType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FoundsSpec {
    pub person: PersonSpec,
    pub document: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PersonSpec {
    pub display_name: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_name: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NodeSpec {
    pub role: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub middle: Option<String>,
    pub runtime: String,
    pub work_role: String,
    /// An agent's AI provider, whose key the tenant must hold (§5.1, `AI_KEY_MISSING`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub config: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<DefinitionSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DefinitionSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handbooks: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VaultSpec {
    pub key: Value,
    pub resource_type: String,
    /// The role of the node the key is assigned to.
    pub to: String,
}

/// The node kinds this stage builds.
const NODE_KINDS: &[&str] = &["AI", "IO"];

/// The roles a part plays for its person (§1.1).
pub const WORK_ROLES: &[&str] = &["conscious", "channel", "agent"];

/// The AI providers an agent may name.
const AI_PROVIDERS: &[&str] = &["anthropic", "openai"];

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.' | b'_')
        })
}

fn is_name_part(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// A JSON value with every object's keys sorted, at every depth, serialized compact: the bytes a
/// hash is taken of, as SY.identity takes a person's document's (`canonical_json`) and Archi an
/// asset's.
pub fn canonical_bytes(value: &Value) -> Vec<u8> {
    canonical_agent_asset_bytes(value).unwrap_or_else(|_| value.to_string().into_bytes())
}

/// Reads a source and checks it: its sections, its fields and the names its templates use. Any
/// error names what is wrong, never a value.
pub fn parse_source(raw: &str) -> Result<ParsedSource, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|err| format!("the source is not JSON: {err}"))?;
    let Some(sections) = value.as_object() else {
        return Err("a source is a JSON object".to_string());
    };
    for key in sections.keys() {
        if SECTIONS_NOT_BUILT_YET.contains(&key.as_str()) {
            return Err(format!(
                "SY.deploy cannot build a source's '{key}' yet: the source is refused before any step"
            ));
        }
        if !SOURCE_SECTIONS.contains(&key.as_str()) {
            return Err(format!("a source has no section '{key}'"));
        }
    }
    let source: Source = serde_json::from_value(value.clone())
        .map_err(|err| format!("the source does not read: {err}"))?;
    check_source(&source)?;
    let canonical = canonical_bytes(&value);
    let hash = format!("sha256:{}", sha256_hex(&canonical));
    Ok(ParsedSource {
        source,
        hash,
        canonical,
    })
}

fn check_source(source: &Source) -> Result<(), String> {
    if source.schema != SOURCE_SCHEMA {
        return Err(format!("a source's schema is {SOURCE_SCHEMA}"));
    }
    if !is_token(&source.id) {
        return Err("a source's id is 1 to 64 of [a-z0-9._-]".to_string());
    }
    if !is_token(&source.version) {
        return Err("a source's version is 1 to 64 of [a-z0-9._-]".to_string());
    }
    if !SOURCE_KINDS.contains(&source.kind.as_str()) {
        return Err(format!("a source's kind is one of {SOURCE_KINDS:?}"));
    }
    if source.title.trim().is_empty() {
        return Err("a source has a title".to_string());
    }
    let mut blank_names = BTreeSet::new();
    for blank in &source.blanks {
        if !is_name_part(&blank.name.replace('_', "")) || blank.name.contains('.') {
            return Err(format!(
                "a blank's name is lowercase letters, digits and '_', not '{}'",
                blank.name
            ));
        }
        if !blank_names.insert(blank.name.as_str()) {
            return Err(format!("the blank '{}' is declared twice", blank.name));
        }
        let needs_resource = matches!(
            blank.blank_type,
            BlankType::WhatsappChannel | BlankType::VaultKey
        );
        match (&blank.resource_type, needs_resource) {
            (None, true) => {
                return Err(format!(
                    "the blank '{}' names the resource_type of its key",
                    blank.name
                ))
            }
            (Some(_), false) => {
                return Err(format!(
                    "the blank '{}' is of a type that takes no resource_type",
                    blank.name
                ))
            }
            (Some(resource), true) if !is_token(resource) => {
                return Err(format!(
                    "the blank '{}' has an invalid resource_type",
                    blank.name
                ))
            }
            _ => {}
        }
        if blank.max_bytes.is_some() && blank.blank_type != BlankType::Name {
            return Err(format!(
                "only a name blank takes max_bytes, not '{}'",
                blank.name
            ));
        }
    }
    if source.nodes.is_empty() {
        return Err("a source deploys at least one node".to_string());
    }
    let mut roles = BTreeMap::new();
    for node in &source.nodes {
        if !is_name_part(&node.role) {
            return Err(format!(
                "a node's role is lowercase letters and digits, not '{}'",
                node.role
            ));
        }
        if roles.insert(node.role.as_str(), node).is_some() {
            return Err(format!("the node role '{}' is declared twice", node.role));
        }
        if !NODE_KINDS.contains(&node.kind.as_str()) {
            return Err(format!(
                "SY.deploy builds nodes of kind {NODE_KINDS:?} in this stage, not '{}'",
                node.kind
            ));
        }
        if let Some(middle) = &node.middle {
            if !is_name_part(middle) {
                return Err(format!(
                    "the node '{}' has a middle name part of lowercase letters and digits",
                    node.role
                ));
            }
        }
        if !is_token(&node.runtime) {
            return Err(format!("the node '{}' names an invalid runtime", node.role));
        }
        if !WORK_ROLES.contains(&node.work_role.as_str()) {
            return Err(format!(
                "the node '{}' has a work_role of {WORK_ROLES:?}",
                node.role
            ));
        }
        if !node.config.is_object() {
            return Err(format!("the node '{}' has a config object", node.role));
        }
        match node.kind.as_str() {
            "AI" => {
                if !node
                    .provider
                    .as_deref()
                    .is_some_and(|provider| AI_PROVIDERS.contains(&provider))
                {
                    return Err(format!(
                        "the agent '{}' names its provider, one of {AI_PROVIDERS:?}",
                        node.role
                    ));
                }
            }
            _ => {
                if node.provider.is_some() || node.definition.is_some() {
                    return Err(format!(
                        "only an agent has a provider and a definition, not the node '{}'",
                        node.role
                    ));
                }
            }
        }
    }
    let refs = known_refs(source);
    let blank_check =
        |template: &Value, what: &str| check_template(template, &blank_names, &refs, what);
    if let Some(founds) = &source.founds {
        if source.kind != "ed" {
            return Err("only a source of kind ed founds a person".to_string());
        }
        blank_check(&founds.person.display_name, "the person's display_name")?;
        if let Some(company) = &founds.person.company_name {
            blank_check(company, "the person's company_name")?;
        }
        if !founds.document.is_object() {
            return Err("the person's document is an object".to_string());
        }
        blank_check(&founds.document, "the person's document")?;
    }
    for node in &source.nodes {
        blank_check(&node.config, &format!("the node '{}' config", node.role))?;
        if let Some(definition) = &node.definition {
            let assets = definition
                .role
                .iter()
                .chain(definition.skills.iter())
                .chain(definition.handbooks.iter())
                .chain(definition.personality.iter());
            for asset in assets {
                // An asset is made before the refs to the assets exist.
                check_template(asset, &blank_names, &asset_free_refs(&refs), "an asset")?;
            }
        }
    }
    for vault in &source.vault {
        if !roles.contains_key(vault.to.as_str()) {
            return Err(format!(
                "a vault entry goes to the node role '{}', which the source does not declare",
                vault.to
            ));
        }
        if !is_token(&vault.resource_type) {
            return Err("a vault entry names an invalid resource_type".to_string());
        }
        blank_check(&vault.key, "a vault entry's key")?;
    }
    Ok(())
}

/// The refs a source's templates may name: the general ones, each node's name and ILK, and the
/// hash of each asset its agents' definitions hold.
fn known_refs(source: &Source) -> BTreeSet<String> {
    let mut refs: BTreeSet<String> = [
        REF_GROUPING_ILK,
        REF_GROUPING_SHORT_ID,
        REF_HIVE,
        REF_TENANT,
    ]
    .iter()
    .map(|name| name.to_string())
    .collect();
    for node in &source.nodes {
        refs.insert(node_name_ref(&node.role));
        refs.insert(node_ilk_ref(&node.role));
        if let Some(definition) = &node.definition {
            if definition.role.is_some() {
                refs.insert(asset_ref(&node.role, "role"));
            }
            if definition.personality.is_some() {
                refs.insert(asset_ref(&node.role, "personality"));
            }
        }
    }
    refs
}

fn asset_free_refs(refs: &BTreeSet<String>) -> BTreeSet<String> {
    refs.iter()
        .filter(|name| !name.starts_with("assets."))
        .cloned()
        .collect()
}

pub fn node_name_ref(role: &str) -> String {
    format!("nodes.{role}.node_name")
}

pub fn node_ilk_ref(role: &str) -> String {
    format!("nodes.{role}.ilk_id")
}

pub fn asset_ref(role: &str, part: &str) -> String {
    format!("assets.{role}.{part}")
}

/// What a template's placeholder is, if it is one.
enum Placeholder<'a> {
    Blank { path: &'a str, optional: bool },
    Ref(&'a str),
}

fn placeholder(value: &Value) -> Result<Option<Placeholder<'_>>, String> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if let Some(path) = object.get("$blank") {
        let path = path.as_str().ok_or("a $blank names a blank, a string")?;
        let optional = match object.get("optional") {
            None => false,
            Some(Value::Bool(optional)) => *optional,
            Some(_) => return Err("a $blank's optional is true or false".to_string()),
        };
        if object
            .keys()
            .any(|key| key != "$blank" && key != "optional")
        {
            return Err("a $blank takes only optional beside it".to_string());
        }
        return Ok(Some(Placeholder::Blank { path, optional }));
    }
    if let Some(name) = object.get("$ref") {
        let name = name.as_str().ok_or("a $ref names a ref, a string")?;
        if object.len() != 1 {
            return Err("a $ref takes nothing beside it".to_string());
        }
        return Ok(Some(Placeholder::Ref(name)));
    }
    if object.keys().any(|key| key.starts_with('$')) {
        return Err("a template's only placeholders are $blank and $ref".to_string());
    }
    Ok(None)
}

fn check_template(
    template: &Value,
    blanks: &BTreeSet<&str>,
    refs: &BTreeSet<String>,
    what: &str,
) -> Result<(), String> {
    match placeholder(template).map_err(|err| format!("{what}: {err}"))? {
        Some(Placeholder::Blank { path, .. }) => {
            let blank = path.split('.').next().unwrap_or_default();
            if !blanks.contains(blank) {
                return Err(format!(
                    "{what} names the blank '{blank}', which the source does not declare"
                ));
            }
            Ok(())
        }
        Some(Placeholder::Ref(name)) => {
            if !refs.contains(name) {
                return Err(format!(
                    "{what} names the ref '{name}', which is none of this source's"
                ));
            }
            Ok(())
        }
        None => match template {
            Value::Object(fields) => fields
                .values()
                .try_for_each(|value| check_template(value, blanks, refs, what)),
            Value::Array(items) => items
                .iter()
                .try_for_each(|value| check_template(value, blanks, refs, what)),
            _ => Ok(()),
        },
    }
}

/// `value` with every null object field dropped, at every depth: a field sent as null is one left
/// out (SY.admin drops the top-level ones before it forwards an order).
pub fn without_nulls(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), without_nulls(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_nulls).collect()),
        other => other.clone(),
    }
}

/// The value at `path` in the blanks: a blank's name, then fields inside it. `None` when it is
/// missing or null.
pub fn blank_value<'a>(blanks: &'a Value, path: &str) -> Option<&'a Value> {
    let mut value = blanks;
    for part in path.split('.') {
        value = value.get(part)?;
    }
    (!value.is_null()).then_some(value)
}

/// Makes a template concrete: each `$blank` from `blanks`, each `$ref` from `refs`. A missing value
/// of an optional `$blank` drops its key (or its item); any other missing value is an error that
/// names it.
pub fn resolve_template(
    template: &Value,
    blanks: &Value,
    refs: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    resolve_inner(template, blanks, refs)?
        .ok_or_else(|| "a template resolved to nothing".to_string())
}

fn resolve_inner(
    template: &Value,
    blanks: &Value,
    refs: &BTreeMap<String, Value>,
) -> Result<Option<Value>, String> {
    match placeholder(template)? {
        Some(Placeholder::Blank { path, optional }) => match blank_value(blanks, path) {
            Some(value) => Ok(Some(value.clone())),
            None if optional => Ok(None),
            None => Err(format!("the blank '{path}' has no value")),
        },
        Some(Placeholder::Ref(name)) => refs
            .get(name)
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("the ref '{name}' has no value yet")),
        None => match template {
            Value::Object(fields) => {
                let mut out = serde_json::Map::new();
                for (key, value) in fields {
                    if let Some(value) = resolve_inner(value, blanks, refs)? {
                        out.insert(key.clone(), value);
                    }
                }
                Ok(Some(Value::Object(out)))
            }
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    if let Some(value) = resolve_inner(item, blanks, refs)? {
                        out.push(value);
                    }
                }
                Ok(Some(Value::Array(out)))
            }
            other => Ok(Some(other.clone())),
        },
    }
}

/// A blank's value refused: which blank, and why, never the value (§5.1, `BLANK_INVALID`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlankError {
    pub blank: String,
    pub reason: String,
}

impl std::fmt::Display for BlankError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the blank '{}' {}", self.blank, self.reason)
    }
}

fn blank_error(blank: &str, reason: impl Into<String>) -> BlankError {
    BlankError {
        blank: blank.to_string(),
        reason: reason.into(),
    }
}

/// The checks of the blanks' values (§5.1): every blank the source declares is filled, and nothing
/// more, each checked by its type. The checks that need the tenant's keys or the other deployments
/// are the caller's (`key_requirements`, `whatsapp_numbers`).
pub fn check_blanks(source: &Source, blanks: &Value) -> Result<(), BlankError> {
    let Some(values) = blanks.as_object() else {
        return Err(blank_error("blanks", "is an object of the source's blanks"));
    };
    if let Some(extra) = values
        .keys()
        .find(|key| !source.blanks.iter().any(|blank| &blank.name == *key))
    {
        return Err(blank_error(extra, "is not a blank of this source"));
    }
    for blank in &source.blanks {
        let Some(value) = values.get(&blank.name).filter(|value| !value.is_null()) else {
            return Err(blank_error(&blank.name, "is not filled"));
        };
        check_blank(blank, value)?;
    }
    Ok(())
}

fn check_text(
    blank: &str,
    field: &str,
    value: Option<&Value>,
    max_bytes: usize,
) -> Result<(), BlankError> {
    let what = if field.is_empty() {
        String::new()
    } else {
        format!("'s {field}")
    };
    let Some(text) = value.and_then(Value::as_str) else {
        return Err(blank_error(
            blank,
            format!("{what} is a string").trim_start().to_string(),
        ));
    };
    if text.trim().is_empty() {
        return Err(blank_error(
            blank,
            format!("{what} is empty").trim_start().to_string(),
        ));
    }
    if text.len() > max_bytes {
        return Err(blank_error(
            blank,
            format!("{what} is at most {max_bytes} bytes")
                .trim_start()
                .to_string(),
        ));
    }
    if text.chars().any(char::is_control) {
        return Err(blank_error(
            blank,
            format!("{what} holds a control character")
                .trim_start()
                .to_string(),
        ));
    }
    Ok(())
}

fn only_fields(
    blank: &str,
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Result<(), BlankError> {
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(extra) => Err(blank_error(
            blank,
            format!("takes only {allowed:?}, not '{extra}'"),
        )),
        None => Ok(()),
    }
}

/// The personality's fields (Archi's personality asset's system fields and the rest).
const PERSONALITY_FIELDS: &[&str] = &[
    "timezone",
    "country_code",
    "region_code",
    "primary_language",
    "additional_languages",
    "description",
    "biographical",
    "narrative",
];

const WHATSAPP_FIELDS: &[&str] = &["phone_number_id", "waba_id", "credential_key"];

/// A WhatsApp phone number id's most digits.
const PHONE_NUMBER_ID_MAX: usize = 32;

fn check_blank(blank: &BlankSpec, value: &Value) -> Result<(), BlankError> {
    let name = blank.name.as_str();
    match blank.blank_type {
        BlankType::Name => check_text(name, "", Some(value), blank.max_bytes.unwrap_or(128)),
        BlankType::Personality => {
            let Some(object) = value.as_object() else {
                return Err(blank_error(name, "is an object"));
            };
            only_fields(name, object, PERSONALITY_FIELDS)?;
            for field in ["timezone", "country_code", "primary_language"] {
                check_text(name, field, object.get(field), 64)?;
            }
            if object.contains_key("region_code") {
                check_text(name, "region_code", object.get("region_code"), 64)?;
            }
            if let Some(languages) = object.get("additional_languages") {
                let ok = languages.as_array().is_some_and(|items| {
                    items.len() <= 16
                        && items.iter().all(|item| {
                            item.as_str()
                                .is_some_and(|text| !text.trim().is_empty() && text.len() <= 64)
                        })
                });
                if !ok {
                    return Err(blank_error(
                        name,
                        "'s additional_languages is a list of at most 16 language codes",
                    ));
                }
            }
            if object.contains_key("description") {
                check_text(name, "description", object.get("description"), 4096)?;
            }
            for field in ["biographical", "narrative"] {
                if object.get(field).is_some_and(|value| !value.is_object()) {
                    return Err(blank_error(name, format!("'s {field} is an object")));
                }
            }
            Ok(())
        }
        BlankType::WhatsappChannel => {
            let Some(object) = value.as_object() else {
                return Err(blank_error(name, "is an object"));
            };
            only_fields(name, object, WHATSAPP_FIELDS)?;
            let phone = object
                .get("phone_number_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if phone.is_empty()
                || phone.len() > PHONE_NUMBER_ID_MAX
                || !phone.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(blank_error(
                    name,
                    format!("'s phone_number_id is 1 to {PHONE_NUMBER_ID_MAX} digits"),
                ));
            }
            check_text(name, "waba_id", object.get("waba_id"), 64)?;
            check_vault_key(name, "credential_key", object.get("credential_key"))
        }
        BlankType::VaultKey => check_vault_key(name, "", Some(value)),
        BlankType::TenantIlk => {
            if value.as_str().is_some_and(super::ids::is_ilk_id) {
                Ok(())
            } else {
                Err(blank_error(name, "is an ILK, ilk:<uuid>"))
            }
        }
        BlankType::Value => Ok(()),
    }
}

/// A vault key's name, as SY.vault takes one.
fn check_vault_key(blank: &str, field: &str, value: Option<&Value>) -> Result<(), BlankError> {
    let what = if field.is_empty() {
        "is".to_string()
    } else {
        format!("'s {field} is")
    };
    let key = value.and_then(Value::as_str).unwrap_or_default();
    let first_ok = key
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if key.len() > 256 || !first_ok || !fluxbee_sdk::vault_key_is_valid(key) {
        return Err(blank_error(
            blank,
            format!("{what} the name of a vault key, as SY.vault takes one"),
        ));
    }
    Ok(())
}

/// A key the tenant must hold for a deployment (§5.1): its name and its `resource_type`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyRequirement {
    pub key: String,
    pub resource_type: String,
}

/// The keys the blanks name, each of the resource type its blank says (`CREDENTIAL_MISSING`).
pub fn key_requirements(source: &Source, blanks: &Value) -> Vec<KeyRequirement> {
    let mut out = Vec::new();
    for blank in &source.blanks {
        let Some(resource_type) = blank.resource_type.clone() else {
            continue;
        };
        let key = match blank.blank_type {
            BlankType::WhatsappChannel => {
                blank_value(blanks, &format!("{}.credential_key", blank.name))
            }
            BlankType::VaultKey => blank_value(blanks, &blank.name),
            _ => None,
        };
        if let Some(key) = key.and_then(Value::as_str) {
            out.push(KeyRequirement {
                key: key.to_string(),
                resource_type,
            });
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The WhatsApp numbers the blanks hold, which no other deployment may hold (`NUMBER_TAKEN`).
pub fn whatsapp_numbers(source: &Source, blanks: &Value) -> Vec<String> {
    source
        .blanks
        .iter()
        .filter(|blank| blank.blank_type == BlankType::WhatsappChannel)
        .filter_map(|blank| blank_value(blanks, &format!("{}.phone_number_id", blank.name)))
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The AI providers the source's agents need a key of (`AI_KEY_MISSING`).
pub fn agent_providers(source: &Source) -> Vec<String> {
    let mut providers: Vec<String> = source
        .nodes
        .iter()
        .filter_map(|node| node.provider.clone())
        .collect();
    providers.sort();
    providers.dedup();
    providers
}

/// How two versions of a source compare: as numbers when both are, else as text; a number is
/// older than a word.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

/// The versions of one source SY.deploy keeps (§3.3): the newest `keep`, and every version a
/// deployment still runs, which a converge and a rollback need.
pub fn versions_kept(
    versions: &[String],
    in_use: &BTreeSet<String>,
    keep: usize,
) -> BTreeSet<String> {
    let mut sorted: Vec<&String> = versions.iter().collect();
    sorted.sort_by(|a, b| compare_versions(b, a));
    sorted.dedup();
    let mut kept: BTreeSet<String> = sorted.into_iter().take(keep).cloned().collect();
    kept.extend(
        versions
            .iter()
            .filter(|version| in_use.contains(*version))
            .cloned(),
    );
    kept
}

/// The blanks of a source as the catalog shows them.
pub fn catalog_blanks(source: &Source) -> Value {
    Value::Array(
        source
            .blanks
            .iter()
            .map(|blank| {
                let mut entry = json!({
                    "name": blank.name,
                    "type": blank.blank_type.as_str(),
                    "description": blank.description,
                });
                if let Some(max) = blank.max_bytes {
                    entry["max_bytes"] = json!(max);
                }
                if let Some(resource_type) = &blank.resource_type {
                    entry["resource_type"] = json!(resource_type);
                }
                entry
            })
            .collect(),
    )
}

/// The parts of a source as the catalog shows them.
pub fn catalog_parts(source: &Source) -> Value {
    Value::Array(
        source
            .nodes
            .iter()
            .map(|node| {
                json!({
                    "role": node.role,
                    "kind": node.kind,
                    "runtime": node.runtime,
                    "work_role": node.work_role,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn plain_ed() -> ParsedSource {
        parse_source(NATIVE_SOURCES[0]).expect("the plain ED's source parses")
    }

    pub fn plain_blanks() -> Value {
        json!({
            "display_name": "Laura Gómez",
            "company_name": "Acme",
            "personality": {
                "timezone": "America/Argentina/Buenos_Aires",
                "country_code": "AR",
                "primary_language": "es",
                "additional_languages": ["en"],
                "description": "Warm and direct.",
                "biographical": {"born": "Rosario"},
                "narrative": {"voice": "calm"}
            },
            "whatsapp": {
                "phone_number_id": "1234567890",
                "waba_id": "998877",
                "credential_key": "acme-laura-wapp"
            }
        })
    }

    #[test]
    fn the_plain_ed_source_reads_with_its_blanks_and_parts() {
        let parsed = plain_ed();
        let source = &parsed.source;
        assert_eq!(source.id, "plain-ed");
        assert_eq!(source.version, "1");
        assert_eq!(source.kind, "ed");
        assert!(
            parsed.hash.starts_with("sha256:") && parsed.hash.len() == 71,
            "{}",
            parsed.hash
        );
        let names: Vec<&str> = source
            .blanks
            .iter()
            .map(|blank| blank.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["display_name", "company_name", "personality", "whatsapp"]
        );
        assert!(source.founds.is_some());
        let roles: Vec<(&str, &str, &str)> = source
            .nodes
            .iter()
            .map(|node| {
                (
                    node.role.as_str(),
                    node.runtime.as_str(),
                    node.work_role.as_str(),
                )
            })
            .collect();
        assert_eq!(
            roles,
            [
                ("conscious", "ai.generic", "conscious"),
                ("channel", "io.wapp", "channel")
            ]
        );
        assert_eq!(agent_providers(source), ["anthropic"]);
        // The same bytes always give the same hash; a change gives another.
        assert_eq!(plain_ed().hash, parsed.hash);
        let changed = NATIVE_SOURCES[0].replace("\"version\": \"1\"", "\"version\": \"2\"");
        assert_ne!(parse_source(&changed).expect("v2").hash, parsed.hash);
    }

    #[test]
    fn a_section_it_cannot_build_refuses_the_source() {
        let mut value: Value = serde_json::from_str(NATIVE_SOURCES[0]).expect("json");
        value["workflows"] = json!([]);
        let err = parse_source(&value.to_string()).expect_err("refused");
        assert!(
            err.contains("cannot build") && err.contains("workflows"),
            "{err}"
        );
        let mut value: Value = serde_json::from_str(NATIVE_SOURCES[0]).expect("json");
        value["extras"] = json!({});
        assert!(parse_source(&value.to_string()).is_err());
    }

    #[test]
    fn a_template_naming_what_is_not_declared_refuses_the_source() {
        let mut value: Value = serde_json::from_str(NATIVE_SOURCES[0]).expect("json");
        value["nodes"][1]["config"]["io"]["dst_node"] = json!({"$ref": "nodes.brain.node_name"});
        let err = parse_source(&value.to_string()).expect_err("refused");
        assert!(err.contains("nodes.brain.node_name"), "{err}");
        let mut value: Value = serde_json::from_str(NATIVE_SOURCES[0]).expect("json");
        value["nodes"][0]["config"]["speaks_as"] = json!({"$blank": "boss"});
        assert!(parse_source(&value.to_string())
            .expect_err("refused")
            .contains("boss"));
        let mut value: Value = serde_json::from_str(NATIVE_SOURCES[0]).expect("json");
        value["nodes"][0]["kind"] = json!("WF");
        assert!(parse_source(&value.to_string()).is_err());
        let mut value: Value = serde_json::from_str(NATIVE_SOURCES[0]).expect("json");
        value["vault"][0]["to"] = json!("nobody");
        assert!(parse_source(&value.to_string()).is_err());
    }

    #[test]
    fn the_blanks_are_exactly_the_source_s_each_checked_by_its_type() {
        let source = plain_ed().source;
        assert_eq!(check_blanks(&source, &plain_blanks()), Ok(()));
        let refused = |change: &dyn Fn(&mut Value), blank: &str| {
            let mut blanks = plain_blanks();
            change(&mut blanks);
            let err = check_blanks(&source, &blanks).expect_err("refused");
            assert_eq!(err.blank, blank, "{err}");
            // The value itself is never named.
            assert!(!err.to_string().contains("Laura"), "{err}");
        };
        refused(
            &|b| {
                b.as_object_mut()
                    .map(|o| o.remove("company_name"))
                    .map(drop)
                    .unwrap_or(())
            },
            "company_name",
        );
        refused(&|b| b["boss"] = json!("x"), "boss");
        refused(
            &|b| b["display_name"] = json!("x".repeat(129)),
            "display_name",
        );
        refused(&|b| b["display_name"] = json!("   "), "display_name");
        refused(&|b| b["display_name"] = json!(7), "display_name");
        refused(
            &|b| b["display_name"] = json!("Laura\nGómez"),
            "display_name",
        );
        refused(
            &|b| b["personality"]["timezone"] = Value::Null,
            "personality",
        );
        refused(&|b| b["personality"]["mood"] = json!("x"), "personality");
        refused(
            &|b| b["personality"]["additional_languages"] = json!("en"),
            "personality",
        );
        refused(
            &|b| b["personality"]["narrative"] = json!("x"),
            "personality",
        );
        refused(
            &|b| b["whatsapp"]["phone_number_id"] = json!("+54 9 11"),
            "whatsapp",
        );
        refused(
            &|b| b["whatsapp"]["phone_number_id"] = json!(1234567890),
            "whatsapp",
        );
        refused(
            &|b| b["whatsapp"]["credential_key"] = json!("Bad Key"),
            "whatsapp",
        );
        refused(&|b| b["whatsapp"]["token"] = json!("x"), "whatsapp");
        refused(&|b| b["whatsapp"]["waba_id"] = json!(""), "whatsapp");
        // 128 bytes of a name is enough.
        let mut blanks = plain_blanks();
        blanks["display_name"] = json!("é".repeat(64));
        assert_eq!(check_blanks(&source, &blanks), Ok(()));
    }

    #[test]
    fn the_blanks_name_the_keys_and_the_number_the_caller_checks() {
        let source = plain_ed().source;
        assert_eq!(
            key_requirements(&source, &plain_blanks()),
            [KeyRequirement {
                key: "acme-laura-wapp".to_string(),
                resource_type: "whatsapp".to_string()
            }]
        );
        assert_eq!(whatsapp_numbers(&source, &plain_blanks()), ["1234567890"]);
    }

    #[test]
    fn a_template_takes_its_blanks_and_refs_and_drops_a_missing_optional() {
        let template = json!({
            "a": {"$blank": "personality.timezone"},
            "b": {"$blank": "personality.region_code", "optional": true},
            "c": [{"$ref": "hive"}, {"$blank": "nope.x", "optional": true}],
            "d": 3
        });
        let refs = BTreeMap::from([("hive".to_string(), json!("worker1"))]);
        assert_eq!(
            resolve_template(&template, &plain_blanks(), &refs).expect("resolved"),
            json!({"a": "America/Argentina/Buenos_Aires", "c": ["worker1"], "d": 3})
        );
        assert!(resolve_template(
            &json!({"$blank": "personality.region_code"}),
            &plain_blanks(),
            &refs
        )
        .is_err());
        assert!(resolve_template(&json!({"$ref": "tenant_id"}), &plain_blanks(), &refs).is_err());
    }

    #[test]
    fn three_versions_are_kept_and_every_one_in_use() {
        let versions: Vec<String> = ["1", "2", "10", "3", "4"]
            .iter()
            .map(|v| v.to_string())
            .collect();
        let none = BTreeSet::new();
        assert_eq!(
            versions_kept(&versions, &none, 3),
            BTreeSet::from(["10".to_string(), "4".to_string(), "3".to_string()])
        );
        let in_use = BTreeSet::from(["1".to_string()]);
        assert_eq!(
            versions_kept(&versions, &in_use, 3),
            BTreeSet::from([
                "10".to_string(),
                "4".to_string(),
                "3".to_string(),
                "1".to_string()
            ])
        );
        assert_eq!(versions_kept(&["1".to_string()], &none, 3).len(), 1);
        assert_eq!(compare_versions("9", "10"), std::cmp::Ordering::Less);
    }
}
