//! The solution package (docs/digital-executive-v1-stage-3.md §1, §2): the one format of a
//! deployable solution, its checks, the templates its configs, documents, assets and WF
//! definitions are written with, and the checks of its blanks' values. SY.deploy reads every
//! package through it, the copies it keeps and Archi's publishes; Archi relies on SY.deploy's
//! answer to a publish.
//!
//! A package holds:
//! - its `kind`: `executive` (the base of an ED: it founds the ED, its conscious and its channels),
//!   `capability` (agents behind an ED, IOs to external systems, a WF at its door) or
//!   `infrastructure` (general for all of a tenant's EDs);
//! - its offer: `origin` (`system`, `catalog`, `custom`) and `offered_to` (`{"tenants": "all"}` or
//!   `{"tenants": ["tnt:..."]}`, a custom package's one tenant; for a capability, `"eds": "all"` or
//!   the short ids of the EDs it is offered to);
//! - `blanks`: each with a name and a type that carries its check;
//! - `founds` (an executive only): the person it founds, its names from the blanks, and its
//!   document;
//! - `nodes`: each with its role, its kind (`AI`, `IO` or `WF`), its middle name part, its
//!   `work_role`, and, for an `AI` or an `IO`, its runtime and its config with the blanks in place
//!   (an agent's with its definition's assets), or, for a `WF`, its definition alone (no runtime,
//!   no config: SY.deploy deploys it with `wf_rules_compile_apply`, on `wf.engine`);
//! - `vault`: which blank's key goes to which node.
//!
//! Its configs, its document, its assets and its WF definitions are templates: an object
//! `{"$blank": "<name>[.<field>]"}` stands for a blank's value (with `"optional": true`, a missing
//! value drops its key), and an object `{"$ref": "<name>"}` for a name or an id SY.deploy makes
//! concrete (`REF_*`, `nodes.<role>.node_name|ilk_id`, `assets.<role>.role|personality`); one it
//! does not know refuses the package. In a WF definition, which uses `$ref` itself (`input.*`,
//! `state.*`, `event.*`), only a `$ref` whose path starts with `grouping.`, `nodes.` or `assets.`
//! is SY.deploy's: every other one stays exactly as written, for the WF engine.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::agent_assets::{canonical_agent_asset_bytes, sha256_hex};

/// The one schema of a package this release reads.
pub const PACKAGE_SCHEMA: u64 = 1;

/// A package's most bytes, as one message carries it (stage 3 §3: `PACKAGE_TOO_LARGE`).
pub const PACKAGE_MAX_BYTES: usize = 128 * 1024;

/// A package's sections.
const PACKAGE_SECTIONS: &[&str] = &[
    "schema",
    "id",
    "version",
    "kind",
    "origin",
    "offered_to",
    "title",
    "description",
    "blanks",
    "founds",
    "nodes",
    "vault",
];

/// The sections a package may hold one day that SY.deploy cannot build yet (stage 3 §10): a
/// package with one is refused before any step.
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

/// The refs a package's templates may name, filled by SY.deploy from the grouping: the ED's ILK
/// (its person), the grouping's short id, its tenant and its hive, and, for a capability of one ED,
/// the node name of the ED's conscious.
pub const REF_GROUPING_ILK: &str = "grouping.ilk_id";
pub const REF_GROUPING_SHORT_ID: &str = "grouping.short_id";
pub const REF_GROUPING_TENANT: &str = "grouping.tenant_id";
pub const REF_GROUPING_HIVE: &str = "grouping.hive";
pub const REF_GROUPING_CONSCIOUS: &str = "grouping.conscious";

/// The node kinds a package deploys.
pub const NODE_AI: &str = "AI";
pub const NODE_IO: &str = "IO";
pub const NODE_WF: &str = "WF";

/// The runtime of every WF node: SY.wf-rules packages each definition as `wf.<workflow name>` on
/// it.
pub const WF_ENGINE: &str = "wf.engine";

/// The roles a part plays for its person (stage 3 §2): the same names as identity's `work_role`.
pub const WORK_ROLE_CONSCIOUS: &str = "conscious";
pub const WORK_ROLE_CHANNEL: &str = "channel";
pub const WORK_ROLE_AGENT: &str = "agent";
pub const WORK_ROLE_CONNECTOR: &str = "connector";
pub const WORK_ROLE_CAPABILITY: &str = "capability";

/// The AI providers an agent may name.
const AI_PROVIDERS: &[&str] = &["anthropic", "openai"];

/// The kinds of solution (stage 3 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PackageKind {
    /// The base of an ED: it founds the ED, its conscious and its channels, in a new ED grouping.
    Executive,
    /// An extension of an ED: agents behind it, IOs to external systems, a WF at its door; into an
    /// ED's grouping, or into its tenant's grouping for every ED of the tenant.
    Capability,
    /// General for all of a tenant's EDs, into the tenant's grouping.
    Infrastructure,
}

impl PackageKind {
    pub const ALL: [PackageKind; 3] = [
        PackageKind::Executive,
        PackageKind::Capability,
        PackageKind::Infrastructure,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PackageKind::Executive => "executive",
            PackageKind::Capability => "capability",
            PackageKind::Infrastructure => "infrastructure",
        }
    }

    /// A kind by its name. `ed`, stage 2's name of an executive, is read only where `alias`
    /// allows it: the copies kept before stage 3, and a filter of the catalog.
    pub fn parse(name: &str, alias: bool) -> Option<PackageKind> {
        match name {
            "executive" => Some(PackageKind::Executive),
            "ed" if alias => Some(PackageKind::Executive),
            "capability" => Some(PackageKind::Capability),
            "infrastructure" => Some(PackageKind::Infrastructure),
            _ => None,
        }
    }

    /// The work roles of its nodes, each with the node kind that plays it.
    fn roles(self) -> &'static [(&'static str, &'static str)] {
        match self {
            PackageKind::Executive => {
                &[(WORK_ROLE_CONSCIOUS, NODE_AI), (WORK_ROLE_CHANNEL, NODE_IO)]
            }
            PackageKind::Capability => &[
                (WORK_ROLE_AGENT, NODE_AI),
                (WORK_ROLE_CONNECTOR, NODE_IO),
                (WORK_ROLE_CAPABILITY, NODE_WF),
            ],
            PackageKind::Infrastructure => {
                &[(WORK_ROLE_AGENT, NODE_AI), (WORK_ROLE_CONNECTOR, NODE_IO)]
            }
        }
    }
}

/// Where a package comes from (stage 3 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Ships with Fluxbee (`/usr/share/fluxbee/solutions/`): Archi reads and derives it, never
    /// edits it.
    System,
    /// Archi's standard, for every tenant.
    Catalog,
    /// Made by Archi for one tenant.
    Custom,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::System => "system",
            Origin::Catalog => "catalog",
            Origin::Custom => "custom",
        }
    }

    pub fn parse(name: &str) -> Option<Origin> {
        match name {
            "system" => Some(Origin::System),
            "catalog" => Some(Origin::Catalog),
            "custom" => Some(Origin::Custom),
            _ => None,
        }
    }
}

/// Who a package is offered to: every one, or the ones named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience {
    All,
    Only(Vec<String>),
}

impl Audience {
    pub fn admits(&self, who: &str) -> bool {
        match self {
            Audience::All => true,
            Audience::Only(names) => names.iter().any(|name| name == who),
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Audience::All => json!("all"),
            Audience::Only(names) => json!(names),
        }
    }

    fn parse(value: &Value, what: &str, form: impl Fn(&str) -> bool) -> Result<Audience, String> {
        match value {
            Value::String(text) if text == "all" => Ok(Audience::All),
            Value::Array(items) => {
                let mut names = Vec::with_capacity(items.len());
                for item in items {
                    match item.as_str() {
                        Some(name) if form(name) => {
                            if !names.iter().any(|known| known == name) {
                                names.push(name.to_string());
                            }
                        }
                        _ => return Err(format!("{what} is \"all\" or a list of their ids")),
                    }
                }
                if names.is_empty() {
                    return Err(format!("{what} names at least one, or is \"all\""));
                }
                Ok(Audience::Only(names))
            }
            _ => Err(format!("{what} is \"all\" or a list of their ids")),
        }
    }
}

/// A package's offer: where it comes from, and who it is offered to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub origin: Origin,
    pub tenants: Audience,
    /// A capability's EDs, by short id; `All` for every other kind.
    pub eds: Audience,
}

impl Offer {
    /// Reads an offer: `origin` and `offered_to`, as a package of `kind` holds them.
    pub fn parse(kind: PackageKind, origin: &Value, offered_to: &Value) -> Result<Offer, String> {
        let origin = origin
            .as_str()
            .and_then(Origin::parse)
            .ok_or("a package's origin is one of system, catalog and custom")?;
        let Some(fields) = offered_to.as_object() else {
            return Err("a package's offered_to is an object: {\"tenants\", \"eds\"?}".to_string());
        };
        if let Some(extra) = fields.keys().find(|key| *key != "tenants" && *key != "eds") {
            return Err(format!(
                "a package's offered_to takes tenants and, for a capability, eds, not '{extra}'"
            ));
        }
        let tenants = Audience::parse(
            fields.get("tenants").unwrap_or(&Value::Null),
            "offered_to.tenants",
            is_tenant_id,
        )?;
        if origin == Origin::Custom
            && !matches!(&tenants, Audience::Only(names) if names.len() == 1)
        {
            return Err("a custom package is offered to exactly one tenant".to_string());
        }
        let eds = match fields.get("eds") {
            None | Some(Value::Null) => Audience::All,
            Some(_) if kind != PackageKind::Capability => {
                return Err("only a capability's offered_to names its eds".to_string())
            }
            Some(value) => Audience::parse(value, "offered_to.eds", is_ed_short_id)?,
        };
        Ok(Offer {
            origin,
            tenants,
            eds,
        })
    }

    /// Its `offered_to`, as a package writes it.
    pub fn offered_to_value(&self, kind: PackageKind) -> Value {
        let mut value = json!({"tenants": self.tenants.to_value()});
        if kind == PackageKind::Capability {
            value["eds"] = self.eds.to_value();
        }
        value
    }
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

/// An agent's definition: its assets, each a template.
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

impl DefinitionSpec {
    pub fn templates(&self) -> impl Iterator<Item = &Value> {
        self.role
            .iter()
            .chain(self.skills.iter())
            .chain(self.handbooks.iter())
            .chain(self.personality.iter())
    }
}

/// A node a package deploys.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeSpec {
    pub role: String,
    /// `AI`, `IO` or `WF`.
    pub kind: String,
    pub middle: Option<String>,
    /// Its runtime, which an AI and an IO name; a WF names none, and runs on `wf.engine`.
    pub runtime: String,
    pub work_role: String,
    /// An agent's AI provider, whose key the tenant must hold (`AI_KEY_MISSING`).
    pub provider: Option<String>,
    /// An AI's or an IO's config, a template; `{}` for a WF.
    pub config: Value,
    /// An agent's definition: its assets.
    pub assets: Option<DefinitionSpec>,
    /// A WF's definition, a template.
    pub workflow: Option<Value>,
}

impl NodeSpec {
    pub fn is_wf(&self) -> bool {
        self.kind == NODE_WF
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VaultSpec {
    pub key: Value,
    pub resource_type: String,
    /// The role of the node the key is assigned to.
    pub to: String,
}

/// A package as read.
#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub schema: u64,
    pub id: String,
    pub version: String,
    pub kind: PackageKind,
    /// Its offer. `None` only for a copy SY.deploy kept before stage 3 (kind `ed`), which serves
    /// the deployments that run it and is offered once a publish of the same content gives it one.
    pub offer: Option<Offer>,
    pub title: String,
    pub description: String,
    pub blanks: Vec<BlankSpec>,
    pub founds: Option<FoundsSpec>,
    pub nodes: Vec<NodeSpec>,
    pub vault: Vec<VaultSpec>,
}

/// The kinds of grouping a package deploys into.
pub const GROUPING_ED: &str = "ed";
pub const GROUPING_TENANT: &str = "tenant";

impl Package {
    /// A capability's door: its one node of work role `capability`, a WF.
    pub fn door(&self) -> Option<&NodeSpec> {
        self.nodes
            .iter()
            .find(|node| node.work_role == WORK_ROLE_CAPABILITY)
    }

    /// Whether it is a capability of every ED of its tenant: its door's `start_from` is the
    /// consciouses marker.
    pub fn for_every_ed(&self) -> bool {
        self.kind == PackageKind::Capability
            && self
                .door()
                .and_then(|door| door.workflow.as_ref())
                .is_some_and(|definition| definition["start_from"] == json!([consciouses_marker()]))
    }

    /// The kind of grouping it deploys into: an executive a new ED grouping; a capability of one
    /// ED that ED's; infrastructure and a capability of every ED their tenant's.
    pub fn deploys_into(&self) -> &'static str {
        match self.kind {
            PackageKind::Executive => GROUPING_ED,
            PackageKind::Capability if !self.for_every_ed() => GROUPING_ED,
            _ => GROUPING_TENANT,
        }
    }
}

/// How a package is read: as a publish (its kind's own name, its offer), or as a copy SY.deploy
/// kept (stage 2's `ed`, and no offer, read too).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Publish,
    KeptCopy,
}

/// A package as read, with its hash.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedPackage {
    pub package: Package,
    /// `sha256:` and the SHA-256 of its canonical bytes: what a deployment pins.
    pub hash: String,
    /// The same of what it deploys: its canonical bytes without its offer, its kind by its own
    /// name. A copy kept before stage 3 and the publish of its content share it.
    pub content_hash: String,
    /// The canonical bytes, what SY.deploy keeps as the package's copy.
    pub canonical: Vec<u8>,
}

/// The consciouses marker a door for every ED of its tenant writes in its `start_from`
/// (docs/wf-orchestration-v1.md §5.6).
pub fn consciouses_marker() -> Value {
    json!({"consciouses": "tenant"})
}

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

fn is_uuid_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .is_some_and(|uuid| uuid::Uuid::parse_str(uuid).is_ok())
}

/// Whether `id` is an ILK id, `ilk:<uuid>`.
pub fn is_ilk_id(id: &str) -> bool {
    is_uuid_id(id, "ilk:")
}

/// Whether `id` is a tenant id, `tnt:<uuid>`.
pub fn is_tenant_id(id: &str) -> bool {
    is_uuid_id(id, "tnt:")
}

/// Whether `id` is an ED grouping's short id: `ed`, a digit from 2 to 7, and 7 of `[a-z2-7]`.
pub fn is_ed_short_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("ed") else {
        return false;
    };
    let bytes = rest.as_bytes();
    bytes.len() == 8
        && matches!(bytes[0], b'2'..=b'7')
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'2'..=b'7'))
}

/// A JSON value with every object's keys sorted, at every depth, serialized compact: the bytes a
/// hash is taken of, as SY.identity takes a person's document's (`canonical_json`) and Archi an
/// asset's.
pub fn canonical_bytes(value: &Value) -> Vec<u8> {
    canonical_agent_asset_bytes(value).unwrap_or_else(|_| value.to_string().into_bytes())
}

/// `sha256:` and the SHA-256 of a value's canonical bytes.
pub fn value_hash(value: &Value) -> String {
    format!("sha256:{}", sha256_hex(&canonical_bytes(value)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    schema: u64,
    id: String,
    version: String,
    kind: String,
    #[serde(default)]
    origin: Option<Value>,
    #[serde(default)]
    offered_to: Option<Value>,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    blanks: Vec<BlankSpec>,
    #[serde(default)]
    founds: Option<FoundsSpec>,
    nodes: Vec<RawNode>,
    #[serde(default)]
    vault: Vec<VaultSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    role: String,
    kind: String,
    #[serde(default)]
    middle: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
    work_role: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    definition: Option<Value>,
}

/// Reads a package and checks it: its sections, its fields, its kind's roles, its door, its offer
/// and the names its templates use. Any error names what is wrong, never a value.
pub fn parse_package(raw: &str, reading: Reading) -> Result<ParsedPackage, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|err| format!("the package is not JSON: {err}"))?;
    parse_package_value(&value, reading)
}

/// The same, of a package already read as JSON (a publish's).
pub fn parse_package_value(value: &Value, reading: Reading) -> Result<ParsedPackage, String> {
    let Some(sections) = value.as_object() else {
        return Err("a package is a JSON object".to_string());
    };
    for key in sections.keys() {
        if SECTIONS_NOT_BUILT_YET.contains(&key.as_str()) {
            return Err(format!(
                "SY.deploy cannot build a package's '{key}' yet: the package is refused before any step"
            ));
        }
        if !PACKAGE_SECTIONS.contains(&key.as_str()) {
            return Err(format!("a package has no section '{key}'"));
        }
    }
    let raw: RawPackage = serde_json::from_value(value.clone())
        .map_err(|err| format!("the package does not read: {err}"))?;
    let package = read_package(raw, reading)?;
    check_package(&package)?;
    let canonical = canonical_bytes(value);
    let hash = format!("sha256:{}", sha256_hex(&canonical));
    let mut content = sections.clone();
    content.remove("origin");
    content.remove("offered_to");
    content.insert("kind".to_string(), json!(package.kind.as_str()));
    let content_hash = value_hash(&Value::Object(content));
    Ok(ParsedPackage {
        package,
        hash,
        content_hash,
        canonical,
    })
}

fn read_package(raw: RawPackage, reading: Reading) -> Result<Package, String> {
    let alias = reading == Reading::KeptCopy;
    let kind = PackageKind::parse(&raw.kind, alias)
        .ok_or("a package's kind is one of executive, capability and infrastructure")?;
    let offer = match (&raw.origin, &raw.offered_to) {
        (Some(origin), Some(offered_to)) => Some(Offer::parse(kind, origin, offered_to)?),
        (None, None) if alias => None,
        _ => return Err("a package names its origin and its offered_to".to_string()),
    };
    if raw.kind == "ed" && offer.is_some() {
        return Err("a package with an offer names its kind executive, not ed".to_string());
    }
    let mut nodes = Vec::with_capacity(raw.nodes.len());
    for node in raw.nodes {
        nodes.push(read_node(node)?);
    }
    Ok(Package {
        schema: raw.schema,
        id: raw.id,
        version: raw.version,
        kind,
        offer,
        title: raw.title,
        description: raw.description,
        blanks: raw.blanks,
        founds: raw.founds,
        nodes,
        vault: raw.vault,
    })
}

fn read_node(raw: RawNode) -> Result<NodeSpec, String> {
    let role = raw.role;
    let mut node = NodeSpec {
        role: role.clone(),
        kind: raw.kind,
        middle: raw.middle,
        runtime: String::new(),
        work_role: raw.work_role,
        provider: raw.provider,
        config: json!({}),
        assets: None,
        workflow: None,
    };
    match node.kind.as_str() {
        NODE_AI | NODE_IO => {
            node.runtime = raw
                .runtime
                .filter(|runtime| is_token(runtime))
                .ok_or_else(|| format!("the node '{role}' names its runtime"))?;
            node.config = raw
                .config
                .filter(Value::is_object)
                .ok_or_else(|| format!("the node '{role}' has a config object"))?;
            if node.kind == NODE_AI {
                if !node
                    .provider
                    .as_deref()
                    .is_some_and(|provider| AI_PROVIDERS.contains(&provider))
                {
                    return Err(format!(
                        "the agent '{role}' names its provider, one of {AI_PROVIDERS:?}"
                    ));
                }
                node.assets = raw
                    .definition
                    .map(|definition| {
                        serde_json::from_value::<DefinitionSpec>(definition).map_err(|err| {
                            format!("the agent '{role}' has a definition of assets: {err}")
                        })
                    })
                    .transpose()?;
            } else if node.provider.is_some() || raw.definition.is_some() {
                return Err(format!(
                    "only an agent has a provider and a definition, not the node '{role}'"
                ));
            }
        }
        NODE_WF => {
            // SY.deploy deploys a WF with `wf_rules_compile_apply`, and SY.wf-rules runs it on
            // wf.engine: a package names no runtime for it.
            if raw.runtime.is_some() {
                return Err(format!(
                    "a WF has no runtime, SY.wf-rules runs it on {WF_ENGINE}: the node '{role}'"
                ));
            }
            node.runtime = WF_ENGINE.to_string();
            if raw.config.is_some() || node.provider.is_some() {
                return Err(format!(
                    "a WF carries its definition, and no config nor provider: the node '{role}'"
                ));
            }
            node.workflow = Some(
                raw.definition
                    .filter(Value::is_object)
                    .ok_or_else(|| format!("the WF '{role}' carries its definition, an object"))?,
            );
        }
        other => {
            return Err(format!(
                "a node is of kind AI, IO or WF, not '{other}': the node '{role}'"
            ))
        }
    }
    Ok(node)
}

fn check_package(package: &Package) -> Result<(), String> {
    if package.schema != PACKAGE_SCHEMA {
        return Err(format!("a package's schema is {PACKAGE_SCHEMA}"));
    }
    if !is_token(&package.id) {
        return Err("a package's id is 1 to 64 of [a-z0-9._-]".to_string());
    }
    if !is_token(&package.version) {
        return Err("a package's version is 1 to 64 of [a-z0-9._-]".to_string());
    }
    if package.title.trim().is_empty() {
        return Err("a package has a title".to_string());
    }
    let blank_names = check_blank_specs(&package.blanks)?;
    check_nodes(package)?;
    if let Some(offer) = &package.offer {
        if package.for_every_ed() && offer.eds != Audience::All {
            return Err(
                "a capability for every ED of its tenant is offered to all of them: its offered_to names no eds"
                    .to_string(),
            );
        }
    }
    let refs = known_refs(package);
    let check = |template: &Value, what: &str| {
        check_template(template, &blank_names, &refs, what, TemplateKind::Plain)
    };
    match (&package.founds, package.kind) {
        (Some(founds), PackageKind::Executive) => {
            check(&founds.person.display_name, "the person's display_name")?;
            if let Some(company) = &founds.person.company_name {
                check(company, "the person's company_name")?;
            }
            if !founds.document.is_object() {
                return Err("the person's document is an object".to_string());
            }
            check(&founds.document, "the person's document")?;
        }
        (None, PackageKind::Executive) => {
            return Err("an executive founds its ED: it has founds".to_string())
        }
        (Some(_), _) => return Err("only an executive founds a person".to_string()),
        (None, _) => {}
    }
    for node in &package.nodes {
        check(&node.config, &format!("the node '{}' config", node.role))?;
        if let Some(assets) = &node.assets {
            // An asset is made before the refs to the assets exist.
            let free = asset_free_refs(&refs);
            for asset in assets.templates() {
                check_template(asset, &blank_names, &free, "an asset", TemplateKind::Plain)?;
            }
        }
        if let Some(definition) = &node.workflow {
            check_template(
                definition,
                &blank_names,
                &refs,
                &format!("the WF '{}' definition", node.role),
                TemplateKind::Workflow,
            )?;
        }
    }
    for vault in &package.vault {
        match package.nodes.iter().find(|node| node.role == vault.to) {
            None => {
                return Err(format!(
                    "a vault entry goes to the node role '{}', which the package does not declare",
                    vault.to
                ))
            }
            Some(node) if node.is_wf() => {
                return Err(format!(
                    "a vault entry goes to an AI or an IO, not to the WF '{}'",
                    vault.to
                ))
            }
            Some(_) => {}
        }
        if !is_token(&vault.resource_type) {
            return Err("a vault entry names an invalid resource_type".to_string());
        }
        check(&vault.key, "a vault entry's key")?;
    }
    Ok(())
}

fn check_blank_specs(blanks: &[BlankSpec]) -> Result<BTreeSet<&str>, String> {
    let mut names = BTreeSet::new();
    for blank in blanks {
        if !is_name_part(&blank.name.replace('_', "")) || blank.name.contains('.') {
            return Err(format!(
                "a blank's name is lowercase letters, digits and '_', not '{}'",
                blank.name
            ));
        }
        if !names.insert(blank.name.as_str()) {
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
    Ok(names)
}

/// The nodes' checks: their names, each one's kind for its work role, the roles of the package's
/// kind, an executive's one conscious, a capability's one door and its `start_from`.
fn check_nodes(package: &Package) -> Result<(), String> {
    if package.nodes.is_empty() {
        return Err("a package deploys at least one node".to_string());
    }
    let mut roles = BTreeSet::new();
    let mut names = BTreeSet::new();
    for node in &package.nodes {
        if !is_name_part(&node.role) {
            return Err(format!(
                "a node's role is lowercase letters and digits, not '{}'",
                node.role
            ));
        }
        if !roles.insert(node.role.as_str()) {
            return Err(format!("the node role '{}' is declared twice", node.role));
        }
        if let Some(middle) = &node.middle {
            if !is_name_part(middle) {
                return Err(format!(
                    "the node '{}' has a middle name part of lowercase letters and digits",
                    node.role
                ));
            }
        }
        // A capability's and an infrastructure's nodes share their grouping with others': each
        // one's middle part names it there.
        if package.kind != PackageKind::Executive && node.middle.is_none() {
            return Err(format!(
                "every node of a package of kind {} has a middle name part: the node '{}'",
                package.kind.as_str(),
                node.role
            ));
        }
        if node.is_wf()
            && !node
                .middle
                .as_deref()
                .is_some_and(|middle| middle.starts_with(|c: char| c.is_ascii_lowercase()))
        {
            return Err(format!(
                "a WF's middle name part starts with a letter: its workflow is <middle>.<short id>, the node '{}'",
                node.role
            ));
        }
        if !names.insert((node.kind.as_str(), node.middle.as_deref())) {
            return Err(format!(
                "two {} nodes would have one name: give the node '{}' another middle name part",
                node.kind, node.role
            ));
        }
        let allowed = package.kind.roles();
        match allowed.iter().find(|(role, _)| *role == node.work_role) {
            None => {
                return Err(format!(
                    "a node of a package of kind {} has a work_role of {:?}: not the node '{}'",
                    package.kind.as_str(),
                    allowed.iter().map(|(role, _)| *role).collect::<Vec<_>>(),
                    node.role
                ))
            }
            Some((role, kind)) if node.kind != *kind => {
                return Err(format!(
                    "the work_role {role} is a node of kind {kind}: not the node '{}'",
                    node.role
                ))
            }
            Some(_) => {}
        }
    }
    let count = |role: &str| {
        package
            .nodes
            .iter()
            .filter(|node| node.work_role == role)
            .count()
    };
    match package.kind {
        PackageKind::Executive if count(WORK_ROLE_CONSCIOUS) != 1 => {
            return Err("an executive has exactly one conscious".to_string())
        }
        PackageKind::Capability if count(WORK_ROLE_CAPABILITY) != 1 => {
            return Err(
                "a capability has exactly one door, a WF of work_role capability".to_string(),
            )
        }
        _ => {}
    }
    if let Some(door) = package.door() {
        let start_from = door
            .workflow
            .as_ref()
            .and_then(|definition| definition.get("start_from"));
        let of_one_ed = json!([{"$ref": REF_GROUPING_CONSCIOUS}]);
        let of_every_ed = json!([consciouses_marker()]);
        if start_from != Some(&of_one_ed) && start_from != Some(&of_every_ed) {
            return Err(format!(
                "a capability's door starts from its ED's conscious, {of_one_ed}, or from every conscious of its tenant, {of_every_ed}: the WF '{}'",
                door.role
            ));
        }
    }
    Ok(())
}

/// The refs a package's templates may name: the grouping's (the ED's ILK and its conscious only
/// where the grouping is an ED's), each AI's and IO's node name and ILK, each WF's node name (its
/// ILK is the orchestrator's, known once it runs), and the hash of each asset its agents'
/// definitions hold.
fn known_refs(package: &Package) -> BTreeSet<String> {
    let mut refs: BTreeSet<String> = [
        REF_GROUPING_SHORT_ID,
        REF_GROUPING_TENANT,
        REF_GROUPING_HIVE,
    ]
    .iter()
    .map(|name| name.to_string())
    .collect();
    if package.deploys_into() == GROUPING_ED {
        refs.insert(REF_GROUPING_ILK.to_string());
        if package.kind == PackageKind::Capability {
            refs.insert(REF_GROUPING_CONSCIOUS.to_string());
        }
    }
    for node in &package.nodes {
        refs.insert(node_name_ref(&node.role));
        if !node.is_wf() {
            refs.insert(node_ilk_ref(&node.role));
        }
        if let Some(assets) = &node.assets {
            if assets.role.is_some() {
                refs.insert(asset_ref(&node.role, "role"));
            }
            if assets.personality.is_some() {
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

/// Whether a `$ref`'s name is in SY.deploy's namespaces: the grouping's, the nodes' and the
/// assets'. In a WF definition any other `$ref` is the WF's own.
fn is_package_ref(name: &str) -> bool {
    ["grouping.", "nodes.", "assets."]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// What a template is: a config, a document or an asset (every `$` key is a placeholder), or a WF
/// definition (only `$blank` and SY.deploy's `$ref` are; the WF's own `$ref` and a JSON Schema's
/// `$` keys stay as they are).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateKind {
    Plain,
    Workflow,
}

/// What a template's placeholder is, if it is one.
enum Placeholder<'a> {
    Blank {
        path: &'a str,
        optional: bool,
    },
    Ref(&'a str),
    /// A value kept as it is: a WF's own `$ref`.
    Literal,
}

fn placeholder(value: &Value, kind: TemplateKind) -> Result<Option<Placeholder<'_>>, String> {
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
        if kind == TemplateKind::Workflow && !is_package_ref(name) {
            return Ok(Some(Placeholder::Literal));
        }
        if object.len() != 1 {
            return Err("a $ref takes nothing beside it".to_string());
        }
        return Ok(Some(Placeholder::Ref(name)));
    }
    if kind == TemplateKind::Plain && object.keys().any(|key| key.starts_with('$')) {
        return Err("a template's only placeholders are $blank and $ref".to_string());
    }
    Ok(None)
}

fn check_template(
    template: &Value,
    blanks: &BTreeSet<&str>,
    refs: &BTreeSet<String>,
    what: &str,
    kind: TemplateKind,
) -> Result<(), String> {
    match placeholder(template, kind).map_err(|err| format!("{what}: {err}"))? {
        Some(Placeholder::Blank { path, .. }) => {
            let blank = path.split('.').next().unwrap_or_default();
            if !blanks.contains(blank) {
                return Err(format!(
                    "{what} names the blank '{blank}', which the package does not declare"
                ));
            }
            Ok(())
        }
        Some(Placeholder::Ref(name)) => {
            if !refs.contains(name) {
                return Err(format!(
                    "{what} names the ref '{name}', which is none of this package's"
                ));
            }
            Ok(())
        }
        Some(Placeholder::Literal) => Ok(()),
        None => match template {
            Value::Object(fields) => fields
                .values()
                .try_for_each(|value| check_template(value, blanks, refs, what, kind)),
            Value::Array(items) => items
                .iter()
                .try_for_each(|value| check_template(value, blanks, refs, what, kind)),
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

/// Makes a config, a document or an asset concrete: each `$blank` from `blanks`, each `$ref` from
/// `refs`. A missing value of an optional `$blank` drops its key (or its item); any other missing
/// value is an error that names it.
pub fn resolve_template(
    template: &Value,
    blanks: &Value,
    refs: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    resolve_inner(template, blanks, refs, TemplateKind::Plain)?
        .ok_or_else(|| "a template resolved to nothing".to_string())
}

/// Makes a WF definition concrete: its `$blank`s and SY.deploy's `$ref`s, the WF's own `$ref`s and
/// a JSON Schema's `$` keys kept as they are.
pub fn resolve_workflow(
    template: &Value,
    blanks: &Value,
    refs: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    resolve_inner(template, blanks, refs, TemplateKind::Workflow)?
        .ok_or_else(|| "a WF definition resolved to nothing".to_string())
}

fn resolve_inner(
    template: &Value,
    blanks: &Value,
    refs: &BTreeMap<String, Value>,
    kind: TemplateKind,
) -> Result<Option<Value>, String> {
    match placeholder(template, kind)? {
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
        Some(Placeholder::Literal) => Ok(Some(template.clone())),
        None => match template {
            Value::Object(fields) => {
                let mut out = Map::new();
                for (key, value) in fields {
                    if let Some(value) = resolve_inner(value, blanks, refs, kind)? {
                        out.insert(key.clone(), value);
                    }
                }
                Ok(Some(Value::Object(out)))
            }
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    if let Some(value) = resolve_inner(item, blanks, refs, kind)? {
                        out.push(value);
                    }
                }
                Ok(Some(Value::Array(out)))
            }
            other => Ok(Some(other.clone())),
        },
    }
}

/// A blank's value refused: which blank, and why, never the value (`BLANK_INVALID`).
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

/// The checks of the blanks' values: every blank the package declares is filled, and nothing
/// more, each checked by its type. The checks that need the tenant's keys or the other deployments
/// are the caller's (`key_requirements`, `whatsapp_numbers`).
pub fn check_blanks(package: &Package, blanks: &Value) -> Result<(), BlankError> {
    let Some(values) = blanks.as_object() else {
        return Err(blank_error(
            "blanks",
            "is an object of the package's blanks",
        ));
    };
    if let Some(extra) = values
        .keys()
        .find(|key| !package.blanks.iter().any(|blank| &blank.name == *key))
    {
        return Err(blank_error(extra, "is not a blank of this package"));
    }
    for blank in &package.blanks {
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
    object: &Map<String, Value>,
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
            if value.as_str().is_some_and(is_ilk_id) {
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

/// A key the tenant must hold for a deployment: its name and its `resource_type`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyRequirement {
    pub key: String,
    pub resource_type: String,
}

/// The keys the blanks name, each of the resource type its blank says (`CREDENTIAL_MISSING`).
pub fn key_requirements(package: &Package, blanks: &Value) -> Vec<KeyRequirement> {
    let mut out = Vec::new();
    for blank in &package.blanks {
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
pub fn whatsapp_numbers(package: &Package, blanks: &Value) -> Vec<String> {
    package
        .blanks
        .iter()
        .filter(|blank| blank.blank_type == BlankType::WhatsappChannel)
        .filter_map(|blank| blank_value(blanks, &format!("{}.phone_number_id", blank.name)))
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The AI providers the package's agents need a key of (`AI_KEY_MISSING`).
pub fn agent_providers(package: &Package) -> Vec<String> {
    let mut providers: Vec<String> = package
        .nodes
        .iter()
        .filter_map(|node| node.provider.clone())
        .collect();
    providers.sort();
    providers.dedup();
    providers
}

/// How two versions of a package compare: as numbers when both are, else as text; a number is
/// older than a word.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

/// The versions of one package SY.deploy keeps: the newest `keep`, and every version a deployment
/// still runs, which a converge and a removal need.
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

/// The blanks of a package as the catalog shows them.
pub fn catalog_blanks(package: &Package) -> Value {
    Value::Array(
        package
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

/// The parts of a package as the catalog shows them.
pub fn catalog_parts(package: &Package) -> Value {
    Value::Array(
        package
            .nodes
            .iter()
            .map(|node| {
                json!({
                    "role": node.role,
                    "kind": node.kind,
                    "middle": node.middle,
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

    /// The plain ED, as the .deb ships it.
    pub const PLAIN_ED: &str = include_str!("../packaging/solutions/plain-ed/1.json");

    pub fn plain_ed() -> ParsedPackage {
        parse_package(PLAIN_ED, Reading::Publish).expect("the plain ED parses")
    }

    /// The plain ED's copy as SY.deploy kept it before stage 3: kind `ed`, no offer.
    pub fn plain_ed_kept_before_stage_3() -> Value {
        let mut value: Value = serde_json::from_str(PLAIN_ED).expect("json");
        value["kind"] = json!("ed");
        let fields = value.as_object_mut().expect("object");
        fields.remove("origin");
        fields.remove("offered_to");
        value
    }

    /// The billing capability, as JSON.
    const BILLING: &str = r#"{
      "schema": 1,
      "id": "billing",
      "version": "1",
      "kind": "capability",
      "origin": "catalog",
      "offered_to": {
        "tenants": "all",
        "eds": "all"
      },
      "title": "Billing",
      "description": "Invoices: the state of an invoice, by its number.",
      "blanks": [
        {
          "name": "crm_key",
          "type": "vault_key",
          "resource_type": "api_key",
          "description": "The CRM's key."
        }
      ],
      "nodes": [
        {
          "role": "billing",
          "kind": "AI",
          "middle": "billing",
          "runtime": "ai.generic",
          "work_role": "agent",
          "provider": "anthropic",
          "config": {
            "behavior": {
              "kind": "ai_chat",
              "model": "claude-sonnet-5-5"
            },
            "crm": {
              "$ref": "nodes.crm.node_name"
            }
          },
          "definition": {
            "role": {
              "asset_type": "role",
              "name": "Billing",
              "description": "You read invoices in the CRM."
            }
          }
        },
        {
          "role": "crm",
          "kind": "IO",
          "middle": "crm",
          "runtime": "io.api",
          "work_role": "connector",
          "config": {
            "auth": {
              "key": {
                "$blank": "crm_key"
              }
            },
            "dst_node": {
              "$ref": "nodes.billing.node_name"
            }
          }
        },
        {
          "role": "door",
          "kind": "WF",
          "middle": "billing",
          "work_role": "capability",
          "definition": {
            "wf_schema_version": "1",
            "workflow_type": "capability.billing",
            "description": "Invoices: the state of an invoice, by its number.",
            "start_from": [
              {
                "$ref": "grouping.conscious"
              }
            ],
            "input_schema": {
              "$schema": "https://json-schema.org/draft/2020-12/schema",
              "type": "object",
              "required": [
                "number"
              ],
              "properties": {
                "number": {
                  "type": "string"
                }
              }
            },
            "initial_state": "reading",
            "terminal_states": [
              "answered"
            ],
            "states": [
              {
                "name": "reading",
                "entry_actions": [
                  {
                    "type": "send_message",
                    "target": {
                      "$ref": "nodes.billing.node_name"
                    },
                    "meta": {
                      "msg": "INVOICE_STATE"
                    },
                    "vars": {
                      "number": {
                        "$ref": "input.number"
                      },
                      "ed": {
                        "$ref": "grouping.short_id"
                      }
                    }
                  }
                ],
                "transitions": [
                  {
                    "event_match": {
                      "src": {
                        "$ref": "nodes.billing.node_name"
                      }
                    },
                    "guard": "true",
                    "target_state": "answered",
                    "actions": [
                      {
                        "type": "reply",
                        "status": "done",
                        "result": {
                          "state": {
                            "$ref": "event.context.evaluation.params.state",
                            "default": ""
                          }
                        }
                      }
                    ]
                  }
                ]
              },
              {
                "name": "answered"
              }
            ]
          }
        }
      ],
      "vault": [
        {
          "key": {
            "$blank": "crm_key"
          },
          "resource_type": "api_key",
          "to": "crm"
        }
      ]
    }"#;

    /// A capability of one ED: a WF at its door, an agent, a connector.
    pub fn billing() -> Value {
        serde_json::from_str(BILLING).expect("the billing fixture is JSON")
    }

    fn refused(value: &Value, reading: Reading) -> String {
        parse_package_value(value, reading).expect_err("refused")
    }

    #[test]
    fn the_shipped_plain_ed_is_a_system_executive_offered_to_every_tenant() {
        let parsed = plain_ed();
        let package = &parsed.package;
        assert_eq!(package.id, "plain-ed");
        assert_eq!(package.version, "1");
        assert_eq!(package.kind, PackageKind::Executive);
        assert_eq!(
            package.offer,
            Some(Offer {
                origin: Origin::System,
                tenants: Audience::All,
                eds: Audience::All,
            })
        );
        assert_eq!(package.deploys_into(), GROUPING_ED);
        assert!(
            parsed.hash.starts_with("sha256:") && parsed.hash.len() == 71,
            "{}",
            parsed.hash
        );
        let names: Vec<&str> = package.blanks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            ["display_name", "company_name", "personality", "whatsapp"]
        );
        let roles: Vec<(&str, &str, &str)> = package
            .nodes
            .iter()
            .map(|n| (n.role.as_str(), n.runtime.as_str(), n.work_role.as_str()))
            .collect();
        assert_eq!(
            roles,
            [
                ("conscious", "ai.generic", "conscious"),
                ("channel", "io.wapp", "channel")
            ]
        );
        assert_eq!(agent_providers(package), ["anthropic"]);
        // The same bytes always give the same hash; a change gives another.
        assert_eq!(plain_ed().hash, parsed.hash);
        let changed = PLAIN_ED.replace("\"version\": \"1\"", "\"version\": \"2\"");
        assert_ne!(
            parse_package(&changed, Reading::Publish).expect("v2").hash,
            parsed.hash
        );
    }

    #[test]
    fn ed_is_read_only_in_a_copy_kept_before_stage_3_which_has_the_same_content() {
        let kept = plain_ed_kept_before_stage_3();
        let copy = parse_package_value(&kept, Reading::KeptCopy).expect("the kept copy reads");
        assert_eq!(copy.package.kind, PackageKind::Executive);
        assert_eq!(copy.package.offer, None);
        // What it deploys is the shipped package's; its bytes, and the hash its deployments pin,
        // are its own.
        assert_eq!(copy.content_hash, plain_ed().content_hash);
        assert_ne!(copy.hash, plain_ed().hash);
        assert_eq!(copy.hash, value_hash(&kept));
        // A publish names its kind's own name and its offer.
        assert!(refused(&kept, Reading::Publish).contains("kind"));
        let mut no_offer: Value = serde_json::from_str(PLAIN_ED).expect("json");
        no_offer.as_object_mut().expect("o").remove("offered_to");
        assert!(refused(&no_offer, Reading::Publish).contains("offered_to"));
        let mut ed_with_offer: Value = serde_json::from_str(PLAIN_ED).expect("json");
        ed_with_offer["kind"] = json!("ed");
        assert!(refused(&ed_with_offer, Reading::KeptCopy).contains("executive"));
    }

    #[test]
    fn a_capability_has_one_door_a_wf_and_agents_and_connectors() {
        let parsed = parse_package_value(&billing(), Reading::Publish).expect("billing reads");
        let package = &parsed.package;
        assert_eq!(package.kind, PackageKind::Capability);
        assert_eq!(package.deploys_into(), GROUPING_ED);
        assert!(!package.for_every_ed());
        let door = package.door().expect("its door");
        assert_eq!(
            (door.kind.as_str(), door.runtime.as_str()),
            (NODE_WF, WF_ENGINE)
        );
        assert!(door.workflow.is_some() && door.assets.is_none());
        assert_eq!(agent_providers(package), ["anthropic"]);

        let change = |edit: &dyn Fn(&mut Value)| {
            let mut value = billing();
            edit(&mut value);
            value
        };
        let cases: Vec<(Value, &str)> = vec![
            // Two doors, none, a door that is no WF, a WF that is no door.
            (
                change(&|v| {
                    let mut second = v["nodes"][2].clone();
                    second["role"] = json!("door2");
                    second["middle"] = json!("billing2");
                    v["nodes"].as_array_mut().expect("nodes").push(second);
                }),
                "exactly one door",
            ),
            (
                change(&|v| {
                    v["nodes"].as_array_mut().expect("nodes").pop();
                }),
                "exactly one door",
            ),
            (
                change(&|v| v["nodes"][1]["work_role"] = json!("capability")),
                "the work_role capability is a node of kind WF",
            ),
            (
                change(&|v| v["nodes"][2]["work_role"] = json!("agent")),
                "the work_role agent is a node of kind AI",
            ),
            // An executive's roles in a capability, a capability's in an executive.
            (
                change(&|v| v["nodes"][0]["work_role"] = json!("conscious")),
                "work_role",
            ),
            // A WF carries its definition and no config; a WF on another runtime.
            (
                change(&|v| v["nodes"][2]["config"] = json!({})),
                "no config",
            ),
            (
                change(&|v| {
                    v["nodes"][2]
                        .as_object_mut()
                        .expect("o")
                        .remove("definition");
                }),
                "carries its definition",
            ),
            (
                change(&|v| v["nodes"][2]["runtime"] = json!("wf.other")),
                "a WF has no runtime",
            ),
            (
                change(&|v| v["nodes"][2]["runtime"] = json!("wf.engine")),
                "a WF has no runtime",
            ),
            // An AI and an IO name theirs.
            (
                change(&|v| {
                    v["nodes"][1].as_object_mut().expect("o").remove("runtime");
                }),
                "names its runtime",
            ),
            // The door starts from its ED's conscious or from every conscious, nothing else.
            (
                change(&|v| {
                    v["nodes"][2]["definition"]["start_from"] = json!(["AI.ed4qm7k2xa@worker1"])
                }),
                "starts from",
            ),
            // Every node of a capability has a middle part; two nodes with one name.
            (
                change(&|v| {
                    v["nodes"][1].as_object_mut().expect("o").remove("middle");
                }),
                "middle name part",
            ),
            (
                change(&|v| {
                    let mut second = v["nodes"][0].clone();
                    second["role"] = json!("billing2");
                    v["nodes"].as_array_mut().expect("nodes").push(second);
                }),
                "another middle name part",
            ),
            (
                change(&|v| v["nodes"][2]["middle"] = json!("2billing")),
                "starts with a letter",
            ),
            // A WF's ILK is the orchestrator's: no template names it; no key goes to a WF.
            (
                change(&|v| v["nodes"][0]["config"]["door"] = json!({"$ref": "nodes.door.ilk_id"})),
                "nodes.door.ilk_id",
            ),
            (
                change(&|v| v["vault"][0]["to"] = json!("door")),
                "not to the WF",
            ),
            // A capability founds nobody.
            (
                change(&|v| v["founds"] = json!({"person": {"display_name": "x"}, "document": {}})),
                "only an executive",
            ),
            // The WF definition's SY.deploy refs are checked; its own are its own.
            (
                change(&|v| {
                    v["nodes"][2]["definition"]["states"][0]["entry_actions"][0]["target"] =
                        json!({"$ref": "nodes.nobody.node_name"})
                }),
                "nodes.nobody.node_name",
            ),
            (
                change(&|v| {
                    v["nodes"][2]["definition"]["description"] =
                        json!({"$ref": "grouping.short_id", "default": "x"})
                }),
                "nothing beside it",
            ),
        ];
        for (value, expected) in cases {
            let err = refused(&value, Reading::Publish);
            assert!(err.contains(expected), "{expected:?} not in {err:?}");
        }
    }

    #[test]
    fn a_capability_of_every_ed_starts_from_the_consciouses_and_names_no_ed() {
        let mut value = billing();
        value["nodes"][2]["definition"]["start_from"] = json!([consciouses_marker()]);
        let parsed = parse_package_value(&value, Reading::Publish).expect("every ED's");
        assert!(parsed.package.for_every_ed());
        assert_eq!(parsed.package.deploys_into(), GROUPING_TENANT);
        // It names no ED: neither its ILK nor its conscious.
        let mut named = value.clone();
        named["nodes"][0]["config"]["speaks_for"] = json!({"$ref": "grouping.ilk_id"});
        assert!(refused(&named, Reading::Publish).contains("grouping.ilk_id"));
        let mut named = value.clone();
        named["nodes"][0]["config"]["conscious"] = json!({"$ref": "grouping.conscious"});
        assert!(refused(&named, Reading::Publish).contains("grouping.conscious"));
        // It is offered to every ED of its tenant.
        let mut narrowed = value;
        narrowed["offered_to"]["eds"] = json!(["ed4qm7k2xa"]);
        assert!(refused(&narrowed, Reading::Publish).contains("all of them"));
    }

    #[test]
    fn an_executive_and_an_infrastructure_hold_their_own_roles() {
        let plain: Value = serde_json::from_str(PLAIN_ED).expect("json");
        let mut no_founds = plain.clone();
        no_founds.as_object_mut().expect("o").remove("founds");
        assert!(refused(&no_founds, Reading::Publish).contains("founds"));
        let mut two = plain.clone();
        two["nodes"][1]["work_role"] = json!("conscious");
        assert!(!refused(&two, Reading::Publish).is_empty());
        let mut agent = plain.clone();
        agent["nodes"][1]["work_role"] = json!("connector");
        assert!(refused(&agent, Reading::Publish).contains("work_role"));
        let mut conscious_ref = plain.clone();
        conscious_ref["nodes"][1]["config"]["io"]["dst_node"] =
            json!({"$ref": "grouping.conscious"});
        assert!(refused(&conscious_ref, Reading::Publish).contains("grouping.conscious"));
        let mut eds = plain;
        eds["offered_to"]["eds"] = json!("all");
        assert!(refused(&eds, Reading::Publish).contains("only a capability"));

        let mut infra = billing();
        infra["kind"] = json!("infrastructure");
        infra["offered_to"] = json!({"tenants": "all"});
        infra["nodes"].as_array_mut().expect("nodes").pop();
        let parsed = parse_package_value(&infra, Reading::Publish).expect("infrastructure");
        assert_eq!(parsed.package.deploys_into(), GROUPING_TENANT);
        let mut with_wf = billing();
        with_wf["kind"] = json!("infrastructure");
        with_wf["offered_to"] = json!({"tenants": "all"});
        assert!(refused(&with_wf, Reading::Publish).contains("work_role"));
        let mut named_ed = infra;
        named_ed["nodes"][0]["config"]["ed"] = json!({"$ref": "grouping.ilk_id"});
        assert!(refused(&named_ed, Reading::Publish).contains("grouping.ilk_id"));
    }

    #[test]
    fn an_offer_names_its_origin_and_a_custom_package_one_tenant() {
        let tenant = "tnt:43d576a3-d712-4d91-9245-5d5463dd693e";
        let other = "tnt:11111111-1111-4111-8111-111111111111";
        let mut custom = billing();
        custom["origin"] = json!("custom");
        custom["offered_to"] = json!({"tenants": [tenant], "eds": ["ed4qm7k2xa"]});
        let offer = parse_package_value(&custom, Reading::Publish)
            .expect("custom")
            .package
            .offer
            .expect("an offer");
        assert_eq!(offer.origin, Origin::Custom);
        assert!(offer.tenants.admits(tenant) && !offer.tenants.admits(other));
        assert!(offer.eds.admits("ed4qm7k2xa") && !offer.eds.admits("ed5aaaaaaa"));
        assert_eq!(
            offer.offered_to_value(PackageKind::Capability),
            json!({"tenants": [tenant], "eds": ["ed4qm7k2xa"]})
        );
        let refused_offer = |origin: Value, offered_to: Value| {
            let mut value = billing();
            value["origin"] = origin;
            value["offered_to"] = offered_to;
            refused(&value, Reading::Publish)
        };
        assert!(refused_offer(json!("custom"), json!({"tenants": "all"})).contains("one tenant"));
        assert!(
            refused_offer(json!("custom"), json!({"tenants": [tenant, other]}))
                .contains("one tenant")
        );
        assert!(refused_offer(json!("mine"), json!({"tenants": "all"})).contains("origin"));
        assert!(refused_offer(json!("catalog"), json!({"tenants": ["acme"]})).contains("tenants"));
        assert!(refused_offer(json!("catalog"), json!({"tenants": []})).contains("tenants"));
        assert!(
            refused_offer(json!("catalog"), json!({"tenants": "all", "eds": ["ed1"]}))
                .contains("eds")
        );
        assert!(
            refused_offer(json!("catalog"), json!({"tenants": "all", "hives": "all"}))
                .contains("hives")
        );
    }

    #[test]
    fn a_section_it_cannot_build_refuses_the_package() {
        let mut value: Value = serde_json::from_str(PLAIN_ED).expect("json");
        value["workflows"] = json!([]);
        let err = refused(&value, Reading::Publish);
        assert!(
            err.contains("cannot build") && err.contains("workflows"),
            "{err}"
        );
        let mut value: Value = serde_json::from_str(PLAIN_ED).expect("json");
        value["extras"] = json!({});
        assert!(parse_package_value(&value, Reading::Publish).is_err());
    }

    #[test]
    fn a_template_naming_what_is_not_declared_refuses_the_package() {
        let plain: Value = serde_json::from_str(PLAIN_ED).expect("json");
        let mut value = plain.clone();
        value["nodes"][1]["config"]["io"]["dst_node"] = json!({"$ref": "nodes.brain.node_name"});
        assert!(refused(&value, Reading::Publish).contains("nodes.brain.node_name"));
        let mut value = plain.clone();
        value["nodes"][0]["config"]["speaks_as"] = json!({"$blank": "boss"});
        assert!(refused(&value, Reading::Publish).contains("boss"));
        let mut value = plain.clone();
        value["nodes"][0]["config"]["x"] = json!({"$schema": "y"});
        assert!(refused(&value, Reading::Publish).contains("placeholders"));
        let mut value = plain.clone();
        value["nodes"][0]["kind"] = json!("XX");
        assert!(parse_package_value(&value, Reading::Publish).is_err());
        let mut value = plain;
        value["vault"][0]["to"] = json!("nobody");
        assert!(parse_package_value(&value, Reading::Publish).is_err());
    }

    #[test]
    fn a_wf_definition_takes_its_blanks_and_sy_deploy_s_refs_and_keeps_its_own() {
        let parsed = parse_package_value(&billing(), Reading::Publish).expect("billing");
        let door = parsed.package.door().expect("door").clone();
        let refs = BTreeMap::from([
            (
                REF_GROUPING_CONSCIOUS.to_string(),
                json!("AI.ed4qm7k2xa@worker1"),
            ),
            (REF_GROUPING_SHORT_ID.to_string(), json!("ed4qm7k2xa")),
            (
                node_name_ref("billing"),
                json!("AI.billing.ed4qm7k2xa@worker1"),
            ),
        ]);
        let template = door.workflow.as_ref().expect("wf");
        let definition = resolve_workflow(template, &json!({}), &refs).expect("resolved");
        // SY.deploy's refs filled: the ED's conscious, the agent's node name, the short id.
        assert_eq!(definition["start_from"], json!(["AI.ed4qm7k2xa@worker1"]));
        let action = &definition["states"][0]["entry_actions"][0];
        assert_eq!(action["target"], json!("AI.billing.ed4qm7k2xa@worker1"));
        assert_eq!(action["vars"]["ed"], json!("ed4qm7k2xa"));
        assert_eq!(
            definition["states"][0]["transitions"][0]["event_match"]["src"],
            json!("AI.billing.ed4qm7k2xa@worker1")
        );
        // The WF's own, exactly as written: input.*, event.* with its default, and a JSON
        // Schema's $schema.
        assert_eq!(action["vars"]["number"], json!({"$ref": "input.number"}));
        assert_eq!(
            definition["states"][0]["transitions"][0]["actions"][0]["result"]["state"],
            json!({"$ref": "event.context.evaluation.params.state", "default": ""})
        );
        assert_eq!(
            definition["input_schema"]["$schema"],
            json!("https://json-schema.org/draft/2020-12/schema")
        );
        // Any other root passes too (state.*, vars.*): the WF engine checks its own refs.
        let own = json!({
            "a": {"$ref": "state.last_reply"},
            "b": {"$ref": "vars.number", "default": 0},
            "c": [{"$ref": "input.items"}, {"$ref": "grouping.short_id"}]
        });
        let blank_names = BTreeSet::new();
        let known: BTreeSet<String> = refs.keys().cloned().collect();
        check_template(&own, &blank_names, &known, "a WF", TemplateKind::Workflow)
            .expect("the WF's own refs pass the checks");
        assert_eq!(
            resolve_workflow(&own, &json!({}), &refs).expect("resolved"),
            json!({
                "a": {"$ref": "state.last_reply"},
                "b": {"$ref": "vars.number", "default": 0},
                "c": [{"$ref": "input.items"}, "ed4qm7k2xa"]
            })
        );
        // Outside a WF definition an unknown $ref is refused, as before.
        assert!(check_template(
            &json!({"x": {"$ref": "input.number"}}),
            &blank_names,
            &known,
            "a config",
            TemplateKind::Plain
        )
        .is_err());
        // The marker is written as it is.
        let marker = json!({"start_from": [consciouses_marker()]});
        assert_eq!(
            resolve_workflow(&marker, &json!({}), &BTreeMap::new()).expect("marker"),
            marker
        );
    }

    #[test]
    fn the_blanks_are_exactly_the_package_s_each_checked_by_its_type() {
        let package = plain_ed().package;
        let blanks = json!({
            "display_name": "Laura Gómez",
            "company_name": "Acme",
            "personality": {"timezone": "America/Argentina/Buenos_Aires", "country_code": "AR", "primary_language": "es"},
            "whatsapp": {"phone_number_id": "1234567890", "waba_id": "998877", "credential_key": "acme-laura-wapp"}
        });
        assert_eq!(check_blanks(&package, &blanks), Ok(()));
        let refused = |change: &dyn Fn(&mut Value), blank: &str| {
            let mut changed = blanks.clone();
            change(&mut changed);
            let err = check_blanks(&package, &changed).expect_err("refused");
            assert_eq!(err.blank, blank, "{err}");
            assert!(!err.to_string().contains("Laura"), "{err}");
        };
        refused(&|b| b["boss"] = json!("x"), "boss");
        refused(
            &|b| b["display_name"] = json!("x".repeat(129)),
            "display_name",
        );
        refused(
            &|b| b["display_name"] = json!("Laura\nGómez"),
            "display_name",
        );
        refused(&|b| b["personality"]["mood"] = json!("x"), "personality");
        refused(
            &|b| b["whatsapp"]["phone_number_id"] = json!("+54 9 11"),
            "whatsapp",
        );
        refused(
            &|b| b["whatsapp"]["credential_key"] = json!("Bad Key"),
            "whatsapp",
        );
        assert_eq!(
            key_requirements(&package, &blanks),
            [KeyRequirement {
                key: "acme-laura-wapp".to_string(),
                resource_type: "whatsapp".to_string()
            }]
        );
        assert_eq!(whatsapp_numbers(&package, &blanks), ["1234567890"]);
        let billing = parse_package_value(&billing(), Reading::Publish)
            .expect("billing")
            .package;
        assert_eq!(
            check_blanks(&billing, &json!({"crm_key": "Not A Key"}))
                .expect_err("refused")
                .blank,
            "crm_key"
        );
        assert_eq!(
            key_requirements(&billing, &json!({"crm_key": "acme-crm"})),
            [KeyRequirement {
                key: "acme-crm".to_string(),
                resource_type: "api_key".to_string()
            }]
        );
    }

    #[test]
    fn a_template_takes_its_blanks_and_refs_and_drops_a_missing_optional() {
        let blanks = json!({"personality": {"timezone": "America/Argentina/Buenos_Aires"}});
        let template = json!({
            "a": {"$blank": "personality.timezone"},
            "b": {"$blank": "personality.region_code", "optional": true},
            "c": [{"$ref": "grouping.hive"}, {"$blank": "nope.x", "optional": true}],
            "d": 3
        });
        let refs = BTreeMap::from([(REF_GROUPING_HIVE.to_string(), json!("worker1"))]);
        assert_eq!(
            resolve_template(&template, &blanks, &refs).expect("resolved"),
            json!({"a": "America/Argentina/Buenos_Aires", "c": ["worker1"], "d": 3})
        );
        assert!(resolve_template(
            &json!({"$blank": "personality.region_code"}),
            &blanks,
            &refs
        )
        .is_err());
        assert!(resolve_template(&json!({"$ref": "grouping.tenant_id"}), &blanks, &refs).is_err());
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
        assert_eq!(versions_kept(&versions, &in_use, 3).len(), 4);
        assert_eq!(compare_versions("9", "10"), std::cmp::Ordering::Less);
    }
}
