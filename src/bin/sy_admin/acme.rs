//! SY.admin's ACME client: the public edge's certificate, issued and renewed by an ACME CA with
//! TLS-ALPN-01 on the :443 the edge already opens (docs/acme-edge-certificates-v1.md §3, §6, §7).
//!
//! - [`AcmeConfig`]: the motherbee's `acme:` section and the certified name (§3). A section that
//!   cannot work turns ACME off with a reason; nothing here stops SY.admin from starting.
//! - [`AcmeCa`]: the CA, behind a small trait so the flow runs against a fake in the tests.
//!   [`InstantAcmeCa`] is the real one (instant-acme 0.8.5, plus a plain GET of the directory).
//! - [`AcmeMesh`]: the calls to the edge, SY.vault and SY.orchestrator. [`RouterMesh`] is the real
//!   one.
//! - [`AcmeService`]: the order flow (§6), the check and its decision table, the conditions and
//!   their alerts (§7), and the four operator actions.
//!
//! The certificate's private key exists in this process's memory and in the vault only: it is
//! never logged and never written to disk. `acme.json` holds no key.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use chrono::SecondsFormat;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use fluxbee_sdk::protocol::{
    MSG_EDGE_ACME_CHALLENGE, MSG_EDGE_ACME_CHALLENGE_RESPONSE, MSG_EDGE_TLS_RELOAD,
    MSG_EDGE_TLS_RELOAD_RESPONSE, MSG_EDGE_TLS_STATUS, MSG_EDGE_TLS_STATUS_RESPONSE,
};
use fluxbee_sdk::{
    RouterDispatcher, DEFAULT_ROOT_TENANT_ID, MSG_VAULT_GET, MSG_VAULT_GET_METADATA,
    MSG_VAULT_GET_METADATA_RESPONSE, MSG_VAULT_GET_RESPONSE, MSG_VAULT_PUT, MSG_VAULT_PUT_RESPONSE,
    MSG_VAULT_ROLLBACK, MSG_VAULT_ROLLBACK_RESPONSE,
};
use json_router::tls_material::{self, LeafInfo};

pub(crate) const LETS_ENCRYPT_DIRECTORY: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub(crate) const LETS_ENCRYPT_STAGING_DIRECTORY: &str =
    "https://acme-staging-v02.api.letsencrypt.org/directory";
/// The vault keys of SY.admin's ACME accounts are `acme_account:<16 hex>` (§3).
pub(crate) const ACCOUNT_KEY_PREFIX: &str = "acme_account:";
/// The namespace neither the admin API nor Cloud writes (§3, §8); the same prefix as
/// `fluxbee_sdk::vault::CLOUD_RESERVED_VAULT_KEY_PREFIXES` holds for Cloud.
const ACCOUNT_KEY_NAMESPACE: &str = "acme_account";
/// The only edge secrets SY.admin writes (§3).
const EDGE_TLS_KEY_PREFIX: &str = "edge_tls";
const TLS_RESOURCE_TYPE: &str = "tls";
const ACCOUNT_RESOURCE_TYPE: &str = "acme_account";
/// Six-day certificates do not fit the 24 h backoff cap (§3).
const SHORTLIVED_PROFILE: &str = "shortlived";
const ACME_SETTINGS: &[&str] = &[
    "terms_agreed",
    "directory_url",
    "staging_directory_url",
    "profile",
    "contact",
    "trust_anchor",
];
const STATE_SCHEMA_VERSION: u32 = 1;
/// `acme.json` keeps the last issued certificates (§3).
const ISSUED_KEPT: usize = 20;
const CHALLENGE_TTL_SECS: u64 = 600;
/// An issued certificate is stored only with more than this left (§6 step 5).
const MIN_REMAINING_SECS: i64 = 2 * 86_400;
const EDGE_UNREACHABLE_AFTER_SECS: i64 = 3_600;
const RENEWAL_OVERDUE_AFTER_SECS: i64 = 48 * 3_600;
const BACKOFF_FIRST_SECS: i64 = 3_600;
const BACKOFF_MAX_SECS: i64 = 86_400;
const RATE_LIMIT_DEFAULT_SECS: i64 = 86_400;
const ARI_REFETCH_MIN_SECS: i64 = 3_600;
const ARI_REFETCH_MAX_SECS: i64 = 86_400;
const CONDITION_CATEGORY: &str = "certificate";

/// The durations of §6 and §7. The tests shorten them; production uses [`Timings::default`].
#[derive(Debug, Clone)]
pub(crate) struct Timings {
    /// The first check after SY.admin starts.
    pub(crate) first_check: Duration,
    /// Between two checks, plus a random delay of up to `check_jitter`.
    pub(crate) check_interval: Duration,
    pub(crate) check_jitter: Duration,
    /// The retry while the edge does not answer, or an issued certificate is not yet served.
    pub(crate) edge_retry: Duration,
    /// One CA call: the library sets no timeout of its own.
    pub(crate) ca_call: Duration,
    /// The order polling, before finalize and after it.
    pub(crate) order_poll: Duration,
    /// One EDGE_TLS_STATUS or EDGE_ACME_CHALLENGE.
    pub(crate) edge_call: Duration,
    /// One EDGE_TLS_RELOAD.
    pub(crate) reload: Duration,
    /// One SY.vault or SY.orchestrator call.
    pub(crate) mesh_call: Duration,
    /// How long an issued certificate waits in memory for the vault, and how often it retries.
    pub(crate) vault_retry_window: Duration,
    pub(crate) vault_retry_every: Duration,
    /// How long the directory's terms URL and profiles are kept before they are read again.
    pub(crate) directory_refresh: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            first_check: Duration::from_secs(120),
            check_interval: Duration::from_secs(6 * 3_600),
            check_jitter: Duration::from_secs(30 * 60),
            edge_retry: Duration::from_secs(600),
            ca_call: Duration::from_secs(30),
            order_poll: Duration::from_secs(300),
            edge_call: Duration::from_secs(20),
            reload: Duration::from_secs(75),
            mesh_call: Duration::from_secs(15),
            vault_retry_window: Duration::from_secs(600),
            vault_retry_every: Duration::from_secs(30),
            directory_refresh: Duration::from_secs(86_400),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Configuration (§3)
// ---------------------------------------------------------------------------------------------

/// A working `acme:` section.
#[derive(Clone)]
pub(crate) struct AcmeSettings {
    /// The host of `admin.public_base_url`, lowercase, without a trailing dot.
    pub(crate) name: String,
    pub(crate) directory_url: String,
    pub(crate) staging_directory_url: String,
    pub(crate) profile: Option<String>,
    pub(crate) contact: Option<String>,
    pub(crate) trust_anchor: Option<PathBuf>,
    /// The roots read from `trust_anchor` at boot (empty: the platform roots).
    pub(crate) trust_anchor_roots: Vec<CertificateDer<'static>>,
}

/// The `acme:` section as SY.admin read it at boot.
#[derive(Clone)]
pub(crate) enum AcmeConfig {
    /// No `acme:` section: nothing is ordered; the expiry checks and their alerts still run.
    Absent {
        name: Option<String>,
    },
    /// A section that cannot work: nothing is ordered, and the reason is in the log and the status.
    Off {
        reason: String,
        name: Option<String>,
    },
    On(AcmeSettings),
}

impl AcmeConfig {
    /// Read the `acme:` section (`None` when the hive.yaml has none) against the effective
    /// `admin.public_base_url`. Never fails: a section that cannot work is [`AcmeConfig::Off`].
    pub(crate) fn from_hive(
        section: Option<&serde_yaml::Value>,
        public_base_url: Option<&str>,
    ) -> Self {
        let name = certified_name(public_base_url);
        let Some(section) = section else {
            return AcmeConfig::Absent { name: name.ok() };
        };
        match parse_section(section, &name) {
            Ok(settings) => AcmeConfig::On(settings),
            Err(reason) => AcmeConfig::Off {
                reason,
                name: name.ok(),
            },
        }
    }

    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            AcmeConfig::Absent { name } | AcmeConfig::Off { name, .. } => name.as_deref(),
            AcmeConfig::On(settings) => Some(settings.name.as_str()),
        }
    }

    /// The roots `acme.trust_anchor` holds (none unless ACME is on with one).
    pub(crate) fn trust_anchor_roots(&self) -> Vec<CertificateDer<'static>> {
        match self {
            AcmeConfig::On(settings) => settings.trust_anchor_roots.clone(),
            _ => Vec::new(),
        }
    }

    /// One line for the boot log.
    pub(crate) fn describe(&self) -> String {
        match self {
            AcmeConfig::Absent { .. } => {
                "no acme section: nothing is ordered; the certificate expiry checks run".to_string()
            }
            AcmeConfig::Off { reason, .. } => format!("ACME is off: {reason}"),
            AcmeConfig::On(settings) => format!(
                "ACME is on for {} (directory {}, profile {})",
                settings.name,
                settings.directory_url,
                settings.profile.as_deref().unwrap_or("the CA's default")
            ),
        }
    }
}

/// The certified name: the host of `admin.public_base_url`, lowercase, with no trailing dot (§3).
/// The URL must be https on port 443, and its host a DNS name with a dot: not an IP literal and
/// not a wildcard.
pub(crate) fn certified_name(public_base_url: Option<&str>) -> Result<String, String> {
    let url = public_base_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "admin.public_base_url is missing".to_string())?;
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("https://") else {
        return Err(format!(
            "admin.public_base_url '{url}' is not an https URL: the certificate is validated on 443"
        ));
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim();
    if authority.contains('@') {
        return Err(format!(
            "admin.public_base_url '{url}' carries user information"
        ));
    }
    if authority.starts_with('[') {
        return Err(format!(
            "admin.public_base_url '{url}' has an IP literal for a host"
        ));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if let Some(port) = port {
        if port != "443" {
            return Err(format!(
                "admin.public_base_url '{url}' uses port {port}: TLS-ALPN-01 validates on 443 only"
            ));
        }
    }
    let host = tls_material::normalize_name(host);
    if host.is_empty() {
        return Err(format!("admin.public_base_url '{url}' has no host"));
    }
    if host.contains('*') {
        return Err(format!(
            "admin.public_base_url '{url}' has a wildcard host: ACME certifies one name"
        ));
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Err(format!(
            "admin.public_base_url '{url}' has an IP literal for a host"
        ));
    }
    if !host.contains('.') {
        return Err(format!(
            "admin.public_base_url '{url}' has a host with no dot"
        ));
    }
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    };
    if host.len() > 253 || !host.split('.').all(valid_label) {
        return Err(format!(
            "admin.public_base_url '{url}' has a host that is not a DNS name"
        ));
    }
    Ok(host)
}

fn parse_section(
    section: &serde_yaml::Value,
    name: &Result<String, String>,
) -> Result<AcmeSettings, String> {
    let map = match section {
        serde_yaml::Value::Mapping(map) => map.clone(),
        serde_yaml::Value::Null => serde_yaml::Mapping::new(),
        _ => return Err("the acme section is not a mapping".to_string()),
    };
    for key in map.keys() {
        let key = key.as_str().unwrap_or("?");
        if !ACME_SETTINGS.contains(&key) {
            return Err(format!(
                "acme.{key} is not a setting (known: {})",
                ACME_SETTINGS.join(", ")
            ));
        }
    }
    let field = |key: &str| map.get(serde_yaml::Value::String(key.to_string()));
    if field("terms_agreed").and_then(serde_yaml::Value::as_bool) != Some(true) {
        return Err(
            "acme.terms_agreed is not true: the operator must accept the CA's terms".to_string(),
        );
    }
    let text = |key: &str| -> Result<Option<String>, String> {
        match field(key) {
            None | Some(serde_yaml::Value::Null) => Ok(None),
            Some(serde_yaml::Value::String(value)) => {
                let value = value.trim();
                Ok((!value.is_empty()).then(|| value.to_string()))
            }
            Some(_) => Err(format!("acme.{key} is not a string")),
        }
    };
    let name = name.clone()?;
    let directory_url =
        text("directory_url")?.unwrap_or_else(|| LETS_ENCRYPT_DIRECTORY.to_string());
    let staging_directory_url = text("staging_directory_url")?
        .unwrap_or_else(|| LETS_ENCRYPT_STAGING_DIRECTORY.to_string());
    for (key, url) in [
        ("directory_url", &directory_url),
        ("staging_directory_url", &staging_directory_url),
    ] {
        let rest = url.to_ascii_lowercase();
        let host = rest.strip_prefix("https://").unwrap_or_default();
        if host.is_empty() || url.chars().any(char::is_whitespace) {
            return Err(format!("acme.{key} '{url}' is not an https URL"));
        }
    }
    let profile = text("profile")?;
    if profile
        .as_deref()
        .is_some_and(|profile| profile.eq_ignore_ascii_case(SHORTLIVED_PROFILE))
    {
        return Err(
            "acme.profile shortlived is refused: its six-day certificates do not fit the 24 h backoff cap"
                .to_string(),
        );
    }
    let contact = text("contact")?;
    let trust_anchor = text("trust_anchor")?.map(PathBuf::from);
    let trust_anchor_roots = match &trust_anchor {
        Some(path) => load_trust_anchor(path)?,
        None => Vec::new(),
    };
    Ok(AcmeSettings {
        name,
        directory_url,
        staging_directory_url,
        profile,
        contact,
        trust_anchor,
        trust_anchor_roots,
    })
}

fn load_trust_anchor(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let pem = fs::read(path).map_err(|err| {
        format!(
            "acme.trust_anchor '{}' cannot be read: {err}",
            path.display()
        )
    })?;
    let mut reader = pem.as_slice();
    let roots: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<_, _>>()
        .map_err(|err| format!("acme.trust_anchor '{}': {err}", path.display()))?;
    if roots.is_empty() {
        return Err(format!(
            "acme.trust_anchor '{}' holds no certificate",
            path.display()
        ));
    }
    Ok(roots)
}

/// Whether `key` is in the namespace of SY.admin's ACME accounts (`acme_account*`), which the
/// admin API never writes (§3, §8).
pub(crate) fn is_acme_account_key(key: &str) -> bool {
    key.trim()
        .to_ascii_lowercase()
        .starts_with(ACCOUNT_KEY_NAMESPACE)
}

/// The vault key of the account for one directory: `acme_account:` and the first 16 hex of the
/// SHA-256 of the directory URL (§3).
pub(crate) fn account_vault_key(directory_url: &str) -> String {
    let digest = hex::encode(Sha256::digest(directory_url.as_bytes()));
    format!("{ACCOUNT_KEY_PREFIX}{}", &digest[..16])
}

/// The edge's secret must be one of the `edge_tls` names (§3): a compromised edge cannot steer
/// SY.admin's write onto another secret.
pub(crate) fn check_edge_secret_name(key: &str) -> Result<(), String> {
    if key.starts_with(EDGE_TLS_KEY_PREFIX) && fluxbee_sdk::vault::vault_key_is_valid(key) {
        Ok(())
    } else {
        Err(format!(
            "the edge names its secret '{key}': SY.admin writes only a secret whose name starts with {EDGE_TLS_KEY_PREFIX}"
        ))
    }
}

/// When the edge's secret exists, its metadata must say `resource_type: tls`, the root tenant and
/// this edge as its owner (§6 step 1).
pub(crate) fn check_edge_secret_metadata(
    metadata: &SecretMetadata,
    edge_node: &str,
) -> Result<(), String> {
    if metadata.resource_type.as_deref() != Some(TLS_RESOURCE_TYPE) {
        return Err(format!(
            "the edge's secret has resource_type {:?}, not {TLS_RESOURCE_TYPE}",
            metadata.resource_type.as_deref().unwrap_or("")
        ));
    }
    if metadata.tenant_id.as_deref() != Some(DEFAULT_ROOT_TENANT_ID) {
        return Err(format!(
            "the edge's secret belongs to tenant {:?}, not the root tenant",
            metadata.tenant_id.as_deref().unwrap_or("")
        ));
    }
    let edge_ilk = fluxbee_sdk::deterministic_system_ilk_id(edge_node);
    if metadata.owner_ilk.as_deref() != Some(edge_ilk.as_str()) {
        return Err(format!(
            "the edge's secret is not owned by {edge_node} ({edge_ilk})"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// What the edge says (§4)
// ---------------------------------------------------------------------------------------------

/// An EDGE_TLS_STATUS answer, or the STATUS fields of an EDGE_TLS_RELOAD answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct EdgeView {
    /// `vault`, `disk` or `plaintext`.
    pub(crate) mode: Option<String>,
    pub(crate) tls_vault_key: Option<String>,
    /// `serving` or `awaiting`.
    pub(crate) state: Option<String>,
    /// Why an awaiting edge serves nothing, typed: `not_found`, `forbidden`, `invalid_material`
    /// or `transient`.
    pub(crate) reason: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) leaf: Option<LeafView>,
    pub(crate) loaded_at: Option<Value>,
    /// A reload's: whether it swapped the certificate (the whole chain).
    pub(crate) changed: Option<bool>,
    /// A reload's: why it kept the current certificate.
    pub(crate) refused: Option<EdgeRefusal>,
    /// The vault write the edge serves (null when it serves nothing from the vault).
    pub(crate) vault_version: Option<i64>,
    pub(crate) vault_updated_at: Option<String>,
}

/// Which write of a secret is current: SY.vault's version and the metadata's `updated_at`. A
/// vault rollback swaps the current and previous versions and reuses their numbers, so a number
/// alone names no write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WriteIdentity {
    pub(crate) version: i64,
    pub(crate) updated_at: Option<String>,
}

impl std::fmt::Display for WriteIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "version {} written {}",
            self.version,
            self.updated_at.as_deref().unwrap_or("at an unknown time")
        )
    }
}

/// A write identity for the log, or "nothing".
fn identity_text(identity: Option<&WriteIdentity>) -> String {
    identity.map_or_else(|| "nothing".to_string(), ToString::to_string)
}

/// A refused reload: the typed verdict and what the edge said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EdgeRefusal {
    pub(crate) reason: String,
    pub(crate) detail: String,
}

impl EdgeRefusal {
    /// `transient`: the vault gave the edge no verdict. That is no answer, never a refusal of
    /// the material: nothing is rolled back for it, and the reload is retried.
    fn transient(&self) -> bool {
        self.reason == "transient"
    }
}

impl std::fmt::Display for EdgeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.reason, self.detail)
    }
}

/// The leaf the edge serves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LeafView {
    /// SHA-256 of the leaf's DER, lowercase hex.
    pub(crate) fingerprint: String,
    pub(crate) not_before: i64,
    pub(crate) not_after: i64,
    pub(crate) names: Vec<String>,
    pub(crate) issuer: Option<String>,
    pub(crate) chain_len: Option<usize>,
}

impl LeafView {
    fn lifetime(&self) -> i64 {
        (self.not_after - self.not_before).max(0)
    }

    /// Two thirds of the lifetime: the renewal point without ARI (D3), and a hand-seeded leaf's.
    fn two_thirds(&self) -> i64 {
        self.not_before + self.lifetime() * 2 / 3
    }
}

impl EdgeView {
    /// Read an edge answer. The leaf comes from `leaf_pem` when the edge sends it, else from the
    /// reported fields (`fingerprint`, `not_before` and `not_after` as Unix seconds, `names`).
    pub(crate) fn from_answer(answer: &Value) -> Result<Self, String> {
        let text = |key: &str| {
            answer
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let Some(mode) = text("mode") else {
            return Err(answer_detail(answer));
        };
        let refused = match answer.get("refused") {
            None | Some(Value::Null) => None,
            Some(refusal) => Some(EdgeRefusal {
                reason: refusal
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                detail: refusal
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| refusal.to_string()),
            }),
        };
        Ok(Self {
            mode: Some(mode),
            tls_vault_key: text("tls_vault_key"),
            state: text("state"),
            reason: text("reason"),
            detail: text("detail"),
            leaf: leaf_from_answer(answer),
            loaded_at: answer.get("loaded_at").filter(|v| !v.is_null()).cloned(),
            changed: answer.get("changed").and_then(Value::as_bool),
            refused,
            vault_version: answer.get("vault_version").and_then(Value::as_i64),
            vault_updated_at: text("vault_updated_at"),
        })
    }

    /// The vault write the edge serves: none when it serves nothing from the vault.
    fn served_write(&self) -> Option<WriteIdentity> {
        self.vault_version.map(|version| WriteIdentity {
            version,
            updated_at: self.vault_updated_at.clone(),
        })
    }

    fn is_vault(&self) -> bool {
        self.mode.as_deref() == Some("vault")
    }

    fn serving(&self) -> bool {
        self.state.as_deref() == Some("serving")
    }

    fn awaiting(&self) -> bool {
        self.state.as_deref() == Some("awaiting")
    }

    fn fingerprint(&self) -> Option<&str> {
        self.leaf.as_ref().map(|leaf| leaf.fingerprint.as_str())
    }
}

fn leaf_from_answer(answer: &Value) -> Option<LeafView> {
    let chain_len = answer
        .get("chain_len")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    if let Some(pem) = answer.get("leaf_pem").and_then(Value::as_str) {
        if let Ok((chain, _)) = tls_material::parse_chain(pem.as_bytes()) {
            if let Some(first) = chain.first() {
                if let Ok(info) = tls_material::leaf_info(first.as_ref(), chain.len()) {
                    return Some(LeafView {
                        fingerprint: info.fingerprint,
                        not_before: info.not_before,
                        not_after: info.not_after,
                        names: info.names,
                        issuer: Some(info.issuer),
                        chain_len: chain_len.or(Some(info.chain_len)),
                    });
                }
            }
        }
    }
    let fingerprint = answer
        .get("fingerprint")
        .and_then(Value::as_str)?
        .trim()
        .to_ascii_lowercase();
    if fingerprint.is_empty() {
        return None;
    }
    let not_before = answer.get("not_before").and_then(Value::as_i64)?;
    let not_after = answer.get("not_after").and_then(Value::as_i64)?;
    let names = answer
        .get("names")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(tls_material::normalize_name)
                .collect()
        })
        .unwrap_or_default();
    Some(LeafView {
        fingerprint,
        not_before,
        not_after,
        names,
        issuer: answer
            .get("issuer")
            .and_then(Value::as_str)
            .map(str::to_string),
        chain_len,
    })
}

fn answer_ok(answer: &Value) -> bool {
    answer
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("ok"))
}

/// The reason an answer gives, for the log and the status.
fn answer_detail(answer: &Value) -> String {
    for key in ["error_detail", "message", "error", "detail", "error_code"] {
        if let Some(text) = answer.get(key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return text.to_string();
            }
        }
    }
    let mut text = answer.to_string();
    text.truncate(300);
    text
}

// ---------------------------------------------------------------------------------------------
// acme.json (§3): no key, ever
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AcmeState {
    pub(crate) schema_version: u32,
    /// The account URL per directory URL.
    #[serde(default)]
    pub(crate) accounts: BTreeMap<String, AccountRecord>,
    /// The production order in flight, resumed after a restart.
    #[serde(default)]
    pub(crate) order: Option<OrderRecord>,
    /// A certificate stored in the vault that the edge does not serve yet.
    #[serde(default)]
    pub(crate) pending_install: Option<PendingInstall>,
    /// The edge refused a certificate SY.admin issued: nothing is ordered until the operator acts.
    #[serde(default)]
    pub(crate) install_refused: Option<InstallRefused>,
    /// The rollback after that refusal did not go through yet; each check retries it.
    #[serde(default)]
    pub(crate) rollback_pending: Option<RollbackPending>,
    #[serde(default)]
    pub(crate) last_attempt: Option<AttemptRecord>,
    #[serde(default)]
    pub(crate) last_test: Option<AttemptRecord>,
    #[serde(default)]
    pub(crate) last_check: Option<CheckRecord>,
    /// The last issued certificates, newest last.
    #[serde(default)]
    pub(crate) issued: Vec<IssuedRecord>,
    #[serde(default)]
    pub(crate) backoff: Backoff,
    /// When the served leaf (if this account issued it) is renewed.
    #[serde(default)]
    pub(crate) renewal: Option<RenewalPlan>,
    #[serde(default)]
    pub(crate) edge: EdgeSeen,
    /// The conditions open now, by kind.
    #[serde(default)]
    pub(crate) conditions: BTreeMap<String, ConditionRecord>,
    /// Closed conditions whose `info` row is not written yet.
    #[serde(default)]
    pub(crate) unsent_closures: Vec<ClosedCondition>,
    #[serde(default)]
    pub(crate) next_check_at: Option<i64>,
}

impl Default for AcmeState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            accounts: BTreeMap::new(),
            order: None,
            pending_install: None,
            install_refused: None,
            rollback_pending: None,
            last_attempt: None,
            last_test: None,
            last_check: None,
            issued: Vec::new(),
            backoff: Backoff::default(),
            renewal: None,
            edge: EdgeSeen::default(),
            conditions: BTreeMap::new(),
            unsent_closures: Vec::new(),
            next_check_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AccountRecord {
    pub(crate) account_url: String,
    pub(crate) vault_key: String,
    pub(crate) recorded_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct OrderRecord {
    pub(crate) directory_url: String,
    pub(crate) account_url: String,
    pub(crate) order_url: String,
    pub(crate) name: String,
    pub(crate) profile: Option<String>,
    pub(crate) replaces: bool,
    pub(crate) kind: RunKind,
    pub(crate) started_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct PendingInstall {
    pub(crate) fingerprint: String,
    pub(crate) serial: String,
    pub(crate) vault_key: String,
    pub(crate) edge_node: String,
    /// The write SY.admin's put made (read back after it): another one current means someone
    /// else wrote, or rolled back, since.
    pub(crate) written: Option<WriteIdentity>,
    pub(crate) stored_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct InstallRefused {
    pub(crate) fingerprint: String,
    pub(crate) serial: String,
    pub(crate) detail: String,
    pub(crate) rolled_back: bool,
    pub(crate) refused_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct RollbackPending {
    pub(crate) vault_key: String,
    pub(crate) fingerprint: String,
    /// The refused write: the rollback applies only while it is still current.
    pub(crate) written: WriteIdentity,
    pub(crate) since_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AttemptRecord {
    pub(crate) kind: RunKind,
    pub(crate) directory_url: String,
    pub(crate) started_at: i64,
    pub(crate) finished_at: i64,
    /// `installed`, `not_served`, `refused`, `tested` or `failed`.
    pub(crate) result: String,
    pub(crate) stage: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) fingerprint: Option<String>,
    /// A test order's: whether its chain verified to a trusted root.
    pub(crate) trusted: Option<bool>,
}

impl AttemptRecord {
    fn failed(&self) -> bool {
        matches!(self.result.as_str(), "failed" | "refused")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct CheckRecord {
    pub(crate) at: i64,
    pub(crate) decision: String,
}

/// One certificate the CA issued to SY.admin. Public facts only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct IssuedRecord {
    /// The serial's raw bytes, hex.
    pub(crate) serial: String,
    pub(crate) fingerprint: String,
    /// The authority key identifier, hex (with the serial, the ARI identifier).
    pub(crate) authority_key_id: Option<String>,
    pub(crate) not_before: i64,
    pub(crate) not_after: i64,
    pub(crate) name: String,
    pub(crate) profile: Option<String>,
    pub(crate) directory_url: String,
    pub(crate) account_url: String,
    pub(crate) issued_at: i64,
    pub(crate) stored: bool,
    pub(crate) installed: bool,
    pub(crate) installed_at: Option<i64>,
}

impl IssuedRecord {
    fn lifetime(&self) -> i64 {
        (self.not_after - self.not_before).max(0)
    }

    fn cert_ref(&self) -> Option<CertRef> {
        Some(CertRef {
            authority_key_id: hex::decode(self.authority_key_id.as_deref()?).ok()?,
            serial: hex::decode(&self.serial).ok()?,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct Backoff {
    pub(crate) failures: u32,
    pub(crate) next_attempt_at: Option<i64>,
    pub(crate) rate_limited_until: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct RenewalPlan {
    pub(crate) fingerprint: String,
    /// `ari` (a random point of the CA's suggested window) or `lifetime` (two thirds).
    pub(crate) source: String,
    pub(crate) renew_at: i64,
    pub(crate) window_start_at: Option<i64>,
    pub(crate) window_end_at: Option<i64>,
    pub(crate) explanation_url: Option<String>,
    /// When to ask for the window again (the CA's Retry-After).
    pub(crate) next_ari_fetch_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct EdgeSeen {
    pub(crate) node: Option<String>,
    pub(crate) not_configured: Option<String>,
    pub(crate) last_status: Option<EdgeView>,
    pub(crate) last_answer_at: Option<i64>,
    /// The first unanswered look since the last answer.
    pub(crate) unreachable_since_at: Option<i64>,
    pub(crate) last_error: Option<String>,
    /// The edge answered EDGE_TLS_STATUS with an error: it cannot report its certificate (no
    /// public listener, another origin). A configuration error, not an unreachable edge.
    pub(crate) config_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ConditionRecord {
    pub(crate) severity: Severity,
    /// The hive the condition is about (the edge's).
    pub(crate) hive: String,
    /// What it said when it opened.
    pub(crate) detail: String,
    /// What it says now.
    pub(crate) current: String,
    pub(crate) opened_at: i64,
    /// Whether its `open` row reached the orchestrator.
    pub(crate) alerted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ClosedCondition {
    pub(crate) kind: String,
    pub(crate) hive: String,
    pub(crate) detail: String,
    pub(crate) closed_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Severity {
    Warning,
    Critical,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunKind {
    Check,
    Renew,
    Test,
    Reload,
}

impl RunKind {
    fn as_str(self) -> &'static str {
        match self {
            RunKind::Check => "check",
            RunKind::Renew => "renew",
            RunKind::Test => "test",
            RunKind::Reload => "reload",
        }
    }
}

/// Read `acme.json`. A missing file is a fresh state; an unreadable one is moved aside (and the
/// returned warning says where) so SY.admin keeps running.
fn load_state(path: &Path, now: i64) -> (AcmeState, Option<String>) {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return (AcmeState::default(), None)
        }
        Err(err) => {
            return (
                AcmeState::default(),
                Some(format!("{} cannot be read: {err}", path.display())),
            )
        }
    };
    match serde_json::from_slice::<AcmeState>(&raw) {
        Ok(state) if state.schema_version == STATE_SCHEMA_VERSION => (state, None),
        Ok(state) => {
            let aside = set_aside(path, now);
            (
                AcmeState::default(),
                Some(format!(
                    "{} has schema {} (expected {STATE_SCHEMA_VERSION}); moved to {aside}",
                    path.display(),
                    state.schema_version
                )),
            )
        }
        Err(err) => {
            let aside = set_aside(path, now);
            (
                AcmeState::default(),
                Some(format!(
                    "{} does not parse ({err}); moved to {aside}",
                    path.display()
                )),
            )
        }
    }
}

fn set_aside(path: &Path, now: i64) -> String {
    let aside = path.with_extension(format!("json.unreadable-{now}"));
    match fs::rename(path, &aside) {
        Ok(()) => aside.display().to_string(),
        Err(err) => format!("nowhere ({err})"),
    }
}

/// Write `acme.json` atomically: a temporary file in the same directory, then a rename.
fn persist_state(path: &Path, state: &AcmeState) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "acme.json has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    let temp = parent.join(format!(
        ".acme-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<(), String> {
        let data = serde_json::to_vec_pretty(state).map_err(|err| err.to_string())?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o640)
            .open(&temp)
            .map_err(|err| format!("create {}: {err}", temp.display()))?;
        file.write_all(&data)
            .map_err(|err| format!("write {}: {err}", temp.display()))?;
        file.sync_all()
            .map_err(|err| format!("sync {}: {err}", temp.display()))?;
        drop(file);
        fs::rename(&temp, path).map_err(|err| format!("replace {}: {err}", path.display()))?;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o640));
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

// ---------------------------------------------------------------------------------------------
// The CA and the mesh, behind traits
// ---------------------------------------------------------------------------------------------

/// What a CA call failed with.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CaError {
    /// `urn:ietf:params:acme:error:rateLimited`; the detail carries the CA's retry time.
    RateLimited(String),
    /// The CA refused the order's `replaces` (or does not do ARI).
    Replaces(String),
    /// Any other problem the CA answered.
    Problem(String),
    Timeout(String),
    Transport(String),
}

impl std::fmt::Display for CaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaError::RateLimited(detail) => write!(f, "rate limited by the CA: {detail}"),
            CaError::Replaces(detail) => write!(f, "the CA refused replaces: {detail}"),
            CaError::Problem(detail) => write!(f, "{detail}"),
            CaError::Timeout(detail) => write!(f, "timed out: {detail}"),
            CaError::Transport(detail) => write!(f, "the CA did not answer: {detail}"),
        }
    }
}

/// What the directory says about itself (§6: "The terms URL comes from a GET of the directory").
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct DirectoryInfo {
    pub(crate) terms_of_service: Option<String>,
    pub(crate) profiles: Vec<String>,
    pub(crate) renewal_info: Option<String>,
}

/// An ARI answer (RFC 9773).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AriAnswer {
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) explanation_url: Option<String>,
    pub(crate) retry_after_secs: Option<i64>,
}

/// A certificate as ARI names it: the authority key identifier and the serial, raw bytes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CertRef {
    pub(crate) authority_key_id: Vec<u8>,
    pub(crate) serial: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrderStatus {
    Pending,
    Ready,
    Processing,
    Valid,
    Invalid,
}

/// The order's authorization for the name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AuthzState {
    /// Waiting for the TLS-ALPN-01 challenge: the name and the key authorization's SHA-256, hex.
    Pending {
        name: String,
        digest: String,
    },
    /// Already valid (an authorization of the last 30 days).
    Valid,
    Other(String),
}

#[async_trait]
pub(crate) trait AcmeCa: Send + Sync {
    /// A plain GET of the directory.
    async fn directory(&self, url: &str) -> Result<DirectoryInfo, CaError>;
    /// The suggested renewal window of one certificate.
    async fn renewal_info(
        &self,
        renewal_info_url: &str,
        cert: &CertRef,
    ) -> Result<AriAnswer, CaError>;
    /// A new account on the directory, with the terms agreed. Returns the account and the
    /// library's credentials (which hold the account key: for the vault only).
    async fn create_account(
        &self,
        directory_url: &str,
        contact: Option<&str>,
    ) -> Result<(Box<dyn CaAccount>, Value), CaError>;
    async fn load_account(&self, credentials: &Value) -> Result<Box<dyn CaAccount>, CaError>;
}

#[async_trait]
pub(crate) trait CaAccount: Send + Sync {
    fn url(&self) -> String;
    fn profiles(&self) -> Vec<String>;
    async fn new_order(
        &self,
        name: &str,
        profile: Option<&str>,
        replaces: Option<&CertRef>,
    ) -> Result<Box<dyn CaOrder>, CaError>;
    async fn resume_order(&self, url: &str) -> Result<Box<dyn CaOrder>, CaError>;
}

#[async_trait]
pub(crate) trait CaOrder: Send {
    fn url(&self) -> String;
    fn status(&mut self) -> OrderStatus;
    async fn authorization(&mut self) -> Result<AuthzState, CaError>;
    async fn set_challenge_ready(&mut self) -> Result<(), CaError>;
    async fn poll_ready(&mut self, limit: Duration) -> Result<OrderStatus, CaError>;
    /// The error the CA gave the challenge, after a refresh of the authorization.
    async fn challenge_error(&mut self) -> Option<String>;
    async fn deactivate_pending(&mut self) -> Result<(), CaError>;
    async fn finalize(&mut self, csr_der: &[u8]) -> Result<(), CaError>;
    /// Wait for the certificate and return the chain, PEM.
    async fn certificate(&mut self, limit: Duration) -> Result<String, CaError>;
}

/// What a mesh call failed with.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MeshError {
    /// Nothing came back in time, or the router answered UNREACHABLE (VPN_BLOCKED included).
    NoAnswer(String),
    /// The node answered with an error.
    Refused { code: String, detail: String },
}

impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeshError::NoAnswer(detail) => write!(f, "no answer: {detail}"),
            MeshError::Refused { code, detail } => write!(f, "{code}: {detail}"),
        }
    }
}

/// The parts of a secret's metadata the checks read.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SecretMetadata {
    pub(crate) resource_type: Option<String>,
    pub(crate) tenant_id: Option<String>,
    pub(crate) owner_ilk: Option<String>,
    pub(crate) version: Option<i64>,
    pub(crate) updated_at: Option<String>,
    /// The ILK whose put made the current write.
    pub(crate) created_by: Option<String>,
}

impl SecretMetadata {
    /// The current write, when the vault reported its version.
    fn write(&self) -> Option<WriteIdentity> {
        self.version.map(|version| WriteIdentity {
            version,
            updated_at: self.updated_at.clone(),
        })
    }
}

#[async_trait]
pub(crate) trait AcmeMesh: Send + Sync {
    async fn edge_status(&self, edge: &str, limit: Duration) -> Result<Value, MeshError>;
    async fn edge_reload(&self, edge: &str, limit: Duration) -> Result<Value, MeshError>;
    async fn edge_challenge(
        &self,
        edge: &str,
        request: Value,
        limit: Duration,
    ) -> Result<Value, MeshError>;
    async fn vault_metadata(
        &self,
        key: &str,
        limit: Duration,
    ) -> Result<Option<SecretMetadata>, MeshError>;
    async fn vault_value(&self, key: &str, limit: Duration) -> Result<Option<Value>, MeshError>;
    /// Returns the version the put created.
    async fn vault_put(
        &self,
        key: &str,
        value: Value,
        metadata: Value,
        limit: Duration,
    ) -> Result<Option<i64>, MeshError>;
    async fn vault_rollback(&self, key: &str, limit: Duration) -> Result<(), MeshError>;
    /// Returns whether the orchestrator wrote a row: `false` is a duplicate of its newest row
    /// for that hive, category, kind and state, which counts as delivered.
    async fn drift_alert(&self, alert: Value, limit: Duration) -> Result<bool, MeshError>;
}

// ---------------------------------------------------------------------------------------------
// The decisions (§7), pure
// ---------------------------------------------------------------------------------------------

/// What a check does with what the edge said.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    /// Not SY.admin's to manage: another TLS mode, or a secret outside `edge_tls*`.
    LeaveAlone(String),
    /// Reload the edge, never order: an issued certificate is not yet served, or the edge awaits
    /// a certificate an order cannot fix.
    Reload(String),
    Order {
        replaces: Option<CertRef>,
        reason: String,
    },
    /// An order is due but held until then (`None`: until the operator acts).
    Held { until: Option<i64>, reason: String },
    /// Nothing is due before `renew_at`.
    Idle { renew_at: i64, reason: String },
    /// A hand-seeded leaf or one for another name: alerts only; `POST /acme/renew` replaces it.
    AlertOnly(String),
}

impl Decision {
    fn describe(&self) -> String {
        match self {
            Decision::LeaveAlone(reason) => format!("leave alone: {reason}"),
            Decision::Reload(reason) => format!("reload: {reason}"),
            Decision::Order { reason, .. } => format!("order: {reason}"),
            Decision::Held { until, reason } => match until {
                Some(until) => format!("held until {}: {reason}", iso(*until)),
                None => format!("held: {reason}"),
            },
            Decision::Idle { renew_at, reason } => {
                format!("idle until {}: {reason}", iso(*renew_at))
            }
            Decision::AlertOnly(reason) => format!("alert only: {reason}"),
        }
    }
}

/// The issued record of the leaf with this fingerprint, when this account of this directory
/// issued it.
fn issued_by_account<'a>(
    state: &'a AcmeState,
    directory_url: &str,
    fingerprint: &str,
) -> Option<&'a IssuedRecord> {
    let account = state.accounts.get(directory_url)?;
    state.issued.iter().rev().find(|record| {
        record.fingerprint.eq_ignore_ascii_case(fingerprint)
            && record.directory_url == directory_url
            && record.account_url == account.account_url
    })
}

/// When the served leaf is renewed: the ARI point kept for it, else two thirds of its lifetime.
fn renewal_point(state: &AcmeState, leaf: &LeafView) -> i64 {
    state
        .renewal
        .as_ref()
        .filter(|plan| plan.fingerprint.eq_ignore_ascii_case(&leaf.fingerprint))
        .map(|plan| plan.renew_at)
        .unwrap_or_else(|| leaf.two_thirds())
}

/// The cap (§7): at most one issuance from this directory per third of the last one's lifetime.
/// It counts the certificates stored and served, or stored and waiting to be: a failure after
/// issuance is bounded by the backoff, and never holds an edge with no certificate.
fn cap_until(state: &AcmeState, directory_url: &str, now: i64) -> Option<i64> {
    let counts = |record: &IssuedRecord| {
        record.stored
            && (record.installed
                || state
                    .pending_install
                    .as_ref()
                    .is_some_and(|pending| pending.fingerprint == record.fingerprint))
    };
    let last = state
        .issued
        .iter()
        .filter(|record| record.directory_url == directory_url && counts(record))
        .max_by_key(|record| record.issued_at)?;
    let until = last.issued_at + last.lifetime() / 3;
    (until > now).then_some(until)
}

/// The failure backoff (§7): 1 h, 2 h, 4 h and so on, up to 24 h.
pub(crate) fn backoff_delay(failures: u32) -> i64 {
    let doublings = failures.saturating_sub(1).min(16);
    (BACKOFF_FIRST_SECS << doublings).min(BACKOFF_MAX_SECS)
}

/// The time a `rateLimited` answer names ("retry after 2026-01-02 03:04:05 UTC"), else 24 h.
pub(crate) fn rate_limited_until(detail: &str, now: i64) -> i64 {
    let fallback = now + RATE_LIMIT_DEFAULT_SECS;
    let Ok(pattern) =
        regex::Regex::new(r"(?i)retry after (\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}:\d{2})")
    else {
        return fallback;
    };
    let Some(found) = pattern.captures(detail) else {
        return fallback;
    };
    let text = format!("{} {}", &found[1], &found[2]);
    match chrono::NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M:%S") {
        Ok(at) => {
            let at = at.and_utc().timestamp();
            if at > now && at <= now + 7 * 86_400 {
                at
            } else {
                fallback
            }
        }
        Err(_) => fallback,
    }
}

/// The decision table of §7 for an edge that answered, with `acme:` configured. `renew_at` is the
/// served leaf's renewal point when this account issued it (ARI or two thirds).
pub(crate) fn decide(
    view: &EdgeView,
    settings: &AcmeSettings,
    state: &AcmeState,
    renew_at: Option<i64>,
    now: i64,
) -> Decision {
    if !view.is_vault() {
        return Decision::LeaveAlone(format!(
            "the edge serves TLS in {} mode; ACME manages a vault edge only",
            view.mode.as_deref().unwrap_or("an unknown")
        ));
    }
    let Some(key) = view.tls_vault_key.as_deref() else {
        return Decision::LeaveAlone("the edge did not name its secret".to_string());
    };
    if let Err(reason) = check_edge_secret_name(key) {
        return Decision::LeaveAlone(reason);
    }
    if let Some(pending) = &state.pending_install {
        let served = view.serving() && view.fingerprint() == Some(pending.fingerprint.as_str());
        if !served {
            return Decision::Reload(format!(
                "the certificate {} SY.admin stored is not served yet",
                short(&pending.fingerprint)
            ));
        }
    }
    if view.awaiting() {
        let reason = view.reason.as_deref().unwrap_or("unknown");
        if reason == "not_found" {
            return gate(
                state,
                settings,
                None,
                "the edge has no certificate: there is nothing to replace".to_string(),
                now,
            );
        }
        return Decision::Reload(format!(
            "the edge awaits a usable certificate ({reason}): an order cannot fix a wrong owner or a bad secret"
        ));
    }
    if !view.serving() {
        return Decision::LeaveAlone(format!(
            "the edge reports the state {:?}",
            view.state.as_deref().unwrap_or("")
        ));
    }
    let Some(leaf) = &view.leaf else {
        return Decision::LeaveAlone("the edge serves without reporting its leaf".to_string());
    };
    let covered = tls_material::name_covered(&leaf.names, &settings.name);
    let ours = issued_by_account(state, &settings.directory_url, &leaf.fingerprint);
    match ours {
        Some(record) if covered => {
            let renew_at = renew_at.unwrap_or_else(|| leaf.two_thirds());
            if now >= renew_at {
                gate(
                    state,
                    settings,
                    record.cert_ref(),
                    format!(
                        "the leaf this account issued is due since {}",
                        iso(renew_at)
                    ),
                    now,
                )
            } else {
                Decision::Idle {
                    renew_at,
                    reason: "the leaf this account issued is not due".to_string(),
                }
            }
        }
        _ if !covered => Decision::AlertOnly(format!(
            "the served leaf does not cover {}; POST /acme/renew certifies it",
            settings.name
        )),
        _ => Decision::AlertOnly(
            "the served leaf was seeded by hand; POST /acme/renew replaces it".to_string(),
        ),
    }
}

/// Hold a due order for the operator, a rate limit, the backoff or the cap.
fn gate(
    state: &AcmeState,
    settings: &AcmeSettings,
    replaces: Option<CertRef>,
    reason: String,
    now: i64,
) -> Decision {
    if let Some(refused) = &state.install_refused {
        return Decision::Held {
            until: None,
            reason: format!(
                "the edge refused the certificate {} ({}): nothing is ordered until the operator acts (POST /acme/renew)",
                short(&refused.fingerprint),
                refused.detail
            ),
        };
    }
    if let Some(until) = state
        .backoff
        .rate_limited_until
        .filter(|until| *until > now)
    {
        return Decision::Held {
            until: Some(until),
            reason: "the CA rate-limited the last order".to_string(),
        };
    }
    if let Some(until) = state.backoff.next_attempt_at.filter(|until| *until > now) {
        return Decision::Held {
            until: Some(until),
            reason: format!("backing off after {} failure(s)", state.backoff.failures),
        };
    }
    if let Some(until) = cap_until(state, &settings.directory_url, now) {
        return Decision::Held {
            until: Some(until),
            reason: "one issuance per third of the lifetime (the cap)".to_string(),
        };
    }
    Decision::Order { replaces, reason }
}

/// A condition open now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenCondition {
    pub(crate) severity: Severity,
    pub(crate) detail: String,
}

/// The conditions of §7, from what the check knows (the edge's last status included).
pub(crate) fn evaluate_conditions(
    settings: Option<&AcmeSettings>,
    state: &AcmeState,
    now: i64,
) -> BTreeMap<String, OpenCondition> {
    let mut open = BTreeMap::new();
    let mut put = |kind: &str, severity: Severity, detail: String| {
        open.insert(kind.to_string(), OpenCondition { severity, detail });
    };
    let edge = state.edge.node.as_deref().unwrap_or("the edge");
    if let Some(since) = state.edge.unreachable_since_at {
        if now - since >= EDGE_UNREACHABLE_AFTER_SECS {
            put(
                "edge_unreachable",
                Severity::Warning,
                format!(
                    "{edge} has not answered EDGE_TLS_STATUS since {} ({})",
                    iso(since),
                    state.edge.last_error.as_deref().unwrap_or("no answer")
                ),
            );
        }
    }
    if let Some(view) = &state.edge.last_status {
        if view.is_vault() && view.awaiting() {
            put(
                "edge_without_certificate",
                Severity::Critical,
                format!(
                    "{edge} serves no certificate (awaiting, {}: {})",
                    view.reason.as_deref().unwrap_or("unknown"),
                    view.detail.as_deref().unwrap_or("no detail")
                ),
            );
        }
        if let Some(leaf) = &view.leaf {
            let lifetime = leaf.lifetime();
            if lifetime > 0 && leaf.not_after - now < lifetime / 6 {
                put(
                    "expiring",
                    Severity::Critical,
                    format!(
                        "the certificate {edge} serves expires at {}: less than a sixth of its lifetime is left",
                        iso(leaf.not_after)
                    ),
                );
            }
            if let Some(settings) = settings.filter(|_| view.is_vault() && view.serving()) {
                let covered = tls_material::name_covered(&leaf.names, &settings.name);
                if !covered {
                    put(
                        "name_not_covered",
                        Severity::Warning,
                        format!(
                            "the certificate {edge} serves does not cover {} (it names {})",
                            settings.name,
                            leaf.names.join(", ")
                        ),
                    );
                }
                match issued_by_account(state, &settings.directory_url, &leaf.fingerprint) {
                    Some(_) if covered => {
                        let point = renewal_point(state, leaf);
                        if now >= point + RENEWAL_OVERDUE_AFTER_SECS {
                            put(
                                "renewal_overdue",
                                Severity::Warning,
                                format!(
                                    "the leaf {} was due at {} and {edge} still serves it",
                                    short(&leaf.fingerprint),
                                    iso(point)
                                ),
                            );
                        }
                    }
                    Some(_) => {}
                    None => {
                        if now >= leaf.two_thirds() {
                            put(
                                "foreign_certificate",
                                Severity::Warning,
                                format!(
                                    "the hand-seeded certificate {edge} serves passed its renewal point ({}); POST /acme/renew replaces it",
                                    iso(leaf.two_thirds())
                                ),
                            );
                        }
                    }
                }
            }
        }
    }
    if settings.is_some() {
        if let Some(attempt) = state.last_attempt.as_ref().filter(|a| a.failed()) {
            put(
                "renewal_failed",
                Severity::Warning,
                format!(
                    "the last {} attempt failed at {} ({}): {}",
                    attempt.kind.as_str(),
                    iso(attempt.finished_at),
                    attempt.stage.as_deref().unwrap_or("-"),
                    attempt.detail.as_deref().unwrap_or("")
                ),
            );
        }
        if let Some(refused) = &state.install_refused {
            put(
                "install_refused",
                Severity::Critical,
                format!(
                    "{edge} refused the certificate {} SY.admin issued: {}",
                    short(&refused.fingerprint),
                    refused.detail
                ),
            );
        }
    }
    open
}

/// Fold the conditions open now into the state: a new one waits for its `open` row, a closed one
/// whose `open` row was written waits for its `info` row. A standing one writes nothing.
fn apply_conditions(
    state: &mut AcmeState,
    open: BTreeMap<String, OpenCondition>,
    hive: &str,
    now: i64,
) {
    let closed: Vec<String> = state
        .conditions
        .keys()
        .filter(|kind| !open.contains_key(*kind))
        .cloned()
        .collect();
    for kind in closed {
        if let Some(record) = state.conditions.remove(&kind) {
            if record.alerted {
                state.unsent_closures.push(ClosedCondition {
                    kind,
                    hive: record.hive,
                    detail: record.detail,
                    closed_at: now,
                });
            }
        }
    }
    for (kind, condition) in open {
        match state.conditions.get_mut(&kind) {
            Some(record) => record.current = condition.detail,
            None => {
                state.conditions.insert(
                    kind,
                    ConditionRecord {
                        severity: condition.severity,
                        hive: hive.to_string(),
                        detail: condition.detail.clone(),
                        current: condition.detail,
                        opened_at: now,
                        alerted: false,
                    },
                );
            }
        }
    }
}

fn alert_payload(kind: &str, severity: &str, hive: &str, detail: &str, state: &str) -> Value {
    json!({
        "category": CONDITION_CATEGORY,
        "kind": kind,
        "severity": severity,
        "hive": hive,
        "detail": detail,
        "state": state,
    })
}

fn iso(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|| ts.to_string())
}

fn short(fingerprint: &str) -> &str {
    &fingerprint[..fingerprint.len().min(16)]
}

/// Replace the Unix-second times of the status with RFC 3339 text.
fn humanize_times(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                let is_time = key.ends_with("_at")
                    || key.ends_with("_until")
                    || key == "not_before"
                    || key == "not_after";
                if is_time {
                    if let Some(ts) = entry.as_i64() {
                        *entry = Value::String(iso(ts));
                        continue;
                    }
                }
                humanize_times(entry);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(humanize_times),
        _ => {}
    }
}

/// The certificate's private key, PEM. Its `Debug` never shows it.
struct SecretPem(String);

impl SecretPem {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretPem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretPem(<redacted>)")
    }
}

/// A fresh P-256 key and a CSR with an empty subject and the name as its only SAN (§6 step 4).
fn new_key_and_csr(name: &str) -> Result<(Vec<u8>, SecretPem), String> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .map_err(|err| format!("generate the certificate key: {err}"))?;
    let mut params = rcgen::CertificateParams::new(vec![name.to_string()])
        .map_err(|err| format!("the CSR for {name}: {err}"))?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    let csr = params
        .serialize_request(&key)
        .map_err(|err| format!("the CSR for {name}: {err}"))?;
    Ok((csr.der().to_vec(), SecretPem(key.serialize_pem())))
}

/// Whether `chain` verifies to one of `roots` for `name` at `now` (§6 step 5).
fn verify_chain(
    chain: &[CertificateDer<'static>],
    name: &str,
    now: i64,
    roots: Vec<CertificateDer<'static>>,
) -> Result<(), String> {
    let mut store = rustls::RootCertStore::empty();
    let (added, _) = store.add_parsable_certificates(roots);
    if added == 0 {
        return Err("no usable root certificate".to_string());
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier =
        rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider)
            .build()
            .map_err(|err| err.to_string())?;
    let server_name = ServerName::try_from(name.to_string()).map_err(|err| err.to_string())?;
    let (leaf, intermediates) = chain
        .split_first()
        .ok_or_else(|| "the chain is empty".to_string())?;
    verifier
        .verify_server_cert(
            leaf,
            intermediates,
            &server_name,
            &[],
            UnixTime::since_unix_epoch(Duration::from_secs(now.max(0) as u64)),
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// The platform roots (`ca-certificates` on the motherbee).
pub(crate) fn platform_roots() -> Result<Vec<CertificateDer<'static>>, String> {
    let loaded = rustls_native_certs::load_native_certs();
    if loaded.certs.is_empty() {
        let errors: Vec<String> = loaded.errors.iter().map(ToString::to_string).collect();
        return Err(format!(
            "no platform root certificates (is ca-certificates installed?) {}",
            errors.join("; ")
        ));
    }
    Ok(loaded.certs)
}

// ---------------------------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------------------------

pub(crate) type EdgeResolver = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;
pub(crate) type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
pub(crate) type Random = Arc<dyn Fn() -> f64 + Send + Sync>;
pub(crate) type RootsLoader =
    Arc<dyn Fn() -> Result<Vec<CertificateDer<'static>>, String> + Send + Sync>;

/// What the service talks to and how it tells time.
pub(crate) struct AcmeDeps {
    pub(crate) ca: Arc<dyn AcmeCa>,
    pub(crate) mesh: Arc<dyn AcmeMesh>,
    /// The public edge (`admin.public_edge_node`, else the one connected ingress).
    pub(crate) edge: EdgeResolver,
    pub(crate) clock: Clock,
    pub(crate) random: Random,
    pub(crate) platform_roots: RootsLoader,
    pub(crate) timings: Timings,
}

/// An operator action refused before it started.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActionRefusal {
    pub(crate) http_status: u16,
    pub(crate) error_code: &'static str,
    pub(crate) detail: String,
    pub(crate) payload: Value,
}

impl ActionRefusal {
    fn new(http_status: u16, error_code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            http_status,
            error_code,
            detail: detail.into(),
            payload: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Progress {
    kind: RunKind,
    stage: String,
    started_at: i64,
    updated_at: i64,
}

/// The single in-flight slot (§6): released when dropped.
struct SlotGuard {
    service: Arc<AcmeService>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        *lock(&self.service.slot) = None;
    }
}

/// Why an order run stopped.
#[derive(Debug, Clone)]
struct Failure {
    stage: &'static str,
    detail: String,
    rate_limited: bool,
}

impl Failure {
    fn new(stage: &'static str, detail: impl Into<String>) -> Self {
        Self {
            stage,
            detail: detail.into(),
            rate_limited: false,
        }
    }

    fn ca(stage: &'static str, err: CaError) -> Self {
        Self {
            stage,
            rate_limited: matches!(err, CaError::RateLimited(_)),
            detail: err.to_string(),
        }
    }

    /// Whether the run reached the CA, so its failure backs off (§7). The look asks the edge and
    /// the vault only.
    fn ca_called(&self) -> bool {
        self.stage != "look"
    }
}

/// How an order run ended.
#[derive(Debug)]
enum FlowEnd {
    /// The edge serves the new certificate.
    Installed {
        fingerprint: String,
    },
    /// Stored; the edge did not confirm it: the next check reloads it, never orders.
    NotServed {
        fingerprint: String,
        detail: String,
    },
    /// The edge refused it, and the secret was rolled back (or its rollback is retried).
    Refused {
        fingerprint: String,
        detail: String,
    },
    /// A test order passed steps 1–5.
    Tested {
        fingerprint: String,
        trusted: bool,
        detail: String,
    },
    Failed(Failure),
}

/// One order run's inputs.
struct Run {
    kind: RunKind,
    settings: AcmeSettings,
    directory_url: String,
    edge_node: String,
    view: EdgeView,
    replaces: Option<CertRef>,
}

/// A rollback SY.vault answered it cannot do: the secret has no previous version (the refused
/// certificate was its first), or no longer exists.
fn nothing_to_roll_back(err: &MeshError) -> bool {
    matches!(
        err,
        MeshError::Refused { code, .. } if code == "NO_PREVIOUS_VERSION" || code == "KEY_NOT_FOUND"
    )
}

/// Why SY.admin may not write the edge's secret now.
#[derive(Debug, Clone, PartialEq)]
enum WriteCheck {
    /// SY.vault did not answer: retried.
    Vault(MeshError),
    /// The vault holds a write the edge does not serve, or the edge changed: nothing is written.
    Refused(String),
}

/// How the rollback of a refused write came out.
#[derive(Debug, Clone, PartialEq)]
enum RollbackOutcome {
    /// The refused write is no longer current: rolled back now, or moved by someone else.
    Done,
    /// The refused write was the secret's first version: there is nothing to roll back to.
    NothingToRollBackTo,
    /// SY.admin does not know which write it made (it could not read it back): it rolls nothing
    /// back on its own.
    Unverified,
    /// SY.vault did not answer: retried at the next check, which checks the write again first.
    Retry(String),
}

/// Why a look gave no status.
#[derive(Debug, Clone, PartialEq)]
enum LookError {
    /// No answer: a timeout, or the router answered UNREACHABLE (VPN_BLOCKED included).
    NoAnswer(String),
    /// The edge answered with an error: it cannot report its certificate.
    Config(String),
}

impl std::fmt::Display for LookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LookError::NoAnswer(detail) => write!(f, "no answer: {detail}"),
            LookError::Config(detail) => {
                write!(f, "the edge cannot report its certificate: {detail}")
            }
        }
    }
}

/// What a reload of an issued certificate came to.
enum Reloaded {
    Served,
    Refused(String),
    NotConfirmed(String),
}

pub(crate) struct AcmeService {
    config: AcmeConfig,
    /// A later finding that turned ACME off (the directory does not offer the profile).
    off_reason: Mutex<Option<String>>,
    /// SY.admin's own ILK: the owner of the account secrets.
    admin_ilk: String,
    /// SY.admin's hive: the alerts' hive when no edge is known.
    hive_id: String,
    state_path: PathBuf,
    state: Mutex<AcmeState>,
    state_warning: Option<String>,
    slot: Mutex<Option<Progress>>,
    directories: Mutex<BTreeMap<String, DirectoryCacheEntry>>,
    deps: AcmeDeps,
}

/// A directory read: when, and what it said (or why it could not be read).
type DirectoryCacheEntry = (i64, Result<DirectoryInfo, String>);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl AcmeService {
    pub(crate) fn new(
        config: AcmeConfig,
        deps: AcmeDeps,
        state_path: PathBuf,
        admin_ilk: String,
        hive_id: String,
    ) -> Arc<Self> {
        let now = (deps.clock)();
        let (state, state_warning) = load_state(&state_path, now);
        if let Some(warning) = &state_warning {
            tracing::error!(warning = %warning, "acme.json was unreadable: ACME starts from an empty state");
        }
        Arc::new(Self {
            config,
            off_reason: Mutex::new(None),
            admin_ilk,
            hive_id,
            state_path,
            state: Mutex::new(state),
            state_warning,
            slot: Mutex::new(None),
            directories: Mutex::new(BTreeMap::new()),
            deps,
        })
    }

    fn now(&self) -> i64 {
        (self.deps.clock)()
    }

    /// The working settings, unless ACME is absent or off.
    fn settings(&self) -> Option<AcmeSettings> {
        match &self.config {
            AcmeConfig::On(settings) if lock(&self.off_reason).is_none() => Some(settings.clone()),
            _ => None,
        }
    }

    fn off_detail(&self) -> String {
        if let Some(reason) = lock(&self.off_reason).clone() {
            return format!("ACME is off: {reason}");
        }
        match &self.config {
            AcmeConfig::Absent { .. } => {
                "ACME is not configured: the motherbee's hive.yaml has no acme section".to_string()
            }
            AcmeConfig::Off { reason, .. } => format!("ACME is off: {reason}"),
            AcmeConfig::On(_) => "ACME is on".to_string(),
        }
    }

    fn turn_off(&self, reason: String) {
        tracing::error!(reason = %reason, "ACME turned off until SY.admin restarts");
        *lock(&self.off_reason) = Some(reason);
    }

    fn read<T>(&self, f: impl FnOnce(&AcmeState) -> T) -> T {
        f(&lock(&self.state))
    }

    /// Change the state and write `acme.json`.
    fn update<T>(&self, f: impl FnOnce(&mut AcmeState) -> T) -> T {
        let mut state = lock(&self.state);
        let out = f(&mut state);
        if let Err(err) = persist_state(&self.state_path, &state) {
            tracing::error!(error = %err, "acme.json could not be written");
        }
        out
    }

    fn try_begin(self: &Arc<Self>, kind: RunKind) -> Result<SlotGuard, ActionRefusal> {
        let now = self.now();
        let mut slot = lock(&self.slot);
        if let Some(progress) = slot.as_ref() {
            let mut refusal = ActionRefusal::new(
                409,
                "ACME_BUSY",
                format!(
                    "an ACME {} is in flight (stage {}, since {}): one order at a time",
                    progress.kind.as_str(),
                    progress.stage,
                    iso(progress.started_at)
                ),
            );
            refusal.payload = json!({ "in_flight": progress });
            return Err(refusal);
        }
        *slot = Some(Progress {
            kind,
            stage: "start".to_string(),
            started_at: now,
            updated_at: now,
        });
        Ok(SlotGuard {
            service: Arc::clone(self),
        })
    }

    fn stage(&self, stage: &str) {
        let now = self.now();
        if let Some(progress) = lock(&self.slot).as_mut() {
            progress.stage = stage.to_string();
            progress.updated_at = now;
        }
    }

    async fn ca<T>(&self, call: impl Future<Output = Result<T, CaError>>) -> Result<T, CaError> {
        match tokio::time::timeout(self.deps.timings.ca_call, call).await {
            Ok(result) => result,
            Err(_) => Err(CaError::Timeout(format!(
                "no CA answer in {}s",
                self.deps.timings.ca_call.as_secs()
            ))),
        }
    }

    /// The loop: the first check 2 min after start, then at the time each check sets.
    pub(crate) async fn run(self: Arc<Self>) {
        tokio::time::sleep(self.deps.timings.first_check).await;
        loop {
            let next = self.check_once().await;
            let wait = (next - self.now()).max(1) as u64;
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
    }

    /// One check (§7). Returns when the next one runs.
    pub(crate) async fn check_once(self: &Arc<Self>) -> i64 {
        let now = self.now();
        let Ok(_slot) = self.try_begin(RunKind::Check) else {
            return now + self.deps.timings.edge_retry.as_secs() as i64;
        };
        let next = self.check_inner().await.max(self.now() + 60);
        self.update(|state| state.next_check_at = Some(next));
        next
    }

    async fn check_inner(self: &Arc<Self>) -> i64 {
        let now = self.now();
        let timings = &self.deps.timings;
        let jitter =
            ((self.deps.random)().clamp(0.0, 1.0) * timings.check_jitter.as_secs() as f64) as i64;
        let regular = now + timings.check_interval.as_secs() as i64 + jitter;
        let retry = now + timings.edge_retry.as_secs() as i64;
        let edge_node = match self.resolve_edge() {
            Some(edge_node) => edge_node,
            None => {
                self.evaluate_and_alert().await;
                return regular;
            }
        };
        self.retry_rollback().await;
        let settings = self.settings();
        if let Some(settings) = &settings {
            self.refresh_directory(settings).await;
        }
        let settings = self.settings();
        self.stage("look");
        let next = match self.look(&edge_node).await {
            Err(LookError::NoAnswer(_)) => {
                self.note_check("no order: the edge did not answer; retried in 10 min");
                retry
            }
            Err(LookError::Config(detail)) => {
                self.note_check(&format!(
                    "no order: the edge cannot report its certificate ({detail})"
                ));
                regular
            }
            Ok(view) => match &settings {
                None => {
                    self.note_check("expiry checks only: ACME is not on");
                    regular
                }
                Some(settings) => self.act(settings, &edge_node, view, regular, retry).await,
            },
        };
        self.evaluate_and_alert().await;
        next
    }

    /// The edge to look at; records it, and forgets the status of a different one.
    fn resolve_edge(&self) -> Option<String> {
        match (self.deps.edge)() {
            Ok(edge_node) => {
                self.note_edge(&edge_node);
                Some(edge_node)
            }
            Err(reason) => {
                self.update(|state| {
                    state.edge = EdgeSeen {
                        not_configured: Some(reason.clone()),
                        ..EdgeSeen::default()
                    };
                });
                None
            }
        }
    }

    /// Record the edge a run talks to; the last status of another one no longer applies.
    fn note_edge(&self, edge_node: &str) {
        self.update(|state| {
            if state.edge.node.as_deref() != Some(edge_node) {
                state.edge = EdgeSeen {
                    node: Some(edge_node.to_string()),
                    ..EdgeSeen::default()
                };
            }
            state.edge.not_configured = None;
        });
    }

    fn note_check(&self, decision: &str) {
        let now = self.now();
        tracing::info!(decision = %decision, "ACME check");
        self.update(|state| {
            state.last_check = Some(CheckRecord {
                at: now,
                decision: decision.to_string(),
            })
        });
    }

    /// EDGE_TLS_STATUS (§6 step 1), recorded as the last status seen. No answer (a timeout, or
    /// the router answering UNREACHABLE) is an unreachable edge; an answer with `status: error`
    /// is an edge that cannot report its certificate, a configuration error.
    async fn look(&self, edge_node: &str) -> Result<EdgeView, LookError> {
        let now = self.now();
        let answer = self
            .deps
            .mesh
            .edge_status(edge_node, self.deps.timings.edge_call)
            .await;
        let answer = match answer {
            Ok(answer) => answer,
            Err(err) => {
                let detail = err.to_string();
                tracing::warn!(edge = %edge_node, detail = %detail, "EDGE_TLS_STATUS got no answer");
                self.update(|state| {
                    state.edge.unreachable_since_at.get_or_insert(now);
                    state.edge.last_error = Some(detail.clone());
                });
                return Err(LookError::NoAnswer(detail));
            }
        };
        let view = if answer_ok(&answer) {
            EdgeView::from_answer(&answer)
        } else {
            Err(answer_detail(&answer))
        };
        match view {
            Ok(view) => {
                self.record_view(&view, now);
                Ok(view)
            }
            Err(detail) => {
                tracing::error!(edge = %edge_node, detail = %detail, "the edge cannot report its certificate");
                self.update(|state| {
                    state.edge.last_answer_at = Some(now);
                    state.edge.unreachable_since_at = None;
                    state.edge.last_error = None;
                    state.edge.config_error = Some(detail.clone());
                });
                Err(LookError::Config(detail))
            }
        }
    }

    fn record_view(&self, view: &EdgeView, now: i64) {
        self.update(|state| {
            state.edge.last_status = Some(view.clone());
            state.edge.last_answer_at = Some(now);
            state.edge.unreachable_since_at = None;
            state.edge.last_error = None;
            state.edge.config_error = None;
        });
    }

    /// Read the directory's terms URL and profiles, once a day. A profile the directory does
    /// not offer turns ACME off (§3).
    async fn refresh_directory(&self, settings: &AcmeSettings) {
        let now = self.now();
        let fresh = lock(&self.directories)
            .get(&settings.directory_url)
            .is_some_and(|(at, info)| {
                info.is_ok() && now - at < self.deps.timings.directory_refresh.as_secs() as i64
            });
        if fresh {
            return;
        }
        let info = self.directory(&settings.directory_url).await;
        if let Ok(info) = &info {
            if let Some(profile) = &settings.profile {
                if !info.profiles.contains(profile) {
                    self.turn_off(format!(
                        "acme.profile {profile} is not offered by {} (it offers: {})",
                        settings.directory_url,
                        if info.profiles.is_empty() {
                            "none".to_string()
                        } else {
                            info.profiles.join(", ")
                        }
                    ));
                }
            }
        }
    }

    async fn directory(&self, url: &str) -> Result<DirectoryInfo, String> {
        let now = self.now();
        let fetched = self
            .ca(self.deps.ca.directory(url))
            .await
            .map_err(|err| err.to_string());
        if let Err(err) = &fetched {
            tracing::warn!(directory = %url, error = %err, "the ACME directory could not be read");
        }
        lock(&self.directories).insert(url.to_string(), (now, fetched.clone()));
        fetched
    }

    /// The decision table, once the edge answered and `acme:` is on.
    async fn act(
        self: &Arc<Self>,
        settings: &AcmeSettings,
        edge_node: &str,
        view: EdgeView,
        regular: i64,
        retry: i64,
    ) -> i64 {
        let now = self.now();
        self.settle_pending(&view).await;
        let renew_at = self.plan_renewal(settings, &view).await;
        let decision = self.read(|state| decide(&view, settings, state, renew_at, now));
        self.note_check(&decision.describe());
        match decision {
            Decision::LeaveAlone(_) | Decision::AlertOnly(_) => regular,
            Decision::Idle { renew_at, .. } => regular.min(renew_at.max(now + 60)),
            Decision::Held { until, .. } => until.map_or(regular, |until| regular.min(until)),
            Decision::Reload(_) => {
                self.stage("reload");
                if self.reload_pending(edge_node).await {
                    regular
                } else {
                    retry
                }
            }
            Decision::Order { replaces, .. } => {
                let run = Run {
                    kind: RunKind::Check,
                    settings: settings.clone(),
                    directory_url: settings.directory_url.clone(),
                    edge_node: edge_node.to_string(),
                    view,
                    replaces,
                };
                let started_at = now;
                let end = self.order_flow(&run).await;
                let pending = matches!(end, FlowEnd::NotServed { .. });
                self.finish(&run, started_at, end);
                let held = self.read(|state| {
                    state
                        .backoff
                        .rate_limited_until
                        .into_iter()
                        .chain(state.backoff.next_attempt_at)
                        .filter(|until| *until > now)
                        .max()
                });
                if pending {
                    retry
                } else {
                    held.map_or(regular, |until| regular.min(until))
                }
            }
        }
    }

    /// An issued certificate the edge now serves is installed; one whose write is no longer the
    /// vault's current one (someone wrote or rolled back the secret since) is forgotten. The
    /// write is matched by version and `updated_at`: a rollback brings a version number back.
    async fn settle_pending(&self, view: &EdgeView) {
        let Some(pending) = self.read(|state| state.pending_install.clone()) else {
            return;
        };
        if view.serving() && view.fingerprint() == Some(pending.fingerprint.as_str()) {
            self.mark_installed(&pending.fingerprint);
            return;
        }
        let Some(written) = pending.written else {
            return;
        };
        if let Ok(metadata) = self
            .deps
            .mesh
            .vault_metadata(&pending.vault_key, self.deps.timings.mesh_call)
            .await
        {
            let current = metadata.as_ref().and_then(SecretMetadata::write);
            if current.as_ref() != Some(&written) {
                tracing::warn!(
                    key = %pending.vault_key,
                    written = %written,
                    current = %identity_text(current.as_ref()),
                    "the edge's secret changed since SY.admin stored its certificate: it is no longer waited for"
                );
                self.update(|state| state.pending_install = None);
            }
        }
    }

    fn mark_installed(&self, fingerprint: &str) {
        let now = self.now();
        tracing::info!(fingerprint = %fingerprint, "the edge serves the certificate SY.admin issued");
        self.update(|state| {
            state.pending_install = None;
            for record in state.issued.iter_mut() {
                if record.fingerprint.eq_ignore_ascii_case(fingerprint) {
                    record.installed = true;
                    record.installed_at.get_or_insert(now);
                }
            }
            if let Some(attempt) = state.last_attempt.as_mut() {
                if attempt.fingerprint.as_deref() == Some(fingerprint)
                    && attempt.result == "not_served"
                {
                    attempt.result = "installed".to_string();
                }
            }
        });
    }

    /// The renewal point of a served leaf this account issued: a random point of the ARI window
    /// (kept, and asked again after the CA's Retry-After), else two thirds of its lifetime.
    async fn plan_renewal(&self, settings: &AcmeSettings, view: &EdgeView) -> Option<i64> {
        let now = self.now();
        let leaf = view.leaf.as_ref().filter(|_| view.serving())?;
        let record = self.read(|state| {
            issued_by_account(state, &settings.directory_url, &leaf.fingerprint).cloned()
        })?;
        let existing = self.read(|state| {
            state
                .renewal
                .clone()
                .filter(|plan| plan.fingerprint.eq_ignore_ascii_case(&leaf.fingerprint))
        });
        if let Some(plan) = &existing {
            if plan.next_ari_fetch_at.is_some_and(|at| at > now) {
                return Some(plan.renew_at);
            }
        }
        let lifetime_plan = RenewalPlan {
            fingerprint: leaf.fingerprint.clone(),
            source: "lifetime".to_string(),
            renew_at: leaf.two_thirds(),
            window_start_at: None,
            window_end_at: None,
            explanation_url: None,
            next_ari_fetch_at: None,
        };
        let ari = match (record.cert_ref(), self.renewal_info_url(settings).await) {
            (Some(cert), Some(url)) => Some(self.ca(self.deps.ca.renewal_info(&url, &cert)).await),
            _ => None,
        };
        let plan = match ari {
            Some(Ok(answer)) if answer.end >= answer.start => {
                let retry = answer
                    .retry_after_secs
                    .unwrap_or(6 * 3_600)
                    .clamp(ARI_REFETCH_MIN_SECS, ARI_REFETCH_MAX_SECS);
                let same_window = existing.as_ref().is_some_and(|plan| {
                    plan.source == "ari"
                        && plan.window_start_at == Some(answer.start)
                        && plan.window_end_at == Some(answer.end)
                });
                let renew_at = match &existing {
                    Some(plan) if same_window => plan.renew_at,
                    _ => {
                        let fraction = (self.deps.random)().clamp(0.0, 1.0);
                        answer.start + ((answer.end - answer.start) as f64 * fraction) as i64
                    }
                };
                RenewalPlan {
                    source: "ari".to_string(),
                    renew_at,
                    window_start_at: Some(answer.start),
                    window_end_at: Some(answer.end),
                    explanation_url: answer.explanation_url,
                    next_ari_fetch_at: Some(now + retry),
                    ..lifetime_plan
                }
            }
            Some(Ok(_)) | None => lifetime_plan,
            Some(Err(err)) => {
                tracing::warn!(error = %err, "ARI gave no window: renewal at two thirds of the lifetime");
                lifetime_plan
            }
        };
        let renew_at = plan.renew_at;
        self.update(|state| state.renewal = Some(plan));
        Some(renew_at)
    }

    async fn renewal_info_url(&self, settings: &AcmeSettings) -> Option<String> {
        let cached = lock(&self.directories)
            .get(&settings.directory_url)
            .and_then(|(_, info)| info.clone().ok());
        let info = match cached {
            Some(info) => info,
            None => self.directory(&settings.directory_url).await.ok()?,
        };
        info.renewal_info
    }

    /// Reload the edge for a certificate SY.admin stored: true when nothing is left waiting.
    async fn reload_pending(&self, edge_node: &str) -> bool {
        let Some(pending) = self.read(|state| state.pending_install.clone()) else {
            // An awaiting edge whose verdict an order cannot fix: reload, then the conditions alert.
            let _ = self.reload_edge(edge_node).await;
            return true;
        };
        match self.reload_for(edge_node, &pending.fingerprint).await {
            Reloaded::Served => {
                self.mark_installed(&pending.fingerprint);
                true
            }
            Reloaded::Refused(detail) => {
                self.refused(
                    &pending.vault_key,
                    pending.written.as_ref(),
                    &pending.fingerprint,
                    &pending.serial,
                    &detail,
                )
                .await;
                true
            }
            Reloaded::NotConfirmed(detail) => {
                tracing::warn!(detail = %detail, "the edge did not confirm the stored certificate; retried");
                false
            }
        }
    }

    /// EDGE_TLS_RELOAD: one re-read and verdict. Its STATUS fields become the last status seen.
    async fn reload_edge(&self, edge_node: &str) -> Result<EdgeView, String> {
        let now = self.now();
        let answer = self
            .deps
            .mesh
            .edge_reload(edge_node, self.deps.timings.reload)
            .await
            .map_err(|err| err.to_string())?;
        if !answer_ok(&answer) {
            return Err(format!(
                "the edge could not reload: {}",
                answer_detail(&answer)
            ));
        }
        let view = EdgeView::from_answer(&answer)?;
        self.record_view(&view, now);
        Ok(view)
    }

    async fn reload_for(&self, edge_node: &str, fingerprint: &str) -> Reloaded {
        match self.reload_edge(edge_node).await {
            Err(detail) => Reloaded::NotConfirmed(detail),
            Ok(view) => {
                if let Some(refused) = view.refused {
                    if refused.transient() {
                        Reloaded::NotConfirmed(format!(
                            "the edge could not read its secret: {refused}"
                        ))
                    } else {
                        Reloaded::Refused(refused.to_string())
                    }
                } else if view.fingerprint() == Some(fingerprint) {
                    Reloaded::Served
                } else {
                    Reloaded::NotConfirmed(format!(
                        "the edge serves {} instead of {}",
                        view.fingerprint().map(short).unwrap_or("nothing"),
                        short(fingerprint)
                    ))
                }
            }
        }
    }

    /// The edge refused a certificate SY.admin issued: roll the secret back at once and order
    /// nothing until the operator acts (§6 step 7). The rollback applies only while the refused
    /// write is still the vault's current one: a rollback swaps the two versions, so a second one
    /// would put the refused certificate back.
    async fn refused(
        &self,
        vault_key: &str,
        written: Option<&WriteIdentity>,
        fingerprint: &str,
        serial: &str,
        detail: &str,
    ) {
        let now = self.now();
        tracing::error!(
            fingerprint = %fingerprint,
            detail = %detail,
            "the edge refused the certificate SY.admin issued: rolling the secret back"
        );
        let outcome = match written {
            Some(written) => self.rollback_if_current(vault_key, written).await,
            None => RollbackOutcome::Unverified,
        };
        let (rolled_back, retry) = match &outcome {
            RollbackOutcome::Done => (true, None),
            RollbackOutcome::NothingToRollBackTo => {
                tracing::error!(key = %vault_key, "the refused certificate was the secret's first version: there is nothing to roll back to");
                (false, None)
            }
            RollbackOutcome::Unverified => {
                tracing::error!(key = %vault_key, "SY.admin could not read back its write: nothing is rolled back automatically; roll back by hand after checking the secret");
                (false, None)
            }
            RollbackOutcome::Retry(err) => {
                tracing::error!(key = %vault_key, error = %err, "the rollback did not go through; each check retries it");
                (false, written.cloned())
            }
        };
        self.update(|state| {
            state.pending_install = None;
            state.install_refused = Some(InstallRefused {
                fingerprint: fingerprint.to_string(),
                serial: serial.to_string(),
                detail: detail.to_string(),
                rolled_back,
                refused_at: now,
            });
            state.rollback_pending = retry.map(|written| RollbackPending {
                vault_key: vault_key.to_string(),
                fingerprint: fingerprint.to_string(),
                written,
                since_at: now,
            });
        });
    }

    /// Roll the secret back if, and only if, its current write is still `written`.
    async fn rollback_if_current(
        &self,
        vault_key: &str,
        written: &WriteIdentity,
    ) -> RollbackOutcome {
        let metadata = match self
            .deps
            .mesh
            .vault_metadata(vault_key, self.deps.timings.mesh_call)
            .await
        {
            Ok(metadata) => metadata,
            Err(err) => {
                return RollbackOutcome::Retry(format!("its metadata gave no answer: {err}"))
            }
        };
        let current = metadata.as_ref().and_then(SecretMetadata::write);
        if current.as_ref() != Some(written) {
            tracing::info!(
                key = %vault_key,
                written = %written,
                current = %identity_text(current.as_ref()),
                "the refused write is no longer current (rolled back already, or written since): nothing to roll back"
            );
            return RollbackOutcome::Done;
        }
        match self
            .deps
            .mesh
            .vault_rollback(vault_key, self.deps.timings.mesh_call)
            .await
        {
            Ok(()) => RollbackOutcome::Done,
            Err(err) if nothing_to_roll_back(&err) => RollbackOutcome::NothingToRollBackTo,
            Err(err) => RollbackOutcome::Retry(err.to_string()),
        }
    }

    async fn retry_rollback(&self) {
        let Some(pending) = self.read(|state| state.rollback_pending.clone()) else {
            return;
        };
        match self
            .rollback_if_current(&pending.vault_key, &pending.written)
            .await
        {
            RollbackOutcome::Done => {
                tracing::info!(key = %pending.vault_key, "the pending rollback is done");
                self.update(|state| {
                    state.rollback_pending = None;
                    if let Some(refused) = state.install_refused.as_mut() {
                        refused.rolled_back = true;
                    }
                });
            }
            RollbackOutcome::NothingToRollBackTo | RollbackOutcome::Unverified => {
                tracing::warn!(key = %pending.vault_key, "the pending rollback has nothing left to roll back to");
                self.update(|state| state.rollback_pending = None);
            }
            RollbackOutcome::Retry(err) => {
                tracing::warn!(key = %pending.vault_key, error = %err, "the pending rollback failed again")
            }
        }
    }

    /// Steps 1–8 of §6 for a production order, steps 1–5 for a test order.
    async fn order_flow(&self, run: &Run) -> FlowEnd {
        self.stage("look");
        let (vault_key, served) = match self.check_edge_secret(&run.edge_node, &run.view).await {
            Ok(checked) => checked,
            Err(failure) => return FlowEnd::Failed(failure),
        };
        self.stage("account");
        let account = match self
            .account(&run.directory_url, run.settings.contact.as_deref())
            .await
        {
            Ok(account) => account,
            Err(failure) => return FlowEnd::Failed(failure),
        };
        if let Some(profile) = &run.settings.profile {
            let offered = account.profiles();
            if !offered.contains(profile) {
                let reason = format!(
                    "acme.profile {profile} is not offered by {}",
                    run.directory_url
                );
                if run.kind != RunKind::Test {
                    self.turn_off(reason.clone());
                }
                return FlowEnd::Failed(Failure::new("order", reason));
            }
        }
        let mut order = match self.place_order(account.as_ref(), run).await {
            Ok(order) => order,
            Err(failure) => return FlowEnd::Failed(failure),
        };
        if let Err(failure) = self.validate(&mut order, run).await {
            self.abandon(&mut order, run.kind).await;
            return FlowEnd::Failed(failure);
        }
        self.stage("finalize");
        let finalized = self.finalize(&mut order, &run.settings.name).await;
        self.forget_order(run.kind);
        let (chain_pem, key_pem) = match finalized {
            Ok(material) => material,
            Err(failure) => return FlowEnd::Failed(failure),
        };
        self.stage("check");
        let now = self.now();
        let account_url = account.url();
        let issued = issued_record(&chain_pem, run, &account_url, now);
        if run.kind != RunKind::Test {
            if let Some(record) = &issued {
                self.update(|state| {
                    state.issued.push(record.clone());
                    let excess = state.issued.len().saturating_sub(ISSUED_KEPT);
                    state.issued.drain(..excess);
                });
            }
        }
        let checked = self.check_material(&chain_pem, &key_pem, run, now);
        let report = match checked {
            Ok(report) => report,
            Err(detail) => return FlowEnd::Failed(Failure::new("check", detail)),
        };
        let fingerprint = report.leaf.fingerprint.clone();
        if run.kind == RunKind::Test {
            return FlowEnd::Tested {
                fingerprint,
                trusted: report.trusted,
                detail: match (&report.trust_detail, report.trusted) {
                    (_, true) => format!(
                        "the chain verified to a trusted root: is {} really a staging CA?",
                        run.directory_url
                    ),
                    (Some(detail), false) => {
                        format!("issued and checked; the root is not trusted, as expected of a staging CA ({detail})")
                    }
                    (None, false) => "issued and checked; the root is not trusted".to_string(),
                },
            };
        }
        self.stage("store");
        let written = match self
            .store(
                &vault_key,
                &run.edge_node,
                run.view.fingerprint(),
                served.as_ref(),
                &chain_pem,
                &key_pem,
            )
            .await
        {
            Ok(written) => written,
            Err(failure) => return FlowEnd::Failed(failure),
        };
        drop(key_pem);
        let serial = hex::encode(&report.leaf.serial);
        self.update(|state| {
            for record in state.issued.iter_mut() {
                if record.fingerprint == fingerprint {
                    record.stored = true;
                }
            }
            state.pending_install = Some(PendingInstall {
                fingerprint: fingerprint.clone(),
                serial: serial.clone(),
                vault_key: vault_key.clone(),
                edge_node: run.edge_node.clone(),
                written: written.clone(),
                stored_at: now,
            });
        });
        self.stage("reload");
        match self.reload_for(&run.edge_node, &fingerprint).await {
            Reloaded::Served => {
                self.mark_installed(&fingerprint);
                FlowEnd::Installed { fingerprint }
            }
            Reloaded::Refused(detail) => {
                self.refused(&vault_key, written.as_ref(), &fingerprint, &serial, &detail)
                    .await;
                FlowEnd::Refused {
                    fingerprint,
                    detail,
                }
            }
            Reloaded::NotConfirmed(detail) => FlowEnd::NotServed {
                fingerprint,
                detail,
            },
        }
    }

    /// Step 1: a vault edge, a secret named `edge_tls*` whose metadata (when it exists) is the
    /// edge's, and a vault that answers before any CA call. SY.admin writes only onto what the
    /// edge serves: the vault's current write must be the one the edge reports, and an edge that
    /// serves nothing from the vault (awaiting, `not_found`) must have no secret at all. Returns
    /// the secret's name and that write (none: no secret).
    async fn check_edge_secret(
        &self,
        edge_node: &str,
        view: &EdgeView,
    ) -> Result<(String, Option<WriteIdentity>), Failure> {
        if !view.is_vault() {
            return Err(Failure::new(
                "look",
                format!(
                    "{edge_node} serves TLS in {} mode: ACME manages a vault edge only",
                    view.mode.as_deref().unwrap_or("an unknown")
                ),
            ));
        }
        let key = view
            .tls_vault_key
            .clone()
            .ok_or_else(|| Failure::new("look", format!("{edge_node} did not name its secret")))?;
        check_edge_secret_name(&key).map_err(|detail| Failure::new("look", detail))?;
        let metadata = self
            .deps
            .mesh
            .vault_metadata(&key, self.deps.timings.mesh_call)
            .await
            .map_err(|err| {
                Failure::new(
                    "look",
                    format!(
                        "SY.vault gave no metadata for {key} ({err}): nothing is asked of the CA"
                    ),
                )
            })?;
        if let Some(metadata) = &metadata {
            check_edge_secret_metadata(metadata, edge_node)
                .map_err(|detail| Failure::new("look", detail))?;
            if metadata.version.is_none() {
                return Err(Failure::new(
                    "look",
                    format!("SY.vault reported no version for {key}"),
                ));
            }
        }
        let current = metadata.as_ref().and_then(SecretMetadata::write);
        let served = view.served_write();
        if current == served {
            return Ok((key, current));
        }
        match self
            .writable(edge_node, &key, view.fingerprint(), served.as_ref())
            .await
        {
            Ok(current) => Ok((key, current)),
            Err(WriteCheck::Vault(err)) => Err(Failure::new(
                "look",
                format!("SY.vault gave no metadata for {key} ({err}): nothing is asked of the CA"),
            )),
            Err(WriteCheck::Refused(detail)) => Err(Failure::new("look", detail)),
        }
    }

    /// Whether SY.admin may write the edge's secret now: the vault's current write must be
    /// `expected` (what the edge served at the look); otherwise one EDGE_TLS_RELOAD, after which
    /// the edge must still serve the leaf the look saw and report the vault's current write. A
    /// metadata-only write (VAULT_ASSIGN) moves `updated_at` without changing the material, and
    /// the reload re-aligns the edge's pair. Returns the write SY.admin may write over.
    async fn writable(
        &self,
        edge_node: &str,
        vault_key: &str,
        fingerprint: Option<&str>,
        expected: Option<&WriteIdentity>,
    ) -> Result<Option<WriteIdentity>, WriteCheck> {
        let current = self.current_write(vault_key).await?;
        if current.as_ref() == expected {
            return Ok(current);
        }
        let view = self.reload_edge(edge_node).await.map_err(|err| {
            WriteCheck::Refused(format!(
                "the vault holds a version the edge does not serve ({vault_key} is {} in the vault, {} at the edge), and the edge did not reload: {err}; SY.admin writes only onto what the edge serves",
                identity_text(current.as_ref()),
                identity_text(expected)
            ))
        })?;
        if view.fingerprint() != fingerprint {
            return Err(WriteCheck::Refused(format!(
                "the edge's certificate changed since the look (it served {}, it serves {}): nothing is written; the next check decides again",
                fingerprint.map(short).unwrap_or("nothing"),
                view.fingerprint().map(short).unwrap_or("nothing")
            )));
        }
        let current = self.current_write(vault_key).await?;
        let served = view.served_write();
        if current == served {
            return Ok(current);
        }
        Err(WriteCheck::Refused(format!(
            "the vault holds a version the edge does not serve: {vault_key} is {} in the vault, {} at the edge; SY.admin writes only onto what the edge serves (POST /acme/reload, roll the secret back, or delete one the edge never served)",
            identity_text(current.as_ref()),
            identity_text(served.as_ref())
        )))
    }

    /// The vault's current write of a secret (none: no secret).
    async fn current_write(&self, vault_key: &str) -> Result<Option<WriteIdentity>, WriteCheck> {
        let metadata = self
            .deps
            .mesh
            .vault_metadata(vault_key, self.deps.timings.mesh_call)
            .await
            .map_err(WriteCheck::Vault)?;
        Ok(metadata.as_ref().and_then(SecretMetadata::write))
    }

    /// The account for a directory (§6): from the vault, or created on first use with the terms
    /// agreed. Its URL must be the one `acme.json` recorded.
    async fn account(
        &self,
        directory_url: &str,
        contact: Option<&str>,
    ) -> Result<Box<dyn CaAccount>, Failure> {
        let now = self.now();
        let key = account_vault_key(directory_url);
        let recorded = self.read(|state| {
            state
                .accounts
                .get(directory_url)
                .map(|account| account.account_url.clone())
        });
        let stored = self
            .deps
            .mesh
            .vault_value(&key, self.deps.timings.mesh_call)
            .await
            .map_err(|err| {
                Failure::new(
                    "look",
                    format!("SY.vault did not give the ACME account {key} ({err})"),
                )
            })?;
        let record = |url: &str| {
            self.update(|state| {
                state.accounts.insert(
                    directory_url.to_string(),
                    AccountRecord {
                        account_url: url.to_string(),
                        vault_key: key.clone(),
                        recorded_at: now,
                    },
                )
            });
        };
        match stored {
            Some(credentials) => {
                let account = self
                    .ca(self.deps.ca.load_account(&credentials))
                    .await
                    .map_err(|err| Failure::ca("account", err))?;
                let url = account.url();
                match recorded {
                    Some(recorded) if recorded != url => Err(Failure::new(
                        "account",
                        format!(
                            "the vault's ACME account for {directory_url} is {url}, but acme.json records {recorded}: refused (a swapped account could reuse an authorization without the edge)"
                        ),
                    )),
                    Some(_) => Ok(account),
                    None => {
                        record(&url);
                        Ok(account)
                    }
                }
            }
            None => {
                let (account, credentials) = self
                    .ca(self.deps.ca.create_account(directory_url, contact))
                    .await
                    .map_err(|err| Failure::ca("account", err))?;
                let url = account.url();
                let metadata = json!({
                    "tenant_id": DEFAULT_ROOT_TENANT_ID,
                    "resource_type": ACCOUNT_RESOURCE_TYPE,
                    "ilk": self.admin_ilk,
                    "description": directory_url,
                });
                self.deps
                    .mesh
                    .vault_put(&key, credentials, metadata, self.deps.timings.mesh_call)
                    .await
                    .map_err(|err| {
                        Failure::new(
                            "account",
                            format!(
                                "the new ACME account {url} could not be stored in {key}: {err}"
                            ),
                        )
                    })?;
                if let Some(recorded) = recorded {
                    tracing::warn!(
                        directory = %directory_url,
                        previous = %recorded,
                        account = %url,
                        "the vault held no ACME account: a new one replaces the one acme.json recorded"
                    );
                }
                tracing::info!(directory = %directory_url, account = %url, "ACME account created");
                record(&url);
                Ok(account)
            }
        }
    }

    /// Step 2: resume the recorded order, else place one (with `replaces`, retried once without
    /// it when the CA refuses it) and record its URL at once.
    async fn place_order(
        &self,
        account: &dyn CaAccount,
        run: &Run,
    ) -> Result<Box<dyn CaOrder>, Failure> {
        self.stage("order");
        let now = self.now();
        let name = &run.settings.name;
        let account_url = account.url();
        if run.kind != RunKind::Test {
            if let Some(recorded) = self.read(|state| state.order.clone()) {
                if recorded.directory_url == run.directory_url
                    && recorded.account_url == account_url
                    && recorded.name == *name
                {
                    match self.ca(account.resume_order(&recorded.order_url)).await {
                        Ok(mut order) => match order.status() {
                            OrderStatus::Pending | OrderStatus::Ready => {
                                tracing::info!(order = %recorded.order_url, "resuming the recorded ACME order");
                                return Ok(order);
                            }
                            status => tracing::info!(
                                order = %recorded.order_url,
                                status = ?status,
                                "the recorded ACME order cannot be finished (its key died with the last run): a new one is placed"
                            ),
                        },
                        Err(err) => tracing::warn!(
                            order = %recorded.order_url,
                            error = %err,
                            "the recorded ACME order could not be fetched: a new one is placed"
                        ),
                    }
                }
                self.update(|state| state.order = None);
            }
        }
        let profile = run.settings.profile.as_deref();
        let mut replaced = run.replaces.is_some();
        let placed = match self
            .ca(account.new_order(name, profile, run.replaces.as_ref()))
            .await
        {
            Err(CaError::Replaces(detail)) if replaced => {
                tracing::warn!(detail = %detail, "the CA refused replaces: the order is placed once more without it");
                replaced = false;
                self.ca(account.new_order(name, profile, None)).await
            }
            other => other,
        };
        let order = placed.map_err(|err| Failure::ca("order", err))?;
        if run.kind != RunKind::Test {
            let record = OrderRecord {
                directory_url: run.directory_url.clone(),
                account_url,
                order_url: order.url(),
                name: name.clone(),
                profile: run.settings.profile.clone(),
                replaces: replaced,
                kind: run.kind,
                started_at: now,
            };
            self.update(|state| state.order = Some(record));
        }
        Ok(order)
    }

    /// Step 3: arm the edge, mark the challenge ready, poll; always clear the challenge.
    async fn validate(&self, order: &mut Box<dyn CaOrder>, run: &Run) -> Result<(), Failure> {
        self.stage("challenge");
        match order.status() {
            OrderStatus::Ready => return Ok(()),
            OrderStatus::Pending => {}
            status => {
                return Err(Failure::new(
                    "challenge",
                    format!("the order is {status:?}"),
                ))
            }
        }
        let authorization = self
            .ca(order.authorization())
            .await
            .map_err(|err| Failure::ca("challenge", err))?;
        let (authz_name, digest) = match authorization {
            AuthzState::Pending { name, digest } => (name, digest),
            AuthzState::Valid => return self.poll(order).await,
            AuthzState::Other(status) => {
                return Err(Failure::new(
                    "challenge",
                    format!("the authorization for {} is {status}", run.settings.name),
                ))
            }
        };
        if tls_material::normalize_name(&authz_name) != run.settings.name {
            return Err(Failure::new(
                "challenge",
                format!("the CA authorizes {authz_name}, not {}", run.settings.name),
            ));
        }
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Failure::new(
                "challenge",
                "the challenge digest is not 32 bytes of hex",
            ));
        }
        let arm = json!({
            "name": run.settings.name,
            "digest": digest,
            "ttl_secs": CHALLENGE_TTL_SECS,
        });
        let armed = self
            .deps
            .mesh
            .edge_challenge(&run.edge_node, arm, self.deps.timings.edge_call)
            .await;
        let armed = match armed {
            Ok(answer) if answer_ok(&answer) && answer["armed"].as_bool() == Some(true) => Ok(()),
            Ok(answer) => Err(answer_detail(&answer)),
            Err(err) => Err(err.to_string()),
        };
        let outcome = match armed {
            Err(detail) => Err(Failure::new(
                "challenge",
                format!("{} did not arm the challenge: {detail}", run.edge_node),
            )),
            Ok(()) => match self.ca(order.set_challenge_ready()).await {
                Err(err) => Err(Failure::ca("challenge", err)),
                Ok(()) => self.poll(order).await,
            },
        };
        self.clear_challenge(&run.edge_node, &run.settings.name, &digest)
            .await;
        outcome
    }

    async fn poll(&self, order: &mut Box<dyn CaOrder>) -> Result<(), Failure> {
        let limit = self.deps.timings.order_poll;
        let polled =
            tokio::time::timeout(limit + self.deps.timings.ca_call, order.poll_ready(limit)).await;
        let problem = match polled {
            Ok(Ok(OrderStatus::Ready)) => return Ok(()),
            Ok(Ok(OrderStatus::Invalid)) => "the order is invalid".to_string(),
            Ok(Ok(status)) => {
                return Err(Failure::new(
                    "challenge",
                    format!("the order is {status:?} after polling"),
                ))
            }
            Ok(Err(CaError::Problem(detail))) => detail,
            Ok(Err(err)) => return Err(Failure::ca("challenge", err)),
            Err(_) => {
                return Err(Failure::new(
                    "challenge",
                    format!("the order was not ready in {}s", limit.as_secs()),
                ))
            }
        };
        let detail =
            match tokio::time::timeout(self.deps.timings.ca_call, order.challenge_error()).await {
                Ok(Some(error)) => error,
                _ => problem,
            };
        Err(Failure::new(
            "challenge",
            format!("the CA did not validate the challenge: {detail}"),
        ))
    }

    async fn clear_challenge(&self, edge_node: &str, name: &str, digest: &str) {
        let clear = json!({ "name": name, "digest": digest, "clear": true });
        match self
            .deps
            .mesh
            .edge_challenge(edge_node, clear, self.deps.timings.edge_call)
            .await
        {
            Ok(answer) if answer_ok(&answer) => {}
            Ok(answer) => {
                tracing::warn!(detail = %answer_detail(&answer), "the challenge clear was refused; it expires on its own")
            }
            Err(err) => {
                tracing::warn!(error = %err, "the challenge clear got no answer; it expires on its own")
            }
        }
    }

    /// An abandoned order: its pending authorizations are deactivated and its record dropped.
    async fn abandon(&self, order: &mut Box<dyn CaOrder>, kind: RunKind) {
        if let Err(err) = self.ca(order.deactivate_pending()).await {
            tracing::warn!(error = %err, "the abandoned authorization could not be deactivated");
        }
        self.forget_order(kind);
    }

    fn forget_order(&self, kind: RunKind) {
        if kind != RunKind::Test {
            self.update(|state| state.order = None);
        }
    }

    /// Step 4: a fresh key and CSR, finalize, the chain.
    async fn finalize(
        &self,
        order: &mut Box<dyn CaOrder>,
        name: &str,
    ) -> Result<(String, SecretPem), Failure> {
        let (csr, key) =
            new_key_and_csr(name).map_err(|detail| Failure::new("finalize", detail))?;
        self.ca(order.finalize(&csr))
            .await
            .map_err(|err| Failure::ca("finalize", err))?;
        let limit = self.deps.timings.order_poll;
        let chain =
            match tokio::time::timeout(limit + self.deps.timings.ca_call, order.certificate(limit))
                .await
            {
                Ok(Ok(chain)) => chain,
                Ok(Err(err)) => return Err(Failure::ca("finalize", err)),
                Err(_) => {
                    return Err(Failure::new(
                        "finalize",
                        format!(
                            "the CA did not hand out the certificate in {}s",
                            limit.as_secs()
                        ),
                    ))
                }
            };
        Ok((chain, key))
    }

    /// Step 5: the shared verdict, the name, a chain with an intermediate that verifies to a
    /// trusted root (a test order expects it not to), and more than 2 days left.
    fn check_material(
        &self,
        chain_pem: &str,
        key_pem: &SecretPem,
        run: &Run,
        now: i64,
    ) -> Result<MaterialReport, String> {
        let name = &run.settings.name;
        let checked = tls_material::inspect(chain_pem.as_bytes(), key_pem.expose().as_bytes(), now)
            .map_err(|err| format!("the certificate fails the edge's verdict: {err}"))?;
        if !tls_material::name_covered(&checked.leaf.names, name) {
            return Err(format!(
                "the leaf names {} and does not cover {name}",
                checked.leaf.names.join(", ")
            ));
        }
        if checked.chain.len() < 2 {
            return Err("the chain is a leaf with no intermediate".to_string());
        }
        let roots = if run.settings.trust_anchor_roots.is_empty() {
            (self.deps.platform_roots)()
        } else {
            Ok(run.settings.trust_anchor_roots.clone())
        };
        let trust = roots.and_then(|roots| verify_chain(&checked.chain, name, now, roots));
        if run.kind != RunKind::Test {
            if let Err(err) = &trust {
                return Err(format!(
                    "the chain does not verify to a trusted root for {name}: {err}"
                ));
            }
        }
        if checked.leaf.not_after - now <= MIN_REMAINING_SECS {
            return Err(format!(
                "the leaf expires at {}, less than 2 days away",
                iso(checked.leaf.not_after)
            ));
        }
        Ok(MaterialReport {
            trusted: trust.is_ok(),
            trust_detail: trust.err(),
            leaf: checked.leaf,
        })
    }

    /// Step 6: put the edge's secret with explicit metadata, only while the vault's current write
    /// is still the one the edge served at the look (`served`; none: still no secret). A vault
    /// that does not answer is retried for 10 min, the material kept in memory meanwhile. Returns
    /// the write the put made, read back (version and `updated_at`), which a rollback checks.
    async fn store(
        &self,
        vault_key: &str,
        edge_node: &str,
        served_leaf: Option<&str>,
        served: Option<&WriteIdentity>,
        chain_pem: &str,
        key_pem: &SecretPem,
    ) -> Result<Option<WriteIdentity>, Failure> {
        let value = json!({ "cert": chain_pem, "key": key_pem.expose() });
        let metadata = json!({
            "tenant_id": DEFAULT_ROOT_TENANT_ID,
            "resource_type": TLS_RESOURCE_TYPE,
            "ilk": fluxbee_sdk::deterministic_system_ilk_id(edge_node),
        });
        let mesh_call = self.deps.timings.mesh_call;
        let deadline = tokio::time::Instant::now() + self.deps.timings.vault_retry_window;
        let mut base = served.cloned();
        let mut unanswered_put = false;
        let put_version = loop {
            // A put SY.vault applied but whose answer was lost shows up, at the retry, as the
            // next version written by SY.admin: that one is SY.admin's own write.
            let next_version = base.as_ref().map_or(1, |write| write.version + 1);
            let attempt = match self.deps.mesh.vault_metadata(vault_key, mesh_call).await {
                Err(err) => Err(err),
                Ok(current_metadata) => {
                    let current = current_metadata.as_ref().and_then(SecretMetadata::write);
                    let landed = unanswered_put
                        && current_metadata.as_ref().is_some_and(|metadata| {
                            metadata.version == Some(next_version)
                                && metadata.created_by.as_deref() == Some(self.admin_ilk.as_str())
                        });
                    if landed {
                        tracing::info!(key = %vault_key, "the put that got no answer had gone through");
                        break Some(next_version);
                    }
                    if current != base {
                        // Changed since the look: write only if the edge, reloaded, still serves
                        // the same leaf from the vault's current write (a metadata-only change).
                        match self
                            .writable(edge_node, vault_key, served_leaf, base.as_ref())
                            .await
                        {
                            Ok(aligned) => base = aligned,
                            Err(WriteCheck::Vault(err)) => {
                                if tokio::time::Instant::now() >= deadline {
                                    return Err(Failure::new(
                                        "store",
                                        format!("SY.vault gave no metadata for {vault_key} ({err}); the issued certificate is dropped"),
                                    ));
                                }
                                tokio::time::sleep(self.deps.timings.vault_retry_every).await;
                                continue;
                            }
                            Err(WriteCheck::Refused(detail)) => {
                                return Err(Failure::new(
                                    "store",
                                    format!("{vault_key} changed since the look: {detail}; the issued certificate is dropped"),
                                ));
                            }
                        }
                    }
                    let put = self
                        .deps
                        .mesh
                        .vault_put(vault_key, value.clone(), metadata.clone(), mesh_call)
                        .await;
                    unanswered_put |= matches!(put, Err(MeshError::NoAnswer(_)));
                    put
                }
            };
            let retryable = match &attempt {
                Ok(version) => break *version,
                Err(MeshError::NoAnswer(_)) => true,
                Err(MeshError::Refused { code, .. }) => {
                    matches!(code.as_str(), "IDENTITY_UNAVAILABLE" | "STORAGE_ERROR")
                }
            };
            let err = attempt.err().map(|err| err.to_string()).unwrap_or_default();
            if !retryable || tokio::time::Instant::now() >= deadline {
                return Err(Failure::new(
                    "store",
                    format!("the issued certificate could not be stored in {vault_key} ({err}); it is dropped"),
                ));
            }
            tracing::warn!(key = %vault_key, error = %err, "the vault did not take the certificate; retrying");
            tokio::time::sleep(self.deps.timings.vault_retry_every).await;
        };
        loop {
            match self.deps.mesh.vault_metadata(vault_key, mesh_call).await {
                Ok(current) => {
                    let written = current
                        .as_ref()
                        .and_then(SecretMetadata::write)
                        .filter(|write| put_version.is_none_or(|version| write.version == version));
                    if written.is_none() {
                        tracing::warn!(key = %vault_key, "SY.admin's write could not be identified: a refusal will not be rolled back automatically");
                    }
                    return Ok(written);
                }
                Err(err) if tokio::time::Instant::now() < deadline => {
                    tracing::warn!(key = %vault_key, error = %err, "the write could not be read back; retrying");
                    tokio::time::sleep(self.deps.timings.vault_retry_every).await;
                }
                Err(err) => {
                    tracing::warn!(key = %vault_key, error = %err, "the write could not be read back: a refusal will not be rolled back automatically");
                    return Ok(None);
                }
            }
        }
    }

    /// Step 8: the attempt, the backoff (§7) and the log.
    fn finish(&self, run: &Run, started_at: i64, end: FlowEnd) {
        let now = self.now();
        let (result, stage, detail, fingerprint, trusted, failure) = match end {
            FlowEnd::Installed { fingerprint } => {
                ("installed", None, None, Some(fingerprint), None, None)
            }
            FlowEnd::NotServed {
                fingerprint,
                detail,
            } => (
                "not_served",
                Some("reload"),
                Some(detail),
                Some(fingerprint),
                None,
                None,
            ),
            FlowEnd::Refused {
                fingerprint,
                detail,
            } => (
                "refused",
                Some("reload"),
                Some(detail),
                Some(fingerprint),
                None,
                None,
            ),
            FlowEnd::Tested {
                fingerprint,
                trusted,
                detail,
            } => (
                "tested",
                None,
                Some(detail),
                Some(fingerprint),
                Some(trusted),
                None,
            ),
            FlowEnd::Failed(failure) => (
                "failed",
                Some(failure.stage),
                Some(failure.detail.clone()),
                None,
                None,
                Some(failure),
            ),
        };
        let attempt = AttemptRecord {
            kind: run.kind,
            directory_url: run.directory_url.clone(),
            started_at,
            finished_at: now,
            result: result.to_string(),
            stage: stage.map(str::to_string),
            detail,
            fingerprint,
            trusted,
        };
        tracing::info!(
            kind = run.kind.as_str(),
            result = result,
            stage = ?attempt.stage,
            detail = ?attempt.detail,
            "ACME attempt finished"
        );
        self.update(|state| {
            if run.kind == RunKind::Test {
                state.last_test = Some(attempt);
                return;
            }
            state.last_attempt = Some(attempt);
            match &failure {
                None => {
                    state.backoff.failures = 0;
                    state.backoff.next_attempt_at = None;
                    state.renewal = None;
                }
                Some(failure) if failure.rate_limited => {
                    state.backoff.rate_limited_until =
                        Some(rate_limited_until(&failure.detail, now));
                }
                Some(failure) if failure.ca_called() => {
                    state.backoff.failures = state.backoff.failures.saturating_add(1);
                    state.backoff.next_attempt_at =
                        Some(now + backoff_delay(state.backoff.failures));
                }
                Some(_) => {}
            }
        });
    }

    /// Evaluate the conditions; write an alert for each that opened and each that closed.
    async fn evaluate_and_alert(&self) {
        let now = self.now();
        let settings = self.settings();
        let hive = self.read(|state| {
            state
                .edge
                .node
                .as_deref()
                .and_then(|node| node.split_once('@').map(|(_, hive)| hive.to_string()))
                .unwrap_or_else(|| self.hive_id.clone())
        });
        self.update(|state| {
            let open = evaluate_conditions(settings.as_ref(), state, now);
            apply_conditions(state, open, &hive, now);
        });
        self.flush_alerts().await;
    }

    /// Write the queued rows: the closures first, so that a condition that closed and opened
    /// again reaches the orchestrator in that order (it compares a row with its newest one for
    /// the hive, category, kind and state). A row the orchestrator already holds as its newest
    /// (`recorded: false`) is a true duplicate and counts as delivered.
    async fn flush_alerts(&self) {
        let (closures, opens) = self.read(|state| {
            (
                state.unsent_closures.clone(),
                state
                    .conditions
                    .iter()
                    .filter(|(_, record)| !record.alerted)
                    .map(|(kind, record)| (kind.clone(), record.clone()))
                    .collect::<Vec<_>>(),
            )
        });
        for closure in closures {
            let alert = alert_payload(
                &closure.kind,
                "info",
                &closure.hive,
                &format!("closed: {}", closure.detail),
                "closed",
            );
            if !self.deliver_alert(&closure.kind, alert).await {
                return;
            }
            self.update(|state| {
                if let Some(at) = state.unsent_closures.iter().position(|c| *c == closure) {
                    state.unsent_closures.remove(at);
                }
            });
        }
        for (kind, record) in opens {
            let alert = alert_payload(
                &kind,
                record.severity.as_str(),
                &record.hive,
                &record.detail,
                "open",
            );
            if !self.deliver_alert(&kind, alert).await {
                return;
            }
            self.update(|state| {
                if let Some(record) = state.conditions.get_mut(&kind) {
                    record.alerted = true;
                }
            });
        }
    }

    /// One `record_drift_alert`; false when the orchestrator did not take it (retried at the next
    /// check).
    async fn deliver_alert(&self, kind: &str, alert: Value) -> bool {
        match self
            .deps
            .mesh
            .drift_alert(alert, self.deps.timings.mesh_call)
            .await
        {
            Ok(true) => true,
            Ok(false) => {
                tracing::info!(kind = %kind, "the orchestrator already holds this certificate row as its newest: delivered");
                true
            }
            Err(err) => {
                tracing::warn!(kind = %kind, error = %err, "the certificate alert was not recorded; retried at the next check");
                false
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // The operator actions (§7)
    // -----------------------------------------------------------------------------------------

    /// `POST /acme/renew`: an order now, forced (no backoff, no cap). Replaces a hand-seeded or
    /// foreign certificate, certifies a new name, and is how the operator lifts an
    /// `install_refused`.
    pub(crate) fn begin_renew(self: &Arc<Self>) -> Result<Value, ActionRefusal> {
        let settings = self
            .settings()
            .ok_or_else(|| ActionRefusal::new(409, "ACME_OFF", self.off_detail()))?;
        let edge_node = (self.deps.edge)()
            .map_err(|reason| ActionRefusal::new(409, "EDGE_NOT_CONFIGURED", reason))?;
        let now = self.now();
        let blocked = self.read(|state| {
            if let Some(until) = state.backoff.rate_limited_until.filter(|until| *until > now) {
                return Some(ActionRefusal::new(
                    409,
                    "ACME_RATE_LIMITED",
                    format!("the CA rate-limited the last order until {}", iso(until)),
                ));
            }
            if let Some(pending) = &state.pending_install {
                return Some(ActionRefusal::new(
                    409,
                    "CERTIFICATE_NOT_SERVED",
                    format!(
                        "the certificate {} SY.admin stored is not served yet: POST /acme/reload first (no new order while an issued certificate waits)",
                        short(&pending.fingerprint)
                    ),
                ));
            }
            if let Some(pending) = &state.rollback_pending {
                return Some(ActionRefusal::new(
                    409,
                    "ROLLBACK_PENDING",
                    format!(
                        "the rollback of the refused certificate {} has not gone through; each check retries it",
                        short(&pending.fingerprint)
                    ),
                ));
            }
            None
        });
        if let Some(refusal) = blocked {
            return Err(refusal);
        }
        let slot = self.try_begin(RunKind::Renew)?;
        let lifted = self.update(|state| state.install_refused.take());
        if let Some(lifted) = lifted {
            tracing::info!(fingerprint = %lifted.fingerprint, "POST /acme/renew lifts the install_refused hold");
        }
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let _slot = slot;
            service.renew_run(settings, edge_node).await;
        });
        Ok(json!({
            "kind": "renew",
            "name": self.config.name(),
            "started_at": iso(now),
            "progress": "GET /acme/status",
        }))
    }

    async fn renew_run(self: Arc<Self>, settings: AcmeSettings, edge_node: String) {
        let started_at = self.now();
        self.note_edge(&edge_node);
        let directory_url = settings.directory_url.clone();
        let view = match self.look(&edge_node).await {
            Ok(view) => view,
            Err(detail) => {
                let run = Run {
                    kind: RunKind::Renew,
                    settings,
                    directory_url,
                    edge_node,
                    view: EdgeView::default(),
                    replaces: None,
                };
                self.finish(
                    &run,
                    started_at,
                    FlowEnd::Failed(Failure::new(
                        "look",
                        format!("the edge gave no status: {detail}"),
                    )),
                );
                self.evaluate_and_alert().await;
                return;
            }
        };
        self.settle_pending(&view).await;
        let replaces = self.read(|state| {
            view.leaf.as_ref().and_then(|leaf| {
                issued_by_account(state, &directory_url, &leaf.fingerprint)
                    .and_then(IssuedRecord::cert_ref)
            })
        });
        let run = Run {
            kind: RunKind::Renew,
            settings,
            directory_url,
            edge_node,
            view,
            replaces,
        };
        let end = self.order_flow(&run).await;
        self.finish(&run, started_at, end);
        self.evaluate_and_alert().await;
    }

    /// `POST /acme/test`: steps 1–5 against the staging directory, with its own account and no
    /// `replaces`; nothing is stored or installed (D5).
    pub(crate) fn begin_test(self: &Arc<Self>) -> Result<Value, ActionRefusal> {
        let settings = self
            .settings()
            .ok_or_else(|| ActionRefusal::new(409, "ACME_OFF", self.off_detail()))?;
        if settings.staging_directory_url == settings.directory_url {
            return Err(ActionRefusal::new(
                409,
                "ACME_TEST_SAME_DIRECTORY",
                "acme.staging_directory_url equals acme.directory_url: a test order would spend a production order",
            ));
        }
        let edge_node = (self.deps.edge)()
            .map_err(|reason| ActionRefusal::new(409, "EDGE_NOT_CONFIGURED", reason))?;
        let slot = self.try_begin(RunKind::Test)?;
        let now = self.now();
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let _slot = slot;
            service.test_run(settings, edge_node).await;
        });
        Ok(json!({
            "kind": "test",
            "name": self.config.name(),
            "directory_url": self.config_staging_url(),
            "started_at": iso(now),
            "progress": "GET /acme/status",
        }))
    }

    fn config_staging_url(&self) -> Option<String> {
        match &self.config {
            AcmeConfig::On(settings) => Some(settings.staging_directory_url.clone()),
            _ => None,
        }
    }

    async fn test_run(self: Arc<Self>, settings: AcmeSettings, edge_node: String) {
        let started_at = self.now();
        self.note_edge(&edge_node);
        let directory_url = settings.staging_directory_url.clone();
        let view = self.look(&edge_node).await;
        let run = Run {
            kind: RunKind::Test,
            settings,
            directory_url,
            edge_node,
            view: view.clone().unwrap_or_default(),
            replaces: None,
        };
        let end = match view {
            Ok(_) => self.order_flow(&run).await,
            Err(detail) => FlowEnd::Failed(Failure::new(
                "look",
                format!("the edge gave no status: {detail}"),
            )),
        };
        self.finish(&run, started_at, end);
    }

    /// `POST /acme/reload`: EDGE_TLS_RELOAD, after a manual change or a vault rollback.
    pub(crate) async fn reload_action(self: &Arc<Self>) -> Result<Value, ActionRefusal> {
        let edge_node = (self.deps.edge)()
            .map_err(|reason| ActionRefusal::new(409, "EDGE_NOT_CONFIGURED", reason))?;
        let _slot = self.try_begin(RunKind::Reload)?;
        self.stage("reload");
        self.note_edge(&edge_node);
        let pending = self.read(|state| state.pending_install.clone());
        if let Some(pending) = &pending {
            if pending.edge_node == edge_node {
                // A newer certificate SY.admin stored is the one this reload installs (or not).
                if let Some(view) = self.read(|state| state.edge.last_status.clone()) {
                    self.settle_pending(&view).await;
                }
            }
        }
        let pending = self.read(|state| state.pending_install.clone());
        let answer = self
            .deps
            .mesh
            .edge_reload(&edge_node, self.deps.timings.reload)
            .await;
        let answer = match answer {
            Ok(answer) => answer,
            Err(err) => {
                return Err(ActionRefusal::new(
                    504,
                    "EDGE_NO_ANSWER",
                    format!("{edge_node} did not answer EDGE_TLS_RELOAD: {err}"),
                ))
            }
        };
        // `status: "error"` means the edge could not run the command; a refusal is `ok`.
        let view = match EdgeView::from_answer(&answer) {
            Ok(view) if answer_ok(&answer) => view,
            Ok(_) | Err(_) => {
                let mut refusal = ActionRefusal::new(
                    502,
                    "EDGE_ERROR",
                    format!("{edge_node} could not reload: {}", answer_detail(&answer)),
                );
                refusal.payload = json!({ "edge_node": edge_node, "answer": answer });
                return Err(refusal);
            }
        };
        self.record_view(&view, self.now());
        if let Some(pending) = &pending {
            match &view.refused {
                // No verdict: the certificate SY.admin stored stays, and is reloaded later.
                Some(refused) if refused.transient() => {}
                Some(refused) => {
                    self.refused(
                        &pending.vault_key,
                        pending.written.as_ref(),
                        &pending.fingerprint,
                        &pending.serial,
                        &refused.to_string(),
                    )
                    .await;
                }
                None if view.fingerprint() == Some(pending.fingerprint.as_str()) => {
                    self.mark_installed(&pending.fingerprint);
                }
                None => {}
            }
        }
        self.evaluate_and_alert().await;
        let mut payload = json!({
            "edge_node": edge_node,
            "changed": view.changed,
            "refused": view.refused,
            "state": view.state,
            "served": view.leaf,
            "answer": answer,
        });
        humanize_times(&mut payload);
        match &view.refused {
            Some(refused) => {
                let (http_status, error_code) = if refused.transient() {
                    (503, "EDGE_VAULT_UNAVAILABLE")
                } else {
                    (422, "EDGE_REFUSED")
                };
                let mut refusal = ActionRefusal::new(
                    http_status,
                    error_code,
                    format!("{edge_node} kept its certificate: {refused}"),
                );
                refusal.payload = payload;
                Err(refusal)
            }
            None => Ok(payload),
        }
    }

    /// `GET /acme/status`.
    pub(crate) async fn status(&self) -> Value {
        let now = self.now();
        let state = self.read(Clone::clone);
        let (config_state, reason) = match (&self.config, lock(&self.off_reason).clone()) {
            (AcmeConfig::Absent { .. }, _) => ("absent", None),
            (AcmeConfig::Off { reason, .. }, _) => ("off", Some(reason.clone())),
            (AcmeConfig::On(_), Some(reason)) => ("off", Some(reason)),
            (AcmeConfig::On(_), None) => ("on", None),
        };
        let mut config = json!({
            "state": config_state,
            "reason": reason,
            "name": self.config.name(),
        });
        if let AcmeConfig::On(settings) = &self.config {
            config["directory_url"] = json!(settings.directory_url);
            config["staging_directory_url"] = json!(settings.staging_directory_url);
            config["profile"] = json!(settings.profile);
            config["contact"] = json!(settings.contact);
            config["trust_anchor"] = json!(settings
                .trust_anchor
                .as_ref()
                .map(|path| path.display().to_string()));
        }
        let directories: Vec<Value> = lock(&self.directories)
            .iter()
            .map(|(url, (at, info))| match info {
                Ok(info) => json!({
                    "url": url,
                    "fetched_at": at,
                    "terms_of_service": info.terms_of_service,
                    "profiles": info.profiles,
                    "renewal_info": info.renewal_info.is_some(),
                }),
                Err(err) => json!({ "url": url, "fetched_at": at, "error": err }),
            })
            .collect();
        let terms_url = match &self.config {
            AcmeConfig::On(settings) => lock(&self.directories)
                .get(&settings.directory_url)
                .and_then(|(_, info)| info.as_ref().ok())
                .and_then(|info| info.terms_of_service.clone()),
            _ => None,
        };
        // The edge as it resolves now (an ingress may have joined since the last check), asked
        // briefly: a status read must not hang on an edge that is down.
        let resolved = (self.deps.edge)();
        let edge_node = resolved.as_ref().ok().cloned();
        let live_limit = self.deps.timings.edge_call.min(Duration::from_secs(5));
        let live = match &edge_node {
            Some(edge) => match self.deps.mesh.edge_status(edge, live_limit).await {
                Ok(answer) if answer_ok(&answer) => match EdgeView::from_answer(&answer) {
                    Ok(view) => json!(view),
                    Err(detail) => json!({ "error": detail }),
                },
                Ok(answer) => json!({ "error": answer_detail(&answer) }),
                Err(err) => json!({ "error": err.to_string() }),
            },
            None => Value::Null,
        };
        let served = state
            .edge
            .last_status
            .as_ref()
            .and_then(|view| view.leaf.as_ref());
        let name_covered = match (served, self.config.name()) {
            (Some(leaf), Some(name)) => Some(tls_material::name_covered(&leaf.names, name)),
            _ => None,
        };
        let issued_by_this_account = match (served, &self.config) {
            (Some(leaf), AcmeConfig::On(settings)) => Some(
                issued_by_account(&state, &settings.directory_url, &leaf.fingerprint).is_some(),
            ),
            _ => None,
        };
        let conditions: Vec<Value> = state
            .conditions
            .iter()
            .map(|(kind, record)| {
                json!({
                    "kind": kind,
                    "severity": record.severity,
                    "hive": record.hive,
                    "detail": record.current,
                    "opened_at": record.opened_at,
                })
            })
            .collect();
        let accounts: Vec<Value> = state
            .accounts
            .iter()
            .map(|(directory, account)| {
                json!({
                    "directory_url": directory,
                    "account_url": account.account_url,
                    "vault_key": account.vault_key,
                    "recorded_at": account.recorded_at,
                })
            })
            .collect();
        let in_flight = lock(&self.slot).clone();
        let mut status = json!({
            "now_at": now,
            "config": config,
            "terms_url": terms_url,
            "directories": directories,
            "accounts": accounts,
            "edge": {
                "node": edge_node,
                "not_configured": resolved.err(),
                "live": live,
                "last_seen": state.edge.last_status,
                "last_answer_at": state.edge.last_answer_at,
                "unreachable_since_at": state.edge.unreachable_since_at,
                "last_error": state.edge.last_error,
                "config_error": state.edge.config_error,
                "name_covered": name_covered,
                "issued_by_this_account": issued_by_this_account,
            },
            "in_flight": in_flight,
            "order": state.order,
            "pending_install": state.pending_install,
            "install_refused": state.install_refused,
            "rollback_pending": state.rollback_pending,
            "last_check": state.last_check,
            "last_attempt": state.last_attempt,
            "last_test": state.last_test,
            "backoff": state.backoff,
            "next_check_at": state.next_check_at,
            "renewal": state.renewal,
            "issued": state.issued,
            "conditions": conditions,
            "warnings": self.state_warning.iter().collect::<Vec<_>>(),
        });
        humanize_times(&mut status);
        status
    }
}

/// What step 5 found.
struct MaterialReport {
    leaf: LeafInfo,
    trusted: bool,
    trust_detail: Option<String>,
}

/// The public facts of an issued chain, for `acme.json`; none when the CA sent no parsable leaf.
fn issued_record(chain_pem: &str, run: &Run, account_url: &str, now: i64) -> Option<IssuedRecord> {
    let (chain, _) = tls_material::parse_chain(chain_pem.as_bytes()).ok()?;
    let leaf = tls_material::leaf_info(chain.first()?.as_ref(), chain.len()).ok()?;
    Some(IssuedRecord {
        serial: hex::encode(&leaf.serial),
        fingerprint: leaf.fingerprint,
        authority_key_id: leaf.authority_key_id.as_deref().map(hex::encode),
        not_before: leaf.not_before,
        not_after: leaf.not_after,
        name: run.settings.name.clone(),
        profile: run.settings.profile.clone(),
        directory_url: run.directory_url.clone(),
        account_url: account_url.to_string(),
        issued_at: now,
        stored: false,
        installed: false,
        installed_at: None,
    })
}

// ---------------------------------------------------------------------------------------------
// The real CA: instant-acme 0.8.5
// ---------------------------------------------------------------------------------------------

/// The CA over instant-acme, and plain GETs of the directory and of ARI. Every connection to the
/// CA trusts the same roots: the system's (`ca-certificates`), plus `acme.trust_anchor` when it
/// is set (a private CA).
pub(crate) struct InstantAcmeCa {
    http: Result<reqwest::Client, String>,
    tls: Result<rustls::ClientConfig, String>,
}

impl InstantAcmeCa {
    /// `platform` loads the system's roots ([`platform_roots`] in production).
    pub(crate) fn new(
        trust_anchor: &[CertificateDer<'static>],
        platform: impl Fn() -> Result<Vec<CertificateDer<'static>>, String>,
    ) -> Self {
        let roots = ca_roots(trust_anchor, platform);
        let http = roots.clone().and_then(|roots| {
            let mut builder = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .tls_built_in_root_certs(false);
            for root in &roots {
                let root = reqwest::Certificate::from_der(root.as_ref())
                    .map_err(|err| format!("a root certificate: {err}"))?;
                builder = builder.add_root_certificate(root);
            }
            builder
                .build()
                .map_err(|err| format!("the HTTP client could not be built: {err}"))
        });
        let tls = roots.and_then(|roots| {
            install_ring_provider();
            let mut store = rustls::RootCertStore::empty();
            store.add_parsable_certificates(roots);
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map(|builder| builder.with_root_certificates(store).with_no_client_auth())
            .map_err(|err| format!("the TLS configuration: {err}"))
        });
        Self { http, tls }
    }

    fn http(&self) -> Result<&reqwest::Client, CaError> {
        self.http
            .as_ref()
            .map_err(|err| CaError::Transport(err.clone()))
    }

    /// An instant-acme account builder whose HTTPS client trusts [`ca_roots`].
    fn account_builder(&self) -> Result<instant_acme::AccountBuilder, CaError> {
        let config = self
            .tls
            .as_ref()
            .map_err(|err| CaError::Transport(err.clone()))?
            .clone();
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(config)
            .https_only()
            .enable_http1()
            .enable_http2()
            .build();
        let client: hyper_util::client::legacy::Client<
            _,
            instant_acme::BodyWrapper<hyper::body::Bytes>,
        > = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build(connector);
        Ok(instant_acme::Account::builder_with_http(Box::new(client)))
    }
}

/// The roots a connection to the CA trusts: the platform's, plus `acme.trust_anchor`. Only the
/// certificates rustls can use as roots are kept.
fn ca_roots(
    trust_anchor: &[CertificateDer<'static>],
    platform: impl Fn() -> Result<Vec<CertificateDer<'static>>, String>,
) -> Result<Vec<CertificateDer<'static>>, String> {
    let mut roots = match platform() {
        Ok(roots) => roots,
        Err(err) if !trust_anchor.is_empty() => {
            tracing::warn!(error = %err, "the CA's TLS is checked against acme.trust_anchor only");
            Vec::new()
        }
        Err(err) => return Err(err),
    };
    roots.extend(trust_anchor.iter().cloned());
    let usable: Vec<CertificateDer<'static>> = roots
        .into_iter()
        .filter(|root| rustls::RootCertStore::empty().add(root.clone()).is_ok())
        .collect();
    if usable.is_empty() {
        return Err("no usable root certificate for the CA's TLS".to_string());
    }
    Ok(usable)
}

fn install_ring_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// An instant-acme error, sorted.
fn classify(err: instant_acme::Error) -> CaError {
    match err {
        instant_acme::Error::Api(problem) => {
            let kind = problem.r#type.clone().unwrap_or_default();
            if kind.ends_with(":rateLimited") {
                CaError::RateLimited(problem.to_string())
            } else {
                CaError::Problem(problem.to_string())
            }
        }
        instant_acme::Error::Timeout(_) => {
            CaError::Timeout("the CA did not update the order in time".to_string())
        }
        other => CaError::Transport(other.to_string()),
    }
}

/// An order error when the order named `replaces`: anything that points to it is
/// [`CaError::Replaces`], so the order is retried without it.
fn classify_order(err: instant_acme::Error, sent_replaces: bool) -> CaError {
    let error = classify(err);
    if !sent_replaces || matches!(error, CaError::RateLimited(_)) {
        return error;
    }
    let text = error.to_string().to_ascii_lowercase();
    if text.contains("replace") || text.contains("renewal information") {
        CaError::Replaces(error.to_string())
    } else {
        error
    }
}

fn cert_identifier(cert: &CertRef) -> instant_acme::CertificateIdentifier<'static> {
    instant_acme::CertificateIdentifier::new(
        rustls::pki_types::Der::from_slice(&cert.authority_key_id),
        rustls::pki_types::Der::from_slice(&cert.serial),
    )
    .into_owned()
}

/// `Retry-After`: seconds, or an HTTP date.
fn retry_after_secs(value: &str, now: i64) -> Option<i64> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<i64>() {
        return Some(secs.max(0));
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|at| (at.timestamp() - now).max(0))
}

#[async_trait]
impl AcmeCa for InstantAcmeCa {
    async fn directory(&self, url: &str) -> Result<DirectoryInfo, CaError> {
        let response = self
            .http()?
            .get(url)
            .send()
            .await
            .map_err(|err| CaError::Transport(err.to_string()))?;
        if !response.status().is_success() {
            return Err(CaError::Problem(format!(
                "GET {url} answered {}",
                response.status()
            )));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|err| CaError::Problem(format!("the directory {url} is not JSON: {err}")))?;
        Ok(DirectoryInfo {
            terms_of_service: body
                .pointer("/meta/termsOfService")
                .and_then(Value::as_str)
                .map(str::to_string),
            profiles: body
                .pointer("/meta/profiles")
                .and_then(Value::as_object)
                .map(|profiles| profiles.keys().cloned().collect())
                .unwrap_or_default(),
            renewal_info: body
                .get("renewalInfo")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn renewal_info(
        &self,
        renewal_info_url: &str,
        cert: &CertRef,
    ) -> Result<AriAnswer, CaError> {
        let url = format!(
            "{}/{}",
            renewal_info_url.trim_end_matches('/'),
            cert_identifier(cert)
        );
        let response = self
            .http()?
            .get(&url)
            .send()
            .await
            .map_err(|err| CaError::Transport(err.to_string()))?;
        if !response.status().is_success() {
            return Err(CaError::Problem(format!(
                "ARI answered {}",
                response.status()
            )));
        }
        let now = chrono::Utc::now().timestamp();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| retry_after_secs(value, now));
        let body: Value = response
            .json()
            .await
            .map_err(|err| CaError::Problem(format!("ARI is not JSON: {err}")))?;
        let time = |pointer: &str| {
            body.pointer(pointer)
                .and_then(Value::as_str)
                .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                .map(|at| at.timestamp())
        };
        let (Some(start), Some(end)) =
            (time("/suggestedWindow/start"), time("/suggestedWindow/end"))
        else {
            return Err(CaError::Problem("ARI gave no suggested window".to_string()));
        };
        Ok(AriAnswer {
            start,
            end,
            explanation_url: body
                .get("explanationURL")
                .and_then(Value::as_str)
                .map(str::to_string),
            retry_after_secs: retry_after,
        })
    }

    async fn create_account(
        &self,
        directory_url: &str,
        contact: Option<&str>,
    ) -> Result<(Box<dyn CaAccount>, Value), CaError> {
        install_ring_provider();
        let contacts: Vec<&str> = contact.into_iter().collect();
        let new_account = instant_acme::NewAccount {
            contact: &contacts,
            terms_of_service_agreed: true,
            only_return_existing: false,
        };
        let (account, credentials) = self
            .account_builder()?
            .create(&new_account, directory_url.to_string(), None)
            .await
            .map_err(classify)?;
        let credentials = serde_json::to_value(&credentials)
            .map_err(|err| CaError::Problem(format!("the account credentials: {err}")))?;
        Ok((Box::new(InstantAcmeAccount { account }), credentials))
    }

    async fn load_account(&self, credentials: &Value) -> Result<Box<dyn CaAccount>, CaError> {
        install_ring_provider();
        let credentials: instant_acme::AccountCredentials =
            serde_json::from_value(credentials.clone()).map_err(|err| {
                CaError::Problem(format!(
                    "the stored account credentials do not parse: {err}"
                ))
            })?;
        let account = self
            .account_builder()?
            .from_credentials(credentials)
            .await
            .map_err(classify)?;
        Ok(Box::new(InstantAcmeAccount { account }))
    }
}

struct InstantAcmeAccount {
    account: instant_acme::Account,
}

#[async_trait]
impl CaAccount for InstantAcmeAccount {
    fn url(&self) -> String {
        self.account.id().to_string()
    }

    fn profiles(&self) -> Vec<String> {
        self.account
            .profiles()
            .map(|profile| profile.name.to_string())
            .collect()
    }

    async fn new_order(
        &self,
        name: &str,
        profile: Option<&str>,
        replaces: Option<&CertRef>,
    ) -> Result<Box<dyn CaOrder>, CaError> {
        let identifiers = [instant_acme::Identifier::Dns(name.to_string())];
        let mut request = instant_acme::NewOrder::new(&identifiers);
        if let Some(profile) = profile {
            request = request.profile(profile);
        }
        if let Some(cert) = replaces {
            request = request.replaces(cert_identifier(cert));
        }
        let order = self
            .account
            .new_order(&request)
            .await
            .map_err(|err| classify_order(err, replaces.is_some()))?;
        Ok(Box::new(InstantAcmeOrder { order }))
    }

    async fn resume_order(&self, url: &str) -> Result<Box<dyn CaOrder>, CaError> {
        let order = self
            .account
            .order(url.to_string())
            .await
            .map_err(classify)?;
        Ok(Box::new(InstantAcmeOrder { order }))
    }
}

struct InstantAcmeOrder {
    order: instant_acme::Order,
}

fn order_status(status: instant_acme::OrderStatus) -> OrderStatus {
    match status {
        instant_acme::OrderStatus::Pending => OrderStatus::Pending,
        instant_acme::OrderStatus::Ready => OrderStatus::Ready,
        instant_acme::OrderStatus::Processing => OrderStatus::Processing,
        instant_acme::OrderStatus::Valid => OrderStatus::Valid,
        instant_acme::OrderStatus::Invalid => OrderStatus::Invalid,
    }
}

fn retry_policy(limit: Duration) -> instant_acme::RetryPolicy {
    instant_acme::RetryPolicy::new()
        .initial_delay(Duration::from_secs(1))
        .backoff(1.5)
        .timeout(limit)
}

#[async_trait]
impl CaOrder for InstantAcmeOrder {
    fn url(&self) -> String {
        self.order.url().to_string()
    }

    fn status(&mut self) -> OrderStatus {
        order_status(self.order.state().status)
    }

    async fn authorization(&mut self) -> Result<AuthzState, CaError> {
        let mut authorizations = self.order.authorizations();
        let mut valid = false;
        while let Some(next) = authorizations.next().await {
            let mut authz = next.map_err(classify)?;
            match authz.status {
                instant_acme::AuthorizationStatus::Valid => valid = true,
                instant_acme::AuthorizationStatus::Pending => {
                    let name = authz.identifier().to_string();
                    let challenge = authz
                        .challenge(instant_acme::ChallengeType::TlsAlpn01)
                        .ok_or_else(|| {
                            CaError::Problem("the CA offers no tls-alpn-01 challenge".to_string())
                        })?;
                    let digest = hex::encode(challenge.key_authorization().digest());
                    return Ok(AuthzState::Pending { name, digest });
                }
                other => return Ok(AuthzState::Other(format!("{other:?}").to_lowercase())),
            }
        }
        if valid {
            Ok(AuthzState::Valid)
        } else {
            Err(CaError::Problem(
                "the order has no authorization".to_string(),
            ))
        }
    }

    async fn set_challenge_ready(&mut self) -> Result<(), CaError> {
        let mut authorizations = self.order.authorizations();
        while let Some(next) = authorizations.next().await {
            let mut authz = next.map_err(classify)?;
            if authz.status != instant_acme::AuthorizationStatus::Pending {
                continue;
            }
            let mut challenge = authz
                .challenge(instant_acme::ChallengeType::TlsAlpn01)
                .ok_or_else(|| {
                    CaError::Problem("the CA offers no tls-alpn-01 challenge".to_string())
                })?;
            return challenge.set_ready().await.map_err(classify);
        }
        Err(CaError::Problem(
            "the order has no pending authorization".to_string(),
        ))
    }

    async fn poll_ready(&mut self, limit: Duration) -> Result<OrderStatus, CaError> {
        self.order
            .poll_ready(&retry_policy(limit))
            .await
            .map(order_status)
            .map_err(classify)
    }

    async fn challenge_error(&mut self) -> Option<String> {
        let mut authorizations = self.order.authorizations();
        while let Some(next) = authorizations.next().await {
            let Ok(mut authz) = next else {
                continue;
            };
            if authz.refresh().await.is_err() {
                continue;
            }
            for challenge in &authz.challenges {
                if let Some(error) = &challenge.error {
                    return Some(error.to_string());
                }
            }
        }
        None
    }

    async fn deactivate_pending(&mut self) -> Result<(), CaError> {
        let mut authorizations = self.order.authorizations();
        while let Some(next) = authorizations.next().await {
            let mut authz = next.map_err(classify)?;
            if authz.status == instant_acme::AuthorizationStatus::Pending {
                authz.deactivate().await.map_err(classify)?;
            }
        }
        Ok(())
    }

    async fn finalize(&mut self, csr_der: &[u8]) -> Result<(), CaError> {
        self.order.finalize_csr(csr_der).await.map_err(classify)
    }

    async fn certificate(&mut self, limit: Duration) -> Result<String, CaError> {
        self.order
            .poll_certificate(&retry_policy(limit))
            .await
            .map_err(classify)
    }
}

// ---------------------------------------------------------------------------------------------
// The real mesh
// ---------------------------------------------------------------------------------------------

/// The edge, SY.vault and SY.orchestrator over the router, as SY.admin talks to them elsewhere.
pub(crate) struct RouterMesh {
    client: Arc<RouterDispatcher>,
    hive_id: String,
    self_ilk_id: String,
}

impl RouterMesh {
    pub(crate) fn new(client: Arc<RouterDispatcher>, hive_id: String, self_ilk_id: String) -> Self {
        Self {
            client,
            hive_id,
            self_ilk_id,
        }
    }

    async fn edge(
        &self,
        edge: &str,
        msg: &str,
        response: &str,
        payload: Value,
        limit: Duration,
    ) -> Result<Value, MeshError> {
        super::send_system_request_with_meta(
            &self.client,
            edge,
            msg,
            response,
            payload,
            16,
            None,
            None,
            Some(edge.to_string()),
            None,
            None,
            limit,
        )
        .await
        .map_err(|err| MeshError::NoAnswer(err.to_string()))
    }

    async fn vault(
        &self,
        action: &str,
        msg: &str,
        response: &str,
        payload: Value,
        limit: Duration,
    ) -> Result<Value, MeshError> {
        let target = format!("SY.vault@{}", self.hive_id);
        super::send_system_request_with_meta(
            &self.client,
            &target,
            msg,
            response,
            payload,
            16,
            Some(self.self_ilk_id.clone()),
            None,
            Some(target.clone()),
            Some(action.to_string()),
            None,
            limit,
        )
        .await
        .map_err(|err| MeshError::NoAnswer(err.to_string()))
    }
}

fn vault_refusal(answer: &Value) -> MeshError {
    MeshError::Refused {
        code: answer
            .get("error_code")
            .and_then(Value::as_str)
            .unwrap_or("ERROR")
            .to_string(),
        detail: answer_detail(answer),
    }
}

fn vault_not_found(answer: &Value) -> bool {
    answer.get("error_code").and_then(Value::as_str) == Some("KEY_NOT_FOUND")
}

#[async_trait]
impl AcmeMesh for RouterMesh {
    async fn edge_status(&self, edge: &str, limit: Duration) -> Result<Value, MeshError> {
        self.edge(
            edge,
            MSG_EDGE_TLS_STATUS,
            MSG_EDGE_TLS_STATUS_RESPONSE,
            json!({}),
            limit,
        )
        .await
    }

    async fn edge_reload(&self, edge: &str, limit: Duration) -> Result<Value, MeshError> {
        self.edge(
            edge,
            MSG_EDGE_TLS_RELOAD,
            MSG_EDGE_TLS_RELOAD_RESPONSE,
            json!({}),
            limit,
        )
        .await
    }

    async fn edge_challenge(
        &self,
        edge: &str,
        request: Value,
        limit: Duration,
    ) -> Result<Value, MeshError> {
        self.edge(
            edge,
            MSG_EDGE_ACME_CHALLENGE,
            MSG_EDGE_ACME_CHALLENGE_RESPONSE,
            request,
            limit,
        )
        .await
    }

    async fn vault_metadata(
        &self,
        key: &str,
        limit: Duration,
    ) -> Result<Option<SecretMetadata>, MeshError> {
        let answer = self
            .vault(
                "vault_get_metadata",
                MSG_VAULT_GET_METADATA,
                MSG_VAULT_GET_METADATA_RESPONSE,
                json!({ "key": key }),
                limit,
            )
            .await?;
        if vault_not_found(&answer) {
            return Ok(None);
        }
        if !answer_ok(&answer) {
            return Err(vault_refusal(&answer));
        }
        let metadata = &answer["metadata"];
        let text = |key: &str| {
            metadata
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        Ok(Some(SecretMetadata {
            resource_type: text("resource_type"),
            tenant_id: text("tenant_id"),
            owner_ilk: text("ilk"),
            version: answer.get("version").and_then(Value::as_i64),
            updated_at: text("updated_at"),
            created_by: text("created_by"),
        }))
    }

    async fn vault_value(&self, key: &str, limit: Duration) -> Result<Option<Value>, MeshError> {
        let answer = self
            .vault(
                "vault_get",
                MSG_VAULT_GET,
                MSG_VAULT_GET_RESPONSE,
                json!({ "key": key }),
                limit,
            )
            .await?;
        if vault_not_found(&answer) {
            return Ok(None);
        }
        if !answer_ok(&answer) {
            return Err(vault_refusal(&answer));
        }
        Ok(answer.get("value").cloned())
    }

    async fn vault_put(
        &self,
        key: &str,
        value: Value,
        metadata: Value,
        limit: Duration,
    ) -> Result<Option<i64>, MeshError> {
        let answer = self
            .vault(
                "vault_put",
                MSG_VAULT_PUT,
                MSG_VAULT_PUT_RESPONSE,
                json!({ "key": key, "value": value, "metadata": metadata }),
                limit,
            )
            .await?;
        if !answer_ok(&answer) {
            return Err(vault_refusal(&answer));
        }
        Ok(answer.get("version").and_then(Value::as_i64))
    }

    async fn vault_rollback(&self, key: &str, limit: Duration) -> Result<(), MeshError> {
        let answer = self
            .vault(
                "vault_rollback",
                MSG_VAULT_ROLLBACK,
                MSG_VAULT_ROLLBACK_RESPONSE,
                json!({ "key": key, "tenant_id": DEFAULT_ROOT_TENANT_ID }),
                limit,
            )
            .await?;
        if answer_ok(&answer) {
            Ok(())
        } else {
            Err(vault_refusal(&answer))
        }
    }

    async fn drift_alert(&self, alert: Value, limit: Duration) -> Result<bool, MeshError> {
        let request = super::AdminRequest {
            action: "record_drift_alert".to_string(),
            payload: alert,
            target: format!("SY.orchestrator@{}", self.hive_id),
            unicast: None,
        };
        let answer = super::send_admin_request(
            &self.client,
            request,
            limit,
            &super::admin_node_name(&self.hive_id),
        )
        .await
        .map_err(|err| MeshError::NoAnswer(err.to_string()))?;
        if super::payload_is_ok(&answer) {
            Ok(answer
                .get("recorded")
                .and_then(Value::as_bool)
                .unwrap_or(true))
        } else {
            Err(vault_refusal(&answer))
        }
    }
}

#[cfg(test)]
#[path = "acme_tests.rs"]
pub(crate) mod tests;
