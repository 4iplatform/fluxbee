use std::ffi::CString;
use std::fs;
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{self, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant as StdInstant};

use memmap2::{Mmap, MmapOptions};
use nix::fcntl::OFlag;
use nix::sys::mman::shm_open;
use nix::sys::stat::fstat;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{Duration, Instant as TokioInstant};
use uuid::Uuid;

use crate::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use crate::{NodeError, NodeReceiver, NodeSender};

pub const MSG_ILK_PROVISION: &str = "ILK_PROVISION";
pub const MSG_ILK_PROVISION_RESPONSE: &str = "ILK_PROVISION_RESPONSE";
pub const MSG_ILK_REGISTER: &str = "ILK_REGISTER";
pub const MSG_ILK_ADD_CHANNEL: &str = "ILK_ADD_CHANNEL";
pub const MSG_ILK_UPDATE: &str = "ILK_UPDATE";
pub const MSG_TNT_CREATE: &str = "TNT_CREATE";
pub const MSG_TNT_APPROVE: &str = "TNT_APPROVE";
pub const MSG_IDENTITY_METRICS: &str = "IDENTITY_METRICS";
const IDENTITY_MAGIC: u32 = 0x4A534944; // "JSID"
const IDENTITY_VERSION: u32 = 2;
const REGION_ALIGNMENT: usize = 64;
const ICH_CHANNEL_TYPE_MAX_LEN: usize = 32;
const ICH_ADDRESS_MAX_LEN: usize = 256;
const ICH_MAP_FLAG_OCCUPIED: u16 = 0x0001;
const ICH_MAP_FLAG_TOMBSTONE: u16 = 0x0002;
const FLAG_ACTIVE: u16 = 0x0001;
const SEQLOCK_READ_TIMEOUT_MS: u64 = 5;
const SHM_ILK_TYPE_HUMAN: u8 = 0;
const SHM_ILK_TYPE_AGENT: u8 = 1;
const SHM_ILK_TYPE_SYSTEM: u8 = 2;
const SHM_REG_STATUS_TEMPORARY: u8 = 0;
const SHM_REG_STATUS_PARTIAL: u8 = 1;
const SHM_REG_STATUS_COMPLETE: u8 = 2;

#[derive(Debug, Clone)]
pub struct IlkProvisionRequest<'a> {
    pub target: &'a str,
    pub ich_id: &'a str,
    pub channel_type: &'a str,
    pub address: &'a str,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct IlkProvisionResult {
    pub ilk_id: String,
    pub registration_status: String,
    pub trace_id: String,
}

#[derive(Debug, Clone)]
pub struct IdentitySystemRequest<'a> {
    pub target: &'a str,
    pub fallback_target: Option<&'a str>,
    pub action: &'a str,
    pub payload: Value,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct IdentitySystemResult {
    pub payload: Value,
    pub effective_target: String,
    pub trace_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct IdentityIlkOption {
    pub ilk_id: String,
    pub display_name: Option<String>,
    pub handler_node: Option<String>,
    pub registration_status: String,
    pub ilk_type: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct IdentityIchOption {
    pub ich_id: String,
    pub channel_type: String,
    pub address: String,
    pub is_primary: bool,
    pub ilks: Vec<IdentityIlkOption>,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("node error: {0}")]
    Node(#[from] NodeError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("identity transport unreachable: reason={reason}, original_dst={original_dst}")]
    Unreachable {
        reason: String,
        original_dst: String,
    },
    #[error("identity transport ttl exceeded: original_dst={original_dst}, last_hop={last_hop}")]
    TtlExceeded {
        original_dst: String,
        last_hop: String,
    },
    #[error("ILK_PROVISION rejected: error_code={error_code}, message={message}")]
    ProvisionRejected { error_code: String, message: String },
    #[error(
        "identity action rejected: action={action}, error_code={error_code}, message={message}"
    )]
    SystemRejected {
        action: String,
        error_code: String,
        message: String,
    },
    #[error("timeout waiting ILK_PROVISION_RESPONSE trace_id={trace_id} target={target} timeout_ms={timeout_ms}")]
    Timeout {
        trace_id: String,
        target: String,
        timeout_ms: u64,
    },
    #[error("timeout waiting identity response: action={action} trace_id={trace_id} target={target} timeout_ms={timeout_ms}")]
    ActionTimeout {
        action: String,
        trace_id: String,
        target: String,
        timeout_ms: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityShmError {
    #[error("invalid channel input: channel_type/address must be non-empty")]
    InvalidChannelInput,
    #[error("invalid hive_id")]
    InvalidHiveId,
    #[error("invalid identity SHM name")]
    InvalidShmName,
    #[error("invalid identity SHM header/layout")]
    InvalidShmHeader,
    #[error("identity SHM seqlock read timeout")]
    SeqLockTimeout,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("nix error: {0}")]
    Nix(#[from] nix::Error),
}

#[derive(Debug, Deserialize)]
struct IlkProvisionResponsePayload {
    status: String,
    #[serde(default)]
    ilk_id: Option<String>,
    #[serde(default)]
    registration_status: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct UnreachablePayload {
    #[serde(default)]
    original_dst: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Deserialize, Default)]
struct TtlExceededPayload {
    #[serde(default)]
    original_dst: String,
    #[serde(default)]
    last_hop: String,
}

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
}

#[derive(Clone, Copy, Debug)]
struct IdentityRegionLimits {
    max_ilks: u32,
    max_tenants: u32,
    max_vocabulary: u32,
    max_ilk_aliases: u32,
}

impl IdentityRegionLimits {
    fn max_ichs(self) -> u32 {
        self.max_ilks.saturating_mul(4)
    }

    fn max_ich_mappings(self) -> u32 {
        self.max_ichs().saturating_mul(2)
    }
}

#[derive(Clone, Copy)]
struct IdentityRegionLayout {
    header_offset: usize,
    tenant_offset: usize,
    ilk_offset: usize,
    ich_offset: usize,
    ich_mapping_offset: usize,
    ilk_alias_offset: usize,
    vocabulary_offset: usize,
    variable_offset: usize,
    total_len: usize,
    limits: IdentityRegionLimits,
}

#[repr(C)]
struct IdentityHeader {
    magic: u32,
    version: u32,
    seq: AtomicU64,
    tenant_count: u32,
    ilk_count: u32,
    ich_count: u32,
    ich_mapping_count: u32,
    vocabulary_count: u32,
    ilk_alias_count: u32,
    max_ilks: u32,
    max_tenants: u32,
    max_ichs: u32,
    max_ich_mappings: u32,
    max_vocabulary: u32,
    max_ilk_aliases: u32,
    updated_at: u64,
    heartbeat: u64,
    owner_uuid: [u8; 16],
    owner_pid: u32,
    is_primary: u8,
    _pad0: [u8; 3],
    hive_id: [u8; 64],
    hive_id_len: u16,
    _reserved: [u8; 6],
}

#[repr(C)]
struct TenantEntry {
    tenant_id: [u8; 16],
    name: [u8; 128],
    domain: [u8; 128],
    status: u8,
    flags: u16,
    _pad0: [u8; 5],
    max_ilks: u32,
    created_at: u64,
    updated_at: u64,
    _reserved: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IlkEntry {
    ilk_id: [u8; 16],
    ilk_type: u8,
    registration_status: u8,
    flags: u16,
    tenant_id: [u8; 16],
    display_name: [u8; 128],
    handler_node: [u8; 128],
    ich_offset: u32,
    ich_count: u16,
    _pad0: [u8; 2],
    roles_offset: u32,
    roles_len: u16,
    _pad1: [u8; 2],
    capabilities_offset: u32,
    capabilities_len: u16,
    _pad2: [u8; 2],
    created_at: u64,
    updated_at: u64,
    _reserved: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IchEntry {
    ich_id: [u8; 16],
    ilk_id: [u8; 16],
    channel_type: [u8; ICH_CHANNEL_TYPE_MAX_LEN],
    address: [u8; ICH_ADDRESS_MAX_LEN],
    flags: u16,
    is_primary: u8,
    _pad0: [u8; 5],
    added_at: u64,
    _reserved: [u8; 16],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IchMappingEntry {
    hash: u64,
    channel_type: [u8; ICH_CHANNEL_TYPE_MAX_LEN],
    address: [u8; ICH_ADDRESS_MAX_LEN],
    ich_id: [u8; 16],
    ilk_id: [u8; 16],
    flags: u16,
    _reserved: [u8; 54],
}

#[repr(C)]
struct IlkAliasEntry {
    old_ilk_id: [u8; 16],
    canonical_ilk_id: [u8; 16],
    expires_at: u64,
    flags: u16,
    _reserved: [u8; 22],
}

#[repr(C)]
struct VocabularyEntry {
    tag: [u8; 64],
    tag_len: u16,
    category: u8,
    flags: u8,
    description: [u8; 128],
    description_len: u16,
    _pad0: [u8; 6],
    created_at: u64,
    deprecated_at: u64,
    _reserved: [u8; 8],
}

/// Sends any identity action to `SY.identity@<hive>` and waits for
/// `<ACTION>_RESPONSE` with matching `trace_id`.
///
/// If `fallback_target` is set, it retries on:
/// - transport `UNREACHABLE` with reason `NODE_NOT_FOUND`
/// - payload `status=error` and `error_code=NOT_PRIMARY`
pub async fn identity_system_call(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    request: IdentitySystemRequest<'_>,
) -> Result<IdentitySystemResult, IdentityError> {
    let target = request.target.trim();
    let action = request.action.trim();
    if target.is_empty() {
        return Err(IdentityError::InvalidRequest(
            "target must be non-empty".to_string(),
        ));
    }
    if action.is_empty() {
        return Err(IdentityError::InvalidRequest(
            "action must be non-empty".to_string(),
        ));
    }
    let timeout = default_timeout(request.timeout);
    let first = send_action_once(
        sender,
        receiver,
        target,
        action,
        request.payload.clone(),
        timeout,
    )
    .await;
    match first {
        Ok((payload, trace_id)) => {
            let Some(fallback_target) = request.fallback_target.map(str::trim) else {
                return Ok(IdentitySystemResult {
                    payload,
                    effective_target: target.to_string(),
                    trace_id,
                });
            };
            if fallback_target.is_empty() || fallback_target == target {
                return Ok(IdentitySystemResult {
                    payload,
                    effective_target: target.to_string(),
                    trace_id,
                });
            }
            let (status, error_code) = payload_status_and_code(&payload);
            if status == Some("error") && error_code == Some("NOT_PRIMARY") {
                let (fallback_payload, fallback_trace_id) = send_action_once(
                    sender,
                    receiver,
                    fallback_target,
                    action,
                    request.payload,
                    timeout,
                )
                .await?;
                return Ok(IdentitySystemResult {
                    payload: fallback_payload,
                    effective_target: fallback_target.to_string(),
                    trace_id: fallback_trace_id,
                });
            }
            Ok(IdentitySystemResult {
                payload,
                effective_target: target.to_string(),
                trace_id,
            })
        }
        Err(IdentityError::Unreachable {
            reason,
            original_dst,
        }) => {
            let Some(fallback_target) = request.fallback_target.map(str::trim) else {
                return Err(IdentityError::Unreachable {
                    reason,
                    original_dst,
                });
            };
            if fallback_target.is_empty() || fallback_target == target || reason != "NODE_NOT_FOUND"
            {
                return Err(IdentityError::Unreachable {
                    reason,
                    original_dst,
                });
            }
            let (fallback_payload, fallback_trace_id) = send_action_once(
                sender,
                receiver,
                fallback_target,
                action,
                request.payload,
                timeout,
            )
            .await?;
            Ok(IdentitySystemResult {
                payload: fallback_payload,
                effective_target: fallback_target.to_string(),
                trace_id: fallback_trace_id,
            })
        }
        Err(err) => Err(err),
    }
}

/// Like [`identity_system_call`], but enforces payload `status="ok"`.
pub async fn identity_system_call_ok(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    request: IdentitySystemRequest<'_>,
) -> Result<IdentitySystemResult, IdentityError> {
    let action = request.action.to_string();
    let out = identity_system_call(sender, receiver, request).await?;
    let (status, error_code) = payload_status_and_code(&out.payload);
    if status == Some("ok") {
        return Ok(out);
    }
    Err(IdentityError::SystemRejected {
        action,
        error_code: error_code.unwrap_or("UNKNOWN").to_string(),
        message: payload_message(&out.payload)
            .unwrap_or("identity returned non-ok status")
            .to_string(),
    })
}

/// Sends `ILK_PROVISION` to `SY.identity@<hive>` and waits for
/// `ILK_PROVISION_RESPONSE` with matching `trace_id`.
pub async fn provision_ilk(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    request: IlkProvisionRequest<'_>,
) -> Result<IlkProvisionResult, IdentityError> {
    let target = request.target.trim();
    let ich_id = request.ich_id.trim();
    let channel_type = request.channel_type.trim();
    let address = request.address.trim();
    if ich_id.is_empty() || channel_type.is_empty() || address.is_empty() {
        return Err(IdentityError::InvalidRequest(
            "ich_id, channel_type and address must be non-empty".to_string(),
        ));
    }
    let call = identity_system_call(
        sender,
        receiver,
        IdentitySystemRequest {
            target,
            fallback_target: None,
            action: MSG_ILK_PROVISION,
            payload: json!({
                "ich_id": ich_id,
                "channel_type": channel_type,
                "address": address,
            }),
            timeout: request.timeout,
        },
    )
    .await?;
    let payload: IlkProvisionResponsePayload = serde_json::from_value(call.payload)?;
    parse_provision_payload(payload, call.trace_id)
}

fn default_timeout(timeout: Duration) -> Duration {
    if timeout.is_zero() {
        Duration::from_secs(5)
    } else {
        timeout
    }
}

fn response_action_for(action: &str) -> String {
    format!("{action}_RESPONSE")
}

fn payload_status_and_code(payload: &Value) -> (Option<&str>, Option<&str>) {
    (
        payload.get("status").and_then(Value::as_str),
        payload.get("error_code").and_then(Value::as_str),
    )
}

fn payload_message(payload: &Value) -> Option<&str> {
    payload
        .get("message")
        .and_then(Value::as_str)
        .filter(|msg| !msg.trim().is_empty())
}

async fn send_action_once(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    target: &str,
    action: &str,
    payload: Value,
    timeout: Duration,
) -> Result<(Value, String), IdentityError> {
    let trace_id = Uuid::new_v4().to_string();
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(action.to_string()),
            src_ilk: None,
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload,
    };
    sender.send(msg).await?;
    let response_action = response_action_for(action);
    let incoming = wait_system_response(
        receiver,
        target,
        &trace_id,
        &response_action,
        action,
        timeout,
    )
    .await?;
    Ok((incoming.payload, trace_id))
}

async fn wait_system_response(
    receiver: &mut NodeReceiver,
    target: &str,
    trace_id: &str,
    expected_msg: &str,
    action: &str,
    timeout: Duration,
) -> Result<Message, IdentityError> {
    let deadline = TokioInstant::now() + timeout;
    loop {
        let now = TokioInstant::now();
        if now >= deadline {
            return Err(IdentityError::ActionTimeout {
                action: action.to_string(),
                trace_id: trace_id.to_string(),
                target: target.to_string(),
                timeout_ms: timeout.as_millis() as u64,
            });
        }
        let remaining = deadline - now;
        let incoming = match receiver.recv_timeout(remaining).await {
            Ok(message) => message,
            Err(NodeError::Timeout) => {
                return Err(IdentityError::ActionTimeout {
                    action: action.to_string(),
                    trace_id: trace_id.to_string(),
                    target: target.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                })
            }
            Err(err) => return Err(IdentityError::Node(err)),
        };
        if incoming.routing.trace_id != trace_id {
            continue;
        }
        if incoming.meta.msg_type != SYSTEM_KIND {
            continue;
        }
        match incoming.meta.msg.as_deref() {
            Some(MSG_UNREACHABLE) => {
                let payload = serde_json::from_value::<UnreachablePayload>(incoming.payload)
                    .unwrap_or_default();
                return Err(IdentityError::Unreachable {
                    reason: payload.reason,
                    original_dst: payload.original_dst,
                });
            }
            Some(MSG_TTL_EXCEEDED) => {
                let payload = serde_json::from_value::<TtlExceededPayload>(incoming.payload)
                    .unwrap_or_default();
                return Err(IdentityError::TtlExceeded {
                    original_dst: payload.original_dst,
                    last_hop: payload.last_hop,
                });
            }
            Some(msg_name) if msg_name == expected_msg => return Ok(incoming),
            _ => continue,
        }
    }
}

/// Canonical identity SHM name for a hive.
pub fn identity_shm_name_for_hive(hive_id: &str) -> Result<String, IdentityShmError> {
    let trimmed = hive_id.trim();
    if trimmed.is_empty() {
        return Err(IdentityShmError::InvalidHiveId);
    }
    let name = format!("/jsr-identity-{trimmed}");
    if name.len() > 64 {
        return Err(IdentityShmError::InvalidShmName);
    }
    Ok(name)
}

/// Loads `hive_id` from `<config_dir>/hive.yaml`.
pub fn load_hive_id(config_dir: &Path) -> Result<String, IdentityShmError> {
    let raw = fs::read_to_string(config_dir.join("hive.yaml"))?;
    let hive: HiveFile = serde_yaml::from_str(&raw)?;
    let hive_id = hive.hive_id.trim().to_string();
    if hive_id.is_empty() {
        return Err(IdentityShmError::InvalidHiveId);
    }
    Ok(hive_id)
}

/// Resolve ILK from identity SHM by `(channel_type, address)`.
/// Returns canonical external format (`ilk:<uuid-v4>`) when found.
pub fn resolve_ilk_from_shm_name(
    identity_shm_name: &str,
    channel_type: &str,
    address: &str,
) -> Result<Option<String>, IdentityShmError> {
    let normalized_channel = channel_type.trim().to_ascii_lowercase();
    let normalized_address = address.trim().to_ascii_lowercase();
    if normalized_channel.is_empty() || normalized_address.is_empty() {
        return Err(IdentityShmError::InvalidChannelInput);
    }
    let reader = match open_identity_shm_reader(identity_shm_name) {
        Ok(reader) => reader,
        Err(err) if is_permission_lookup_unavailable(&err) => return Ok(None),
        Err(err) => return Err(err),
    };
    let ilk = match reader.resolve_ich_mapping(&normalized_channel, &normalized_address) {
        Ok(ilk) => ilk,
        Err(err) if is_permission_lookup_unavailable(&err) => return Ok(None),
        Err(err) => return Err(err),
    };
    Ok(ilk.map(|ilk_id| format!("ilk:{}", Uuid::from_bytes(ilk_id))))
}

/// Resolve ILK via explicit hive id (`/jsr-identity-<hive_id>`).
pub fn resolve_ilk_from_hive_id(
    hive_id: &str,
    channel_type: &str,
    address: &str,
) -> Result<Option<String>, IdentityShmError> {
    let shm_name = identity_shm_name_for_hive(hive_id)?;
    resolve_ilk_from_shm_name(&shm_name, channel_type, address)
}

/// Resolve ILK using `hive.yaml` from config dir.
pub fn resolve_ilk_from_hive_config(
    config_dir: &Path,
    channel_type: &str,
    address: &str,
) -> Result<Option<String>, IdentityShmError> {
    let hive_id = load_hive_id(config_dir)?;
    resolve_ilk_from_hive_id(&hive_id, channel_type, address)
}

/// Check whether a tenant exists in identity SHM by canonical tenant id (`tenant:<uuid>`).
pub fn tenant_exists_in_shm_name(
    identity_shm_name: &str,
    tenant_id: &str,
) -> Result<bool, IdentityShmError> {
    let tenant_uuid = parse_prefixed_uuid(tenant_id, "tenant")?;
    let reader = match open_identity_shm_reader(identity_shm_name) {
        Ok(reader) => reader,
        Err(err) if is_permission_lookup_unavailable(&err) => return Ok(false),
        Err(err) => return Err(err),
    };
    match reader.tenant_exists(tenant_uuid.into_bytes()) {
        Ok(exists) => Ok(exists),
        Err(err) if is_permission_lookup_unavailable(&err) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Check whether a tenant exists for one hive by canonical tenant id (`tenant:<uuid>`).
pub fn tenant_exists_in_hive_id(hive_id: &str, tenant_id: &str) -> Result<bool, IdentityShmError> {
    let shm_name = identity_shm_name_for_hive(hive_id)?;
    tenant_exists_in_shm_name(&shm_name, tenant_id)
}

/// Check whether a tenant exists using `hive.yaml` from config dir.
pub fn tenant_exists_in_hive_config(
    config_dir: &Path,
    tenant_id: &str,
) -> Result<bool, IdentityShmError> {
    let hive_id = load_hive_id(config_dir)?;
    tenant_exists_in_hive_id(&hive_id, tenant_id)
}

/// Check whether an ILK exists in identity SHM by canonical ilk id (`ilk:<uuid>`).
pub fn ilk_exists_in_shm_name(
    identity_shm_name: &str,
    ilk_id: &str,
) -> Result<bool, IdentityShmError> {
    let ilk_uuid = parse_prefixed_uuid(ilk_id, "ilk")?;
    let reader = match open_identity_shm_reader(identity_shm_name) {
        Ok(reader) => reader,
        Err(err) if is_permission_lookup_unavailable(&err) => return Ok(false),
        Err(err) => return Err(err),
    };
    match reader.ilk_exists(ilk_uuid.into_bytes()) {
        Ok(exists) => Ok(exists),
        Err(err) if is_permission_lookup_unavailable(&err) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Check whether an ILK exists for one hive by canonical ilk id (`ilk:<uuid>`).
pub fn ilk_exists_in_hive_id(hive_id: &str, ilk_id: &str) -> Result<bool, IdentityShmError> {
    let shm_name = identity_shm_name_for_hive(hive_id)?;
    ilk_exists_in_shm_name(&shm_name, ilk_id)
}

/// Check whether an ILK exists using `hive.yaml` from config dir.
pub fn ilk_exists_in_hive_config(
    config_dir: &Path,
    ilk_id: &str,
) -> Result<bool, IdentityShmError> {
    let hive_id = load_hive_id(config_dir)?;
    ilk_exists_in_hive_id(&hive_id, ilk_id)
}

/// List active ICH entries from identity SHM together with their candidate ILKs.
pub fn list_ich_options_from_shm_name(
    identity_shm_name: &str,
) -> Result<Vec<IdentityIchOption>, IdentityShmError> {
    let reader = open_identity_shm_reader(identity_shm_name)?;
    reader.list_ich_options()
}

/// List active ICH entries from identity SHM for one hive.
pub fn list_ich_options_from_hive_id(
    hive_id: &str,
) -> Result<Vec<IdentityIchOption>, IdentityShmError> {
    let shm_name = identity_shm_name_for_hive(hive_id)?;
    list_ich_options_from_shm_name(&shm_name)
}

/// List active ICH entries from identity SHM using `hive.yaml` from config dir.
pub fn list_ich_options_from_hive_config(
    config_dir: &Path,
) -> Result<Vec<IdentityIchOption>, IdentityShmError> {
    let hive_id = load_hive_id(config_dir)?;
    list_ich_options_from_hive_id(&hive_id)
}

struct IdentityShmReader {
    mmap: Mmap,
    layout: IdentityRegionLayout,
}

impl IdentityShmReader {
    fn resolve_ich_mapping(
        &self,
        channel_type: &str,
        address: &str,
    ) -> Result<Option<[u8; 16]>, IdentityShmError> {
        let header = header_ref::<IdentityHeader>(self.mmap.as_ref(), self.layout.header_offset)
            .ok_or(IdentityShmError::InvalidShmHeader)?;
        resolve_ich_mapping_from_region(
            header,
            self.mmap.as_ref(),
            &self.layout,
            channel_type,
            address,
        )
    }

    fn list_ich_options(&self) -> Result<Vec<IdentityIchOption>, IdentityShmError> {
        let header = header_ref::<IdentityHeader>(self.mmap.as_ref(), self.layout.header_offset)
            .ok_or(IdentityShmError::InvalidShmHeader)?;
        read_ich_options_from_region(header, self.mmap.as_ref(), &self.layout)
    }

    fn tenant_exists(&self, tenant_id: [u8; 16]) -> Result<bool, IdentityShmError> {
        let header = header_ref::<IdentityHeader>(self.mmap.as_ref(), self.layout.header_offset)
            .ok_or(IdentityShmError::InvalidShmHeader)?;
        read_tenant_exists_from_region(header, self.mmap.as_ref(), &self.layout, tenant_id)
    }

    fn ilk_exists(&self, ilk_id: [u8; 16]) -> Result<bool, IdentityShmError> {
        let header = header_ref::<IdentityHeader>(self.mmap.as_ref(), self.layout.header_offset)
            .ok_or(IdentityShmError::InvalidShmHeader)?;
        read_ilk_exists_from_region(header, self.mmap.as_ref(), &self.layout, ilk_id)
    }
}

fn parse_provision_payload(
    payload: IlkProvisionResponsePayload,
    trace_id: String,
) -> Result<IlkProvisionResult, IdentityError> {
    if payload.status != "ok" {
        return Err(IdentityError::ProvisionRejected {
            error_code: payload.error_code.unwrap_or_else(|| "UNKNOWN".to_string()),
            message: payload
                .message
                .unwrap_or_else(|| "identity returned non-ok status".to_string()),
        });
    }
    let ilk_id = payload
        .ilk_id
        .ok_or_else(|| IdentityError::InvalidResponse("missing ilk_id in response".to_string()))?;
    if !is_prefixed_uuid(&ilk_id, "ilk") {
        return Err(IdentityError::InvalidResponse(format!(
            "invalid ilk_id format '{}'",
            ilk_id
        )));
    }
    let registration_status = payload
        .registration_status
        .unwrap_or_else(|| "temporary".to_string());
    Ok(IlkProvisionResult {
        ilk_id,
        registration_status,
        trace_id,
    })
}

fn open_identity_shm_reader(name: &str) -> Result<IdentityShmReader, IdentityShmError> {
    if name.trim().is_empty() || name.len() > 64 {
        return Err(IdentityShmError::InvalidShmName);
    }
    let cstr = CString::new(name).map_err(|_| IdentityShmError::InvalidShmName)?;
    let fd = shm_open(
        cstr.as_c_str(),
        OFlag::O_RDONLY,
        nix::sys::stat::Mode::empty(),
    )?;
    let stat = fstat(fd.as_raw_fd())?;
    if stat.st_size <= 0 {
        return Err(IdentityShmError::InvalidShmHeader);
    }
    let mmap = unsafe {
        MmapOptions::new()
            .len(stat.st_size as usize)
            .map(fd.as_raw_fd())?
    };
    if !is_region_valid(mmap.as_ref()) {
        return Err(IdentityShmError::InvalidShmHeader);
    }
    let header =
        header_ref::<IdentityHeader>(mmap.as_ref(), 0).ok_or(IdentityShmError::InvalidShmHeader)?;
    if header.version != IDENTITY_VERSION {
        return Err(IdentityShmError::InvalidShmHeader);
    }
    let limits = IdentityRegionLimits {
        max_ilks: header.max_ilks,
        max_tenants: header.max_tenants,
        max_vocabulary: header.max_vocabulary,
        max_ilk_aliases: header.max_ilk_aliases,
    };
    let layout = layout_identity(limits);
    if layout.total_len > stat.st_size as usize {
        return Err(IdentityShmError::InvalidShmHeader);
    }
    Ok(IdentityShmReader { mmap, layout })
}

fn is_permission_lookup_unavailable(err: &IdentityShmError) -> bool {
    match err {
        IdentityShmError::Nix(errno) => {
            matches!(*errno, nix::errno::Errno::EACCES | nix::errno::Errno::EPERM)
        }
        IdentityShmError::Io(io_err) => io_err.kind() == std::io::ErrorKind::PermissionDenied,
        _ => false,
    }
}

fn resolve_ich_mapping_from_region(
    header: &IdentityHeader,
    mmap: &[u8],
    layout: &IdentityRegionLayout,
    channel_type: &str,
    address: &str,
) -> Result<Option<[u8; 16]>, IdentityShmError> {
    let start = StdInstant::now();
    let mut odd_seq_spins = 0u64;
    let mut seq_retry_count = 0u64;
    let mut last_seq = 0u64;
    loop {
        if start.elapsed() > StdDuration::from_millis(SEQLOCK_READ_TIMEOUT_MS) {
            tracing::warn!(
                elapsed_us = start.elapsed().as_micros() as u64,
                odd_seq_spins,
                seq_retry_count,
                last_seq,
                tenant_count = header.tenant_count,
                ilk_count = header.ilk_count,
                ich_count = header.ich_count,
                ich_mapping_count = header.ich_mapping_count,
                ilk_alias_count = header.ilk_alias_count,
                "sdk identity shm read timed out under seqlock"
            );
            return Err(IdentityShmError::SeqLockTimeout);
        }
        let s1 = header.seq.load(Ordering::Acquire);
        last_seq = s1;
        if s1 & 1 != 0 {
            odd_seq_spins = odd_seq_spins.saturating_add(1);
            std::hint::spin_loop();
            continue;
        }
        atomic::fence(Ordering::Acquire);
        let mappings = read_slice::<IchMappingEntry>(
            mmap,
            layout.ich_mapping_offset,
            layout.limits.max_ich_mappings() as usize,
        )
        .ok_or(IdentityShmError::InvalidShmHeader)?;
        let resolved = resolve_ich_mapping_from_entries(mappings, channel_type, address);
        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return Ok(resolved);
        }
        seq_retry_count = seq_retry_count.saturating_add(1);
        last_seq = s2;
    }
}

fn read_ich_options_from_region(
    header: &IdentityHeader,
    mmap: &[u8],
    layout: &IdentityRegionLayout,
) -> Result<Vec<IdentityIchOption>, IdentityShmError> {
    let start = StdInstant::now();
    loop {
        if start.elapsed() > StdDuration::from_millis(SEQLOCK_READ_TIMEOUT_MS) {
            return Err(IdentityShmError::SeqLockTimeout);
        }
        let s1 = header.seq.load(Ordering::Acquire);
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        atomic::fence(Ordering::Acquire);

        let max_ilks = usize::min(header.ilk_count as usize, layout.limits.max_ilks as usize);
        let max_ichs = usize::min(header.ich_count as usize, layout.limits.max_ichs() as usize);
        let ilks = read_slice::<IlkEntry>(mmap, layout.ilk_offset, layout.limits.max_ilks as usize)
            .ok_or(IdentityShmError::InvalidShmHeader)?;
        let ichs =
            read_slice::<IchEntry>(mmap, layout.ich_offset, layout.limits.max_ichs() as usize)
                .ok_or(IdentityShmError::InvalidShmHeader)?;
        let ilk_snapshot = ilks[..max_ilks].to_vec();
        let ich_snapshot = ichs[..max_ichs].to_vec();

        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return Ok(build_ich_options(&ilk_snapshot, &ich_snapshot));
        }
    }
}

fn read_tenant_exists_from_region(
    header: &IdentityHeader,
    mmap: &[u8],
    layout: &IdentityRegionLayout,
    tenant_id: [u8; 16],
) -> Result<bool, IdentityShmError> {
    let start = StdInstant::now();
    loop {
        if start.elapsed() > StdDuration::from_millis(SEQLOCK_READ_TIMEOUT_MS) {
            return Err(IdentityShmError::SeqLockTimeout);
        }
        let s1 = header.seq.load(Ordering::Acquire);
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        atomic::fence(Ordering::Acquire);

        let max_tenants = usize::min(
            header.tenant_count as usize,
            layout.limits.max_tenants as usize,
        );
        let tenants = read_slice::<TenantEntry>(
            mmap,
            layout.tenant_offset,
            layout.limits.max_tenants as usize,
        )
        .ok_or(IdentityShmError::InvalidShmHeader)?;
        let exists = tenants[..max_tenants]
            .iter()
            .any(|entry| entry.flags & FLAG_ACTIVE != 0 && entry.tenant_id == tenant_id);

        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return Ok(exists);
        }
    }
}

fn read_ilk_exists_from_region(
    header: &IdentityHeader,
    mmap: &[u8],
    layout: &IdentityRegionLayout,
    ilk_id: [u8; 16],
) -> Result<bool, IdentityShmError> {
    let start = StdInstant::now();
    loop {
        if start.elapsed() > StdDuration::from_millis(SEQLOCK_READ_TIMEOUT_MS) {
            return Err(IdentityShmError::SeqLockTimeout);
        }
        let s1 = header.seq.load(Ordering::Acquire);
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        atomic::fence(Ordering::Acquire);

        let max_ilks = usize::min(header.ilk_count as usize, layout.limits.max_ilks as usize);
        let ilks = read_slice::<IlkEntry>(mmap, layout.ilk_offset, layout.limits.max_ilks as usize)
            .ok_or(IdentityShmError::InvalidShmHeader)?;
        let exists = ilks[..max_ilks]
            .iter()
            .any(|entry| entry.flags & FLAG_ACTIVE != 0 && entry.ilk_id == ilk_id);

        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return Ok(exists);
        }
    }
}

fn is_region_valid(mmap: &[u8]) -> bool {
    let Some(magic_bytes) = mmap.get(0..4) else {
        return false;
    };
    let Ok(magic_bytes) = <[u8; 4]>::try_from(magic_bytes) else {
        return false;
    };
    let magic = u32::from_ne_bytes(magic_bytes);
    match magic {
        IDENTITY_MAGIC => header_ref::<IdentityHeader>(mmap, 0)
            .is_some_and(|header| header.version == IDENTITY_VERSION),
        _ => false,
    }
}

fn resolve_ich_mapping_from_entries(
    mappings: &[IchMappingEntry],
    channel_type: &str,
    address: &str,
) -> Option<[u8; 16]> {
    if mappings.is_empty() {
        return None;
    }
    let hash = compute_ich_hash(channel_type, address);
    let table_len = mappings.len();
    let mut idx = (hash as usize) % table_len;
    for _ in 0..table_len {
        let entry = mappings[idx];
        if entry.flags == 0 {
            return None;
        }
        if entry.flags & ICH_MAP_FLAG_TOMBSTONE != 0 {
            idx = (idx + 1) % table_len;
            continue;
        }
        if entry.flags & ICH_MAP_FLAG_OCCUPIED != 0
            && entry.hash == hash
            && fixed_str_matches(&entry.channel_type, channel_type)
            && fixed_str_matches(&entry.address, address)
        {
            return Some(entry.ilk_id);
        }
        idx = (idx + 1) % table_len;
    }
    None
}

fn fixed_str_matches(buf: &[u8], value: &str) -> bool {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).unwrap_or("") == value
}

fn fixed_str_to_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end])
        .unwrap_or("")
        .trim()
        .to_string()
}

fn prefixed_uuid_from_bytes(prefix: &str, bytes: [u8; 16]) -> String {
    format!("{prefix}:{}", Uuid::from_bytes(bytes))
}

fn ilk_type_from_shm(value: u8) -> String {
    match value {
        SHM_ILK_TYPE_HUMAN => "human",
        SHM_ILK_TYPE_AGENT => "agent",
        SHM_ILK_TYPE_SYSTEM => "system",
        _ => "unknown",
    }
    .to_string()
}

fn registration_status_from_shm(value: u8) -> String {
    match value {
        SHM_REG_STATUS_TEMPORARY => "temporary",
        SHM_REG_STATUS_PARTIAL => "partial",
        SHM_REG_STATUS_COMPLETE => "complete",
        _ => "unknown",
    }
    .to_string()
}

fn build_ich_options(ilks: &[IlkEntry], ichs: &[IchEntry]) -> Vec<IdentityIchOption> {
    let mut ilk_map: std::collections::HashMap<[u8; 16], IdentityIlkOption> =
        std::collections::HashMap::new();
    for ilk in ilks {
        if ilk.flags & FLAG_ACTIVE == 0 {
            continue;
        }
        ilk_map.insert(
            ilk.ilk_id,
            IdentityIlkOption {
                ilk_id: prefixed_uuid_from_bytes("ilk", ilk.ilk_id),
                display_name: {
                    let value = fixed_str_to_string(&ilk.display_name);
                    (!value.is_empty()).then_some(value)
                },
                handler_node: {
                    let value = fixed_str_to_string(&ilk.handler_node);
                    (!value.is_empty()).then_some(value)
                },
                registration_status: registration_status_from_shm(ilk.registration_status),
                ilk_type: ilk_type_from_shm(ilk.ilk_type),
            },
        );
    }

    let mut grouped: std::collections::BTreeMap<(String, String, String), IdentityIchOption> =
        std::collections::BTreeMap::new();
    for ich in ichs {
        if ich.flags & FLAG_ACTIVE == 0 {
            continue;
        }
        let ich_id = prefixed_uuid_from_bytes("ich", ich.ich_id);
        let channel_type = fixed_str_to_string(&ich.channel_type);
        let address = fixed_str_to_string(&ich.address);
        if ich_id.is_empty() || channel_type.is_empty() || address.is_empty() {
            continue;
        }
        let ilk = ilk_map.get(&ich.ilk_id).cloned();
        let entry = grouped
            .entry((ich_id.clone(), channel_type.clone(), address.clone()))
            .or_insert_with(|| IdentityIchOption {
                ich_id: ich_id.clone(),
                channel_type: channel_type.clone(),
                address: address.clone(),
                is_primary: ich.is_primary != 0,
                ilks: Vec::new(),
            });
        entry.is_primary = entry.is_primary || ich.is_primary != 0;
        if let Some(ilk) = ilk {
            if !entry
                .ilks
                .iter()
                .any(|candidate| candidate.ilk_id == ilk.ilk_id)
            {
                entry.ilks.push(ilk);
            }
        }
    }

    let mut out: Vec<_> = grouped.into_values().collect();
    for option in &mut out {
        option.ilks.sort_by(|a, b| {
            a.ilk_id
                .cmp(&b.ilk_id)
                .then_with(|| a.display_name.cmp(&b.display_name))
        });
    }
    out.sort_by(|a, b| {
        a.channel_type
            .cmp(&b.channel_type)
            .then_with(|| a.address.cmp(&b.address))
            .then_with(|| a.ich_id.cmp(&b.ich_id))
    });
    out
}

fn compute_ich_hash(channel_type: &str, address: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for b in channel_type.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash ^= 0xff;
    hash = hash.wrapping_mul(FNV_PRIME);
    for b in address.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn layout_identity(limits: IdentityRegionLimits) -> IdentityRegionLayout {
    let max_tenants = limits.max_tenants as usize;
    let max_ilks = limits.max_ilks as usize;
    let max_ichs = limits.max_ichs() as usize;
    let max_ich_mappings = limits.max_ich_mappings() as usize;
    let max_ilk_aliases = limits.max_ilk_aliases as usize;
    let max_vocabulary = limits.max_vocabulary as usize;

    let header_size = size_of::<IdentityHeader>();
    let tenant_size = size_of::<TenantEntry>() * max_tenants;
    let ilk_size = size_of::<IlkEntry>() * max_ilks;
    let ich_size = size_of::<IchEntry>() * max_ichs;
    let mapping_size = size_of::<IchMappingEntry>() * max_ich_mappings;
    let alias_size = size_of::<IlkAliasEntry>() * max_ilk_aliases;
    let vocabulary_size = size_of::<VocabularyEntry>() * max_vocabulary;

    let header_offset = 0;
    let tenant_offset = align_up(header_offset + header_size, REGION_ALIGNMENT);
    let ilk_offset = align_up(tenant_offset + tenant_size, REGION_ALIGNMENT);
    let ich_offset = align_up(ilk_offset + ilk_size, REGION_ALIGNMENT);
    let ich_mapping_offset = align_up(ich_offset + ich_size, REGION_ALIGNMENT);
    let ilk_alias_offset = align_up(ich_mapping_offset + mapping_size, REGION_ALIGNMENT);
    let vocabulary_offset = align_up(ilk_alias_offset + alias_size, REGION_ALIGNMENT);
    let variable_offset = align_up(vocabulary_offset + vocabulary_size, REGION_ALIGNMENT);
    let total_len = variable_offset;

    IdentityRegionLayout {
        header_offset,
        tenant_offset,
        ilk_offset,
        ich_offset,
        ich_mapping_offset,
        ilk_alias_offset,
        vocabulary_offset,
        variable_offset,
        total_len,
        limits,
    }
}

fn align_up(value: usize, align: usize) -> usize {
    if align == 0 {
        value
    } else {
        (value + (align - 1)) & !(align - 1)
    }
}

fn header_ref<T>(mmap: &[u8], offset: usize) -> Option<&T> {
    let size = size_of::<T>();
    if offset.checked_add(size)? > mmap.len() {
        return None;
    }
    let ptr = mmap.as_ptr().wrapping_add(offset) as *const T;
    if (ptr as usize) % std::mem::align_of::<T>() != 0 {
        return None;
    }
    Some(unsafe { &*ptr })
}

fn read_slice<T>(mmap: &[u8], offset: usize, len: usize) -> Option<&[T]> {
    let elem = size_of::<T>();
    let bytes = len.checked_mul(elem)?;
    if offset.checked_add(bytes)? > mmap.len() {
        return None;
    }
    let ptr = mmap.as_ptr().wrapping_add(offset) as *const T;
    if (ptr as usize) % std::mem::align_of::<T>() != 0 {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn is_prefixed_uuid(value: &str, prefix: &str) -> bool {
    let trimmed = value.trim();
    let expected = format!("{prefix}:");
    if !trimmed.starts_with(&expected) {
        return false;
    }
    Uuid::parse_str(&trimmed[expected.len()..]).is_ok()
}

fn parse_prefixed_uuid(value: &str, prefix: &str) -> Result<Uuid, IdentityShmError> {
    let trimmed = value.trim();
    let expected = format!("{prefix}:");
    if !trimmed.starts_with(&expected) {
        return Err(IdentityShmError::InvalidChannelInput);
    }
    Uuid::parse_str(&trimmed[expected.len()..]).map_err(|_| IdentityShmError::InvalidChannelInput)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_lookup_is_treated_as_not_found() {
        let out =
            match Err::<Option<String>, _>(IdentityShmError::Nix(nix::errno::Errno::EACCES)) {
                Err(err) if is_permission_lookup_unavailable(&err) => Ok(None),
                Err(err) => Err(err),
                Ok(value) => Ok(value),
            }
            .expect("permission denied should degrade to lookup unavailable");
        assert_eq!(out, None);
    }

    #[test]
    fn parse_provision_payload_ok() {
        let payload = IlkProvisionResponsePayload {
            status: "ok".to_string(),
            ilk_id: Some("ilk:550e8400-e29b-41d4-a716-446655440000".to_string()),
            registration_status: Some("temporary".to_string()),
            error_code: None,
            message: None,
        };
        let out = parse_provision_payload(payload, "trace-1".to_string()).expect("ok payload");
        assert_eq!(out.ilk_id, "ilk:550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(out.registration_status, "temporary");
    }

    #[test]
    fn parse_provision_payload_rejected() {
        let payload = IlkProvisionResponsePayload {
            status: "error".to_string(),
            ilk_id: None,
            registration_status: None,
            error_code: Some("UNAUTHORIZED_REGISTRAR".to_string()),
            message: Some("forbidden".to_string()),
        };
        let err = parse_provision_payload(payload, "trace-1".to_string()).expect_err("must reject");
        assert!(matches!(
            err,
            IdentityError::ProvisionRejected {
                error_code,
                message
            } if error_code == "UNAUTHORIZED_REGISTRAR" && message == "forbidden"
        ));
    }

    #[test]
    fn parse_provision_payload_invalid_ilk() {
        let payload = IlkProvisionResponsePayload {
            status: "ok".to_string(),
            ilk_id: Some("bad-ilk".to_string()),
            registration_status: None,
            error_code: None,
            message: None,
        };
        let err = parse_provision_payload(payload, "trace-1".to_string()).expect_err("must fail");
        assert!(matches!(err, IdentityError::InvalidResponse(_)));
    }

    #[test]
    fn build_ich_options_groups_candidates_by_ich_entry() {
        let ilk_a = IlkEntry {
            ilk_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")
                .unwrap()
                .into_bytes(),
            ilk_type: SHM_ILK_TYPE_HUMAN,
            registration_status: SHM_REG_STATUS_COMPLETE,
            flags: FLAG_ACTIVE,
            tenant_id: [0; 16],
            display_name: encode_fixed::<128>("Alice"),
            handler_node: encode_fixed::<128>("SY.frontdesk.gov@motherbee"),
            ich_offset: 0,
            ich_count: 0,
            _pad0: [0; 2],
            roles_offset: 0,
            roles_len: 0,
            _pad1: [0; 2],
            capabilities_offset: 0,
            capabilities_len: 0,
            _pad2: [0; 2],
            created_at: 0,
            updated_at: 0,
            _reserved: [0; 8],
        };
        let ilk_b = IlkEntry {
            ilk_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001")
                .unwrap()
                .into_bytes(),
            ilk_type: SHM_ILK_TYPE_AGENT,
            registration_status: SHM_REG_STATUS_PARTIAL,
            flags: FLAG_ACTIVE,
            tenant_id: [0; 16],
            display_name: encode_fixed::<128>("Support bot"),
            handler_node: encode_fixed::<128>("AI.helper@motherbee"),
            ich_offset: 0,
            ich_count: 0,
            _pad0: [0; 2],
            roles_offset: 0,
            roles_len: 0,
            _pad1: [0; 2],
            capabilities_offset: 0,
            capabilities_len: 0,
            _pad2: [0; 2],
            created_at: 0,
            updated_at: 0,
            _reserved: [0; 8],
        };
        let ich_primary = IchEntry {
            ich_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440010")
                .unwrap()
                .into_bytes(),
            ilk_id: ilk_a.ilk_id,
            channel_type: encode_fixed::<ICH_CHANNEL_TYPE_MAX_LEN>("whatsapp"),
            address: encode_fixed::<ICH_ADDRESS_MAX_LEN>("5491112345678"),
            flags: FLAG_ACTIVE,
            is_primary: 1,
            _pad0: [0; 5],
            added_at: 0,
            _reserved: [0; 16],
        };
        let ich_same = IchEntry {
            ich_id: ich_primary.ich_id,
            ilk_id: ilk_b.ilk_id,
            channel_type: encode_fixed::<ICH_CHANNEL_TYPE_MAX_LEN>("whatsapp"),
            address: encode_fixed::<ICH_ADDRESS_MAX_LEN>("5491112345678"),
            flags: FLAG_ACTIVE,
            is_primary: 0,
            _pad0: [0; 5],
            added_at: 0,
            _reserved: [0; 16],
        };

        let options = build_ich_options(&[ilk_a, ilk_b], &[ich_primary, ich_same]);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].channel_type, "whatsapp");
        assert_eq!(options[0].address, "5491112345678");
        assert!(options[0].is_primary);
        assert_eq!(options[0].ilks.len(), 2);
        assert_eq!(
            options[0].ilks[0].ilk_id,
            "ilk:550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(
            options[0].ilks[1].ilk_id,
            "ilk:550e8400-e29b-41d4-a716-446655440001"
        );
    }

    #[test]
    fn read_tenant_exists_returns_true_for_active_tenant() {
        let header = IdentityHeader {
            magic: IDENTITY_MAGIC,
            version: IDENTITY_VERSION,
            seq: AtomicU64::new(0),
            tenant_count: 1,
            ilk_count: 0,
            ich_count: 0,
            ich_mapping_count: 0,
            vocabulary_count: 0,
            ilk_alias_count: 0,
            max_ilks: 1,
            max_tenants: 1,
            max_ichs: 4,
            max_ich_mappings: 8,
            max_vocabulary: 0,
            max_ilk_aliases: 0,
            updated_at: 0,
            heartbeat: 0,
            owner_uuid: [0; 16],
            owner_pid: 0,
            is_primary: 1,
            _pad0: [0; 3],
            hive_id: [0; 64],
            hive_id_len: 0,
            _reserved: [0; 6],
        };
        let tenant = TenantEntry {
            tenant_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440010")
                .unwrap()
                .into_bytes(),
            name: encode_fixed::<128>("ACME"),
            domain: [0; 128],
            status: 1,
            flags: FLAG_ACTIVE,
            _pad0: [0; 5],
            max_ilks: 10,
            created_at: 0,
            updated_at: 0,
            _reserved: [0; 8],
        };
        let layout = layout_identity(IdentityRegionLimits {
            max_ilks: 1,
            max_tenants: 1,
            max_vocabulary: 0,
            max_ilk_aliases: 0,
        });
        let mut mmap = vec![0u8; layout.total_len];
        write_struct(&mut mmap, layout.header_offset, &header);
        write_struct(&mut mmap, layout.tenant_offset, &tenant);

        let exists =
            read_tenant_exists_from_region(&header, &mmap, &layout, tenant.tenant_id).unwrap();
        assert!(exists);
    }

    #[test]
    fn read_ilk_exists_returns_true_for_active_ilk() {
        let header = IdentityHeader {
            magic: IDENTITY_MAGIC,
            version: IDENTITY_VERSION,
            seq: AtomicU64::new(0),
            tenant_count: 0,
            ilk_count: 1,
            ich_count: 0,
            ich_mapping_count: 0,
            vocabulary_count: 0,
            ilk_alias_count: 0,
            max_ilks: 1,
            max_tenants: 1,
            max_ichs: 4,
            max_ich_mappings: 8,
            max_vocabulary: 0,
            max_ilk_aliases: 0,
            updated_at: 0,
            heartbeat: 0,
            owner_uuid: [0; 16],
            owner_pid: 0,
            is_primary: 1,
            _pad0: [0; 3],
            hive_id: [0; 64],
            hive_id_len: 0,
            _reserved: [0; 6],
        };
        let ilk = IlkEntry {
            ilk_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440020")
                .unwrap()
                .into_bytes(),
            ilk_type: SHM_ILK_TYPE_HUMAN,
            registration_status: SHM_REG_STATUS_COMPLETE,
            flags: FLAG_ACTIVE,
            tenant_id: [0; 16],
            display_name: encode_fixed::<128>("Alice"),
            handler_node: [0; 128],
            ich_offset: 0,
            ich_count: 0,
            _pad0: [0; 2],
            roles_offset: 0,
            roles_len: 0,
            _pad1: [0; 2],
            capabilities_offset: 0,
            capabilities_len: 0,
            _pad2: [0; 2],
            created_at: 0,
            updated_at: 0,
            _reserved: [0; 8],
        };
        let layout = layout_identity(IdentityRegionLimits {
            max_ilks: 1,
            max_tenants: 1,
            max_vocabulary: 0,
            max_ilk_aliases: 0,
        });
        let mut mmap = vec![0u8; layout.total_len];
        write_struct(&mut mmap, layout.header_offset, &header);
        write_struct(&mut mmap, layout.ilk_offset, &ilk);

        let exists = read_ilk_exists_from_region(&header, &mmap, &layout, ilk.ilk_id).unwrap();
        assert!(exists);
    }

    fn encode_fixed<const N: usize>(value: &str) -> [u8; N] {
        let mut out = [0u8; N];
        let bytes = value.as_bytes();
        let len = bytes.len().min(N);
        out[..len].copy_from_slice(&bytes[..len]);
        out
    }

    fn write_struct<T>(buffer: &mut [u8], offset: usize, value: &T) {
        let size = size_of::<T>();
        let end = offset + size;
        let src = unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size) };
        buffer[offset..end].copy_from_slice(src);
    }
}
