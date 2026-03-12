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
const SEQLOCK_READ_TIMEOUT_MS: u64 = 5;

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
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(action.to_string()),
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
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
    let reader = open_identity_shm_reader(identity_shm_name)?;
    let ilk = reader.resolve_ich_mapping(&normalized_channel, &normalized_address)?;
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
            let mappings = read_slice::<IchMappingEntry>(
                self.mmap.as_ref(),
                self.layout.ich_mapping_offset,
                self.layout.limits.max_ich_mappings() as usize,
            )
            .ok_or(IdentityShmError::InvalidShmHeader)?;
            let resolved = resolve_ich_mapping_from_entries(mappings, channel_type, address);
            atomic::fence(Ordering::Acquire);
            let s2 = header.seq.load(Ordering::Acquire);
            if s1 == s2 {
                return Ok(resolved);
            }
        }
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
    let header =
        header_ref::<IdentityHeader>(mmap.as_ref(), 0).ok_or(IdentityShmError::InvalidShmHeader)?;
    if header.magic != IDENTITY_MAGIC || header.version != IDENTITY_VERSION {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
