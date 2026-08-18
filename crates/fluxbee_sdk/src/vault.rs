use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use std::sync::Arc;

use crate::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use crate::rpc::{PendingMatcher, RouteMatch, RouterDispatcher, RpcError, RpcRequestLabels};
use crate::NodeError;

pub const VAULT_REF_PREFIX: &str = "vault://";

pub const MSG_VAULT_PUT: &str = "VAULT_PUT";
pub const MSG_VAULT_PUT_RESPONSE: &str = "VAULT_PUT_RESPONSE";
pub const MSG_VAULT_GET: &str = "VAULT_GET";
pub const MSG_VAULT_GET_RESPONSE: &str = "VAULT_GET_RESPONSE";
pub const MSG_VAULT_GET_METADATA: &str = "VAULT_GET_METADATA";
pub const MSG_VAULT_GET_METADATA_RESPONSE: &str = "VAULT_GET_METADATA_RESPONSE";
pub const MSG_VAULT_LIST: &str = "VAULT_LIST";
pub const MSG_VAULT_LIST_RESPONSE: &str = "VAULT_LIST_RESPONSE";
pub const MSG_VAULT_DELETE: &str = "VAULT_DELETE";
pub const MSG_VAULT_DELETE_RESPONSE: &str = "VAULT_DELETE_RESPONSE";
pub const MSG_VAULT_ROTATE: &str = "VAULT_ROTATE";
pub const MSG_VAULT_ROTATE_RESPONSE: &str = "VAULT_ROTATE_RESPONSE";
pub const MSG_VAULT_ROLLBACK: &str = "VAULT_ROLLBACK";
pub const MSG_VAULT_ROLLBACK_RESPONSE: &str = "VAULT_ROLLBACK_RESPONSE";

/// Canonical resource types vault knows about. Drives the consumer-side
/// match in Model D' (`resolve_resource(ResourceType::Openai, ...)` → vault
/// returns the most recent secret with `metadata.resource_type == "openai"`).
///
/// Serialized as a plain lowercase `snake_case` string on the wire so the
/// JSON payload is consistent regardless of which language/SDK builds the
/// request. `Custom` covers providers not yet in the enum; promote to a
/// dedicated variant once stable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceType {
    Postgres,
    Mysql,
    Redis,
    Mongodb,
    Openai,
    Anthropic,
    Gemini,
    Mistral,
    Cohere,
    Perplexity,
    GoogleCalendar,
    Gmail,
    GoogleDrive,
    GoogleSheets,
    GoogleDocs,
    GoogleSlides,
    GoogleCloud,
    MicrosoftGraph,
    OutlookEmail,
    OutlookCalendar,
    Teams,
    Sharepoint,
    Slack,
    Discord,
    Hubspot,
    Salesforce,
    LinkedHelper,
    Github,
    Gitlab,
    Jira,
    Linear,
    Notion,
    Stripe,
    Twilio,
    Sendgrid,
    Smtp,
    Imap,
    Aws,
    Azure,
    S3,
    Webhook,
    BearerToken,
    ApiKey,
    OAuthBundle,
    /// String must already be normalized (`normalize_resource_type`).
    Custom(String),
}

impl ResourceType {
    pub fn as_str(&self) -> &str {
        match self {
            ResourceType::Postgres => "postgres",
            ResourceType::Mysql => "mysql",
            ResourceType::Redis => "redis",
            ResourceType::Mongodb => "mongodb",
            ResourceType::Openai => "openai",
            ResourceType::Anthropic => "anthropic",
            ResourceType::Gemini => "gemini",
            ResourceType::Mistral => "mistral",
            ResourceType::Cohere => "cohere",
            ResourceType::Perplexity => "perplexity",
            ResourceType::GoogleCalendar => "google_calendar",
            ResourceType::Gmail => "gmail",
            ResourceType::GoogleDrive => "google_drive",
            ResourceType::GoogleSheets => "google_sheets",
            ResourceType::GoogleDocs => "google_docs",
            ResourceType::GoogleSlides => "google_slides",
            ResourceType::GoogleCloud => "google_cloud",
            ResourceType::MicrosoftGraph => "microsoft_graph",
            ResourceType::OutlookEmail => "outlook_email",
            ResourceType::OutlookCalendar => "outlook_calendar",
            ResourceType::Teams => "teams",
            ResourceType::Sharepoint => "sharepoint",
            ResourceType::Slack => "slack",
            ResourceType::Discord => "discord",
            ResourceType::Hubspot => "hubspot",
            ResourceType::Salesforce => "salesforce",
            ResourceType::LinkedHelper => "linked_helper",
            ResourceType::Github => "github",
            ResourceType::Gitlab => "gitlab",
            ResourceType::Jira => "jira",
            ResourceType::Linear => "linear",
            ResourceType::Notion => "notion",
            ResourceType::Stripe => "stripe",
            ResourceType::Twilio => "twilio",
            ResourceType::Sendgrid => "sendgrid",
            ResourceType::Smtp => "smtp",
            ResourceType::Imap => "imap",
            ResourceType::Aws => "aws",
            ResourceType::Azure => "azure",
            ResourceType::S3 => "s3",
            ResourceType::Webhook => "webhook",
            ResourceType::BearerToken => "bearer_token",
            ResourceType::ApiKey => "api_key",
            ResourceType::OAuthBundle => "oauth_bundle",
            ResourceType::Custom(s) => s.as_str(),
        }
    }

    /// Parse from the wire string (already normalized). Unknown values
    /// become `Custom`. Use [`normalize_resource_type`] first if the input
    /// might not be canonical.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "postgres" => Self::Postgres,
            "mysql" => Self::Mysql,
            "redis" => Self::Redis,
            "mongodb" => Self::Mongodb,
            "openai" => Self::Openai,
            "anthropic" => Self::Anthropic,
            "gemini" => Self::Gemini,
            "mistral" => Self::Mistral,
            "cohere" => Self::Cohere,
            "perplexity" => Self::Perplexity,
            "google_calendar" => Self::GoogleCalendar,
            "gmail" => Self::Gmail,
            "google_drive" => Self::GoogleDrive,
            "google_sheets" => Self::GoogleSheets,
            "google_docs" => Self::GoogleDocs,
            "google_slides" => Self::GoogleSlides,
            "google_cloud" => Self::GoogleCloud,
            "microsoft_graph" => Self::MicrosoftGraph,
            "outlook_email" => Self::OutlookEmail,
            "outlook_calendar" => Self::OutlookCalendar,
            "teams" => Self::Teams,
            "sharepoint" => Self::Sharepoint,
            "slack" => Self::Slack,
            "discord" => Self::Discord,
            "hubspot" => Self::Hubspot,
            "salesforce" => Self::Salesforce,
            "linked_helper" => Self::LinkedHelper,
            "github" => Self::Github,
            "gitlab" => Self::Gitlab,
            "jira" => Self::Jira,
            "linear" => Self::Linear,
            "notion" => Self::Notion,
            "stripe" => Self::Stripe,
            "twilio" => Self::Twilio,
            "sendgrid" => Self::Sendgrid,
            "smtp" => Self::Smtp,
            "imap" => Self::Imap,
            "aws" => Self::Aws,
            "azure" => Self::Azure,
            "s3" => Self::S3,
            "webhook" => Self::Webhook,
            "bearer_token" => Self::BearerToken,
            "api_key" => Self::ApiKey,
            "oauth_bundle" => Self::OAuthBundle,
            other => Self::Custom(other.to_string()),
        }
    }

    pub fn known_wire_values() -> &'static [&'static str] {
        &[
            "postgres",
            "mysql",
            "redis",
            "mongodb",
            "openai",
            "anthropic",
            "gemini",
            "mistral",
            "cohere",
            "perplexity",
            "google_calendar",
            "gmail",
            "google_drive",
            "google_sheets",
            "google_docs",
            "google_slides",
            "google_cloud",
            "microsoft_graph",
            "outlook_email",
            "outlook_calendar",
            "teams",
            "sharepoint",
            "slack",
            "discord",
            "hubspot",
            "salesforce",
            "linked_helper",
            "github",
            "gitlab",
            "jira",
            "linear",
            "notion",
            "stripe",
            "twilio",
            "sendgrid",
            "smtp",
            "imap",
            "aws",
            "azure",
            "s3",
            "webhook",
            "bearer_token",
            "api_key",
            "oauth_bundle",
        ]
    }
}

impl serde::Serialize for ResourceType {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ResourceType {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Ok(ResourceType::from_wire(&s))
    }
}

/// Normalize a free-form resource type string into the canonical wire form.
/// Lowercase, replace runs of non-alphanumeric with single `_`, trim
/// leading/trailing `_`. Rejects empty / digits-only / over 64 chars.
///
/// Examples: `"OpenAI"` → `"openai"`, `"Google Calendar"` → `"google_calendar"`,
/// `"linked-helper"` → `"linked_helper"`.
pub fn normalize_resource_type(raw: &str) -> Result<String, String> {
    let lower = raw.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_underscore = true;
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        return Err(format!("resource_type '{raw}' normalized to empty string"));
    }
    if out.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "resource_type '{raw}' must contain at least one letter"
        ));
    }
    if out.len() > 64 {
        return Err(format!("resource_type '{out}' is too long (max 64 chars)"));
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultMetadata {
    pub tenant_id: String,
    /// Legacy field in the Phase J (pre-Model-D') schema. New consumers in
    /// Model D' should use `resource_type` + optional `ilk` instead and
    /// leave `owner_ilk` defaulted (empty string is acceptable on the wire
    /// during the transition; vault rewrites under Phase J' drop it).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_ilk: String,
    /// Canonical resource type (e.g. "openai", "postgres"). Required in
    /// Model D' write paths; `None` only for legacy reads of older secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    /// Owner ILK in Model D'. `None` means the secret lives in the tenant's
    /// pool and any caller of the same tenant can read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultPutRequest {
    pub key: String,
    pub value: Value,
    pub metadata: VaultMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultPutResponse {
    pub status: String,
    pub key: String,
    pub version: i64,
    #[serde(default)]
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultGetRequest {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultGetMetadataRequest {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultValueResponse {
    pub status: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<VaultMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub type VaultGetResponse = VaultValueResponse;
pub type VaultGetMetadataResponse = VaultValueResponse;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Filter by resource type (canonical wire form, e.g. "openai"). New
    /// in Model D'; older vault servers ignore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    /// Filter by owner ILK. Pass `Some("ilk:<uuid>")` to match secrets
    /// dedicated to that ILK; pass `Some("")` to match pool secrets
    /// explicitly. `None` means "don't filter by owner".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilk: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<VaultFilter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSecretSummary {
    pub key: String,
    pub metadata: VaultMetadata,
    pub version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_accessed_at: Option<String>,
    pub access_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListResponse {
    pub status: String,
    pub count: usize,
    #[serde(default)]
    pub secrets: Vec<VaultSecretSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultKeyRequest {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultRotateRequest {
    pub key: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDeleteResponse {
    pub status: String,
    pub key: String,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultRotateResponse {
    pub status: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultRollbackResponse {
    pub status: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct VaultRetryPolicy {
    pub max_elapsed: Duration,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub jitter_ratio: f64,
}

impl Default for VaultRetryPolicy {
    fn default() -> Self {
        Self {
            max_elapsed: Duration::from_secs(60),
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
            jitter_ratio: 0.20,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("node error: {0}")]
    Node(#[from] NodeError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("vault action timed out action={action} trace_id={trace_id} target={target} timeout_ms={timeout_ms}")]
    ActionTimeout {
        action: String,
        trace_id: String,
        target: String,
        timeout_ms: u64,
    },
    /// The request never reached the vault: no route, no gateway, peer down.
    ///
    /// Distinct from [`VaultError::Service`] on purpose. Collapsing transport failures into
    /// `Service` told every consumer "the vault reached a verdict" when in fact nothing came
    /// back — and SY.edge, reading that, refused to retry a cert reload that a one-second WAN
    /// blip had interrupted, abandoning a legitimate rotation until the certificate expired.
    #[error("vault unreachable: reason={reason} original_dst={original_dst}")]
    Unreachable {
        reason: String,
        original_dst: String,
    },
    /// The request expired in the mesh before arriving. Also transport, also not a verdict.
    #[error("vault ttl exceeded: original_dst={original_dst} last_hop={last_hop}")]
    TtlExceeded {
        original_dst: String,
        last_hop: String,
    },
    #[error("vault returned error code={code} message={message}")]
    Service { code: String, message: String },
    #[error("invalid vault ref")]
    InvalidVaultRef,
    #[error("vault response missing value field for key={key}")]
    EmptyValue { key: String },
}

/// How long a boot-time consumer waits for the vault to become REACHABLE before starting
/// degraded. Generous on purpose: the cost of waiting is a slower boot, while the cost of
/// giving up too early is a node that stays broken until someone notices and restarts it.
pub const VAULT_BOOT_WAIT: Duration = Duration::from_secs(60);
/// Gap between reachability attempts while inside [`VAULT_BOOT_WAIT`].
pub const VAULT_BOOT_RETRY_INTERVAL: Duration = Duration::from_millis(750);

/// The charset SY.vault accepts for a secret key: `[a-z0-9:_-]`, first char `[a-z0-9]`, 1..=256
/// bytes.
///
/// Lives here because the rule has TWO sides that ship in different cargo workspaces: SY.vault
/// enforces it, and every consumer that names a key in config (IO.slack's `slack.auth.key`, and
/// anything that follows the vault_ref family pattern) has to respect it. When only one side knew
/// the rule, the io.slack test fixtures modelled `slack/IO.slack@motherbee` — four illegal
/// characters — and stayed green forever because a unit test never calls the vault. Copying that
/// example into a real hive gets `INVALID_REQUEST: invalid key format`.
pub fn vault_key_is_valid(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 256 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b':' | b'_' | b'-'))
}

/// True when nothing came back, as opposed to the vault answering.
///
/// Only these are worth retrying. Keep this the single definition of that split: SY.edge's TLS
/// reload and the boot-time DB lookups all depend on "no verdict" meaning the same thing.
pub fn vault_failure_is_transport(err: &VaultError) -> bool {
    match err {
        VaultError::Node(_)
        | VaultError::ActionTimeout { .. }
        | VaultError::Unreachable { .. }
        | VaultError::TtlExceeded { .. } => true,
        VaultError::Json(_)
        | VaultError::Service { .. }
        | VaultError::InvalidVaultRef
        | VaultError::EmptyValue { .. } => false,
    }
}

/// Identity of the caller of a vault L2 action.
///
/// Both fields are mandatory:
/// - `src_ilk`: caller's own ILK (`ilk:<uuid>`), resolved at boot from
///   identity SHM via `fluxbee_sdk::identity::wait_for_self_system_ilk_id`.
///   Vault uses it to look up the caller's `tenant_id` and `ilk_type` for
///   same-tenant/owner authorisation (vault spec D1, VA-D2/D4/D5).
/// - `src_l2_name`: caller's full L2 name (`SY.foo@motherbee`), used by vault
///   for the `SY.admin@*` / `SY.architect@*` override fast-path (VA-D3).
///
/// Callers that omit these will be rejected by vault for any tenant-scoped
/// auth path. The override fast-path only covers admin/architect.
#[derive(Debug, Clone, Copy)]
pub struct VaultCaller<'a> {
    pub src_ilk: &'a str,
    pub src_l2_name: &'a str,
}

impl<'a> VaultCaller<'a> {
    pub fn new(src_ilk: &'a str, src_l2_name: &'a str) -> Self {
        Self {
            src_ilk,
            src_l2_name,
        }
    }
}

fn ensure_ok(
    status: &str,
    error_code: Option<&str>,
    message: Option<&str>,
) -> Result<(), VaultError> {
    if status.eq_ignore_ascii_case("ok") {
        return Ok(());
    }
    Err(VaultError::Service {
        code: error_code.unwrap_or("VAULT_ERROR").to_string(),
        message: message
            .unwrap_or("vault returned non-ok status")
            .to_string(),
    })
}

/// Owned variant of [`VaultCaller`]. The dispatcher-backed [`VaultClient`]
/// stores it so callers don't need to keep the original `&str` slices alive
/// for the lifetime of the client.
#[derive(Debug, Clone)]
pub struct VaultCallerOwned {
    pub src_ilk: String,
    pub src_l2_name: String,
}

impl VaultCallerOwned {
    pub fn new(src_ilk: impl Into<String>, src_l2_name: impl Into<String>) -> Self {
        Self {
            src_ilk: src_ilk.into(),
            src_l2_name: src_l2_name.into(),
        }
    }

    fn as_borrowed(&self) -> VaultCaller<'_> {
        VaultCaller {
            src_ilk: &self.src_ilk,
            src_l2_name: &self.src_l2_name,
        }
    }
}

impl<'a> From<VaultCaller<'a>> for VaultCallerOwned {
    fn from(value: VaultCaller<'a>) -> Self {
        Self {
            src_ilk: value.src_ilk.to_string(),
            src_l2_name: value.src_l2_name.to_string(),
        }
    }
}

/// Typed Vault client built over the shared [`RouterDispatcher`]. Replaces
/// the legacy free `resolve_resource(&NodeSender, &mut NodeReceiver, …)` path
/// — same wire shape (same `meta.src_ilk`, same `routing.src_l2_name`, same
/// `MSG_VAULT_*` action codes), but the response is awaited through the
/// dispatcher's `send_with_matcher`, so concurrent vault calls multiplex by
/// `trace_id` and unrelated SYSTEM traffic doesn't satisfy our waiter.
///
/// `SY.vault` sees zero behavior change.
#[derive(Clone)]
pub struct VaultClient {
    dispatcher: Arc<RouterDispatcher>,
    hive_id: String,
    caller: VaultCallerOwned,
}

impl std::fmt::Debug for VaultClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultClient")
            .field("hive_id", &self.hive_id)
            .field("caller", &self.caller)
            .finish_non_exhaustive()
    }
}

impl VaultClient {
    pub fn new(
        dispatcher: Arc<RouterDispatcher>,
        hive_id: impl Into<String>,
        caller: VaultCallerOwned,
    ) -> Self {
        Self {
            dispatcher,
            hive_id: hive_id.into(),
            caller,
        }
    }

    pub fn caller(&self) -> VaultCaller<'_> {
        self.caller.as_borrowed()
    }

    pub fn hive_id(&self) -> &str {
        &self.hive_id
    }

    /// The canonical router dispatcher this client rides on. Exposed so in-mesh tools
    /// (e.g. the AI publish action) can send an `ADMIN_COMMAND` to `SY.admin` over the
    /// same socket without the node threading a second dispatcher handle.
    pub fn dispatcher(&self) -> &std::sync::Arc<RouterDispatcher> {
        &self.dispatcher
    }

    /// Vault target node for this client's hive: `SY.vault@<hive>`.
    fn vault_target(&self) -> String {
        format!("SY.vault@{}", self.hive_id)
    }

    /// Build the on-wire message for an individual vault action. Preserves
    /// the exact shape used by the legacy `send_action_once` so SY.vault
    /// authorization (`src_ilk`, `src_l2_name`, `target`) is unchanged.
    fn build_vault_message(&self, target: &str, action: &str, payload: Value) -> Message {
        Message {
            routing: Routing {
                // dispatcher will substitute its own UUID if this is empty;
                // we leave it blank to let the dispatcher own connection
                // identity.
                src: String::new(),
                src_l2_name: Some(self.caller.src_l2_name.clone()),
                dst: Destination::Unicast(target.to_string()),
                ttl: 16,
                // dispatcher generates the trace_id when empty.
                trace_id: String::new(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(action.to_string()),
                src_ilk: Some(self.caller.src_ilk.clone()),
                target: Some(target.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    fn vault_matcher(response_msg: &str) -> PendingMatcher {
        PendingMatcher::new(
            vec![RouteMatch::exact(SYSTEM_KIND, response_msg)],
            vec![
                RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
                RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
            ],
            // Unrelated SYSTEM messages with a colliding trace are flagged
            // as malformed responses (same posture as send_system_rpc).
            vec![RouteMatch::any_msg_type(SYSTEM_KIND)],
        )
    }

    async fn send_vault_action(
        &self,
        action: &str,
        response_msg: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, VaultError> {
        let target = self.vault_target();
        let outgoing = self.build_vault_message(&target, action, payload);
        let matcher = Self::vault_matcher(response_msg);
        let labels = RpcRequestLabels::new(&target, action, response_msg);
        let response = self
            .dispatcher
            .send_with_matcher(outgoing, matcher, labels, timeout)
            .await
            .map_err(map_rpc_error)?;
        Ok(response.payload)
    }

    /// Typed `VAULT_GET` over the shared dispatcher.
    pub async fn get(&self, key: &str, timeout: Duration) -> Result<VaultGetResponse, VaultError> {
        let payload = json!(VaultGetRequest {
            key: key.to_string()
        });
        let raw = self
            .send_vault_action(MSG_VAULT_GET, MSG_VAULT_GET_RESPONSE, payload, timeout)
            .await?;
        let parsed: VaultGetResponse = serde_json::from_value(raw)?;
        ensure_ok(
            &parsed.status,
            parsed.error_code.as_deref(),
            parsed.message.as_deref(),
        )?;
        Ok(parsed)
    }

    /// Typed `VAULT_LIST` over the shared dispatcher.
    pub async fn list(
        &self,
        filter: Option<VaultFilter>,
        timeout: Duration,
    ) -> Result<VaultListResponse, VaultError> {
        let payload = json!(VaultListRequest { filter });
        let raw = self
            .send_vault_action(MSG_VAULT_LIST, MSG_VAULT_LIST_RESPONSE, payload, timeout)
            .await?;
        let parsed: VaultListResponse = serde_json::from_value(raw)?;
        ensure_ok(
            &parsed.status,
            parsed.error_code.as_deref(),
            parsed.message.as_deref(),
        )?;
        Ok(parsed)
    }

    /// Model D' resource resolution, identical semantics to the legacy free
    /// `resolve_resource`:
    /// 1. Dedicated secret for the caller's ILK (skipped when no ILK).
    /// 2. Pool secret for the caller's tenant.
    /// 3. Root-tenant pool secret (skipped when caller is already root).
    /// [`Self::resolve_resource`], but waiting out a vault that is not up YET.
    ///
    /// Boot-time consumers (SY.storage, SY.identity) need a secret before they can serve
    /// anything, and on a `.deb` upgrade systemd restarts the whole hive at once — so losing the
    /// race against `sy-vault` is normal, not exceptional. The push-based rescue those consumers
    /// already have (react to a later `VAULT_SECRET_CHANGED`) does not cover it: the vault's
    /// bootstrap announcements are a ONE-SHOT event, and a consumer that was not yet listening
    /// never gets a second one. It then sits degraded until a human restarts it — which is
    /// exactly how a hive's admin plane stayed down behind `STORAGE_NOT_READY`.
    ///
    /// So: PULL until the vault actually answers, and only then let the caller judge the answer.
    ///
    /// Retries transport failures only. A vault that reached a verdict — "no such secret",
    /// "denied", an unusable payload — will reach the same one on the next attempt, and spinning
    /// on it would only delay an honest degraded start.
    pub async fn resolve_resource_awaiting_vault(
        &self,
        resource: ResourceType,
        my_tenant: &str,
        timeout: Duration,
        budget: Duration,
        who: &str,
    ) -> Result<Option<Value>, VaultError> {
        let started_at = Instant::now();
        loop {
            let attempt = self
                .resolve_resource(resource.clone(), my_tenant, timeout)
                .await;
            let err = match attempt {
                Err(err) if vault_failure_is_transport(&err) => err,
                other => return other,
            };
            let waited = started_at.elapsed();
            if waited >= budget {
                tracing::warn!(
                    who = %who,
                    waited_secs = waited.as_secs(),
                    error = %err,
                    "vault never became reachable within the boot budget; giving up"
                );
                return Err(err);
            }
            tracing::warn!(
                who = %who,
                waited_secs = waited.as_secs(),
                budget_secs = budget.as_secs(),
                error = %err,
                "vault not reachable yet; retrying"
            );
            tokio::time::sleep(VAULT_BOOT_RETRY_INTERVAL).await;
        }
    }

    pub async fn resolve_resource(
        &self,
        resource: ResourceType,
        my_tenant: &str,
        timeout: Duration,
    ) -> Result<Option<Value>, VaultError> {
        let resource_str = resource.as_str().to_string();

        if !self.caller.src_ilk.is_empty() {
            if let Some(value) = self
                .list_then_get_first(
                    &resource_str,
                    my_tenant,
                    Some(self.caller.src_ilk.clone()),
                    timeout,
                )
                .await?
            {
                return Ok(Some(value));
            }
        }

        if let Some(value) = self
            .list_then_get_first(&resource_str, my_tenant, Some(String::new()), timeout)
            .await?
        {
            return Ok(Some(value));
        }

        if my_tenant != crate::identity::DEFAULT_ROOT_TENANT_ID {
            if let Some(value) = self
                .list_then_get_first(
                    &resource_str,
                    crate::identity::DEFAULT_ROOT_TENANT_ID,
                    Some(String::new()),
                    timeout,
                )
                .await?
            {
                return Ok(Some(value));
            }
        }

        Ok(None)
    }

    async fn list_then_get_first(
        &self,
        resource_type: &str,
        tenant_id: &str,
        ilk_filter: Option<String>,
        timeout: Duration,
    ) -> Result<Option<Value>, VaultError> {
        let filter = VaultFilter {
            prefix: None,
            tenant_id: Some(tenant_id.to_string()),
            resource_type: Some(resource_type.to_string()),
            ilk: ilk_filter,
            tags: Vec::new(),
            limit: Some(1),
        };
        let list = self.list(Some(filter), timeout).await?;
        let Some(summary) = list.secrets.into_iter().next() else {
            return Ok(None);
        };
        let response = self.get(&summary.key, timeout).await?;
        Ok(response.value)
    }
}

/// Bridge `RpcError` → `VaultError`. Vault callers care about three things:
/// transport disconnect, action timeout, and everything else (`Service`).
fn map_rpc_error(err: RpcError) -> VaultError {
    match err {
        RpcError::Node(node) => VaultError::Node(node),
        RpcError::Disconnected => VaultError::Node(NodeError::Disconnected),
        RpcError::Timeout {
            trace_id,
            target,
            request_msg,
            timeout_ms,
            ..
        } => VaultError::ActionTimeout {
            action: request_msg,
            trace_id,
            target,
            timeout_ms,
        },
        RpcError::Unreachable {
            reason,
            original_dst,
        } => VaultError::Unreachable {
            reason,
            original_dst,
        },
        RpcError::TtlExceeded {
            original_dst,
            last_hop,
        } => VaultError::TtlExceeded {
            original_dst,
            last_hop,
        },
        other => VaultError::Service {
            code: "VAULT_RPC_ERROR".to_string(),
            message: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_type_known_values_round_trip_as_wire_strings() {
        let cases = [
            (ResourceType::Openai, "openai"),
            (ResourceType::GoogleCalendar, "google_calendar"),
            (ResourceType::GoogleSheets, "google_sheets"),
            (ResourceType::MicrosoftGraph, "microsoft_graph"),
            (ResourceType::OutlookCalendar, "outlook_calendar"),
            (ResourceType::LinkedHelper, "linked_helper"),
            (ResourceType::BearerToken, "bearer_token"),
            (ResourceType::OAuthBundle, "oauth_bundle"),
        ];
        for (resource, wire) in cases {
            assert_eq!(resource.as_str(), wire);
            assert_eq!(ResourceType::from_wire(wire), resource);
            assert_eq!(
                serde_json::to_string(&resource).unwrap(),
                format!("\"{wire}\"")
            );
            assert_eq!(
                serde_json::from_str::<ResourceType>(&format!("\"{wire}\"")).unwrap(),
                resource
            );
        }
    }

    #[test]
    fn resource_type_custom_preserves_normalized_unknown_values() {
        let resource = ResourceType::from_wire("new_provider");
        assert_eq!(resource, ResourceType::Custom("new_provider".to_string()));
        assert_eq!(
            serde_json::to_string(&resource).unwrap(),
            "\"new_provider\""
        );
    }

    #[test]
    fn normalize_resource_type_accepts_common_display_names() {
        let cases = [
            ("OpenAI", "openai"),
            ("Google Calendar", "google_calendar"),
            ("Linked-Helper", "linked_helper"),
            ("Bearer Token", "bearer_token"),
            ("OAuth Bundle", "oauth_bundle"),
            ("Microsoft Graph", "microsoft_graph"),
        ];
        for (raw, expected) in cases {
            assert_eq!(normalize_resource_type(raw).unwrap(), expected);
        }
    }

    fn vault_test_profile() -> crate::rpc::OperationalRouteProfile {
        // Minimal: a single command channel, no pre-pending rules.
        // Vault responses must flow through the pending-matcher path; any
        // pre-pending rule covering SYSTEM_KIND would intercept them before
        // the matcher gets a chance.
        crate::rpc::OperationalRouteProfile::builder()
            .command_channel("system")
            .post_pending_rule(
                RouteMatch::any_msg_type(SYSTEM_KIND),
                crate::rpc::RouteTarget::Command("system"),
            )
            .build()
            .expect("profile builds")
    }

    fn caller() -> VaultCallerOwned {
        VaultCallerOwned::new("ilk:1111-test", "SY.test@motherbee")
    }

    fn ok_get_response(trace_id: &str, key: &str, value: Value) -> Message {
        Message {
            routing: Routing {
                src: "vault-uuid".to_string(),
                src_l2_name: Some("SY.vault@motherbee".to_string()),
                dst: Destination::Unicast("SY.test@motherbee".to_string()),
                ttl: 16,
                trace_id: trace_id.to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_VAULT_GET_RESPONSE.to_string()),
                ..Meta::default()
            },
            payload: json!({
                "status": "ok",
                "key": key,
                "value": value,
            }),
        }
    }

    #[tokio::test]
    async fn vault_client_preserves_src_ilk_and_src_l2_name_on_the_wire() {
        let (dispatcher, mut harness) =
            crate::rpc::RouterDispatcherTestHarness::new("SY.test@motherbee", vault_test_profile());
        let client = VaultClient::new(dispatcher.clone(), "motherbee".to_string(), caller());

        // Spawn a get; we don't care about the response for this test, only
        // about what shows up on the wire.
        let _bg = tokio::spawn({
            let client = client.clone();
            async move { client.get("kv/openai", Duration::from_millis(100)).await }
        });

        let outgoing = harness
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("vault request reaches the wire");
        assert_eq!(outgoing.meta.msg.as_deref(), Some(MSG_VAULT_GET));
        assert_eq!(outgoing.meta.msg_type, SYSTEM_KIND);
        assert_eq!(outgoing.meta.src_ilk.as_deref(), Some("ilk:1111-test"));
        assert_eq!(
            outgoing.routing.src_l2_name.as_deref(),
            Some("SY.test@motherbee")
        );
        assert_eq!(outgoing.meta.target.as_deref(), Some("SY.vault@motherbee"));
        assert!(!outgoing.routing.trace_id.is_empty());
    }

    #[tokio::test]
    async fn vault_client_multiplexes_concurrent_calls_by_trace_id() {
        let (dispatcher, mut harness) =
            crate::rpc::RouterDispatcherTestHarness::new("SY.test@motherbee", vault_test_profile());
        let client = VaultClient::new(dispatcher.clone(), "motherbee".to_string(), caller());

        let a = tokio::spawn({
            let client = client.clone();
            async move { client.get("kv/a", Duration::from_secs(2)).await }
        });
        let b = tokio::spawn({
            let client = client.clone();
            async move { client.get("kv/b", Duration::from_secs(2)).await }
        });

        let out_a = harness
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("first request hits the wire");
        let out_b = harness
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("second request hits the wire");
        assert_ne!(
            out_a.routing.trace_id, out_b.routing.trace_id,
            "concurrent vault calls must get distinct trace_ids"
        );

        // Reply in reverse order — multiplexing must still route each reply
        // to the correct waiter.
        harness
            .inject(ok_get_response(
                &out_b.routing.trace_id,
                "kv/b",
                json!("secret_b"),
            ))
            .await
            .expect("inject b");
        harness
            .inject(ok_get_response(
                &out_a.routing.trace_id,
                "kv/a",
                json!("secret_a"),
            ))
            .await
            .expect("inject a");

        let response_a = a.await.expect("task a").expect("get a");
        let response_b = b.await.expect("task b").expect("get b");
        assert_eq!(response_a.value, Some(json!("secret_a")));
        assert_eq!(response_b.value, Some(json!("secret_b")));
    }

    #[tokio::test]
    async fn vault_client_wrong_msg_for_same_trace_id_is_classified_invalid_response() {
        let (dispatcher, mut harness) =
            crate::rpc::RouterDispatcherTestHarness::new("SY.test@motherbee", vault_test_profile());
        let client = VaultClient::new(dispatcher.clone(), "motherbee".to_string(), caller());

        let call = tokio::spawn({
            let client = client.clone();
            async move { client.get("kv/x", Duration::from_millis(500)).await }
        });

        let outgoing = harness
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("request hits the wire");
        let trace_id = outgoing.routing.trace_id.clone();

        // Inject a different SYSTEM message reusing the same trace_id. The
        // vault matcher classifies it as `invalid_response` (it shares
        // SYSTEM_KIND but is not MSG_VAULT_GET_RESPONSE / UNREACHABLE /
        // TTL_EXCEEDED), which surfaces as VaultError::Service through our
        // RpcError → VaultError bridge. The key property is that the call
        // does NOT silently treat the noise as a vault payload.
        let bogus = Message {
            routing: Routing {
                src: "noise-uuid".to_string(),
                src_l2_name: Some("SY.noise@motherbee".to_string()),
                dst: Destination::Unicast("SY.test@motherbee".to_string()),
                ttl: 16,
                trace_id: trace_id.clone(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some("UNRELATED_SYSTEM_MESSAGE".to_string()),
                ..Meta::default()
            },
            payload: json!({"junk": true}),
        };
        harness.inject(bogus).await.expect("inject noise");

        let result = call.await.expect("task").err();
        match result {
            Some(VaultError::Service { code, .. }) => {
                assert_eq!(code, "VAULT_RPC_ERROR");
            }
            other => panic!("expected VaultError::Service{{VAULT_RPC_ERROR}}, got {other:?}"),
        }
    }

    #[test]
    fn vault_response_serialization_parses_standard_error_shape() {
        let raw = r#"{
            "status": "error",
            "count": 0,
            "secrets": [],
            "error_code": "UNAUTHORIZED",
            "message": "denied"
        }"#;
        let response: VaultListResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(response.status, "error");
        assert_eq!(response.error_code.as_deref(), Some("UNAUTHORIZED"));
        assert_eq!(response.message.as_deref(), Some("denied"));
        assert!(response.secrets.is_empty());
    }

    /// The two failures SY.storage ACTUALLY hit on the live motherbee, verbatim from its
    /// journal, on a `.deb` upgrade that restarted vault and storage at the same instant:
    ///
    ///   vault resource lookup for postgres failed: vault unreachable:
    ///     reason=NODE_NOT_FOUND original_dst=SY.vault@motherbee
    ///   vault resource lookup for postgres failed: node error: disconnected
    ///
    /// Both mean "the vault had not announced yet", and both must be retryable. Six occurrences
    /// in one journal — losing this race is the NORMAL outcome of a simultaneous restart, not a
    /// rare one, and each left the hive's admin plane down behind STORAGE_NOT_READY until a
    /// human intervened.
    #[test]
    fn the_boot_race_failures_observed_in_production_are_retryable() {
        let node_not_found = VaultError::Unreachable {
            reason: "NODE_NOT_FOUND".to_string(),
            original_dst: "SY.vault@motherbee".to_string(),
        };
        assert!(
            vault_failure_is_transport(&node_not_found),
            "NODE_NOT_FOUND is the router saying the vault has not announced yet, not a verdict"
        );
        assert!(
            node_not_found.to_string().contains("NODE_NOT_FOUND"),
            "the journal line this test is pinned to must stay recognizable"
        );

        let disconnected = VaultError::Node(NodeError::Disconnected);
        assert!(
            vault_failure_is_transport(&disconnected),
            "a dropped connection carries no verdict either"
        );
    }

    /// The other half: a vault that ANSWERED must not be retried. Spinning on a settled verdict
    /// would only delay an honest degraded start, and would turn "there is no secret" — a real
    /// operator condition — into a node that hangs for the whole boot budget on every start.
    #[test]
    fn a_vault_that_answered_is_never_retried() {
        for settled in [
            VaultError::Service {
                code: "FORBIDDEN".to_string(),
                message: "denied".to_string(),
            },
            VaultError::InvalidVaultRef,
            VaultError::EmptyValue {
                key: "postgres".to_string(),
            },
        ] {
            assert!(
                !vault_failure_is_transport(&settled),
                "{settled} is a verdict; retrying it would just stall the boot"
            );
        }
    }

    /// Waiting must be bounded. A vault that never comes up has to end in a degraded start, not
    /// a process that hangs forever holding the hive's admin plane hostage.
    #[test]
    fn the_boot_wait_is_bounded_and_polls_more_than_once() {
        assert!(
            VAULT_BOOT_WAIT >= Duration::from_secs(30),
            "too short a budget re-creates the bug on a slow boot"
        );
        assert!(
            VAULT_BOOT_WAIT <= Duration::from_secs(300),
            "the wait must stay bounded so a vaultless hive still finishes booting"
        );
        assert!(
            VAULT_BOOT_RETRY_INTERVAL > Duration::ZERO
                && VAULT_BOOT_RETRY_INTERVAL * 10 <= VAULT_BOOT_WAIT,
            "the budget must allow many attempts, not one or two"
        );
    }
}
