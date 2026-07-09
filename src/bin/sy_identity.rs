use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::time;
use tokio_postgres::{error::SqlState, Config as PgConfig, NoTls};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, VaultSecretChangedPayload, VaultSecretInterest,
    MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
};
use fluxbee_sdk::{
    build_node_config_response_message, managed_node_config_path, try_handle_default_node_status,
    NodeConfig, NodeSender, OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher,
    VaultCallerOwned, VaultClient, NODE_CONFIG_APPLY_MODE_REPLACE,
};
use json_router::shm::{
    copy_bytes_with_len, now_epoch_ms, sha256_hex_to_bytes, IchEntry, IdentityRegionLimits,
    IdentityRegionWriter, IlkAliasEntry, IlkEntry, TenantEntry, VocabularyEntry, FLAG_ACTIVE,
    ICH_ADDRESS_MAX_LEN, ICH_CHANNEL_TYPE_MAX_LEN, IDENTITY_DEFINITION_MAX_HANDBOOKS,
    IDENTITY_DEFINITION_MAX_SKILLS,
};

type IdentityError = Box<dyn std::error::Error + Send + Sync>;
const PRIMARY_HIVE_ID: &str = "motherbee";

const DEFAULT_DEFAULT_TENANT_NAME: &str = "fluxbee";
const DEFAULT_ROOT_TENANT_ID: &str = "tnt:00000000-0000-0000-0000-000000000001";
const DEFAULT_MERGE_ALIAS_TTL_SECS: u64 = 3600;
const ALIAS_GC_INTERVAL_SECS: u64 = 30;
const DEFAULT_IDENTITY_SYNC_PORT: u16 = 9100;
const IDENTITY_DB_NAME: &str = "fluxbee_identity";
const IDENTITY_NODE_BASE_NAME: &str = "SY.identity";
const IDENTITY_NODE_VERSION: &str = "2.0";
const IDENTITY_CONFIG_SCHEMA_VERSION: u32 = 1;
const IDENTITY_FULL_SYNC_CHUNK_ITEMS: usize = 256;
const IDENTITY_SYNC_VERSION: u32 = 1;
const SYNC_OP_FULL_SYNC_REQUEST: &str = "IDENTITY_FULL_SYNC_REQUEST";
const SYNC_OP_FULL_SYNC: &str = "full_sync";
const SYNC_OP_DELTA_SUBSCRIBE: &str = "IDENTITY_DELTA_SUBSCRIBE";
const SYNC_OP_DELTA: &str = "IDENTITY_DELTA";
const SYNC_OP_DELTA_ACK: &str = "IDENTITY_DELTA_ACK";
// Upstream push (replica → primary): a replica publishes its own `@hive` ilks so
// motherbee converges to the additive union of the whole mesh ("who exists").
const SYNC_OP_DELTA_PUBLISH: &str = "IDENTITY_DELTA_PUBLISH";
const SYNC_OP_PUBLISH_OK: &str = "IDENTITY_DELTA_PUBLISH_OK";
const SYNC_OP_PUBLISH_SNAPSHOT: &str = "IDENTITY_PUBLISH_SNAPSHOT";
const IDENTITY_DELTA_ACK_TIMEOUT_MS: u64 = 2_000;
const IDENTITY_DELTA_MAX_RETRIES: u32 = 3;
// How often a replica re-pushes its full self-owned ilk set so the primary
// reconciles (recovers upserts AND hard-deletes lost on a reconnect gap).
const IDENTITY_PUBLISH_RECONCILE_SECS: u64 = 30;
// Bound on the primary's ingest queue: a flooding/buggy replica applies
// backpressure to its own publish connection instead of growing primary memory.
const IDENTITY_INGEST_CHANNEL_CAP: usize = 4096;
// Hard cap on a single :9100 sync frame (one JSON line). A peer that streams
// bytes without a newline would otherwise grow the read buffer without bound and
// OOM the process (G-4). 16 MiB is far above any legitimate full-sync chunk
// (IDENTITY_FULL_SYNC_CHUNK_ITEMS records) or delta frame.
const MAX_SYNC_LINE_BYTES: usize = 16 * 1024 * 1024;
// Idle read timeout for post-handshake sync frames: a peer that stops sending
// mid-stream is dropped rather than pinning a reader task forever (G-4). Sized
// generously for large full-syncs over a slow WAN link (per-frame, not total).
const IDENTITY_SYNC_READ_IDLE_SECS: u64 = 60;
// Upper bound on the advertised full-sync chunk count, checked before allocating
// the reassembly buffer, so a crafted/corrupted total_chunks (u32, up to ~4.3e9)
// cannot drive a multi-hundred-GB allocation that aborts a booting replica
// (G-7). 65_536 chunks * 256 items/chunk = ~16.7M records, far above any real
// store; the reassembly Vec is then at most ~a few MB.
const MAX_FULL_SYNC_CHUNKS: usize = 65_536;
// Cap on concurrently-registered delta subscribers on the primary. Auth already
// gates who may subscribe (F-01); this bounds fan-out fd/memory even so (G-5).
const MAX_DELTA_SUBSCRIBERS: usize = 256;
// Per-subscriber outbound delta queue depth. A bounded channel means a slow/
// stuck subscriber is dropped (recovers via full-sync on reconnect) instead of
// letting healthy mesh activity grow primary memory without bound (G-5).
const IDENTITY_SUBSCRIBER_CHANNEL_CAP: usize = 1024;
// Cap on concurrently-handled :9100 sync connections. Each accepted connection
// is handled in its own task (F-02); this bounds the fan-out so a connect storm
// cannot exhaust fds/memory. Excess connections are dropped and the peer retries.
const MAX_CONCURRENT_SYNC_CONNS: usize = 64;
// Write deadline for a single sync frame. A peer that stops reading mid-stream
// (e.g. during full-sync) is dropped rather than pinning its handler task
// forever (F-02). Generous for large frames over a slow WAN link.
const IDENTITY_SYNC_WRITE_TIMEOUT_SECS: u64 = 30;
const DEFAULT_IDENTITY_SHM_MAX_ILKS: u32 = 8_192;
const DEFAULT_IDENTITY_SHM_MAX_TENANTS: u32 = 1_024;
const DEFAULT_IDENTITY_SHM_MAX_VOCABULARY: u32 = 4_096;
const AGENT_DEFINITION_MAX_SKILLS: usize = IDENTITY_DEFINITION_MAX_SKILLS;
const AGENT_DEFINITION_MAX_HANDBOOKS: usize = IDENTITY_DEFINITION_MAX_HANDBOOKS;
const SHM_ILK_TYPE_HUMAN: u8 = 0;
const SHM_ILK_TYPE_AGENT: u8 = 1;
const SHM_ILK_TYPE_SYSTEM: u8 = 2;
const SHM_REG_STATUS_TEMPORARY: u8 = 0;
const SHM_REG_STATUS_PARTIAL: u8 = 1;
const SHM_REG_STATUS_COMPLETE: u8 = 2;
const SHM_TENANT_STATUS_PENDING: u8 = 0;
const SHM_TENANT_STATUS_ACTIVE: u8 = 1;
const SHM_TENANT_STATUS_SUSPENDED: u8 = 2;

const MSG_ILK_PROVISION: &str = "ILK_PROVISION";
const MSG_ILK_PROVISION_RESPONSE: &str = "ILK_PROVISION_RESPONSE";
const MSG_ILK_LIST: &str = "ILK_LIST";
const MSG_ILK_LIST_RESPONSE: &str = "ILK_LIST_RESPONSE";
const MSG_ILK_GET: &str = "ILK_GET";
const MSG_ILK_GET_RESPONSE: &str = "ILK_GET_RESPONSE";
const MSG_ILK_REGISTER: &str = "ILK_REGISTER";
const MSG_ILK_REGISTER_RESPONSE: &str = "ILK_REGISTER_RESPONSE";
const MSG_ILK_ADD_CHANNEL: &str = "ILK_ADD_CHANNEL";
const MSG_ILK_ADD_CHANNEL_RESPONSE: &str = "ILK_ADD_CHANNEL_RESPONSE";
const MSG_ILK_UPDATE: &str = "ILK_UPDATE";
const MSG_ILK_UPDATE_RESPONSE: &str = "ILK_UPDATE_RESPONSE";
const MSG_ILK_SET_DEFINITION: &str = "ILK_SET_DEFINITION";
const MSG_ILK_SET_DEFINITION_RESPONSE: &str = "ILK_SET_DEFINITION_RESPONSE";
const MSG_ILK_DELETE: &str = "ILK_DELETE";
const MSG_ILK_DELETE_RESPONSE: &str = "ILK_DELETE_RESPONSE";
const MSG_ICH_SET_ENABLED: &str = "ICH_SET_ENABLED";
const MSG_ICH_SET_ENABLED_RESPONSE: &str = "ICH_SET_ENABLED_RESPONSE";
const MSG_TNT_CREATE: &str = "TNT_CREATE";
const MSG_TNT_CREATE_RESPONSE: &str = "TNT_CREATE_RESPONSE";
const MSG_TNT_LIST: &str = "TNT_LIST";
const MSG_TNT_LIST_RESPONSE: &str = "TNT_LIST_RESPONSE";
const MSG_TNT_GET: &str = "TNT_GET";
const MSG_TNT_GET_RESPONSE: &str = "TNT_GET_RESPONSE";
const MSG_TNT_UPDATE: &str = "TNT_UPDATE";
const MSG_TNT_UPDATE_RESPONSE: &str = "TNT_UPDATE_RESPONSE";
const MSG_TNT_SET_SPONSOR: &str = "TNT_SET_SPONSOR";
const MSG_TNT_SET_SPONSOR_RESPONSE: &str = "TNT_SET_SPONSOR_RESPONSE";
const MSG_TNT_APPROVE: &str = "TNT_APPROVE";
const MSG_TNT_APPROVE_RESPONSE: &str = "TNT_APPROVE_RESPONSE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityDbSecretSource {
    /// A `postgres_url_ref` is persisted locally and was resolvable through
    /// SY.vault at the last `resolve_database_url` attempt. Note: the variant
    /// name keeps "LocalFile" for backward compatibility with state files
    /// already on disk, but the meaning is now "vault ref persisted locally".
    LocalFile,
    Missing,
}

impl IdentityDbSecretSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "vault",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone)]
struct IdentityControlState {
    schema_version: u32,
    config_version: u64,
    secret_source: IdentityDbSecretSource,
    db_ready: bool,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdentityConfigStateFile {
    schema_version: u32,
    config_version: u64,
    node_name: String,
    config: Value,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    wan: Option<WanSection>,
    #[serde(default)]
    government: Option<GovernmentSection>,
    #[serde(default)]
    identity: Option<IdentitySection>,
    #[serde(default)]
    database: Option<DatabaseSection>,
    #[serde(default)]
    system_nodes: Option<SystemNodesSection>,
}

#[derive(Debug, Deserialize)]
struct SystemNodesSection {
    #[serde(default)]
    motherbee: Option<RoleSystemNodes>,
    #[serde(default)]
    worker: Option<RoleSystemNodes>,
}

#[derive(Debug, Clone, Deserialize)]
struct RoleSystemNodes {
    nodes: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    // identity does not use wait_for; orchestrator does. Parsed for schema completeness.
    wait_for: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GovernmentSection {
    #[serde(default)]
    identity_frontdesk: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WanSection {
    #[serde(default)]
    gateway_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdentitySection {
    #[serde(default)]
    merge_alias_ttl_secs: Option<u64>,
    #[serde(default)]
    max_ilks: Option<u32>,
    #[serde(default)]
    max_tenants: Option<u32>,
    #[serde(default)]
    max_vocabulary: Option<u32>,
    #[serde(default)]
    max_ilk_aliases: Option<u32>,
    #[serde(default)]
    sync: Option<IdentitySyncSection>,
}

#[derive(Debug, Deserialize)]
struct IdentitySyncSection {
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    upstream: Option<String>,
    /// Per-hive HMAC peer-auth on the :9100 channel: "disabled" (default) |
    /// "required" (mutual challenge-response handshake at connect; reject peers
    /// without the hive's key).
    #[serde(default)]
    auth: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DatabaseSection {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IlkProvisionRequest {
    ich_id: String,
    channel_type: String,
    address: String,
    #[serde(default)]
    tenant_id: Option<String>,
    /// Optional ilk type — only the originating IO node knows whether its
    /// external counterpart is a human or an agent. Allowed values: `"human"`
    /// or `"agent"`. Defaults to `"human"` when absent. `"system"` is reserved
    /// for SY-internal creation paths and is rejected here.
    #[serde(default)]
    ilk_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IlkGetRequest {
    ilk_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IlkRegisterRequest {
    ilk_id: String,
    ilk_type: String,
    tenant_id: String,
    identification: Value,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ChannelInput {
    ich_id: String,
    #[serde(rename = "type")]
    channel_type: String,
    address: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct IlkAddChannelRequest {
    ilk_id: String,
    channel: ChannelInput,
    #[serde(default)]
    merge_from_ilk_id: Option<String>,
    #[serde(default)]
    change_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IchSetEnabledRequest {
    ich_id: String,
    enabled: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IlkUpdateRequest {
    ilk_id: String,
    #[serde(default)]
    add_channels: Vec<ChannelInput>,
    #[serde(default)]
    change_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IlkSetDefinitionRequest {
    ilk_id: String,
    definition: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IlkDeleteRequest {
    ilk_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TntCreateRequest {
    name: String,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    settings: Option<Value>,
    #[serde(default)]
    sponsor_tenant_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TntApproveRequest {
    tenant_id: String,
    approved_by: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TntGetRequest {
    tenant_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TntUpdateRequest {
    tenant_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    settings: Option<Value>,
    #[serde(default)]
    sponsor_tenant_id: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TntSetSponsorRequest {
    tenant_id: String,
    sponsor_tenant_id: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TenantRecord {
    tenant_id: String,
    name: String,
    domain: Option<String>,
    status: String,
    settings: Value,
    sponsor_tenant_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ChannelRecord {
    ich_id: String,
    channel_type: String,
    address: String,
    owner_l2_name: Option<String>,
    enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct IlkRecord {
    ilk_id: String,
    ilk_type: String,
    registration_status: String,
    tenant_id: String,
    identification: Value,
    definition: Value,
    channels: Vec<ChannelRecord>,
    deleted_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AliasRecord {
    canonical_ilk_id: String,
    expires_at_ms: u64,
}

#[derive(Debug, Default, Clone)]
struct IdentityStore {
    tenants: HashMap<String, TenantRecord>,
    ilks: HashMap<String, IlkRecord>,
    // (channel_type_lower, address_lower, tenant_id_lower) -> ilk_id
    ich_lookup: HashMap<(String, String, String), String>,
    aliases: HashMap<String, AliasRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IdentitySyncRequest {
    operation: String,
    /// Set on `DELTA_PUBLISH`: the publishing replica's hive_id, used by the
    /// primary for the per-hive authority check (a replica may only push ilks it
    /// owns, i.e. whose node_name ends with `@<hive_id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hive_id: Option<String>,
}

/// Full self-owned ilk set a replica pushes on connect + every
/// `IDENTITY_PUBLISH_RECONCILE_SECS`. The primary reconciles its view of this
/// hive to exactly this set (upsert present, hard-remove absent), so upserts and
/// hard-deletes both converge even after a reconnect gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdentityPublishSnapshot {
    operation: String,
    seq: u64,
    hive_id: String,
    ilks: Vec<IlkRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IdentityDeltaAck {
    operation: String,
    seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AliasSnapshotRecord {
    old_ilk_id: String,
    canonical_ilk_id: String,
    expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdentityFullSyncChunk {
    version: u32,
    operation: String,
    chunk: u32,
    total_chunks: u32,
    tenants: Vec<TenantRecord>,
    ilks: Vec<IlkRecord>,
    aliases: Vec<AliasSnapshotRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IdentitySyncError {
    status: String,
    error_code: String,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum IdentityDelta {
    TenantUpsert { tenant: TenantRecord },
    IlkUpsert { ilk: IlkRecord },
    IlkDelete { ilk_id: String },
    AliasUpsert { alias: AliasSnapshotRecord },
    AliasDelete { old_ilk_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdentityDeltaEnvelope {
    version: u32,
    operation: String,
    seq: u64,
    delta: IdentityDelta,
}

impl IdentityStore {
    fn tenant_child_count(&self, tenant_id: &str) -> usize {
        self.tenants
            .values()
            .filter(|entry| entry.sponsor_tenant_id.as_deref() == Some(tenant_id))
            .count()
    }

    fn tenant_ilk_count(&self, tenant_id: &str) -> usize {
        self.ilks
            .values()
            .filter(|ilk| ilk.deleted_at_ms.is_none() && ilk.tenant_id == tenant_id)
            .count()
    }

    fn tenant_summary_value(&self, tenant: &TenantRecord) -> Value {
        let child_count = self.tenant_child_count(&tenant.tenant_id);
        let ilk_count = self.tenant_ilk_count(&tenant.tenant_id);
        let is_root = tenant.sponsor_tenant_id.is_none();
        let is_sponsor = child_count > 0;
        json!({
            "tenant_id": tenant.tenant_id,
            "name": tenant.name,
            "domain": tenant.domain,
            "status": tenant.status,
            "settings": tenant.settings,
            "sponsor_tenant_id": tenant.sponsor_tenant_id,
            "child_count": child_count,
            "ilk_count": ilk_count,
            "is_root": is_root,
            "is_sponsor": is_sponsor,
        })
    }

    fn find_tenant_by_hint(
        &self,
        name: &str,
        domain: Option<&str>,
    ) -> Option<(TenantRecord, &'static str)> {
        let normalized_domain = normalize_tenant_domain(domain);
        if let Some(expected_domain) = normalized_domain.as_deref() {
            let mut tenant_ids: Vec<&String> = self.tenants.keys().collect();
            tenant_ids.sort_unstable();
            for tenant_id in tenant_ids {
                let Some(tenant) = self.tenants.get(tenant_id) else {
                    continue;
                };
                if normalize_tenant_domain(tenant.domain.as_deref()).as_deref()
                    == Some(expected_domain)
                {
                    return Some((tenant.clone(), "domain"));
                }
            }
        }

        let normalized_name = normalize_tenant_name(name);
        let mut tenant_ids: Vec<&String> = self.tenants.keys().collect();
        tenant_ids.sort_unstable();
        for tenant_id in tenant_ids {
            let Some(tenant) = self.tenants.get(tenant_id) else {
                continue;
            };
            if normalize_tenant_name(&tenant.name) == normalized_name {
                return Some((tenant.clone(), "name"));
            }
        }

        None
    }

    fn get_ilk_payload(&self, requested_ilk_id: &str) -> Result<Value, String> {
        let requested_ilk_id = requested_ilk_id.trim();
        if requested_ilk_id.is_empty() {
            return Err("INVALID_REQUEST".to_string());
        }
        let _ = parse_prefixed_uuid(requested_ilk_id, "ilk")?;

        let alias = self.aliases.get(requested_ilk_id).cloned();
        let canonical_ilk_id = alias
            .as_ref()
            .map(|entry| entry.canonical_ilk_id.clone())
            .unwrap_or_else(|| requested_ilk_id.to_string());

        let Some(ilk) = self.ilks.get(&canonical_ilk_id).cloned() else {
            return Err("NOT_FOUND".to_string());
        };

        let tenant = self.tenants.get(&ilk.tenant_id).cloned();
        let merged_aliases: Vec<Value> = self
            .aliases
            .iter()
            .filter_map(|(old_ilk_id, entry)| {
                (entry.canonical_ilk_id == canonical_ilk_id).then(|| {
                    json!({
                        "old_ilk_id": old_ilk_id,
                        "canonical_ilk_id": entry.canonical_ilk_id,
                        "expires_at_ms": entry.expires_at_ms,
                    })
                })
            })
            .collect();

        Ok(json!({
            "status": "ok",
            "ilk_id": canonical_ilk_id,
            "queried_ilk_id": requested_ilk_id,
            "canonical_ilk_id": canonical_ilk_id,
            "alias_resolved": requested_ilk_id != ilk.ilk_id,
            "queried_alias": alias.as_ref().map(|entry| json!({
                "old_ilk_id": requested_ilk_id,
                "canonical_ilk_id": entry.canonical_ilk_id,
                "expires_at_ms": entry.expires_at_ms,
            })),
            "merged_aliases": merged_aliases,
            "ilk": ilk,
            "tenant": tenant,
        }))
    }

    fn list_ilks_payload(&self) -> Value {
        let mut ilk_ids: Vec<&String> = self.ilks.keys().collect();
        ilk_ids.sort_unstable();

        let ilks: Vec<Value> = ilk_ids
            .into_iter()
            .filter_map(|ilk_id| {
                let ilk = self.ilks.get(ilk_id)?;
                let tenant = self.tenants.get(&ilk.tenant_id);
                let display_name =
                    identification_str(&ilk.identification, "display_name").map(str::to_string);
                let node_name =
                    identification_str(&ilk.identification, "node_name").map(str::to_string);
                Some(json!({
                    "ilk_id": ilk.ilk_id,
                    "ilk_type": ilk.ilk_type,
                    "registration_status": ilk.registration_status,
                    "tenant_id": ilk.tenant_id,
                    "tenant_name": tenant.map(|entry| entry.name.clone()),
                    "display_name": display_name,
                    "node_name": node_name,
                    "definition_present": agent_definition_present(&ilk.definition),
                    "channel_count": ilk.channels.len(),
                    "channels": ilk.channels.iter().map(|channel| json!({
                        "ich_id": channel.ich_id,
                        "channel_type": channel.channel_type,
                        "address": channel.address,
                        "owner_l2_name": channel.owner_l2_name,
                        "enabled": channel.enabled,
                    })).collect::<Vec<_>>(),
                    "deleted_at_ms": ilk.deleted_at_ms,
                }))
            })
            .collect();

        json!({
            "status": "ok",
            "count": ilks.len(),
            "ilks": ilks,
        })
    }

    fn get_tenant_payload(&self, tenant_id: &str) -> Result<Value, String> {
        let tenant_id = tenant_id.trim();
        if tenant_id.is_empty() {
            return Err("INVALID_REQUEST".to_string());
        }
        let _ = parse_prefixed_uuid(tenant_id, "tnt")?;
        let Some(tenant) = self.tenants.get(tenant_id).cloned() else {
            return Err("TENANT_NOT_FOUND".to_string());
        };

        let sponsor = tenant
            .sponsor_tenant_id
            .as_ref()
            .and_then(|sponsor_id| self.tenants.get(sponsor_id).cloned());
        let child_count = self.tenant_child_count(tenant_id);
        let ilk_count = self.tenant_ilk_count(tenant_id);
        let is_root = tenant.sponsor_tenant_id.is_none();
        let is_sponsor = child_count > 0;
        let mut child_tenants: Vec<TenantRecord> = self
            .tenants
            .values()
            .filter(|entry| entry.sponsor_tenant_id.as_deref() == Some(tenant_id))
            .cloned()
            .collect();
        child_tenants.sort_by(|a, b| a.tenant_id.cmp(&b.tenant_id));
        let children: Vec<Value> = child_tenants
            .iter()
            .map(|entry| self.tenant_summary_value(entry))
            .collect();

        Ok(json!({
            "status": "ok",
            "tenant_id": tenant_id,
            "tenant": tenant,
            "sponsor": sponsor,
            "child_count": child_count,
            "ilk_count": ilk_count,
            "is_root": is_root,
            "is_sponsor": is_sponsor,
            "children": children,
        }))
    }

    fn list_tenants_payload(&self) -> Value {
        let mut tenant_ids: Vec<&String> = self.tenants.keys().collect();
        tenant_ids.sort_unstable();

        let tenants: Vec<Value> = tenant_ids
            .into_iter()
            .filter_map(|tenant_id| {
                let tenant = self.tenants.get(tenant_id)?;
                Some(self.tenant_summary_value(tenant))
            })
            .collect();

        json!({
            "status": "ok",
            "count": tenants.len(),
            "tenants": tenants,
        })
    }

    fn find_active_ilk_by_identification_key(&self, key: &str, expected: &str) -> Option<String> {
        let expected = expected.trim();
        if expected.is_empty() {
            return None;
        }
        self.ilks.iter().find_map(|(ilk_id, ilk)| {
            if ilk.deleted_at_ms.is_some() {
                return None;
            }
            let value = ilk
                .identification
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if value == Some(expected) {
                Some(ilk_id.clone())
            } else {
                None
            }
        })
    }

    fn with_default_tenant() -> Self {
        let mut out = Self::default();
        out.ensure_default_root_tenant();
        out
    }

    fn ensure_default_root_tenant(&mut self) -> bool {
        match self.tenants.get_mut(DEFAULT_ROOT_TENANT_ID) {
            Some(tenant) => {
                let mut changed = false;
                if tenant.name != DEFAULT_DEFAULT_TENANT_NAME {
                    tenant.name = DEFAULT_DEFAULT_TENANT_NAME.to_string();
                    changed = true;
                }
                if tenant.status != "active" {
                    tenant.status = "active".to_string();
                    changed = true;
                }
                if tenant.domain.is_some() {
                    tenant.domain = None;
                    changed = true;
                }
                if tenant.settings != json!({}) {
                    tenant.settings = json!({});
                    changed = true;
                }
                if tenant.sponsor_tenant_id.is_some() {
                    tenant.sponsor_tenant_id = None;
                    changed = true;
                }
                changed
            }
            None => {
                self.tenants.insert(
                    DEFAULT_ROOT_TENANT_ID.to_string(),
                    TenantRecord {
                        tenant_id: DEFAULT_ROOT_TENANT_ID.to_string(),
                        name: DEFAULT_DEFAULT_TENANT_NAME.to_string(),
                        domain: None,
                        status: "active".to_string(),
                        settings: json!({}),
                        sponsor_tenant_id: None,
                    },
                );
                true
            }
        }
    }

    fn default_tenant_id(&self) -> Option<String> {
        if self.tenants.contains_key(DEFAULT_ROOT_TENANT_ID) {
            return Some(DEFAULT_ROOT_TENANT_ID.to_string());
        }
        None
    }

    fn ensure_system_ilks_from_hive(&mut self, hive: &HiveFile) -> Result<Vec<IlkRecord>, String> {
        self.ensure_default_root_tenant();
        let nodes = system_nodes_for_hive(hive)?;
        let mut changed = Vec::new();
        for base_name in nodes {
            let node_name = ensure_l2_name(&base_name, &hive.hive_id);
            let ilk_id = deterministic_system_ilk_id(&node_name);
            let next = IlkRecord {
                ilk_id: ilk_id.clone(),
                ilk_type: "system".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: DEFAULT_ROOT_TENANT_ID.to_string(),
                identification: json!({
                    "node_name": node_name,
                    "system_node": base_name,
                    "service": name_to_service(&base_name),
                    "hive_id": hive.hive_id.as_str(),
                    "source": "hive.system_nodes"
                }),
                definition: json!({}),
                channels: Vec::new(),
                deleted_at_ms: None,
            };
            let needs_upsert = self
                .ilks
                .get(&ilk_id)
                .map(|existing| {
                    existing.ilk_type != next.ilk_type
                        || existing.registration_status != next.registration_status
                        || existing.tenant_id != next.tenant_id
                        || existing.identification != next.identification
                        || existing.deleted_at_ms.is_some()
                })
                .unwrap_or(true);
            if needs_upsert {
                self.ilks.insert(ilk_id, next.clone());
                changed.push(next);
            }
        }
        Ok(changed)
    }

    fn provision_temporary_ilk(&mut self, req: IlkProvisionRequest) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.ich_id, "ich")?;
        validate_non_empty("channel_type", &req.channel_type)?;
        validate_non_empty("address", &req.address)?;
        validate_max_len("channel_type", &req.channel_type, ICH_CHANNEL_TYPE_MAX_LEN)?;
        validate_max_len("address", &req.address, ICH_ADDRESS_MAX_LEN)?;

        let ilk_type = match req.ilk_type.as_deref().map(str::trim) {
            Some(value) if !value.is_empty() => {
                validate_provision_ilk_type(value)?;
                value.to_string()
            }
            _ => "human".to_string(),
        };

        let tenant_id = req
            .tenant_id
            .clone()
            .or_else(|| self.default_tenant_id())
            .ok_or_else(|| "missing default tenant".to_string())?;
        let key = canonical_ich_key(&req.channel_type, &req.address, &tenant_id);
        if let Some(existing) = self.ich_lookup.get(&key) {
            let status = self
                .ilks
                .get(existing)
                .map(|ilk| ilk.registration_status.as_str())
                .unwrap_or("temporary");
            return Ok(json!({
                "status": "ok",
                "ilk_id": existing,
                "registration_status": status,
            }));
        }

        let ilk_id = format!("ilk:{}", Uuid::new_v4());

        let ilk = IlkRecord {
            ilk_id: ilk_id.clone(),
            ilk_type,
            registration_status: "temporary".to_string(),
            tenant_id,
            identification: json!({}),
            definition: json!({}),
            channels: vec![ChannelRecord {
                ich_id: req.ich_id,
                channel_type: req.channel_type,
                address: req.address,
                owner_l2_name: None,
                enabled: false,
            }],
            deleted_at_ms: None,
        };
        self.ich_lookup.insert(key, ilk_id.clone());
        self.ilks.insert(ilk_id.clone(), ilk);

        Ok(json!({
            "status": "ok",
            "ilk_id": ilk_id,
            "registration_status": "temporary",
        }))
    }

    fn register_ilk(&mut self, req: IlkRegisterRequest) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.ilk_id, "ilk")?;
        let _ = parse_prefixed_uuid(&req.tenant_id, "tnt")?;
        validate_ilk_type(&req.ilk_type)?;
        let Some(target_tenant) = self.tenants.get(&req.tenant_id) else {
            return Err("INVALID_TENANT".to_string());
        };
        if target_tenant.status.eq_ignore_ascii_case("pending") {
            return Err("TENANT_PENDING".to_string());
        }

        let requested_ilk_id = req.ilk_id.clone();
        let node_name = req
            .identification
            .get("node_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
        let canonical_ilk_id = node_name
            .as_deref()
            .and_then(|name| self.find_active_ilk_by_identification_key("node_name", name))
            .unwrap_or_else(|| requested_ilk_id.clone());

        match self.ilks.get_mut(&canonical_ilk_id) {
            Some(existing) => {
                if existing.deleted_at_ms.is_some() {
                    return Err("ILK_NOT_FOUND".to_string());
                }
                // Never let ILK_REGISTER mutate/downgrade a system ilk (minted
                // only by ensure_system_ilks_from_hive). Closes the node_name-match
                // hijack where a register could overwrite e.g. SY.vault@hive —
                // breaking that service's root-pool access/routing. (F-08)
                if existing.ilk_type.trim() == "system" {
                    return Err("SYSTEM_ILK_PROTECTED".to_string());
                }
                let tenant_change = existing.tenant_id != req.tenant_id;
                if tenant_change && !existing.registration_status.eq("temporary") {
                    return Err("INVALID_TENANT_TRANSITION".to_string());
                }
                existing.ilk_type = req.ilk_type;
                existing.tenant_id = req.tenant_id;
                existing.registration_status = "complete".to_string();
                existing.identification = req.identification;
            }
            None => {
                self.ilks.insert(
                    canonical_ilk_id.clone(),
                    IlkRecord {
                        ilk_id: canonical_ilk_id.clone(),
                        ilk_type: req.ilk_type,
                        registration_status: "complete".to_string(),
                        tenant_id: req.tenant_id,
                        identification: req.identification,
                        definition: json!({}),
                        channels: Vec::new(),
                        deleted_at_ms: None,
                    },
                );
            }
        }

        Ok(json!({
            "status": "ok",
            "ilk_id": canonical_ilk_id,
        }))
    }

    fn add_channel(
        &mut self,
        req: IlkAddChannelRequest,
        owner_l2_name: Option<&str>,
        merge_alias_ttl_secs: u64,
    ) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.ilk_id, "ilk")?;
        validate_channel_input(&req.channel)?;
        let normalized_owner_l2_name = normalize_optional_owner_l2_name(owner_l2_name)?;

        let canonical_ilk_id = req.ilk_id.clone();
        let response_ich_id = req.channel.ich_id.clone();
        let target = self
            .ilks
            .get_mut(&canonical_ilk_id)
            .ok_or_else(|| "ILK_NOT_FOUND".to_string())?;
        if target.deleted_at_ms.is_some() {
            return Err("ILK_NOT_FOUND".to_string());
        }

        let key = canonical_ich_key(
            &req.channel.channel_type,
            &req.channel.address,
            &target.tenant_id,
        );
        self.ich_lookup.insert(key, canonical_ilk_id.clone());

        let already = target
            .channels
            .iter()
            .any(|c| c.ich_id == req.channel.ich_id);
        if !already {
            target.channels.push(ChannelRecord {
                ich_id: req.channel.ich_id,
                channel_type: req.channel.channel_type,
                address: req.channel.address,
                owner_l2_name: normalized_owner_l2_name.clone(),
                enabled: false,
            });
        }

        if let Some(old_ilk) = req.merge_from_ilk_id {
            let _ = parse_prefixed_uuid(&old_ilk, "ilk")?;
            if old_ilk == canonical_ilk_id {
                return Err("INVALID_MERGE_SOURCE".to_string());
            }
            let source = self
                .ilks
                .get(&old_ilk)
                .ok_or_else(|| "INVALID_MERGE_SOURCE".to_string())?;
            if source.deleted_at_ms.is_some() || source.registration_status != "temporary" {
                return Err("INVALID_MERGE_SOURCE".to_string());
            }

            let source_tenant_id = source.tenant_id.clone();
            let source_channels = source.channels.clone();
            let source_keys: Vec<(String, String, String)> = source_channels
                .iter()
                .map(|ch| canonical_ich_key(&ch.channel_type, &ch.address, &source_tenant_id))
                .collect();

            let canonical = self
                .ilks
                .get_mut(&canonical_ilk_id)
                .ok_or_else(|| "ILK_NOT_FOUND".to_string())?;
            for ch in source_channels {
                if !canonical
                    .channels
                    .iter()
                    .any(|existing| existing.ich_id == ch.ich_id)
                {
                    canonical.channels.push(ch);
                }
            }
            for key in source_keys {
                self.ich_lookup.insert(key, canonical_ilk_id.clone());
            }

            let ttl_ms = merge_alias_ttl_secs.saturating_mul(1000);
            let expires_at_ms = now_epoch_ms().saturating_add(ttl_ms);
            self.aliases.insert(
                old_ilk.clone(),
                AliasRecord {
                    canonical_ilk_id: canonical_ilk_id.clone(),
                    expires_at_ms,
                },
            );
        }

        Ok(json!({
            "status": "ok",
            "ilk_id": canonical_ilk_id,
            "ich_id": response_ich_id,
            "owner_l2_name": normalized_owner_l2_name,
            "enabled": false,
            "change_reason": req.change_reason,
        }))
    }

    fn update_ilk(&mut self, req: IlkUpdateRequest) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.ilk_id, "ilk")?;

        let entry = self
            .ilks
            .get_mut(&req.ilk_id)
            .ok_or_else(|| "ILK_NOT_FOUND".to_string())?;
        if entry.deleted_at_ms.is_some() {
            return Err("ILK_NOT_FOUND".to_string());
        }

        for ch in &req.add_channels {
            validate_channel_input(ch)?;
            let key = canonical_ich_key(&ch.channel_type, &ch.address, &entry.tenant_id);
            self.ich_lookup.insert(key, req.ilk_id.clone());
            let exists = entry.channels.iter().any(|c| c.ich_id == ch.ich_id);
            if !exists {
                entry.channels.push(ChannelRecord {
                    ich_id: ch.ich_id.clone(),
                    channel_type: ch.channel_type.clone(),
                    address: ch.address.clone(),
                    owner_l2_name: None,
                    enabled: false,
                });
            }
        }

        Ok(json!({
            "status": "ok",
            "ilk_id": req.ilk_id,
            "change_reason": req.change_reason,
        }))
    }

    fn set_ilk_definition(&mut self, req: IlkSetDefinitionRequest) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.ilk_id, "ilk")?;
        let entry = self
            .ilks
            .get_mut(&req.ilk_id)
            .ok_or_else(|| "ILK_NOT_FOUND".to_string())?;
        if entry.deleted_at_ms.is_some() {
            return Err("ILK_NOT_FOUND".to_string());
        }
        if entry.ilk_type != "agent" {
            return Err("INVALID_ILK_TYPE".to_string());
        }
        let normalized = normalize_agent_definition(&req.definition)?;
        entry.definition = normalized.clone();
        Ok(json!({
            "status": "ok",
            "ilk_id": req.ilk_id,
            "definition": normalized,
        }))
    }

    /// Enumerate alias `old_ilk_id`s that reference `ilk_id` either as their
    /// own key or as their canonical target. Used by the `MSG_ILK_DELETE`
    /// handler to emit explicit `AliasDelete` deltas so replicas and the
    /// identity SHM converge — `apply_identity_shm_event`'s `IlkDelete` arm
    /// only clears ich/ilk entries, never aliases.
    fn alias_old_ids_referencing(&self, ilk_id: &str) -> Vec<String> {
        self.aliases
            .iter()
            .filter_map(|(old_ilk_id, alias)| {
                (old_ilk_id == ilk_id || alias.canonical_ilk_id == ilk_id)
                    .then(|| old_ilk_id.clone())
            })
            .collect()
    }

    fn delete_ilk(&mut self, req: IlkDeleteRequest) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.ilk_id, "ilk")?;
        let entry = self
            .ilks
            .get(&req.ilk_id)
            .ok_or_else(|| "ILK_NOT_FOUND".to_string())?;
        if is_well_known_system_ilk(entry) {
            return Err("SYSTEM_ILK_PROTECTED".to_string());
        }
        let removed_aliases = self.alias_old_ids_referencing(&req.ilk_id);
        for old_ilk_id in &removed_aliases {
            self.aliases.remove(old_ilk_id);
        }
        self.ich_lookup
            .retain(|_, mapped_ilk| mapped_ilk != &req.ilk_id);
        self.ilks.remove(&req.ilk_id);
        Ok(json!({
            "status": "ok",
            "ilk_id": req.ilk_id,
            "removed_alias_count": removed_aliases.len(),
        }))
    }

    fn create_tenant(&mut self, req: TntCreateRequest) -> Result<Value, String> {
        validate_non_empty("name", &req.name)?;
        if let Some(sponsor_tenant_id) = req.sponsor_tenant_id.as_deref() {
            let _ = parse_prefixed_uuid(sponsor_tenant_id, "tnt")?;
            if !self.tenants.contains_key(sponsor_tenant_id) {
                return Err("INVALID_SPONSOR_TENANT".to_string());
            }
        }
        let normalized_name = req.name.trim().to_string();
        let normalized_domain = req
            .domain
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
        let status = req
            .status
            .unwrap_or_else(|| "pending".to_string())
            .trim()
            .to_ascii_lowercase();
        if !matches!(status.as_str(), "pending" | "active" | "suspended") {
            return Err("INVALID_REQUEST".to_string());
        }

        if let Some((tenant, matched_by)) =
            self.find_tenant_by_hint(&normalized_name, normalized_domain.as_deref())
        {
            return Ok(json!({
                "status": "ok",
                "tenant_id": tenant.tenant_id,
                "created": false,
                "matched_by": matched_by,
                "sponsor_tenant_id": tenant.sponsor_tenant_id,
            }));
        }

        let tenant_id = format!("tnt:{}", Uuid::new_v4());
        let sponsor_tenant_id = req.sponsor_tenant_id.clone();
        self.tenants.insert(
            tenant_id.clone(),
            TenantRecord {
                tenant_id: tenant_id.clone(),
                name: normalized_name,
                domain: normalized_domain,
                status,
                settings: req.settings.unwrap_or_else(|| json!({})),
                sponsor_tenant_id: sponsor_tenant_id.clone(),
            },
        );

        Ok(json!({
            "status": "ok",
            "tenant_id": tenant_id,
            "created": true,
            "matched_by": serde_json::Value::Null,
            "sponsor_tenant_id": sponsor_tenant_id,
        }))
    }

    fn validate_sponsor_assignment(
        &self,
        tenant_id: &str,
        sponsor_tenant_id: Option<&str>,
    ) -> Result<Option<String>, String> {
        let tenant_id = tenant_id.trim();
        let _ = parse_prefixed_uuid(tenant_id, "tnt")?;
        let Some(sponsor_tenant_id) = sponsor_tenant_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let _ = parse_prefixed_uuid(sponsor_tenant_id, "tnt")?;
        if sponsor_tenant_id == tenant_id {
            return Err("INVALID_SPONSOR_RELATION".to_string());
        }
        if !self.tenants.contains_key(sponsor_tenant_id) {
            return Err("INVALID_SPONSOR_TENANT".to_string());
        }

        let mut cursor = Some(sponsor_tenant_id.to_string());
        while let Some(current_id) = cursor {
            if current_id == tenant_id {
                return Err("INVALID_SPONSOR_RELATION".to_string());
            }
            cursor = self
                .tenants
                .get(&current_id)
                .and_then(|tenant| tenant.sponsor_tenant_id.clone());
        }

        Ok(Some(sponsor_tenant_id.to_string()))
    }

    fn parse_optional_sponsor_update(
        &self,
        tenant_id: &str,
        sponsor_tenant_id: Option<Value>,
    ) -> Result<Option<Option<String>>, String> {
        let Some(raw_value) = sponsor_tenant_id else {
            return Ok(None);
        };
        match raw_value {
            Value::Null => Ok(Some(None)),
            Value::String(value) => self
                .validate_sponsor_assignment(tenant_id, Some(&value))
                .map(Some),
            _ => Err("INVALID_REQUEST".to_string()),
        }
    }

    fn update_tenant(&mut self, req: TntUpdateRequest) -> Result<Value, String> {
        let tenant_id = req.tenant_id.trim().to_string();
        let _ = parse_prefixed_uuid(&tenant_id, "tnt")?;
        let sponsor_update =
            self.parse_optional_sponsor_update(&tenant_id, req.sponsor_tenant_id)?;

        let tenant = self
            .tenants
            .get_mut(&tenant_id)
            .ok_or_else(|| "TENANT_NOT_FOUND".to_string())?;

        if let Some(name) = req.name {
            validate_non_empty("name", &name)?;
            tenant.name = name.trim().to_string();
        }
        if let Some(domain) = req.domain {
            tenant.domain = normalize_tenant_domain(Some(&domain));
        }
        if let Some(status) = req.status {
            let status = status.trim().to_ascii_lowercase();
            if !matches!(status.as_str(), "pending" | "active" | "suspended") {
                return Err("INVALID_REQUEST".to_string());
            }
            tenant.status = status;
        }
        if let Some(settings) = req.settings {
            tenant.settings = settings;
        }
        if let Some(sponsor_tenant_id) = sponsor_update {
            tenant.sponsor_tenant_id = sponsor_tenant_id;
        }

        Ok(json!({
            "status": "ok",
            "tenant_id": tenant_id,
            "sponsor_tenant_id": tenant.sponsor_tenant_id,
        }))
    }

    fn set_tenant_sponsor(&mut self, req: TntSetSponsorRequest) -> Result<Value, String> {
        self.update_tenant(TntUpdateRequest {
            tenant_id: req.tenant_id,
            name: None,
            domain: None,
            status: None,
            settings: None,
            sponsor_tenant_id: Some(req.sponsor_tenant_id),
        })
    }

    fn set_ich_enabled(&mut self, ich_id: &str, enabled: bool) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(ich_id, "ich")?;
        let mut updated: Option<(String, Option<String>)> = None;
        for (ilk_id, ilk) in &mut self.ilks {
            if ilk.deleted_at_ms.is_some() {
                continue;
            }
            if let Some(channel) = ilk
                .channels
                .iter_mut()
                .find(|channel| channel.ich_id == ich_id)
            {
                channel.enabled = enabled;
                updated = Some((ilk_id.clone(), channel.owner_l2_name.clone()));
                break;
            }
        }
        let Some((ilk_id, owner_l2_name)) = updated else {
            return Err("ICH_NOT_FOUND".to_string());
        };
        Ok(json!({
            "status": "ok",
            "ilk_id": ilk_id,
            "ich_id": ich_id,
            "enabled": enabled,
            "owner_l2_name": owner_l2_name,
        }))
    }

    fn ich_owner_l2_name(&self, ich_id: &str) -> Result<Option<String>, String> {
        let _ = parse_prefixed_uuid(ich_id, "ich")?;
        for ilk in self.ilks.values() {
            if ilk.deleted_at_ms.is_some() {
                continue;
            }
            if let Some(channel) = ilk.channels.iter().find(|channel| channel.ich_id == ich_id) {
                return Ok(channel.owner_l2_name.clone());
            }
        }
        Err("ICH_NOT_FOUND".to_string())
    }

    fn approve_tenant(&mut self, req: TntApproveRequest) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.tenant_id, "tnt")?;
        let _ = parse_prefixed_uuid(&req.approved_by, "ilk")?;
        let tenant = self
            .tenants
            .get_mut(&req.tenant_id)
            .ok_or_else(|| "INVALID_TENANT".to_string())?;
        tenant.status = "active".to_string();

        Ok(json!({
            "status": "ok",
            "tenant_id": req.tenant_id,
            "approved_by": req.approved_by,
        }))
    }

    fn metrics(&self) -> Value {
        let deleted_ilks = self
            .ilks
            .values()
            .filter(|entry| entry.deleted_at_ms.is_some())
            .count();
        json!({
            "tenant_count": self.tenants.len(),
            "ilk_count": self.ilks.len(),
            "ich_count": self.ich_lookup.len(),
            "alias_count": self.aliases.len(),
            "deleted_ilk_count": deleted_ilks,
        })
    }

    fn gc_expired_aliases(&mut self, now_ms: u64) -> usize {
        let mut expired_old_ids = Vec::new();
        self.aliases.retain(|old_ilk_id, entry| {
            let keep = entry.expires_at_ms > now_ms;
            if !keep {
                expired_old_ids.push(old_ilk_id.clone());
            }
            keep
        });

        for old_ilk_id in &expired_old_ids {
            if let Some(ilk) = self.ilks.get_mut(old_ilk_id) {
                if ilk.registration_status == "temporary" && ilk.deleted_at_ms.is_none() {
                    ilk.deleted_at_ms = Some(now_ms);
                }
            }
        }
        if !expired_old_ids.is_empty() {
            self.ich_lookup
                .retain(|_, ilk_id| !expired_old_ids.iter().any(|old| old == ilk_id));
        }
        expired_old_ids.len()
    }

    fn build_full_sync_chunks(&self, chunk_items: usize) -> Vec<IdentityFullSyncChunk> {
        let chunk_items = chunk_items.max(1);
        let mut tenants: Vec<TenantRecord> = self.tenants.values().cloned().collect();
        let mut ilks: Vec<IlkRecord> = self.ilks.values().cloned().collect();
        let mut aliases: Vec<AliasSnapshotRecord> = self
            .aliases
            .iter()
            .map(|(old_ilk_id, alias)| AliasSnapshotRecord {
                old_ilk_id: old_ilk_id.clone(),
                canonical_ilk_id: alias.canonical_ilk_id.clone(),
                expires_at_ms: alias.expires_at_ms,
            })
            .collect();
        tenants.sort_by(|a, b| a.tenant_id.cmp(&b.tenant_id));
        ilks.sort_by(|a, b| a.ilk_id.cmp(&b.ilk_id));
        aliases.sort_by(|a, b| a.old_ilk_id.cmp(&b.old_ilk_id));

        let tenant_chunks = tenants.len().div_ceil(chunk_items);
        let ilk_chunks = ilks.len().div_ceil(chunk_items);
        let alias_chunks = aliases.len().div_ceil(chunk_items);
        let total_chunks = tenant_chunks.max(ilk_chunks).max(alias_chunks).max(1);

        let mut out = Vec::with_capacity(total_chunks);
        for i in 0..total_chunks {
            out.push(IdentityFullSyncChunk {
                version: IDENTITY_SYNC_VERSION,
                operation: SYNC_OP_FULL_SYNC.to_string(),
                chunk: (i + 1) as u32,
                total_chunks: total_chunks as u32,
                tenants: slice_chunk(&tenants, i, chunk_items),
                ilks: slice_chunk(&ilks, i, chunk_items),
                aliases: slice_chunk(&aliases, i, chunk_items),
            });
        }
        out
    }

    fn from_full_sync_chunks(chunks: &[IdentityFullSyncChunk]) -> Result<Self, String> {
        let mut ordered = chunks.to_vec();
        ordered.sort_by_key(|chunk| chunk.chunk);

        let mut store = IdentityStore::default();

        for chunk in &ordered {
            for tenant in &chunk.tenants {
                store
                    .tenants
                    .insert(tenant.tenant_id.clone(), tenant.clone());
            }
            for ilk in &chunk.ilks {
                store.ilks.insert(ilk.ilk_id.clone(), ilk.clone());
            }
            for alias in &chunk.aliases {
                store.aliases.insert(
                    alias.old_ilk_id.clone(),
                    AliasRecord {
                        canonical_ilk_id: alias.canonical_ilk_id.clone(),
                        expires_at_ms: alias.expires_at_ms,
                    },
                );
            }
        }

        for (ilk_id, ilk) in &store.ilks {
            if ilk.deleted_at_ms.is_some() {
                continue;
            }
            for channel in &ilk.channels {
                let key =
                    canonical_ich_key(&channel.channel_type, &channel.address, &ilk.tenant_id);
                store.ich_lookup.insert(key, ilk_id.clone());
            }
        }

        if store.tenants.is_empty() {
            return Err("full sync payload did not include tenants".to_string());
        }
        Ok(store)
    }

    fn apply_delta(&mut self, delta: IdentityDelta) {
        match delta {
            IdentityDelta::TenantUpsert { tenant } => {
                self.tenants.insert(tenant.tenant_id.clone(), tenant);
            }
            IdentityDelta::IlkUpsert { ilk } => {
                let ilk_id = ilk.ilk_id.clone();
                self.ich_lookup
                    .retain(|_, mapped_ilk| mapped_ilk != &ilk_id);
                if ilk.deleted_at_ms.is_none() {
                    for channel in &ilk.channels {
                        let key = canonical_ich_key(
                            &channel.channel_type,
                            &channel.address,
                            &ilk.tenant_id,
                        );
                        self.ich_lookup.insert(key, ilk_id.clone());
                    }
                }
                self.ilks.insert(ilk_id, ilk);
            }
            IdentityDelta::IlkDelete { ilk_id } => {
                // Never hard-remove a well-known system ilk — mirrors the
                // delete_ilk handler's SYSTEM_ILK_PROTECTED guard, and closes the
                // forged-snapshot DOS where a publish snapshot omitting
                // SY.identity@<hive> would otherwise reconcile-delete it mesh-wide.
                if self
                    .ilks
                    .get(&ilk_id)
                    .map(is_well_known_system_ilk)
                    .unwrap_or(false)
                {
                    return;
                }
                self.aliases.retain(|old_ilk_id, alias| {
                    old_ilk_id != &ilk_id && alias.canonical_ilk_id != ilk_id
                });
                self.ich_lookup
                    .retain(|_, mapped_ilk| mapped_ilk != &ilk_id);
                self.ilks.remove(&ilk_id);
            }
            IdentityDelta::AliasUpsert { alias } => {
                self.aliases.insert(
                    alias.old_ilk_id.clone(),
                    AliasRecord {
                        canonical_ilk_id: alias.canonical_ilk_id,
                        expires_at_ms: alias.expires_at_ms,
                    },
                );
            }
            IdentityDelta::AliasDelete { old_ilk_id } => {
                self.aliases.remove(&old_ilk_id);
            }
        }
    }

    /// All ilks owned by `hive_id` (active **and** soft-deleted tombstones), for a
    /// replica's reconciliation snapshot. Ownership is the `@<hive>` suffix of the
    /// ilk's L2 `node_name` (the canonical, forge-proof owner — see
    /// `ilk_owning_hive`).
    fn self_owned_ilks(&self, hive_id: &str) -> Vec<IlkRecord> {
        self.ilks
            .values()
            .filter(|ilk| ilk_owning_hive(ilk).as_deref() == Some(hive_id))
            .cloned()
            .collect()
    }

    /// Reconcile this store's view of `hive_id`'s ilks to exactly `incoming`
    /// (a replica's authoritative self-owned set): upsert everything in
    /// `incoming`, and hard-remove any ilk this store currently attributes to
    /// `hive_id` that is absent from `incoming` (recovers hard-deletes). Only
    /// touches ilks owned by `hive_id` — other hives' ilks are never affected
    /// (additive union across hives). Returns the deltas applied, for broadcast.
    fn reconcile_hive_ilks(
        &mut self,
        hive_id: &str,
        incoming: Vec<IlkRecord>,
    ) -> Vec<IdentityDelta> {
        let incoming_ids: HashSet<String> = incoming.iter().map(|ilk| ilk.ilk_id.clone()).collect();
        let stale: Vec<String> = self
            .ilks
            .values()
            .filter(|ilk| {
                ilk_owning_hive(ilk).as_deref() == Some(hive_id)
                    && !incoming_ids.contains(&ilk.ilk_id)
                    // Well-known system ilks are never reconcile-removed (a forged
                    // snapshot that omits them must not delete them); apply_delta
                    // enforces this too as a net.
                    && !is_well_known_system_ilk(ilk)
            })
            .map(|ilk| ilk.ilk_id.clone())
            .collect();
        let mut deltas = Vec::new();
        for ilk in incoming {
            // Never overwrite an ilk_id that already belongs to a different hive
            // (key-collision guard, mirrors delta_authorized_for_hive).
            if let Some(existing) = self.ilks.get(&ilk.ilk_id) {
                if ilk_owning_hive(existing).as_deref() != Some(hive_id) {
                    continue;
                }
                // Skip a no-op upsert (already byte-identical) to avoid churn.
                if existing == &ilk {
                    continue;
                }
            }
            self.apply_delta(IdentityDelta::IlkUpsert { ilk: ilk.clone() });
            deltas.push(IdentityDelta::IlkUpsert { ilk });
        }
        for ilk_id in stale {
            self.apply_delta(IdentityDelta::IlkDelete {
                ilk_id: ilk_id.clone(),
            });
            deltas.push(IdentityDelta::IlkDelete { ilk_id });
        }
        deltas
    }
}

/// The hive that owns an ilk = the `@<hive>` suffix of its L2 `node_name`. This
/// is the forge-proof owner used for the per-hive authority check: a replica may
/// only push ilks whose node_name ends with `@<its-own-hive>`. Falls back to the
/// explicit `identification.hive_id` only when the node_name carries no `@`.
fn ilk_owning_hive(ilk: &IlkRecord) -> Option<String> {
    if let Some(node_name) = ilk.identification.get("node_name").and_then(|v| v.as_str()) {
        if let Some((_, hive)) = node_name.rsplit_once('@') {
            let hive = hive.trim();
            if !hive.is_empty() {
                return Some(hive.to_string());
            }
        }
    }
    ilk.identification
        .get("hive_id")
        .and_then(|v| v.as_str())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
}

/// A replica-pushed ilk may assert `ilk_type = "system"` ONLY when it is a
/// genuine deterministic SY.* system ilk of the publisher's own hive: node_name
/// `SY.<svc>@<publisher_hive>` whose `ilk_id` is the deterministic system id.
/// This stops a compromised/buggy worker from relabelling an arbitrary owned
/// (agent/human) ilk as `system` — the exact type `sy_vault::authorize_read`
/// treats as a root-tenant pool master key. Non-system types are always allowed
/// for owned ilks (unknown strings fail safe to non-privileged in SHM).
fn ingest_ilk_type_authorized(ilk: &IlkRecord, publisher_hive: &str) -> bool {
    if ilk.ilk_type.trim() != "system" {
        return true;
    }
    let Some(node_name) = ilk
        .identification
        .get("node_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
    else {
        return false;
    };
    if !node_name.starts_with("SY.") {
        return false;
    }
    if ilk_owning_hive(ilk).as_deref() != Some(publisher_hive) {
        return false;
    }
    ilk.ilk_id == deterministic_system_ilk_id(node_name)
}

/// Per-hive authority: is `publisher_hive` allowed to push this delta? A replica
/// may only upsert/delete ilks it owns (`@<publisher_hive>`). For `IlkDelete`
/// (which carries only an ilk_id) ownership is resolved against the existing ilk
/// in `store`; an unknown id is rejected, so a replica can neither forge a
/// `@motherbee`/`@other-hive` ilk nor delete one it does not own. TenantUpsert
/// is primary-only and never accepted from a replica.
fn delta_authorized_for_hive(
    delta: &IdentityDelta,
    publisher_hive: &str,
    store: &IdentityStore,
) -> bool {
    match delta {
        IdentityDelta::IlkUpsert { ilk } => {
            // Claimed owner (node_name `@hive`) must be the publisher, AND the
            // ilk_id must not already belong to a different hive — otherwise a
            // replica could overwrite another hive's ilk by reusing its ilk_id
            // with a forged `@self` node_name (key-collision attack).
            if ilk_owning_hive(ilk).as_deref() != Some(publisher_hive) {
                return false;
            }
            // A replica must not mint a privileged `system` identity for an ilk
            // it merely owns; `system` is bound to the deterministic SY.* shape.
            if !ingest_ilk_type_authorized(ilk, publisher_hive) {
                return false;
            }
            match store.ilks.get(&ilk.ilk_id) {
                Some(existing) => ilk_owning_hive(existing).as_deref() == Some(publisher_hive),
                None => true,
            }
        }
        IdentityDelta::IlkDelete { ilk_id } => {
            store.ilks.get(ilk_id).and_then(ilk_owning_hive).as_deref() == Some(publisher_hive)
        }
        IdentityDelta::AliasUpsert { alias } => {
            // Both the redirected source id (`old_ilk_id`, the key the alias is
            // stored under and the id whose resolution it hijacks) AND the
            // canonical target must be owned by the publisher. Checking only the
            // canonical let a replica shadow ANY ilk id — including other hives'
            // system ilks — by aliasing it onto an ilk it owns. Never allow
            // shadowing a well-known system ilk's id.
            if store
                .ilks
                .get(&alias.old_ilk_id)
                .map(is_well_known_system_ilk)
                .unwrap_or(false)
            {
                return false;
            }
            let owned_by_publisher = |id: &str| {
                store.ilks.get(id).and_then(ilk_owning_hive).as_deref() == Some(publisher_hive)
            };
            owned_by_publisher(&alias.old_ilk_id) && owned_by_publisher(&alias.canonical_ilk_id)
        }
        IdentityDelta::AliasDelete { old_ilk_id } => {
            store
                .ilks
                .get(old_ilk_id)
                .and_then(ilk_owning_hive)
                .as_deref()
                == Some(publisher_hive)
        }
        // Tenants are primary-authoritative; never accepted from a replica push.
        IdentityDelta::TenantUpsert { .. } => false,
    }
}

struct IdentityRuntime {
    hive_id: String,
    is_primary: bool,
    db_config: Option<PgConfig>,
    merge_alias_ttl_secs: u64,
    store: IdentityStore,
    // action -> allowed prefixes by node name
    // e.g. "ILK_PROVISION" -> ["IO."]
    // special full names allowed are represented as exacts in `allowed_exacts`.
    allowed_prefixes: HashMap<&'static str, Vec<&'static str>>,
    allowed_exacts: HashMap<&'static str, HashSet<String>>,
}

impl IdentityRuntime {
    fn new(
        hive: &HiveFile,
        _state_dir: PathBuf,
        is_primary: bool,
        db_config: Option<PgConfig>,
    ) -> Self {
        let mut allowed_prefixes: HashMap<&'static str, Vec<&'static str>> = HashMap::new();
        allowed_prefixes.insert(MSG_ILK_PROVISION, vec!["IO."]);
        allowed_prefixes.insert(MSG_ILK_LIST, vec!["SY.admin@"]);
        allowed_prefixes.insert(MSG_ILK_GET, vec!["SY.admin@"]);
        allowed_prefixes.insert(MSG_TNT_LIST, vec!["SY.admin@"]);
        allowed_prefixes.insert(MSG_TNT_GET, vec!["SY.admin@"]);
        allowed_prefixes.insert(
            MSG_ILK_REGISTER,
            vec!["SY.frontdesk.gov@", "SY.orchestrator@"],
        );
        allowed_prefixes.insert(MSG_ILK_ADD_CHANNEL, vec!["IO.", "SY.frontdesk.gov@"]);
        allowed_prefixes.insert(MSG_ILK_UPDATE, vec!["SY.orchestrator@"]);
        allowed_prefixes.insert(MSG_ILK_SET_DEFINITION, vec!["SY.admin@", "SY.architect@"]);
        allowed_prefixes.insert(MSG_ILK_DELETE, vec!["SY.admin@", "SY.orchestrator@"]);
        allowed_prefixes.insert(
            MSG_ICH_SET_ENABLED,
            vec!["IO.", "SY.admin@", "SY.architect@", "SY.frontdesk.gov@"],
        );
        allowed_prefixes.insert(
            MSG_TNT_CREATE,
            vec!["SY.admin@", "SY.architect@", "SY.frontdesk.gov@"],
        );
        allowed_prefixes.insert(
            MSG_TNT_UPDATE,
            vec!["SY.admin@", "SY.architect@", "SY.frontdesk.gov@"],
        );
        allowed_prefixes.insert(
            MSG_TNT_SET_SPONSOR,
            vec!["SY.admin@", "SY.architect@", "SY.frontdesk.gov@"],
        );
        allowed_prefixes.insert(MSG_TNT_APPROVE, vec!["SY.admin@"]);
        allowed_prefixes.insert("CONFIG_GET", vec!["SY.admin@"]);
        allowed_prefixes.insert("CONFIG_SET", vec!["SY.admin@"]);

        let mut allowed_exacts: HashMap<&'static str, HashSet<String>> = HashMap::new();
        let mut bootstrap = HashSet::new();
        bootstrap.insert(format!("SY.identity@{}", hive.hive_id));
        allowed_exacts.insert(MSG_ILK_REGISTER, bootstrap.clone());
        allowed_exacts.insert(MSG_ILK_UPDATE, bootstrap.clone());
        allowed_exacts.insert(MSG_ILK_SET_DEFINITION, bootstrap.clone());
        allowed_exacts.insert(MSG_ILK_DELETE, bootstrap);
        if let Some(frontdesk_node) = configured_identity_frontdesk_node_name(hive) {
            allowed_exacts
                .entry(MSG_ILK_REGISTER)
                .or_default()
                .insert(frontdesk_node.clone());
            allowed_exacts
                .entry(MSG_ILK_ADD_CHANNEL)
                .or_default()
                .insert(frontdesk_node.clone());
            allowed_exacts
                .entry(MSG_ICH_SET_ENABLED)
                .or_default()
                .insert(frontdesk_node.clone());
            allowed_exacts
                .entry(MSG_TNT_CREATE)
                .or_default()
                .insert(frontdesk_node.clone());
            allowed_exacts
                .entry(MSG_TNT_UPDATE)
                .or_default()
                .insert(frontdesk_node.clone());
            allowed_exacts
                .entry(MSG_TNT_SET_SPONSOR)
                .or_default()
                .insert(frontdesk_node);
        }

        let merge_alias_ttl_secs = hive
            .identity
            .as_ref()
            .and_then(|cfg| cfg.merge_alias_ttl_secs)
            .unwrap_or(DEFAULT_MERGE_ALIAS_TTL_SECS);

        Self {
            hive_id: hive.hive_id.clone(),
            is_primary,
            db_config,
            merge_alias_ttl_secs,
            store: IdentityStore::with_default_tenant(),
            allowed_prefixes,
            allowed_exacts,
        }
    }

    async fn process_system_message(
        &mut self,
        sender: &NodeSender,
        msg: &Message,
        identity_shm: Option<&mut IdentityRegionWriter>,
        control_state: &mut IdentityControlState,
        node_name: &str,
    ) -> Result<Vec<IdentityDeltaEnvelope>, IdentityError> {
        if try_handle_default_node_status(sender, msg).await? {
            return Ok(Vec::new());
        }
        let Some(action) = msg.meta.msg.as_deref() else {
            return Ok(Vec::new());
        };
        let action_started = Instant::now();
        let trace_id = msg.routing.trace_id.clone();
        let src_l2_name = msg.routing.src_l2_name.as_deref();

        if action == MSG_ILK_PROVISION {
            tracing::info!(
                action,
                trace_id = %trace_id,
                src_uuid = %msg.routing.src,
                src_l2_name = src_l2_name.unwrap_or("<unknown>"),
                payload = %msg.payload,
                "identity received ILK_PROVISION request"
            );
        }
        // Vault broadcast: handled before the table-driven auth check (which has
        // no entry for VAULT_SECRET_CHANGED). handle_vault_secret_changed does
        // its OWN fail-closed origin check on the router-stamped src_l2_name so a
        // forged broadcast cannot restart-loop the identity primary.
        if action == MSG_VAULT_SECRET_CHANGED {
            handle_vault_secret_changed(
                msg,
                self.is_primary,
                node_name,
                src_l2_name,
                &self.hive_id,
            );
            return Ok(Vec::new());
        }
        if !self.is_authorized(action, src_l2_name) {
            let payload =
                unauthorized_identity_source_payload(action, &msg.routing.src, src_l2_name);
            send_system_response(sender, msg, response_name(action), payload).await?;
            return Ok(Vec::new());
        }
        if action == "CONFIG_GET" {
            let payload =
                build_identity_config_get_payload(self.is_primary, node_name, control_state);
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            sender.send(response).await?;
            return Ok(Vec::new());
        }
        if action == "CONFIG_SET" {
            let payload =
                apply_identity_config_set(msg, self.is_primary, node_name, control_state)?;
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            sender.send(response).await?;
            return Ok(Vec::new());
        }
        if action == "PING" {
            let payload = json!({
                "status": "ok",
                "ok": true,
                "node_name": node_name,
                "state": identity_state_label(self.is_primary, control_state),
                "database": {
                    "mode": "postgres",
                    "source": identity_effective_source(self.is_primary, control_state),
                    "configured": identity_secret_configured(self.is_primary, control_state),
                    "ready": control_state.db_ready
                }
            });
            send_system_response(sender, msg, "PONG", payload).await?;
            return Ok(Vec::new());
        }
        if action == "STATUS" {
            let payload = json!({
                "status": "ok",
                "ok": true,
                "node_name": node_name,
                "role": if self.is_primary { "primary" } else { "replica" },
                "state": identity_state_label(self.is_primary, control_state),
                "schema_version": control_state.schema_version,
                "config_version": control_state.config_version,
                "database": {
                    "mode": "postgres",
                    "source": identity_effective_source(self.is_primary, control_state),
                    "configured": identity_secret_configured(self.is_primary, control_state),
                    "ready": control_state.db_ready,
                    "last_error": control_state.last_error.clone()
                }
            });
            send_system_response(sender, msg, "STATUS_RESPONSE", payload).await?;
            return Ok(Vec::new());
        }
        if !self.is_primary && action_requires_primary(action) {
            let payload = json!({
                "status": "error",
                "error_code": "NOT_PRIMARY",
                "message": "identity replica is read-only for this action; route request to primary",
                "action": action,
                "replica_hive_id": self.hive_id,
            });
            send_system_response(sender, msg, response_name(action), payload).await?;
            return Ok(Vec::new());
        }
        if self.is_primary && action_requires_primary(action) && self.db_config.is_none() {
            let payload = json!({
                "status": "error",
                "error_code": "DB_NOT_READY",
                "message": control_state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "identity primary DB is not ready; configure DB secret and restart sy-identity".to_string()),
                "action": action,
                "node_name": node_name,
                "state": identity_state_label(self.is_primary, control_state),
            });
            send_system_response(sender, msg, response_name(action), payload).await?;
            return Ok(Vec::new());
        }

        let mut deltas: Vec<IdentityDeltaEnvelope> = Vec::new();
        let payload = match action {
            MSG_ILK_LIST => self.store.list_ilks_payload(),
            MSG_ILK_GET => match serde_json::from_value::<IlkGetRequest>(msg.payload.clone()) {
                Ok(req) => match self.store.get_ilk_payload(&req.ilk_id) {
                    Ok(ok) => ok,
                    Err(code) => error_payload(&code, "failed to get ilk"),
                },
                Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
            },
            MSG_TNT_LIST => self.store.list_tenants_payload(),
            MSG_TNT_GET => match serde_json::from_value::<TntGetRequest>(msg.payload.clone()) {
                Ok(req) => match self.store.get_tenant_payload(&req.tenant_id) {
                    Ok(ok) => ok,
                    Err(code) => error_payload(&code, "failed to get tenant"),
                },
                Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
            },
            MSG_ILK_PROVISION => {
                match serde_json::from_value::<IlkProvisionRequest>(msg.payload.clone()) {
                    Ok(req) => {
                        let snapshot = if self.is_primary && self.db_config.is_some() {
                            Some(self.store.clone())
                        } else {
                            None
                        };
                        let provision_started = Instant::now();
                        match self.store.provision_temporary_ilk(req) {
                            Ok(ok) => {
                                tracing::info!(
                                    action,
                                    trace_id = %trace_id,
                                    elapsed_us = provision_started.elapsed().as_micros() as u64,
                                    response = %ok,
                                    "identity store provision completed"
                                );
                                if let Some(ilk_id) = ok.get("ilk_id").and_then(Value::as_str) {
                                    if let Some(ilk) = self.store.ilks.get(ilk_id).cloned() {
                                        if self.is_primary {
                                            if let Some(database_config) = self.db_config.as_ref() {
                                                if let Err(err) = persist_ilk_state_in_db(
                                                    database_config,
                                                    &ilk,
                                                    None,
                                                )
                                                .await
                                                {
                                                    if let Some(snapshot) = snapshot {
                                                        self.store = snapshot;
                                                    }
                                                    db_write_error_payload(
                                                        "failed to persist provisioned ilk",
                                                        err.as_ref(),
                                                    )
                                                } else {
                                                    deltas.push(delta_envelope(
                                                        IdentityDelta::IlkUpsert { ilk },
                                                    ));
                                                    ok
                                                }
                                            } else {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::IlkUpsert { ilk },
                                                ));
                                                ok
                                            }
                                        } else {
                                            deltas.push(delta_envelope(IdentityDelta::IlkUpsert {
                                                ilk,
                                            }));
                                            ok
                                        }
                                    } else {
                                        ok
                                    }
                                } else {
                                    ok
                                }
                            }
                            Err(code) => error_payload(&code, "failed to provision ilk"),
                        }
                    }
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_ILK_REGISTER => {
                match serde_json::from_value::<IlkRegisterRequest>(msg.payload.clone()) {
                    Ok(req) => {
                        let snapshot = if self.is_primary && self.db_config.is_some() {
                            Some(self.store.clone())
                        } else {
                            None
                        };
                        match self.store.register_ilk(req) {
                            Ok(ok) => {
                                if let Some(ilk_id) = ok.get("ilk_id").and_then(Value::as_str) {
                                    if let Some(ilk) = self.store.ilks.get(ilk_id).cloned() {
                                        if self.is_primary {
                                            if let Some(database_config) = self.db_config.as_ref() {
                                                if let Err(err) = persist_ilk_state_in_db(
                                                    database_config,
                                                    &ilk,
                                                    None,
                                                )
                                                .await
                                                {
                                                    if let Some(snapshot) = snapshot {
                                                        self.store = snapshot;
                                                    }
                                                    db_write_error_payload(
                                                        "failed to persist registered ilk",
                                                        err.as_ref(),
                                                    )
                                                } else {
                                                    deltas.push(delta_envelope(
                                                        IdentityDelta::IlkUpsert { ilk },
                                                    ));
                                                    ok
                                                }
                                            } else {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::IlkUpsert { ilk },
                                                ));
                                                ok
                                            }
                                        } else {
                                            deltas.push(delta_envelope(IdentityDelta::IlkUpsert {
                                                ilk,
                                            }));
                                            ok
                                        }
                                    } else {
                                        ok
                                    }
                                } else {
                                    ok
                                }
                            }
                            Err(code) => error_payload(&code, "failed to register ilk"),
                        }
                    }
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_ILK_ADD_CHANNEL => {
                match serde_json::from_value::<IlkAddChannelRequest>(msg.payload.clone()) {
                    Ok(req) => {
                        let snapshot = if self.is_primary && self.db_config.is_some() {
                            Some(self.store.clone())
                        } else {
                            None
                        };
                        match self.store.add_channel(
                            req.clone(),
                            src_l2_name,
                            self.merge_alias_ttl_secs,
                        ) {
                            Ok(ok) => {
                                let alias_delta =
                                    req.merge_from_ilk_id.as_ref().and_then(|old_ilk_id| {
                                        self.store.aliases.get(old_ilk_id).map(|alias| {
                                            AliasSnapshotRecord {
                                                old_ilk_id: old_ilk_id.clone(),
                                                canonical_ilk_id: alias.canonical_ilk_id.clone(),
                                                expires_at_ms: alias.expires_at_ms,
                                            }
                                        })
                                    });
                                if let Some(ilk_id) = ok.get("ilk_id").and_then(Value::as_str) {
                                    if let Some(ilk) = self.store.ilks.get(ilk_id).cloned() {
                                        if self.is_primary {
                                            if let Some(database_config) = self.db_config.as_ref() {
                                                if let Err(err) = persist_ilk_state_in_db(
                                                    database_config,
                                                    &ilk,
                                                    alias_delta.as_ref(),
                                                )
                                                .await
                                                {
                                                    if let Some(snapshot) = snapshot {
                                                        self.store = snapshot;
                                                    }
                                                    db_write_error_payload(
                                                        "failed to persist channel/merge update",
                                                        err.as_ref(),
                                                    )
                                                } else {
                                                    deltas.push(delta_envelope(
                                                        IdentityDelta::IlkUpsert { ilk },
                                                    ));
                                                    if let Some(alias) = alias_delta {
                                                        deltas.push(delta_envelope(
                                                            IdentityDelta::AliasUpsert { alias },
                                                        ));
                                                    }
                                                    ok
                                                }
                                            } else {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::IlkUpsert { ilk },
                                                ));
                                                if let Some(alias) = alias_delta {
                                                    deltas.push(delta_envelope(
                                                        IdentityDelta::AliasUpsert { alias },
                                                    ));
                                                }
                                                ok
                                            }
                                        } else {
                                            deltas.push(delta_envelope(IdentityDelta::IlkUpsert {
                                                ilk,
                                            }));
                                            if let Some(alias) = alias_delta {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::AliasUpsert { alias },
                                                ));
                                            }
                                            ok
                                        }
                                    } else {
                                        ok
                                    }
                                } else {
                                    ok
                                }
                            }
                            Err(code) => error_payload(&code, "failed to add channel"),
                        }
                    }
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_ICH_SET_ENABLED => {
                match serde_json::from_value::<IchSetEnabledRequest>(msg.payload.clone()) {
                    Ok(req) => {
                        if let Value::Bool(enabled) = req.enabled {
                            if let Err(payload) = authorize_ich_enabled_mutation(
                                &self.store,
                                &req.ich_id,
                                src_l2_name,
                            ) {
                                payload
                            } else {
                                let snapshot = if self.is_primary && self.db_config.is_some() {
                                    Some(self.store.clone())
                                } else {
                                    None
                                };
                                match self.store.set_ich_enabled(&req.ich_id, enabled) {
                                    Ok(ok) => {
                                        if let Some(ilk_id) =
                                            ok.get("ilk_id").and_then(Value::as_str)
                                        {
                                            if let Some(ilk) = self.store.ilks.get(ilk_id).cloned()
                                            {
                                                if self.is_primary {
                                                    if let Some(database_config) =
                                                        self.db_config.as_ref()
                                                    {
                                                        if let Err(err) = persist_ilk_state_in_db(
                                                            database_config,
                                                            &ilk,
                                                            None,
                                                        )
                                                        .await
                                                        {
                                                            if let Some(snapshot) = snapshot {
                                                                self.store = snapshot;
                                                            }
                                                            db_write_error_payload(
                                                            "failed to persist ich enabled update",
                                                            err.as_ref(),
                                                        )
                                                        } else {
                                                            deltas.push(delta_envelope(
                                                                IdentityDelta::IlkUpsert { ilk },
                                                            ));
                                                            ok
                                                        }
                                                    } else {
                                                        deltas.push(delta_envelope(
                                                            IdentityDelta::IlkUpsert { ilk },
                                                        ));
                                                        ok
                                                    }
                                                } else {
                                                    deltas.push(delta_envelope(
                                                        IdentityDelta::IlkUpsert { ilk },
                                                    ));
                                                    ok
                                                }
                                            } else {
                                                ok
                                            }
                                        } else {
                                            ok
                                        }
                                    }
                                    Err(code) => {
                                        error_payload(&code, "failed to set ich enabled state")
                                    }
                                }
                            }
                        } else {
                            error_payload("INVALID_ICH_STATE", "enabled must be boolean")
                        }
                    }
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_ILK_UPDATE => match serde_json::from_value::<IlkUpdateRequest>(msg.payload.clone())
            {
                Ok(req) => {
                    let snapshot = if self.is_primary && self.db_config.is_some() {
                        Some(self.store.clone())
                    } else {
                        None
                    };
                    match self.store.update_ilk(req) {
                        Ok(ok) => {
                            if let Some(ilk_id) = ok.get("ilk_id").and_then(Value::as_str) {
                                if let Some(ilk) = self.store.ilks.get(ilk_id).cloned() {
                                    if self.is_primary {
                                        if let Some(database_config) = self.db_config.as_ref() {
                                            if let Err(err) =
                                                persist_ilk_state_in_db(database_config, &ilk, None)
                                                    .await
                                            {
                                                if let Some(snapshot) = snapshot {
                                                    self.store = snapshot;
                                                }
                                                db_write_error_payload(
                                                    "failed to persist ilk update",
                                                    err.as_ref(),
                                                )
                                            } else {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::IlkUpsert { ilk },
                                                ));
                                                ok
                                            }
                                        } else {
                                            deltas.push(delta_envelope(IdentityDelta::IlkUpsert {
                                                ilk,
                                            }));
                                            ok
                                        }
                                    } else {
                                        deltas
                                            .push(delta_envelope(IdentityDelta::IlkUpsert { ilk }));
                                        ok
                                    }
                                } else {
                                    ok
                                }
                            } else {
                                ok
                            }
                        }
                        Err(code) => error_payload(&code, "failed to update ilk"),
                    }
                }
                Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
            },
            MSG_ILK_SET_DEFINITION => {
                match serde_json::from_value::<IlkSetDefinitionRequest>(msg.payload.clone()) {
                    Ok(req) => {
                        let snapshot = if self.is_primary && self.db_config.is_some() {
                            Some(self.store.clone())
                        } else {
                            None
                        };
                        match self.store.set_ilk_definition(req) {
                            Ok(ok) => {
                                if let Some(ilk_id) = ok.get("ilk_id").and_then(Value::as_str) {
                                    if let Some(ilk) = self.store.ilks.get(ilk_id).cloned() {
                                        if self.is_primary {
                                            if let Some(database_config) = self.db_config.as_ref() {
                                                if let Err(err) = persist_ilk_state_in_db(
                                                    database_config,
                                                    &ilk,
                                                    None,
                                                )
                                                .await
                                                {
                                                    if let Some(snapshot) = snapshot {
                                                        self.store = snapshot;
                                                    }
                                                    db_write_error_payload(
                                                        "failed to persist ilk definition",
                                                        err.as_ref(),
                                                    )
                                                } else {
                                                    deltas.push(delta_envelope(
                                                        IdentityDelta::IlkUpsert { ilk },
                                                    ));
                                                    ok
                                                }
                                            } else {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::IlkUpsert { ilk },
                                                ));
                                                ok
                                            }
                                        } else {
                                            deltas.push(delta_envelope(IdentityDelta::IlkUpsert {
                                                ilk,
                                            }));
                                            ok
                                        }
                                    } else {
                                        ok
                                    }
                                } else {
                                    ok
                                }
                            }
                            Err(code) => error_payload(&code, "failed to set ilk definition"),
                        }
                    }
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_ILK_DELETE => {
                match serde_json::from_value::<IlkDeleteRequest>(msg.payload.clone()) {
                    Ok(req) => {
                        let snapshot = if self.is_primary && self.db_config.is_some() {
                            Some(self.store.clone())
                        } else {
                            None
                        };
                        let ilk_id = req.ilk_id.clone();
                        // Capture alias old_ilk_ids whose removal we'll need to
                        // propagate to replicas/SHM. The store consumes them
                        // when delete_ilk runs; without explicit AliasDelete
                        // deltas the SHM layer would keep stale alias entries
                        // because apply_identity_shm_event's IlkDelete arm only
                        // clears ich/ilk entries, not aliases.
                        let removed_alias_old_ids = self.store.alias_old_ids_referencing(&ilk_id);
                        match self.store.delete_ilk(req) {
                            Ok(ok) => {
                                let mut emit_deltas = || {
                                    for old_ilk_id in &removed_alias_old_ids {
                                        deltas.push(delta_envelope(IdentityDelta::AliasDelete {
                                            old_ilk_id: old_ilk_id.clone(),
                                        }));
                                    }
                                    deltas.push(delta_envelope(IdentityDelta::IlkDelete {
                                        ilk_id: ilk_id.clone(),
                                    }));
                                };
                                if self.is_primary {
                                    if let Some(database_config) = self.db_config.as_ref() {
                                        if let Err(err) =
                                            delete_ilk_in_db(database_config, &ilk_id).await
                                        {
                                            if let Some(snapshot) = snapshot {
                                                self.store = snapshot;
                                            }
                                            db_write_error_payload(
                                                "failed to delete ilk",
                                                err.as_ref(),
                                            )
                                        } else {
                                            emit_deltas();
                                            ok
                                        }
                                    } else {
                                        emit_deltas();
                                        ok
                                    }
                                } else {
                                    emit_deltas();
                                    ok
                                }
                            }
                            Err(code) => error_payload(&code, "failed to delete ilk"),
                        }
                    }
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_TNT_CREATE => match serde_json::from_value::<TntCreateRequest>(msg.payload.clone())
            {
                Ok(req) => {
                    // Snapshot BEFORE the mutation and restore on DB failure —
                    // mirroring TNT_UPDATE/ILK_DELETE. create_tenant is idempotent:
                    // for an already-existing tenant it returns created=false
                    // WITHOUT mutating the store, so the old blind
                    // `tenants.remove(tenant_id)` on DB error would evict a
                    // pre-existing (possibly root) tenant. Snapshot-restore is a
                    // no-op for the dedup path and a correct rollback for a real
                    // create.
                    let snapshot = if self.is_primary && self.db_config.is_some() {
                        Some(self.store.clone())
                    } else {
                        None
                    };
                    match self.store.create_tenant(req) {
                        Ok(ok) => {
                            if let Some(tenant_id) = ok
                                .get("tenant_id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                            {
                                if let Some(tenant) = self.store.tenants.get(&tenant_id).cloned() {
                                    if self.is_primary {
                                        if let Some(database_config) = self.db_config.as_ref() {
                                            if let Err(err) =
                                                upsert_tenant_in_db(database_config, &tenant).await
                                            {
                                                if let Some(snapshot) = snapshot {
                                                    self.store = snapshot;
                                                }
                                                db_write_error_payload(
                                                    "failed to persist tenant",
                                                    err.as_ref(),
                                                )
                                            } else {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::TenantUpsert { tenant },
                                                ));
                                                ok
                                            }
                                        } else {
                                            deltas.push(delta_envelope(
                                                IdentityDelta::TenantUpsert { tenant },
                                            ));
                                            ok
                                        }
                                    } else {
                                        deltas.push(delta_envelope(IdentityDelta::TenantUpsert {
                                            tenant,
                                        }));
                                        ok
                                    }
                                } else {
                                    ok
                                }
                            } else {
                                ok
                            }
                        }
                        Err(code) => error_payload(&code, "failed to create tenant"),
                    }
                }
                Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
            },
            MSG_TNT_UPDATE => match serde_json::from_value::<TntUpdateRequest>(msg.payload.clone())
            {
                Ok(req) => {
                    let snapshot = if self.is_primary && self.db_config.is_some() {
                        Some(self.store.clone())
                    } else {
                        None
                    };
                    match self.store.update_tenant(req) {
                        Ok(ok) => {
                            if let Some(tenant_id) = ok
                                .get("tenant_id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                            {
                                if let Some(tenant) = self.store.tenants.get(&tenant_id).cloned() {
                                    if self.is_primary {
                                        if let Some(database_config) = self.db_config.as_ref() {
                                            if let Err(err) =
                                                upsert_tenant_in_db(database_config, &tenant).await
                                            {
                                                if let Some(snapshot) = snapshot {
                                                    self.store = snapshot;
                                                }
                                                db_write_error_payload(
                                                    "failed to persist tenant update",
                                                    err.as_ref(),
                                                )
                                            } else {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::TenantUpsert { tenant },
                                                ));
                                                ok
                                            }
                                        } else {
                                            deltas.push(delta_envelope(
                                                IdentityDelta::TenantUpsert { tenant },
                                            ));
                                            ok
                                        }
                                    } else {
                                        deltas.push(delta_envelope(IdentityDelta::TenantUpsert {
                                            tenant,
                                        }));
                                        ok
                                    }
                                } else {
                                    ok
                                }
                            } else {
                                ok
                            }
                        }
                        Err(code) => error_payload(&code, "failed to update tenant"),
                    }
                }
                Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
            },
            MSG_TNT_SET_SPONSOR => {
                match serde_json::from_value::<TntSetSponsorRequest>(msg.payload.clone()) {
                    Ok(req) => {
                        let snapshot = if self.is_primary && self.db_config.is_some() {
                            Some(self.store.clone())
                        } else {
                            None
                        };
                        match self.store.set_tenant_sponsor(req) {
                            Ok(ok) => {
                                if let Some(tenant_id) = ok
                                    .get("tenant_id")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                {
                                    if let Some(tenant) =
                                        self.store.tenants.get(&tenant_id).cloned()
                                    {
                                        if self.is_primary {
                                            if let Some(database_config) = self.db_config.as_ref() {
                                                if let Err(err) =
                                                    upsert_tenant_in_db(database_config, &tenant)
                                                        .await
                                                {
                                                    if let Some(snapshot) = snapshot {
                                                        self.store = snapshot;
                                                    }
                                                    db_write_error_payload(
                                                        "failed to persist tenant sponsor update",
                                                        err.as_ref(),
                                                    )
                                                } else {
                                                    deltas.push(delta_envelope(
                                                        IdentityDelta::TenantUpsert { tenant },
                                                    ));
                                                    ok
                                                }
                                            } else {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::TenantUpsert { tenant },
                                                ));
                                                ok
                                            }
                                        } else {
                                            deltas.push(delta_envelope(
                                                IdentityDelta::TenantUpsert { tenant },
                                            ));
                                            ok
                                        }
                                    } else {
                                        ok
                                    }
                                } else {
                                    ok
                                }
                            }
                            Err(code) => error_payload(&code, "failed to set tenant sponsor"),
                        }
                    }
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_TNT_APPROVE => {
                match serde_json::from_value::<TntApproveRequest>(msg.payload.clone()) {
                    Ok(req) => {
                        let snapshot = if self.is_primary && self.db_config.is_some() {
                            Some(self.store.clone())
                        } else {
                            None
                        };
                        match self.store.approve_tenant(req) {
                            Ok(ok) => {
                                if let Some(tenant_id) = ok.get("tenant_id").and_then(Value::as_str)
                                {
                                    if let Some(tenant) = self.store.tenants.get(tenant_id).cloned()
                                    {
                                        if self.is_primary {
                                            if let Some(database_config) = self.db_config.as_ref() {
                                                if let Err(err) =
                                                    upsert_tenant_in_db(database_config, &tenant)
                                                        .await
                                                {
                                                    if let Some(snapshot) = snapshot {
                                                        self.store = snapshot;
                                                    }
                                                    db_write_error_payload(
                                                        "failed to persist tenant approval",
                                                        err.as_ref(),
                                                    )
                                                } else {
                                                    deltas.push(delta_envelope(
                                                        IdentityDelta::TenantUpsert { tenant },
                                                    ));
                                                    ok
                                                }
                                            } else {
                                                deltas.push(delta_envelope(
                                                    IdentityDelta::TenantUpsert { tenant },
                                                ));
                                                ok
                                            }
                                        } else {
                                            deltas.push(delta_envelope(
                                                IdentityDelta::TenantUpsert { tenant },
                                            ));
                                            ok
                                        }
                                    } else {
                                        ok
                                    }
                                } else {
                                    ok
                                }
                            }
                            Err(code) => error_payload(&code, "failed to approve tenant"),
                        }
                    }
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            "IDENTITY_METRICS" => {
                json!({
                    "status": "ok",
                    "metrics": self.store.metrics(),
                })
            }
            _ => error_payload(
                "INVALID_REQUEST",
                &format!("action '{}' is not supported", action),
            ),
        };

        if !deltas.is_empty() {
            if let Some(writer) = identity_shm {
                let shm_apply_started = Instant::now();
                // G-8: a LOCAL SHM write failure must NOT suppress replication of
                // an already-committed (DB) change or the client response. The
                // old `?` here returned Err out of the handler, so the caller
                // skipped broadcast_deltas AND send_system_response, leaving the
                // primary ahead of replicas and the caller hung. Log and press on;
                // the local SHM is stale until the next successful apply, but the
                // durable store, replicas, and the response stay consistent.
                match apply_identity_shm_deltas(writer, &self.store, action, &deltas) {
                    Ok(()) => {
                        let shm_state = writer.debug_state();
                        tracing::info!(
                            action,
                            trace_id = %trace_id,
                            delta_count = deltas.len(),
                            elapsed_us = shm_apply_started.elapsed().as_micros() as u64,
                            shm_seq = shm_state.map(|s| s.seq),
                            shm_tenant_count = shm_state.map(|s| s.tenant_count),
                            shm_ilk_count = shm_state.map(|s| s.ilk_count),
                            shm_ich_count = shm_state.map(|s| s.ich_count),
                            shm_mapping_count = shm_state.map(|s| s.ich_mapping_count),
                            shm_updated_at = shm_state.map(|s| s.updated_at),
                            "identity shm delta apply completed"
                        );
                    }
                    Err(err) => {
                        tracing::error!(
                            action,
                            trace_id = %trace_id,
                            delta_count = deltas.len(),
                            error = %err,
                            "identity shm delta apply FAILED; broadcasting delta and \
                             responding anyway (local SHM stale until next apply)"
                        );
                    }
                }
            }
        }

        tracing::info!(
            action,
            trace_id = %trace_id,
            elapsed_us = action_started.elapsed().as_micros() as u64,
            "identity sending system response"
        );
        send_system_response(sender, msg, response_name(action), payload).await?;
        Ok(deltas)
    }

    async fn run_alias_gc(&mut self) -> Result<Vec<IdentityDeltaEnvelope>, IdentityError> {
        let now_ms = now_epoch_ms();
        let expired_aliases: Vec<String> = self
            .store
            .aliases
            .iter()
            .filter_map(|(old_ilk_id, alias)| {
                if alias.expires_at_ms <= now_ms {
                    Some(old_ilk_id.clone())
                } else {
                    None
                }
            })
            .collect();
        // Snapshot before mutating memory so a DB GC failure rolls back cleanly.
        // Otherwise (G-6) memory drops the expired alias / tombstones the ilk,
        // the DB write errors and the deltas built from `expired_aliases` are
        // discarded, and the NEXT cycle recomputes an empty expired set from the
        // already-mutated memory — so SHM and every replica keep the expired
        // alias and the un-tombstoned ilk indefinitely.
        let snapshot = if self.is_primary && self.db_config.is_some() {
            Some(self.store.clone())
        } else {
            None
        };
        let removed_local = self.store.gc_expired_aliases(now_ms);
        if removed_local > 0 {
            tracing::info!(removed = removed_local, "identity alias gc applied locally");
        }

        if self.is_primary {
            if let Some(database_config) = self.db_config.as_ref() {
                match gc_aliases_in_db(database_config).await {
                    Ok(removed_db) => {
                        if removed_db > 0 {
                            tracing::info!(
                                removed = removed_db,
                                "identity alias gc applied in database"
                            );
                        }
                    }
                    Err(err) => {
                        if let Some(snapshot) = snapshot {
                            self.store = snapshot;
                        }
                        tracing::warn!(
                            error = %err,
                            "identity alias gc DB write failed; rolled back in-memory gc, \
                             will retry next cycle (keeps memory/SHM/replicas consistent)"
                        );
                        return Ok(Vec::new());
                    }
                }
            }
        }

        let mut deltas = Vec::new();
        for old_ilk_id in expired_aliases {
            deltas.push(delta_envelope(IdentityDelta::AliasDelete {
                old_ilk_id: old_ilk_id.clone(),
            }));
            if let Some(ilk) = self.store.ilks.get(&old_ilk_id) {
                if ilk.deleted_at_ms.is_some() {
                    deltas.push(delta_envelope(IdentityDelta::IlkUpsert {
                        ilk: ilk.clone(),
                    }));
                }
            }
        }
        Ok(deltas)
    }

    fn is_authorized(&self, action: &str, source_name: Option<&str>) -> bool {
        if matches!(
            action,
            "IDENTITY_METRICS"
                | "IDENTITY_METRICS_RESPONSE"
                | "PING"
                | "PING_RESPONSE"
                | "STATUS"
                | "STATUS_RESPONSE"
        ) {
            return true;
        }

        let Some(name) = source_name else {
            return false;
        };

        if let Some(exacts) = self.allowed_exacts.get(action) {
            if exacts.contains(name) {
                return true;
            }
        }

        let Some(prefixes) = self.allowed_prefixes.get(action) else {
            return false;
        };
        // Same-hive-only roles: a privileged SY control-plane identity from ANOTHER hive must
        // not administer THIS hive's identity authority (F-07). The same-hive scoping LOGIC now
        // lives in system_policy::prefix_allowed_same_hive_scoped — shared with the router's
        // authority() rule so the two can't drift; only the per-message allowlist DATA stays
        // here. IO.* (worker IO provisioning against the sole motherbee authority) and
        // SY.orchestrator@ (cross-hive control plane) are legitimately cross-hive.
        const SAME_HIVE_ONLY_PREFIXES: [&str; 3] =
            ["SY.admin@", "SY.architect@", "SY.frontdesk.gov@"];
        json_router::router::system_policy::prefix_allowed_same_hive_scoped(
            name,
            &self.hive_id,
            prefixes,
            &SAME_HIVE_ONLY_PREFIXES,
        )
    }
}

fn configured_identity_frontdesk_node_name(hive: &HiveFile) -> Option<String> {
    hive.government
        .as_ref()
        .and_then(|government| government.identity_frontdesk.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.contains('@') {
                value.to_string()
            } else {
                format!("{value}@{}", hive.hive_id)
            }
        })
}

#[tokio::main]
async fn main() -> Result<(), IdentityError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_identity supports only Linux targets.");
        std::process::exit(1);
    }

    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let socket_dir = json_router::paths::router_socket_dir();

    let hive = load_hive(&config_dir)?;
    let is_primary = is_mother_role(hive.role.as_deref());
    if !is_primary && !is_worker_role(hive.role.as_deref()) {
        return Err("sy.identity supports only role=motherbee|worker".into());
    }
    if is_primary && hive.hive_id != PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid hive.yaml: role=motherbee requires hive_id='{}' (got '{}')",
            PRIMARY_HIVE_ID, hive.hive_id
        )
        .into());
    }
    if !is_primary && hive.hive_id == PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid hive.yaml: hive_id='{}' is reserved for role=motherbee",
            PRIMARY_HIVE_ID
        )
        .into());
    }
    let node_name = ensure_l2_name(IDENTITY_NODE_BASE_NAME, &hive.hive_id);
    // Identity's own ILK is deterministic, so we can compute it locally
    // without waiting for any SHM to be populated (we're the one writing it).
    let self_ilk_id = deterministic_system_ilk_id(&node_name);

    let node_config = NodeConfig {
        name: IDENTITY_NODE_BASE_NAME.to_string(),
        router_socket: socket_dir.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: IDENTITY_NODE_VERSION.to_string(),
    };

    // Option-C fix (race ARCHI-BUG-12): connect persistently to the router
    // FIRST so we are announced BEFORE asking vault anything. The vault
    // lookup below reuses the same dispatcher. If vault hasn't booted yet,
    // the lookup returns Missing, but our persistent registration means
    // vault's bootstrap VAULT_SECRET_CHANGED broadcast (emitted when it
    // arrives) lands in our system receiver and triggers the exit(0) rescue.
    let profile = build_identity_rpc_profile()
        .map_err(|err| format!("sy.identity rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    let sender = dispatcher.sender_snapshot();
    tracing::info!(node_name = %sender.full_name(), "sy.identity connected to router");

    let vault_client = VaultClient::new(
        dispatcher.clone(),
        hive.hive_id.clone(),
        VaultCallerOwned::new(self_ilk_id.clone(), node_name.clone()),
    );

    let (database_url, db_secret_source, vault_lookup_error) = if is_primary {
        resolve_database_url(
            &vault_client,
            &node_name,
            fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
        )
        .await
    } else {
        (None, IdentityDbSecretSource::Missing, None)
    };
    let (db_config, db_init_error) = if is_primary {
        let (cfg, init_err) = initialize_identity_database_backend(database_url.as_deref()).await;
        let init_err = vault_lookup_error.or(init_err);
        (cfg, init_err)
    } else {
        tracing::info!(
            role = %hive.role.clone().unwrap_or_else(|| "unknown".to_string()),
            "sy.identity running without local DB (replica/non-primary mode)"
        );
        (None, None)
    };
    let mut control_state = bootstrap_identity_control_state(
        &node_name,
        db_secret_source,
        is_primary,
        db_config.is_some(),
        db_init_error,
    )?;
    if is_primary && !control_state.db_ready {
        tracing::warn!(
            node_name = %node_name,
            db_secret_source = %control_state.secret_source.as_str(),
            error = ?control_state.last_error,
            "sy.identity started without active DB backend; CONFIG_SET + restart required"
        );
    }
    let mut runtime = IdentityRuntime::new(&hive, state_dir.clone(), is_primary, db_config);
    if is_primary {
        if let Some(database_config) = runtime.db_config.clone() {
            match load_identity_store_from_db(&database_config).await {
                Ok(mut store) => {
                    if store.ensure_default_root_tenant() {
                        if let Some(default_tenant) =
                            store.tenants.get(DEFAULT_ROOT_TENANT_ID).cloned()
                        {
                            if let Err(err) =
                                upsert_tenant_in_db(&database_config, &default_tenant).await
                            {
                                tracing::warn!(error = %err, "failed to persist fixed default tenant in primary db bootstrap");
                            } else {
                                tracing::info!(
                                    tenant_id = %default_tenant.tenant_id,
                                    "persisted fixed default tenant in primary db bootstrap"
                                );
                            }
                        }
                    }
                    let metrics = store.metrics();
                    runtime.store = store;
                    tracing::info!(metrics = %metrics, "loaded identity store from primary db");
                }
                Err(err) => {
                    control_state.db_ready = false;
                    control_state.last_error = Some(format!(
                        "failed to load identity store from primary db: {err}"
                    ));
                    persist_identity_config_state(&node_name, is_primary, &control_state)?;
                    tracing::warn!(error = %err, "failed to load identity store from primary db; continuing with in-memory bootstrap");
                }
            }
        }
    }
    let identity_shm_name = identity_shm_name(&hive.hive_id);
    let identity_limits = identity_region_limits(&hive);
    let mut identity_shm = match IdentityRegionWriter::open_or_create(
        &identity_shm_name,
        Uuid::new_v4(),
        &hive.hive_id,
        is_primary,
        identity_limits,
    ) {
        Ok(writer) => Some(writer),
        Err(err) => {
            tracing::warn!(
                shm = %identity_shm_name,
                error = %err,
                "identity shm unavailable; IO lookup via SHM will be degraded"
            );
            None
        }
    };
    let sync_port = identity_sync_port(&hive);
    let sync_upstream = identity_sync_upstream(&hive);
    let auth_required = identity_sync_auth_required(&hive);
    let self_hive = hive.hive_id.clone();
    // A replica needs its own per-hive HMAC key to run the client handshake.
    // Fail-closed: auth required but key missing aborts startup (rather than
    // silently falling back to an unauthenticated channel).
    let self_auth_key: Option<json_router::mesh_hmac::MeshHmacKey> = if auth_required && !is_primary
    {
        let path = identity_hmac_key_path(&self_hive);
        match json_router::mesh_hmac::MeshHmacKey::load_from_file(&path) {
            Ok(k) => Some(k),
            Err(err) => {
                return Err(format!(
                    "identity.sync.auth=required but HMAC key missing at {}: {err}",
                    path.display()
                )
                .into());
            }
        }
    } else {
        None
    };
    if auth_required {
        tracing::info!(
            role = if is_primary { "primary" } else { "replica" },
            "identity sync auth: required (per-hive HMAC)"
        );
    }
    let sync_listener = if is_primary {
        let bind_addr = format!("0.0.0.0:{sync_port}");
        let listener = TcpListener::bind(&bind_addr).await?;
        tracing::info!(bind = %bind_addr, "identity sync listener ready");
        Some(listener)
    } else {
        None
    };
    if !is_primary {
        if let Some(upstream) = sync_upstream.as_deref() {
            match fetch_full_sync_from_primary(upstream, self_auth_key.as_ref(), &self_hive).await {
                Ok(store) => {
                    let metrics = store.metrics();
                    runtime.store = store;
                    tracing::info!(upstream = %upstream, metrics = %metrics, "identity full sync bootstrap applied");
                }
                Err(err) => {
                    // F-05: surface this LOUDLY (a replica running on stale/empty
                    // state serves inconsistent identity to router/vault/IO). We
                    // do NOT exit here — the primary may be briefly unreachable
                    // during firstboot ordering, and exiting would restart-storm.
                    // A delta gap after a successful boot DOES exit for re-sync.
                    tracing::error!(upstream = %upstream, error = %err, "identity full sync bootstrap FAILED; replica is DEGRADED and starting with local in-memory state (will converge once the delta stream reconnects)");
                }
            }
        } else {
            tracing::warn!("identity replica mode without identity.sync.upstream; starting with local in-memory state");
        }
    }
    match runtime.store.ensure_system_ilks_from_hive(&hive) {
        Ok(changed_ilks) => {
            if !changed_ilks.is_empty() {
                tracing::info!(
                    count = changed_ilks.len(),
                    "seeded deterministic system ILKs from hive.yaml"
                );
            }
            if is_primary {
                if let Some(database_config) = runtime.db_config.clone() {
                    for ilk in &changed_ilks {
                        if let Err(err) = persist_ilk_state_in_db(&database_config, ilk, None).await
                        {
                            tracing::warn!(
                                ilk_id = %ilk.ilk_id,
                                error = %err,
                                "failed to persist deterministic system ILK"
                            );
                        }
                    }
                }
            }
        }
        Err(err) => {
            return Err(format!("failed to seed system ILKs from hive.yaml: {err}").into());
        }
    }
    if let Some(writer) = identity_shm.as_mut() {
        if let Err(err) = sync_identity_shm_mappings(writer, &runtime.store) {
            tracing::warn!(error = %err, "initial identity shm sync failed");
        }
    }
    // Identity is the SHM writer; it derives its own ILK locally rather than
    // waiting on the SHM read path it just populated.
    tracing::info!(self_ilk_id = %self_ilk_id, "resolved self system ILK");
    let (delta_event_tx, mut delta_event_rx) = mpsc::unbounded_channel::<IdentityDeltaEnvelope>();
    // Upstream publish (replica → primary): local ilk deltas a replica pushes up.
    let (upstream_tx, upstream_rx) = mpsc::unbounded_channel::<UpstreamFrame>();
    // Ingest (primary): frames received from replicas' publish connections.
    // Bounded so a flooding replica backpressures its own connection instead of
    // growing primary memory unboundedly.
    let (ingest_tx, mut ingest_rx) = mpsc::channel::<IngestFrame>(IDENTITY_INGEST_CHANNEL_CAP);
    if !is_primary {
        if let Some(upstream) = sync_upstream.clone() {
            let sub_key = self_auth_key.clone();
            let sub_hive = self_hive.clone();
            tokio::spawn(async move {
                run_delta_subscription_loop(upstream, delta_event_tx, sub_key, sub_hive).await;
            });
        }
        if let Some(upstream) = sync_upstream.clone() {
            let publish_hive = hive.hive_id.clone();
            let pub_key = self_auth_key.clone();
            tokio::spawn(async move {
                run_delta_publish_loop(upstream, publish_hive, upstream_rx, pub_key).await;
            });
        }
    }
    tracing::info!(
        hive = %hive.hive_id,
        role = %hive.role.clone().unwrap_or_else(|| "unknown".to_string()),
        "sy.identity started"
    );

    // Vault changes arrive event-driven via VAULT_SECRET_CHANGED
    // broadcasts; the consumer handler (`handle_vault_secret_changed`)
    // triggers exit(0) so systemd restarts identity and the Postgres
    // pool reconnects with the new secret.

    let mut heartbeat = time::interval(Duration::from_secs(5));
    let mut alias_gc_tick = time::interval(Duration::from_secs(ALIAS_GC_INTERVAL_SECS));
    // Replica reconciliation beat: re-push the full self-owned ilk set so the
    // primary converges (recovers upserts AND hard-deletes after a reconnect gap).
    // The first tick fires immediately, publishing the boot-seeded ilks.
    let mut publish_reconcile_tick =
        time::interval(Duration::from_secs(IDENTITY_PUBLISH_RECONCILE_SECS));
    let sync_listener = sync_listener;
    let mut delta_subscribers: Vec<mpsc::Sender<IdentityDeltaEnvelope>> = Vec::new();
    let mut next_delta_seq: u64 = 1;
    // F-02: each accepted :9100 connection is handled in its own task so a slow
    // reader cannot pin the event loop. The semaphore caps concurrent handlers;
    // handlers return a registered subscriber over `new_sub_rx` and request an
    // on-demand full-sync snapshot over `chunks_req_rx` (built here, on the loop,
    // only for authenticated FULL_SYNC requests — G-9).
    let sync_conn_sem = Arc::new(Semaphore::new(MAX_CONCURRENT_SYNC_CONNS));
    let (new_sub_tx, mut new_sub_rx) =
        mpsc::unbounded_channel::<mpsc::Sender<IdentityDeltaEnvelope>>();
    let (chunks_req_tx, mut chunks_req_rx) =
        mpsc::unbounded_channel::<oneshot::Sender<Arc<Vec<IdentityFullSyncChunk>>>>();
    let mut system_rx = dispatcher
        .take_command_receiver(RPC_CH_SYSTEM)
        .await
        .map_err(|err| format!("sy.identity system receiver: {err}"))?;
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                tracing::debug!(metrics = %runtime.store.metrics(), "identity heartbeat");
                if let Some(writer) = identity_shm.as_mut() {
                    writer.update_heartbeat();
                }
            }
            _ = publish_reconcile_tick.tick() => {
                // Replica: push the full self-owned ilk set upstream so the
                // primary reconciles to it (additive across hives; recovers
                // upserts + hard-deletes). No-op on the primary.
                if !is_primary {
                    let snapshot = runtime.store.self_owned_ilks(&hive.hive_id);
                    let _ = upstream_tx.send(UpstreamFrame::Snapshot(snapshot));
                }
            }
            maybe_ingest = ingest_rx.recv() => {
                // Primary: a replica published its ilks. Authority-check, apply
                // (additive union / per-hive reconcile), update the SHM, and
                // re-broadcast so every subscriber (the other replicas) converges.
                if let Some(frame) = maybe_ingest {
                    // Defense-in-depth: a publish claiming the primary's OWN hive
                    // is an impersonation attempt — the primary owns its ilks and
                    // nobody publishes them upward. (The sync port itself is not
                    // yet peer-authenticated; closing the impersonate-primary case
                    // bounds the blast radius. See ilk-bidirectional-sync.md.)
                    let claimed = match &frame {
                        IngestFrame::Delta { publisher_hive, .. } => publisher_hive,
                        IngestFrame::Snapshot { publisher_hive, .. } => publisher_hive,
                    };
                    let applied: Vec<IdentityDelta> = if claimed == &hive.hive_id {
                        tracing::warn!(hive = %claimed, "rejected identity publish impersonating the primary hive");
                        Vec::new()
                    } else { match frame {
                        IngestFrame::Delta { publisher_hive, delta } => {
                            if delta_authorized_for_hive(&delta, &publisher_hive, &runtime.store) {
                                runtime.store.apply_delta(delta.clone());
                                vec![delta]
                            } else {
                                tracing::warn!(hive = %publisher_hive, "rejected unauthorized identity delta from replica");
                                Vec::new()
                            }
                        }
                        IngestFrame::Snapshot { publisher_hive, ilks } => {
                            if ilks.iter().all(|ilk| {
                                ilk_owning_hive(ilk).as_deref() == Some(publisher_hive.as_str())
                                    && ingest_ilk_type_authorized(ilk, publisher_hive.as_str())
                            }) {
                                runtime.store.reconcile_hive_ilks(&publisher_hive, ilks)
                            } else {
                                tracing::warn!(hive = %publisher_hive, "rejected publish snapshot containing foreign or unauthorized-system ilks");
                                Vec::new()
                            }
                        }
                    }};
                    if !applied.is_empty() {
                        // F-06: persist replica-published ilks so they survive a
                        // primary restart. Previously these were memory/SHM/
                        // broadcast only, so a primary restart while the owning
                        // worker was down dropped them from the global (DB-backed)
                        // view. Best-effort: on failure the worker re-pushes its
                        // full self-owned set on the next reconcile tick, so this
                        // converges. (Aliases are TTL'd/regenerable, not persisted
                        // on this path.)
                        if is_primary {
                            if let Some(cfg) = runtime.db_config.as_ref() {
                                for delta in &applied {
                                    let res = match delta {
                                        IdentityDelta::IlkUpsert { ilk } => {
                                            persist_ilk_state_in_db(cfg, ilk, None).await
                                        }
                                        IdentityDelta::IlkDelete { ilk_id } => {
                                            delete_ilk_in_db(cfg, ilk_id).await
                                        }
                                        _ => Ok(()),
                                    };
                                    if let Err(err) = res {
                                        tracing::warn!(
                                            error = %err,
                                            "identity failed to persist replica-published ilk; \
                                             will re-persist on the next reconcile tick"
                                        );
                                    }
                                }
                            }
                        }
                        if let Some(writer) = identity_shm.as_mut() {
                            if let Err(err) = sync_identity_shm_mappings(writer, &runtime.store) {
                                tracing::warn!(error = %err, "identity shm sync failed after replica ingest");
                            }
                        }
                        let mut envelopes: Vec<IdentityDeltaEnvelope> =
                            applied.into_iter().map(delta_envelope).collect();
                        assign_delta_seqs(&mut envelopes, &mut next_delta_seq);
                        broadcast_deltas(&mut delta_subscribers, &envelopes);
                    }
                }
            }
            _ = alias_gc_tick.tick() => {
                match runtime.run_alias_gc().await {
                    Ok(mut deltas) => {
                        if !deltas.is_empty() {
                            if let Some(writer) = identity_shm.as_mut() {
                                if let Err(err) = sync_identity_shm_mappings(writer, &runtime.store) {
                                    tracing::warn!(error = %err, "identity shm sync failed after alias gc");
                                }
                            }
                        }
                        if is_primary && !deltas.is_empty() {
                            assign_delta_seqs(&mut deltas, &mut next_delta_seq);
                            broadcast_deltas(&mut delta_subscribers, &deltas);
                        } else if !deltas.is_empty() {
                            push_local_deltas_upstream(&deltas, &upstream_tx);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "identity alias gc failed");
                    }
                }
            }
            maybe_delta = delta_event_rx.recv() => {
                if let Some(envelope) = maybe_delta {
                    runtime.store.apply_delta(envelope.delta);
                    if let Some(writer) = identity_shm.as_mut() {
                        if let Err(err) = sync_identity_shm_mappings(writer, &runtime.store) {
                            tracing::warn!(error = %err, "identity shm sync failed after delta apply");
                        }
                    }
                }
            }
            accepted = async {
                match sync_listener.as_ref() {
                    Some(listener) => listener.accept().await.ok(),
                    None => future::pending().await,
                }
            } => {
                if let Some((stream, remote_addr)) = accepted {
                    // F-02: handle the connection OFF the main loop so a slow
                    // reader (e.g. during full-sync streaming) cannot pin the
                    // event loop. Bound concurrency with a semaphore so a connect
                    // storm cannot exhaust fds/memory (excess dropped, peer retries).
                    match Arc::clone(&sync_conn_sem).try_acquire_owned() {
                        Ok(permit) => {
                            let ingest_tx2 = ingest_tx.clone();
                            let chunks_req_tx2 = chunks_req_tx.clone();
                            let new_sub_tx2 = new_sub_tx.clone();
                            tokio::spawn(async move {
                                let _permit = permit;
                                match handle_sync_connection(
                                    stream,
                                    chunks_req_tx2,
                                    ingest_tx2,
                                    auth_required,
                                )
                                .await
                                {
                                    Ok(Some(subscriber)) => {
                                        // Registration happens on the main loop
                                        // (new_sub branch) so delta_subscribers is
                                        // only ever touched there.
                                        let _ = new_sub_tx2.send(subscriber);
                                    }
                                    Ok(None) => {}
                                    Err(err) => {
                                        tracing::warn!(remote = %remote_addr, error = %err, "identity sync request failed");
                                    }
                                }
                            });
                        }
                        Err(_) => {
                            tracing::warn!(
                                remote = %remote_addr,
                                cap = MAX_CONCURRENT_SYNC_CONNS,
                                "identity sync connection cap reached; dropping connection"
                            );
                        }
                    }
                }
            }
            maybe_new_sub = new_sub_rx.recv() => {
                if let Some(subscriber) = maybe_new_sub {
                    if delta_subscribers.len() >= MAX_DELTA_SUBSCRIBERS {
                        // Cap fan-out (G-5): drop the sender (its task exits when
                        // the channel closes). The peer retries later.
                        tracing::warn!(
                            cap = MAX_DELTA_SUBSCRIBERS,
                            "identity delta subscriber cap reached; rejecting new subscriber"
                        );
                    } else {
                        tracing::info!("identity delta subscriber connected");
                        delta_subscribers.push(subscriber);
                    }
                }
            }
            maybe_chunks_req = chunks_req_rx.recv() => {
                if let Some(reply) = maybe_chunks_req {
                    // On-demand full-sync snapshot for an authenticated FULL_SYNC
                    // request (G-9): bounded CPU (clone+sort), no I/O — the
                    // streaming to the peer happens in the spawned handler task.
                    let chunks =
                        Arc::new(runtime.store.build_full_sync_chunks(IDENTITY_FULL_SYNC_CHUNK_ITEMS));
                    let _ = reply.send(chunks);
                }
            }
            maybe_msg = system_rx.recv() => {
                let Some(msg) = maybe_msg else {
                    tracing::warn!("sy.identity system channel closed; exiting main loop");
                    return Ok(());
                };
                let sender = dispatcher.sender_snapshot();

                if msg.meta.msg_type != SYSTEM_KIND {
                    continue;
                }

                match runtime
                    .process_system_message(
                        &sender,
                        &msg,
                        identity_shm.as_mut(),
                        &mut control_state,
                        &node_name,
                    )
                    .await
                {
                    Ok(mut deltas) => {
                        if is_primary && !deltas.is_empty() {
                            assign_delta_seqs(&mut deltas, &mut next_delta_seq);
                            broadcast_deltas(&mut delta_subscribers, &deltas);
                        } else if !deltas.is_empty() {
                            push_local_deltas_upstream(&deltas, &upstream_tx);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, action = ?msg.meta.msg, "failed to process system message");
                    }
                }
            }
        }
    }
}

fn identity_sync_port(hive: &HiveFile) -> u16 {
    hive.identity
        .as_ref()
        .and_then(|identity| identity.sync.as_ref())
        .and_then(|sync| sync.port)
        .unwrap_or(DEFAULT_IDENTITY_SYNC_PORT)
}

fn identity_sync_upstream(hive: &HiveFile) -> Option<String> {
    hive.identity
        .as_ref()
        .and_then(|identity| identity.sync.as_ref())
        .and_then(|sync| sync.upstream.as_ref())
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

/// Whether the :9100 channel requires the per-hive HMAC handshake.
///
/// FAIL-CLOSED: auth is REQUIRED unless the operator explicitly opts out with a
/// recognized token. An absent `identity.sync.auth`, an empty value, or any typo
/// (`require`, `enabled`, `true`, …) all resolve to `required` — a config
/// mistake can no longer silently leave the identity authority's replication
/// channel open to exfiltration/poisoning. The only way to run without auth is a
/// deliberate, loudly-logged `disabled`/`off`/`none`.
fn identity_sync_auth_required(hive: &HiveFile) -> bool {
    let raw = hive
        .identity
        .as_ref()
        .and_then(|identity| identity.sync.as_ref())
        .and_then(|sync| sync.auth.as_deref());
    auth_mode_required(raw)
}

/// Fail-closed core of [`identity_sync_auth_required`]: an absent value, empty
/// string, or any unrecognized token resolves to REQUIRED; only the explicit,
/// loudly-logged `disabled`/`off`/`none` disables auth.
fn auth_mode_required(raw: Option<&str>) -> bool {
    match raw {
        None => true,
        Some(mode) => match mode.trim().to_ascii_lowercase().as_str() {
            "required" | "" => true,
            "disabled" | "off" | "none" => {
                tracing::warn!(
                    "identity.sync.auth={} — :9100 identity sync authentication is DISABLED. \
                     Any host reaching the port can exfiltrate and poison identity state. \
                     Only use this on a strictly isolated/loopback network.",
                    mode.trim()
                );
                false
            }
            other => {
                tracing::warn!(
                    "identity.sync.auth='{other}' is unrecognized; treating as REQUIRED \
                     (fail-closed). Use 'required', or (insecure) 'disabled'."
                );
                true
            }
        },
    }
}

fn identity_hmac_key_path(hive_id: &str) -> std::path::PathBuf {
    json_router::mesh_hmac::key_path(hive_id)
}

fn identity_shm_name(hive_id: &str) -> String {
    format!("/jsr-identity-{}", hive_id.trim())
}

fn identity_region_limits(hive: &HiveFile) -> IdentityRegionLimits {
    let section = hive.identity.as_ref();
    let max_ilks = section
        .and_then(|identity| identity.max_ilks)
        .unwrap_or(DEFAULT_IDENTITY_SHM_MAX_ILKS)
        .max(1);
    let max_tenants = section
        .and_then(|identity| identity.max_tenants)
        .unwrap_or(DEFAULT_IDENTITY_SHM_MAX_TENANTS)
        .max(1);
    let max_vocabulary = section
        .and_then(|identity| identity.max_vocabulary)
        .unwrap_or(DEFAULT_IDENTITY_SHM_MAX_VOCABULARY)
        .max(1);
    let max_ilk_aliases = section
        .and_then(|identity| identity.max_ilk_aliases)
        .unwrap_or(max_ilks)
        .max(1);
    IdentityRegionLimits {
        max_ilks,
        max_tenants,
        max_vocabulary,
        max_ilk_aliases,
    }
}

fn parse_ilk_type_for_shm(value: &str) -> u8 {
    match value.trim() {
        "human" => SHM_ILK_TYPE_HUMAN,
        "agent" => SHM_ILK_TYPE_AGENT,
        "system" => SHM_ILK_TYPE_SYSTEM,
        // Fail SAFE, not open: an unrecognized/empty ilk_type must NOT be
        // published as the most-privileged `system` type (vault treats `system`
        // as a root-pool master key). Map the unknown to the least-privileged
        // `human` so corruption / a partial sync / a bad writer denies rather
        // than grants. Legit `system` ilks always carry the exact "system"
        // string; anything else here is a bug worth surfacing.
        other => {
            tracing::warn!(
                ilk_type = %other,
                "unrecognized ilk_type published to SHM; defaulting to human (fail-safe)"
            );
            SHM_ILK_TYPE_HUMAN
        }
    }
}

fn parse_registration_status_for_shm(value: &str) -> u8 {
    match value.trim() {
        "temporary" => SHM_REG_STATUS_TEMPORARY,
        "partial" => SHM_REG_STATUS_PARTIAL,
        "complete" => SHM_REG_STATUS_COMPLETE,
        _ => SHM_REG_STATUS_TEMPORARY,
    }
}

fn parse_tenant_status_for_shm(value: &str) -> u8 {
    match value.trim() {
        "pending" => SHM_TENANT_STATUS_PENDING,
        "active" => SHM_TENANT_STATUS_ACTIVE,
        "suspended" => SHM_TENANT_STATUS_SUSPENDED,
        _ => SHM_TENANT_STATUS_PENDING,
    }
}

fn identification_str<'a>(identification: &'a Value, key: &str) -> Option<&'a str> {
    identification
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn sync_identity_shm_mappings(
    writer: &mut IdentityRegionWriter,
    store: &IdentityStore,
) -> Result<(), IdentityError> {
    let now_ms = now_epoch_ms();
    let mut tenant_entries: Vec<TenantEntry> = Vec::new();
    let mut ilk_entries: Vec<IlkEntry> = Vec::new();
    let mut ich_entries: Vec<IchEntry> = Vec::new();
    let mut alias_entries: Vec<IlkAliasEntry> = Vec::new();
    let vocabulary_entries: Vec<VocabularyEntry> = Vec::new();

    let mut tenant_ids: Vec<String> = store.tenants.keys().cloned().collect();
    tenant_ids.sort_unstable();
    for tenant_id in tenant_ids {
        let Some(tenant) = store.tenants.get(&tenant_id) else {
            continue;
        };
        let Ok(tenant_uuid) = parse_prefixed_uuid(&tenant.tenant_id, "tnt") else {
            tracing::warn!(
                tenant_id = %tenant.tenant_id,
                "skipping invalid tenant_id during identity shm sync"
            );
            continue;
        };
        let mut entry = TenantEntry {
            tenant_id: *tenant_uuid.as_bytes(),
            name: [0u8; 128],
            domain: [0u8; 128],
            status: parse_tenant_status_for_shm(&tenant.status),
            flags: FLAG_ACTIVE,
            _pad0: [0u8; 5],
            max_ilks: tenant
                .settings
                .get("max_ilks")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32,
            sponsor_tenant_id: tenant
                .sponsor_tenant_id
                .as_deref()
                .map(|sponsor_id| {
                    parse_prefixed_uuid(sponsor_id, "tnt").map(|uuid| *uuid.as_bytes())
                })
                .transpose()?
                .unwrap_or([0u8; 16]),
            created_at: now_ms,
            updated_at: now_ms,
        };
        copy_bytes_with_len(&mut entry.name, tenant.name.trim());
        if let Some(domain) = tenant
            .domain
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            copy_bytes_with_len(&mut entry.domain, domain);
        }
        tenant_entries.push(entry);
    }

    let mut channel_to_ich: HashMap<(String, String, String), [u8; 16]> = HashMap::new();
    let mut ilk_ids: Vec<String> = store.ilks.keys().cloned().collect();
    ilk_ids.sort_unstable();
    for ilk_id in ilk_ids {
        let Some(ilk) = store.ilks.get(&ilk_id) else {
            continue;
        };
        if ilk.deleted_at_ms.is_some() {
            continue;
        }
        let Ok(ilk_uuid) = parse_prefixed_uuid(&ilk.ilk_id, "ilk") else {
            tracing::warn!(
                ilk_id = %ilk.ilk_id,
                "skipping invalid ilk_id during identity shm sync"
            );
            continue;
        };
        let Ok(tenant_uuid) = parse_prefixed_uuid(&ilk.tenant_id, "tnt") else {
            tracing::warn!(
                ilk_id = %ilk.ilk_id,
                tenant_id = %ilk.tenant_id,
                "skipping ILK with invalid tenant_id during identity shm sync"
            );
            continue;
        };

        let ich_offset = ich_entries.len() as u32;
        let mut ich_count: u16 = 0;
        for (idx, channel) in ilk.channels.iter().enumerate() {
            let channel_type = channel.channel_type.trim().to_ascii_lowercase();
            let address = channel.address.trim().to_ascii_lowercase();
            if channel_type.is_empty() || address.is_empty() {
                continue;
            }
            let Ok(ich_uuid) = parse_prefixed_uuid(&channel.ich_id, "ich") else {
                tracing::warn!(
                    ilk_id = %ilk.ilk_id,
                    ich_id = %channel.ich_id,
                    "skipping invalid ich_id during identity shm sync"
                );
                continue;
            };
            let mut ich_entry = IchEntry {
                ich_id: *ich_uuid.as_bytes(),
                ilk_id: *ilk_uuid.as_bytes(),
                tenant_id: *tenant_uuid.as_bytes(),
                channel_type: [0u8; ICH_CHANNEL_TYPE_MAX_LEN],
                address: [0u8; ICH_ADDRESS_MAX_LEN],
                flags: FLAG_ACTIVE,
                owner_l2_name: [0u8; 128],
                is_primary: if idx == 0 { 1 } else { 0 },
                enabled: if channel.enabled { 1 } else { 0 },
                _pad0: [0u8; 5],
                added_at: now_ms,
            };
            copy_bytes_with_len(&mut ich_entry.channel_type, &channel_type);
            copy_bytes_with_len(&mut ich_entry.address, &address);
            if let Some(owner_l2_name) = channel.owner_l2_name.as_deref() {
                copy_bytes_with_len(&mut ich_entry.owner_l2_name, owner_l2_name);
            }
            ich_entries.push(ich_entry);
            ich_count = ich_count.saturating_add(1);
            channel_to_ich.insert(
                (
                    channel_type,
                    address,
                    ilk.tenant_id.trim().to_ascii_lowercase(),
                ),
                *ich_uuid.as_bytes(),
            );
        }

        let mut ilk_entry = IlkEntry {
            ilk_id: *ilk_uuid.as_bytes(),
            ilk_type: parse_ilk_type_for_shm(&ilk.ilk_type),
            registration_status: parse_registration_status_for_shm(&ilk.registration_status),
            flags: FLAG_ACTIVE,
            tenant_id: *tenant_uuid.as_bytes(),
            display_name: [0u8; 128],
            handler_node: [0u8; 128],
            ich_offset,
            ich_count,
            _pad0: [0u8; 2],
            role_hash: [0u8; 32],
            skill_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_SKILLS],
            skill_count: 0,
            handbook_count: 0,
            handbook_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_HANDBOOKS],
            created_at: now_ms,
            updated_at: now_ms,
            personality_hash: [0u8; 32],
            _reserved: [0u8; 8],
        };
        apply_definition_to_ilk_entry(&mut ilk_entry, &ilk.definition)?;
        let display_name = identification_str(&ilk.identification, "display_name")
            .or_else(|| identification_str(&ilk.identification, "node_name"))
            .unwrap_or(ilk.ilk_id.as_str());
        copy_bytes_with_len(&mut ilk_entry.display_name, display_name);
        if let Some(handler_node) = identification_str(&ilk.identification, "node_name") {
            copy_bytes_with_len(&mut ilk_entry.handler_node, handler_node);
        }
        ilk_entries.push(ilk_entry);
    }

    let mut alias_keys: Vec<String> = store.aliases.keys().cloned().collect();
    alias_keys.sort_unstable();
    for old_ilk_id in alias_keys {
        let Some(alias) = store.aliases.get(&old_ilk_id) else {
            continue;
        };
        if alias.expires_at_ms <= now_ms {
            continue;
        }
        let Ok(old_uuid) = parse_prefixed_uuid(&old_ilk_id, "ilk") else {
            tracing::warn!(
                old_ilk_id = %old_ilk_id,
                "skipping invalid alias old_ilk_id during identity shm sync"
            );
            continue;
        };
        let Ok(canonical_uuid) = parse_prefixed_uuid(&alias.canonical_ilk_id, "ilk") else {
            tracing::warn!(
                old_ilk_id = %old_ilk_id,
                canonical_ilk_id = %alias.canonical_ilk_id,
                "skipping invalid alias canonical_ilk_id during identity shm sync"
            );
            continue;
        };
        alias_entries.push(IlkAliasEntry {
            old_ilk_id: *old_uuid.as_bytes(),
            canonical_ilk_id: *canonical_uuid.as_bytes(),
            expires_at: alias.expires_at_ms,
            flags: FLAG_ACTIVE,
            _reserved: [0u8; 22],
        });
    }

    writer.write_snapshot_entries(
        &tenant_entries,
        &ilk_entries,
        &ich_entries,
        &alias_entries,
        &vocabulary_entries,
    )?;

    let mut mapped_channels = 0u64;
    let mut lookup_keys: Vec<(String, String, String)> = store.ich_lookup.keys().cloned().collect();
    lookup_keys.sort_unstable();
    for (channel_type, address, tenant_id_key) in lookup_keys {
        let Some(ilk_id) =
            store
                .ich_lookup
                .get(&(channel_type.clone(), address.clone(), tenant_id_key.clone()))
        else {
            continue;
        };
        let Ok(ilk_uuid) = parse_prefixed_uuid(ilk_id, "ilk") else {
            tracing::warn!(ilk_id = %ilk_id, "skipping invalid ilk_id during identity shm sync");
            continue;
        };
        let Some(ich_id) = channel_to_ich
            .get(&(channel_type.clone(), address.clone(), tenant_id_key.clone()))
            .copied()
        else {
            tracing::warn!(
                ilk_id = %ilk_id,
                channel_type = %channel_type,
                address = %address,
                "missing ich_id for lookup key during identity shm sync"
            );
            continue;
        };
        let Some(ilk_record) = store.ilks.get(ilk_id) else {
            continue;
        };
        let Ok(tenant_uuid) = parse_prefixed_uuid(&ilk_record.tenant_id, "tnt") else {
            tracing::warn!(
                ilk_id = %ilk_id,
                "skipping invalid tenant_id during identity shm mapping sync"
            );
            continue;
        };
        if let Err(err) = writer.upsert_ich_mapping(
            &channel_type,
            &address,
            ich_id,
            *ilk_uuid.as_bytes(),
            *tenant_uuid.as_bytes(),
        ) {
            tracing::warn!(
                ilk_id = %ilk_id,
                channel_type = %channel_type,
                address = %address,
                error = %err,
                "failed to upsert identity shm ich mapping"
            );
            continue;
        }
        mapped_channels = mapped_channels.saturating_add(1);
    }
    tracing::debug!(
        mapped_channels,
        tenant_count = tenant_entries.len(),
        ilk_count = ilk_entries.len(),
        ich_count = ich_entries.len(),
        alias_count = alias_entries.len(),
        "identity shm snapshot sync applied"
    );
    Ok(())
}

fn tenant_entry_from_record(tenant: &TenantRecord) -> Result<TenantEntry, IdentityError> {
    let tenant_uuid = parse_prefixed_uuid(&tenant.tenant_id, "tnt")?;
    let now_ms = now_epoch_ms();
    let mut entry = TenantEntry {
        tenant_id: *tenant_uuid.as_bytes(),
        name: [0u8; 128],
        domain: [0u8; 128],
        status: parse_tenant_status_for_shm(&tenant.status),
        flags: FLAG_ACTIVE,
        _pad0: [0u8; 5],
        max_ilks: tenant
            .settings
            .get("max_ilks")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        sponsor_tenant_id: tenant
            .sponsor_tenant_id
            .as_deref()
            .map(|sponsor_id| parse_prefixed_uuid(sponsor_id, "tnt").map(|uuid| *uuid.as_bytes()))
            .transpose()?
            .unwrap_or([0u8; 16]),
        created_at: now_ms,
        updated_at: now_ms,
    };
    copy_bytes_with_len(&mut entry.name, tenant.name.trim());
    if let Some(domain) = tenant
        .domain
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        copy_bytes_with_len(&mut entry.domain, domain);
    }
    Ok(entry)
}

fn ilk_entry_from_record(ilk: &IlkRecord) -> Result<IlkEntry, IdentityError> {
    let ilk_uuid = parse_prefixed_uuid(&ilk.ilk_id, "ilk")?;
    let tenant_uuid = parse_prefixed_uuid(&ilk.tenant_id, "tnt")?;
    let now_ms = now_epoch_ms();
    let mut entry = IlkEntry {
        ilk_id: *ilk_uuid.as_bytes(),
        ilk_type: parse_ilk_type_for_shm(&ilk.ilk_type),
        registration_status: parse_registration_status_for_shm(&ilk.registration_status),
        flags: FLAG_ACTIVE,
        tenant_id: *tenant_uuid.as_bytes(),
        display_name: [0u8; 128],
        handler_node: [0u8; 128],
        ich_offset: 0,
        ich_count: 0,
        _pad0: [0u8; 2],
        role_hash: [0u8; 32],
        skill_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_SKILLS],
        skill_count: 0,
        handbook_count: 0,
        handbook_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_HANDBOOKS],
        created_at: now_ms,
        updated_at: now_ms,
        personality_hash: [0u8; 32],
        _reserved: [0u8; 8],
    };
    apply_definition_to_ilk_entry(&mut entry, &ilk.definition)?;
    let display_name = identification_str(&ilk.identification, "display_name")
        .or_else(|| identification_str(&ilk.identification, "node_name"))
        .unwrap_or(ilk.ilk_id.as_str());
    copy_bytes_with_len(&mut entry.display_name, display_name);
    if let Some(handler_node) = identification_str(&ilk.identification, "node_name") {
        copy_bytes_with_len(&mut entry.handler_node, handler_node);
    }
    Ok(entry)
}

fn ich_entries_from_ilk_record(
    ilk: &IlkRecord,
) -> Result<Vec<(IchEntry, String, String)>, IdentityError> {
    let ilk_uuid = parse_prefixed_uuid(&ilk.ilk_id, "ilk")?;
    let now_ms = now_epoch_ms();
    let mut entries = Vec::with_capacity(ilk.channels.len());
    for (idx, channel) in ilk.channels.iter().enumerate() {
        let ich_uuid = parse_prefixed_uuid(&channel.ich_id, "ich")?;
        let channel_type = channel.channel_type.trim().to_ascii_lowercase();
        let address = channel.address.trim().to_ascii_lowercase();
        if channel_type.is_empty() || address.is_empty() {
            continue;
        }
        let mut entry = IchEntry {
            ich_id: *ich_uuid.as_bytes(),
            ilk_id: *ilk_uuid.as_bytes(),
            tenant_id: parse_prefixed_uuid(&ilk.tenant_id, "tnt")?.into_bytes(),
            channel_type: [0u8; ICH_CHANNEL_TYPE_MAX_LEN],
            address: [0u8; ICH_ADDRESS_MAX_LEN],
            flags: FLAG_ACTIVE,
            owner_l2_name: [0u8; 128],
            is_primary: if idx == 0 { 1 } else { 0 },
            enabled: if channel.enabled { 1 } else { 0 },
            _pad0: [0u8; 5],
            added_at: now_ms,
        };
        copy_bytes_with_len(&mut entry.channel_type, &channel_type);
        copy_bytes_with_len(&mut entry.address, &address);
        if let Some(owner_l2_name) = channel.owner_l2_name.as_deref() {
            copy_bytes_with_len(&mut entry.owner_l2_name, owner_l2_name);
        }
        entries.push((entry, channel_type, address));
    }
    Ok(entries)
}

fn alias_entry_from_record(alias: &AliasSnapshotRecord) -> Result<IlkAliasEntry, IdentityError> {
    let old_uuid = parse_prefixed_uuid(&alias.old_ilk_id, "ilk")?;
    let canonical_uuid = parse_prefixed_uuid(&alias.canonical_ilk_id, "ilk")?;
    Ok(IlkAliasEntry {
        old_ilk_id: *old_uuid.as_bytes(),
        canonical_ilk_id: *canonical_uuid.as_bytes(),
        expires_at: alias.expires_at_ms,
        flags: FLAG_ACTIVE,
        _reserved: [0u8; 22],
    })
}

fn apply_identity_shm_delta(
    writer: &mut IdentityRegionWriter,
    delta: &IdentityDelta,
) -> Result<(), IdentityError> {
    match delta {
        IdentityDelta::TenantUpsert { tenant } => {
            writer.upsert_tenant_entry(tenant_entry_from_record(tenant)?)?;
        }
        IdentityDelta::IlkUpsert { ilk } => {
            let ilk_uuid = parse_prefixed_uuid(&ilk.ilk_id, "ilk")?;
            let tenant_uuid = parse_prefixed_uuid(&ilk.tenant_id, "tnt")?;
            writer.upsert_ilk_entry(ilk_entry_from_record(ilk)?)?;
            writer.clear_ich_mappings_for_ilk(*ilk_uuid.as_bytes())?;
            let ich_entries = ich_entries_from_ilk_record(ilk)?;
            let entries_only: Vec<IchEntry> =
                ich_entries.iter().map(|(entry, _, _)| *entry).collect();
            writer.replace_ich_entries_for_ilk(*ilk_uuid.as_bytes(), &entries_only)?;
            for (entry, channel_type, address) in ich_entries {
                writer.upsert_ich_mapping(
                    &channel_type,
                    &address,
                    entry.ich_id,
                    *ilk_uuid.as_bytes(),
                    *tenant_uuid.as_bytes(),
                )?;
            }
        }
        IdentityDelta::IlkDelete { ilk_id } => {
            let ilk_uuid = parse_prefixed_uuid(ilk_id, "ilk")?;
            writer.clear_ich_mappings_for_ilk(*ilk_uuid.as_bytes())?;
            writer.replace_ich_entries_for_ilk(*ilk_uuid.as_bytes(), &[])?;
            writer.remove_ilk_entry(*ilk_uuid.as_bytes())?;
        }
        IdentityDelta::AliasUpsert { alias } => {
            writer.upsert_ilk_alias_entry(alias_entry_from_record(alias)?)?;
        }
        IdentityDelta::AliasDelete { old_ilk_id } => {
            let old_uuid = parse_prefixed_uuid(old_ilk_id, "ilk")?;
            writer.remove_ilk_alias_entry(*old_uuid.as_bytes())?;
        }
    }
    Ok(())
}

fn apply_identity_shm_provision_fast(
    writer: &mut IdentityRegionWriter,
    ilk: &IlkRecord,
) -> Result<bool, IdentityError> {
    if ilk.registration_status != "temporary" || ilk.channels.is_empty() {
        return Ok(false);
    }

    let ilk_uuid = parse_prefixed_uuid(&ilk.ilk_id, "ilk")?;
    if writer.read_snapshot().is_some_and(|snapshot| {
        snapshot
            .ilks
            .iter()
            .any(|entry| entry.ilk_id == *ilk_uuid.as_bytes())
    }) {
        tracing::debug!(
            ilk_id = %ilk.ilk_id,
            "identity shm fast provision skipped; ilk already present"
        );
        return Ok(false);
    }

    let ich_entries = ich_entries_from_ilk_record(ilk)?;
    if ich_entries.is_empty() {
        return Ok(false);
    }

    let entries_only: Vec<IchEntry> = ich_entries.iter().map(|(entry, _, _)| *entry).collect();
    let apply_started = Instant::now();
    let ilk_entry = ilk_entry_from_record(ilk)?;
    writer.provision_temporary_ilk(ilk_entry, &entries_only)?;
    tracing::info!(
        ilk_id = %ilk.ilk_id,
        channel_count = entries_only.len(),
        elapsed_us = apply_started.elapsed().as_micros() as u64,
        "identity shm provision fast path applied"
    );
    Ok(true)
}

fn apply_identity_shm_deltas(
    writer: &mut IdentityRegionWriter,
    store: &IdentityStore,
    action: &str,
    deltas: &[IdentityDeltaEnvelope],
) -> Result<(), IdentityError> {
    if deltas
        .iter()
        .any(|delta| matches!(&delta.delta, IdentityDelta::IlkDelete { .. }))
    {
        sync_identity_shm_mappings(writer, store)?;
        return Ok(());
    }

    if action == MSG_ILK_PROVISION
        && deltas.len() == 1
        && matches!(&deltas[0].delta, IdentityDelta::IlkUpsert { .. })
    {
        if let IdentityDelta::IlkUpsert { ilk } = &deltas[0].delta {
            if apply_identity_shm_provision_fast(writer, ilk)? {
                return Ok(());
            }
        }
    }

    for delta in deltas {
        if let Err(err) = apply_identity_shm_delta(writer, &delta.delta) {
            tracing::warn!(
                action,
                error = %err,
                "identity shm incremental apply failed; rebuilding full snapshot"
            );
            sync_identity_shm_mappings(writer, store)?;
            return Ok(());
        }
    }
    Ok(())
}

// --- :9100 per-hive HMAC handshake (mutual challenge-response) ---

const AUTH_OP_HELLO: &str = "IDENTITY_AUTH_HELLO";
const AUTH_OP_CHALLENGE: &str = "IDENTITY_AUTH_CHALLENGE";
const AUTH_OP_RESPONSE: &str = "IDENTITY_AUTH_RESPONSE";
const AUTH_OP_OK: &str = "IDENTITY_AUTH_OK";
const AUTH_OP_ERROR: &str = "IDENTITY_AUTH_ERROR";
/// Per-read timeout during the handshake, so a peer that connects and stalls
/// cannot pin the primary's accept path (it awaits the handshake inline).
const AUTH_HANDSHAKE_READ_TIMEOUT_SECS: u64 = 10;

#[derive(Serialize, Deserialize)]
struct AuthHello {
    operation: String,
    hive: String,
    nonce: String,
}
#[derive(Serialize, Deserialize)]
struct AuthChallenge {
    operation: String,
    nonce: String,
}
#[derive(Serialize, Deserialize)]
struct AuthMac {
    operation: String,
    mac: String,
}
#[derive(Serialize, Deserialize)]
struct AuthErrorMsg {
    operation: String,
    message: String,
}

/// Write `buf` with a hard deadline (F-02): a peer that stops reading cannot pin
/// the writer task. On timeout the connection is errored and the caller drops it.
async fn write_timed(
    w: &mut tokio::net::tcp::OwnedWriteHalf,
    buf: &[u8],
) -> Result<(), IdentityError> {
    time::timeout(
        Duration::from_secs(IDENTITY_SYNC_WRITE_TIMEOUT_SECS),
        w.write_all(buf),
    )
    .await
    .map_err(|_| -> IdentityError { "sync write timeout".into() })??;
    Ok(())
}

/// Flush with the same deadline as [`write_timed`].
async fn flush_timed(w: &mut tokio::net::tcp::OwnedWriteHalf) -> Result<(), IdentityError> {
    time::timeout(
        Duration::from_secs(IDENTITY_SYNC_WRITE_TIMEOUT_SECS),
        w.flush(),
    )
    .await
    .map_err(|_| -> IdentityError { "sync flush timeout".into() })??;
    Ok(())
}

async fn auth_write_line(
    w: &mut tokio::net::tcp::OwnedWriteHalf,
    value: &impl Serialize,
) -> Result<(), IdentityError> {
    write_timed(w, serde_json::to_string(value)?.as_bytes()).await?;
    write_timed(w, b"\n").await?;
    flush_timed(w).await?;
    Ok(())
}

/// Read one `\n`-terminated frame with a HARD size cap (G-4). Unlike
/// `read_line`/`read_until`, which grow the buffer until EOF or newline, this
/// errors once `max_bytes` is exceeded, so a peer streaming bytes with no
/// newline cannot OOM the process. Uses the `AsyncBufRead` fill/consume API to
/// avoid per-byte awaits. Returns the number of bytes read into `line`.
async fn read_capped_line(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    line: &mut String,
    max_bytes: usize,
) -> Result<usize, IdentityError> {
    let mut raw: Vec<u8> = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            break; // EOF
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            raw.extend_from_slice(&available[..=pos]);
            let consumed = pos + 1;
            reader.consume(consumed);
            break;
        }
        raw.extend_from_slice(available);
        let consumed = available.len();
        reader.consume(consumed);
        if raw.len() > max_bytes {
            return Err(format!("sync frame exceeds max line size {max_bytes}").into());
        }
    }
    if raw.len() > max_bytes {
        return Err(format!("sync frame exceeds max line size {max_bytes}").into());
    }
    let n = raw.len();
    match std::str::from_utf8(&raw) {
        Ok(s) => line.push_str(s),
        Err(_) => return Err("sync frame is not valid utf-8".into()),
    }
    Ok(n)
}

/// Capped read (G-4) with an optional idle deadline for the whole frame. On
/// timeout the connection is errored (and the caller closes it), so a peer that
/// stalls mid-frame cannot pin a reader task.
async fn read_sync_line(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    line: &mut String,
    max_bytes: usize,
    timeout: Option<Duration>,
) -> Result<usize, IdentityError> {
    match timeout {
        Some(d) => time::timeout(d, read_capped_line(reader, line, max_bytes))
            .await
            .map_err(|_| -> IdentityError { "sync read timeout".into() })?,
        None => read_capped_line(reader, line, max_bytes).await,
    }
}

async fn auth_read_line(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> Result<String, IdentityError> {
    let mut line = String::new();
    let n = read_sync_line(
        reader,
        &mut line,
        MAX_SYNC_LINE_BYTES,
        Some(Duration::from_secs(AUTH_HANDSHAKE_READ_TIMEOUT_SECS)),
    )
    .await?;
    if n == 0 {
        return Err("auth handshake: connection closed".into());
    }
    Ok(line.trim().to_string())
}

/// Server side: read HELLO, load the claimed hive's key, challenge, verify the
/// client's proof, then prove ourselves. Returns the authenticated hive_id. On
/// any failure sends AUTH_ERROR and errors out (the caller closes the conn).
async fn server_auth_handshake(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<String, IdentityError> {
    use json_router::mesh_hmac::{self, MeshHmacKey};
    let hello: AuthHello = serde_json::from_str(&auth_read_line(reader).await?)
        .map_err(|e| -> IdentityError { format!("auth hello parse: {e}").into() })?;
    // Uniform wire-level rejection (no hive-existence / parse-vs-key oracle); the
    // specific reason is only logged locally via the returned Err.
    async fn reject(
        write_half: &mut tokio::net::tcp::OwnedWriteHalf,
        log: String,
    ) -> Result<String, IdentityError> {
        let _ = auth_write_line(
            write_half,
            &AuthErrorMsg {
                operation: AUTH_OP_ERROR.into(),
                message: "authentication failed".into(),
            },
        )
        .await;
        Err(log.into())
    }
    if hello.operation != AUTH_OP_HELLO {
        return reject(write_half, "auth: expected hello".into()).await;
    }
    let hive = hello.hive.trim().to_string();
    // Validate the attacker-controlled claimed hive_id BEFORE it touches the
    // filesystem path (guards against `/abs` or `..` traversal in key_path).
    if !mesh_hmac::is_valid_hive_id(&hive) {
        return reject(write_half, format!("auth: invalid hive id '{hive}'")).await;
    }
    let key = match MeshHmacKey::load_from_file(&identity_hmac_key_path(&hive)) {
        Ok(k) => k,
        Err(err) => {
            return reject(write_half, format!("auth: no key for hive '{hive}': {err}")).await;
        }
    };
    let server_nonce = mesh_hmac::random_nonce();
    auth_write_line(
        write_half,
        &AuthChallenge {
            operation: AUTH_OP_CHALLENGE.into(),
            nonce: server_nonce.clone(),
        },
    )
    .await?;
    let resp: AuthMac = serde_json::from_str(&auth_read_line(reader).await?)
        .map_err(|e| -> IdentityError { format!("auth response parse: {e}").into() })?;
    if resp.operation != AUTH_OP_RESPONSE {
        return reject(write_half, "auth: expected response".into()).await;
    }
    if mesh_hmac::verify_proof(
        &key,
        mesh_hmac::CLIENT_CONTEXT,
        &server_nonce,
        &hive,
        &resp.mac,
    )
    .is_err()
    {
        return reject(
            write_half,
            format!("auth: HMAC verification failed for hive '{hive}'"),
        )
        .await;
    }
    let server_proof = mesh_hmac::prove(&key, mesh_hmac::SERVER_CONTEXT, &hello.nonce, &hive);
    auth_write_line(
        write_half,
        &AuthMac {
            operation: AUTH_OP_OK.into(),
            mac: server_proof,
        },
    )
    .await?;
    Ok(hive)
}

/// Client side: identify as `self_hive`, answer the primary's challenge, and
/// verify the primary proves it holds the same key (mutual auth).
async fn client_auth_handshake(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    key: &json_router::mesh_hmac::MeshHmacKey,
    self_hive: &str,
) -> Result<(), IdentityError> {
    use json_router::mesh_hmac;
    let client_nonce = mesh_hmac::random_nonce();
    auth_write_line(
        write_half,
        &AuthHello {
            operation: AUTH_OP_HELLO.into(),
            hive: self_hive.to_string(),
            nonce: client_nonce.clone(),
        },
    )
    .await?;
    let challenge: AuthChallenge =
        serde_json::from_str(&auth_read_line(reader).await?).map_err(|e| -> IdentityError {
            format!("auth challenge parse (rejected?): {e}").into()
        })?;
    if challenge.operation != AUTH_OP_CHALLENGE {
        return Err("auth: primary did not challenge (rejected?)".into());
    }
    let response = mesh_hmac::prove(key, mesh_hmac::CLIENT_CONTEXT, &challenge.nonce, self_hive);
    auth_write_line(
        write_half,
        &AuthMac {
            operation: AUTH_OP_RESPONSE.into(),
            mac: response,
        },
    )
    .await?;
    let ok: AuthMac = serde_json::from_str(&auth_read_line(reader).await?)
        .map_err(|e| -> IdentityError { format!("auth ok parse (rejected?): {e}").into() })?;
    if ok.operation != AUTH_OP_OK {
        return Err("auth: primary rejected the handshake".into());
    }
    mesh_hmac::verify_proof(
        key,
        mesh_hmac::SERVER_CONTEXT,
        &client_nonce,
        self_hive,
        &ok.mac,
    )
    .map_err(|_| -> IdentityError {
        "auth: primary HMAC verification failed (wrong key?)".into()
    })?;
    Ok(())
}

/// Replica connect helper: TCP-connect to the primary and run the client auth
/// handshake when `auth_key` is set. Returns the framed (reader, writer) ready
/// for the sync protocol.
async fn connect_and_auth(
    upstream: &str,
    auth_key: Option<&json_router::mesh_hmac::MeshHmacKey>,
    self_hive: &str,
) -> Result<
    (
        BufReader<tokio::net::tcp::OwnedReadHalf>,
        tokio::net::tcp::OwnedWriteHalf,
    ),
    IdentityError,
> {
    let stream = TcpStream::connect(upstream).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    if let Some(key) = auth_key {
        client_auth_handshake(&mut reader, &mut write_half, key, self_hive).await?;
    }
    Ok((reader, write_half))
}

async fn handle_sync_connection(
    stream: TcpStream,
    chunks_req_tx: mpsc::UnboundedSender<oneshot::Sender<Arc<Vec<IdentityFullSyncChunk>>>>,
    ingest_tx: mpsc::Sender<IngestFrame>,
    auth_required: bool,
) -> Result<Option<mpsc::Sender<IdentityDeltaEnvelope>>, IdentityError> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    // Per-hive HMAC handshake before any protocol message (when required).
    let authed_hive: Option<String> = if auth_required {
        Some(server_auth_handshake(&mut reader, &mut write_half).await?)
    } else {
        None
    };
    // Timed read (via auth_read_line) plus this whole function now running in a
    // per-connection task (F-02) so a peer that stalls cannot pin the main loop.
    let request_line = auth_read_line(&mut reader).await?;
    let request: IdentitySyncRequest = serde_json::from_str(&request_line)?;
    match request.operation.as_str() {
        SYNC_OP_FULL_SYNC_REQUEST => {
            // Build the snapshot on-demand via the main loop (G-9: only for an
            // authenticated FULL_SYNC, never eagerly per connection). The
            // (potentially slow) streaming below runs in this spawned task, off
            // the main loop (F-02), with per-write deadlines.
            let (reply_tx, reply_rx) = oneshot::channel();
            chunks_req_tx.send(reply_tx).map_err(|_| -> IdentityError {
                "identity main loop unavailable for full sync".into()
            })?;
            let chunks = reply_rx
                .await
                .map_err(|_| -> IdentityError { "full sync snapshot request dropped".into() })?;
            for chunk in chunks.iter() {
                let encoded = serde_json::to_string(chunk)?;
                write_timed(&mut write_half, encoded.as_bytes()).await?;
                write_timed(&mut write_half, b"\n").await?;
            }
            flush_timed(&mut write_half).await?;
            Ok(None)
        }
        SYNC_OP_DELTA_SUBSCRIBE => {
            // Bounded (G-5): a slow subscriber whose queue fills is dropped by
            // broadcast_deltas (it recovers via full-sync on reconnect) instead
            // of buffering deltas without bound and exhausting primary memory.
            let (tx, mut rx) =
                mpsc::channel::<IdentityDeltaEnvelope>(IDENTITY_SUBSCRIBER_CHANNEL_CAP);
            let ack = json!({
                "status": "ok",
                "operation": "IDENTITY_DELTA_SUBSCRIBED"
            });
            write_timed(&mut write_half, serde_json::to_string(&ack)?.as_bytes()).await?;
            write_timed(&mut write_half, b"\n").await?;
            flush_timed(&mut write_half).await?;
            tokio::spawn(async move {
                while let Some(envelope) = rx.recv().await {
                    let encoded = match serde_json::to_string(&envelope) {
                        Ok(encoded) => encoded,
                        Err(err) => {
                            tracing::warn!(error = %err, seq = envelope.seq, "failed to encode identity delta frame");
                            continue;
                        }
                    };
                    let mut acked = false;
                    for attempt in 1..=IDENTITY_DELTA_MAX_RETRIES {
                        if write_timed(&mut write_half, encoded.as_bytes())
                            .await
                            .is_err()
                        {
                            return;
                        }
                        if write_timed(&mut write_half, b"\n").await.is_err() {
                            return;
                        }
                        if flush_timed(&mut write_half).await.is_err() {
                            return;
                        }
                        match wait_for_delta_ack(&mut reader, envelope.seq).await {
                            Ok(()) => {
                                acked = true;
                                break;
                            }
                            Err(err) => {
                                tracing::warn!(
                                    seq = envelope.seq,
                                    attempt,
                                    max_retries = IDENTITY_DELTA_MAX_RETRIES,
                                    error = %err,
                                    "identity delta ack not received; retrying"
                                );
                            }
                        }
                    }
                    if !acked {
                        tracing::warn!(
                            seq = envelope.seq,
                            max_retries = IDENTITY_DELTA_MAX_RETRIES,
                            "identity delta ack retries exhausted; closing subscriber stream"
                        );
                        return;
                    }
                }
            });
            Ok(Some(tx))
        }
        SYNC_OP_DELTA_PUBLISH => {
            // A replica publishes its own `@hive` ilks upstream. Require the
            // hive_id so the main loop can enforce per-hive authority.
            let Some(publisher_hive) = request
                .hive_id
                .as_deref()
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(str::to_string)
            else {
                let payload = IdentitySyncError {
                    status: "error".to_string(),
                    error_code: "INVALID_REQUEST".to_string(),
                    message: "DELTA_PUBLISH requires hive_id".to_string(),
                };
                write_half
                    .write_all(serde_json::to_string(&payload)?.as_bytes())
                    .await?;
                write_half.write_all(b"\n").await?;
                write_half.flush().await?;
                return Ok(None);
            };
            // Bind the HMAC-authenticated identity to the published-as hive_id:
            // a peer must not authenticate as one hive and push another's ilks.
            if let Some(ref authed) = authed_hive {
                if authed != &publisher_hive {
                    let payload = IdentitySyncError {
                        status: "error".to_string(),
                        error_code: "AUTH_MISMATCH".to_string(),
                        message: format!(
                            "authenticated hive '{authed}' may not publish as '{publisher_hive}'"
                        ),
                    };
                    write_half
                        .write_all(serde_json::to_string(&payload)?.as_bytes())
                        .await?;
                    write_half.write_all(b"\n").await?;
                    write_half.flush().await?;
                    return Ok(None);
                }
            }
            let ack = json!({ "status": "ok", "operation": SYNC_OP_PUBLISH_OK });
            write_half
                .write_all(serde_json::to_string(&ack)?.as_bytes())
                .await?;
            write_half.write_all(b"\n").await?;
            write_half.flush().await?;
            // Read the publish stream until the replica disconnects; the main
            // loop applies + re-broadcasts the ingested frames.
            tokio::spawn(async move {
                if let Err(err) = run_publish_reader(
                    &mut reader,
                    &mut write_half,
                    publisher_hive.clone(),
                    ingest_tx,
                )
                .await
                {
                    tracing::warn!(hive = %publisher_hive, error = %err, "identity publish reader ended");
                }
            });
            Ok(None)
        }
        _ => {
            let payload = IdentitySyncError {
                status: "error".to_string(),
                error_code: "INVALID_REQUEST".to_string(),
                message: format!("unsupported sync operation '{}'", request.operation),
            };
            let encoded = serde_json::to_string(&payload)?;
            write_half.write_all(encoded.as_bytes()).await?;
            write_half.write_all(b"\n").await?;
            write_half.flush().await?;
            Ok(None)
        }
    }
}

async fn fetch_full_sync_from_primary(
    upstream: &str,
    auth_key: Option<&json_router::mesh_hmac::MeshHmacKey>,
    self_hive: &str,
) -> Result<IdentityStore, IdentityError> {
    let (mut reader, mut write_half) = connect_and_auth(upstream, auth_key, self_hive).await?;
    let request = IdentitySyncRequest {
        operation: SYNC_OP_FULL_SYNC_REQUEST.to_string(),
        hive_id: None,
    };
    let encoded = serde_json::to_string(&request)?;
    write_half.write_all(encoded.as_bytes()).await?;
    write_half.write_all(b"\n").await?;
    write_half.flush().await?;

    let mut line = String::new();
    let mut expected_chunks: Option<usize> = None;
    let mut received: Vec<Option<IdentityFullSyncChunk>> = Vec::new();

    loop {
        line.clear();
        let n = read_sync_line(
            &mut reader,
            &mut line,
            MAX_SYNC_LINE_BYTES,
            Some(Duration::from_secs(IDENTITY_SYNC_READ_IDLE_SECS)),
        )
        .await?;
        if n == 0 {
            break;
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }

        if let Ok(err_payload) = serde_json::from_str::<IdentitySyncError>(raw) {
            if err_payload.status == "error" {
                return Err(format!(
                    "full sync rejected: {} ({})",
                    err_payload.error_code, err_payload.message
                )
                .into());
            }
        }

        let chunk: IdentityFullSyncChunk = serde_json::from_str(raw)?;
        if chunk.operation != SYNC_OP_FULL_SYNC {
            return Err(format!("unexpected sync operation '{}'", chunk.operation).into());
        }

        let total = chunk.total_chunks as usize;
        let idx = chunk.chunk as usize;
        if total == 0 || idx == 0 || idx > total {
            return Err("invalid chunk numbering in full sync payload".into());
        }
        // G-7: bound the advertised chunk count BEFORE allocating the reassembly
        // buffer, so a crafted/corrupted total_chunks cannot drive a huge alloc
        // that aborts the process.
        if total > MAX_FULL_SYNC_CHUNKS {
            return Err(format!(
                "full sync total_chunks {total} exceeds max {MAX_FULL_SYNC_CHUNKS}"
            )
            .into());
        }

        if let Some(expected) = expected_chunks {
            if expected != total {
                return Err("inconsistent total_chunks in full sync payload".into());
            }
        } else {
            expected_chunks = Some(total);
            received.resize(total, None);
        }

        if let Some(slot) = received.get_mut(idx - 1) {
            *slot = Some(chunk);
        } else {
            return Err("chunk index out of range".into());
        }

        if received.iter().all(|entry| entry.is_some()) {
            break;
        }
    }

    if received.is_empty() || received.iter().any(|entry| entry.is_none()) {
        return Err("incomplete full sync stream".into());
    }

    let chunks: Vec<IdentityFullSyncChunk> = received.into_iter().flatten().collect();
    IdentityStore::from_full_sync_chunks(&chunks)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err).into())
}

fn slice_chunk<T: Clone>(items: &[T], chunk_index: usize, chunk_size: usize) -> Vec<T> {
    let start = chunk_index.saturating_mul(chunk_size);
    if start >= items.len() {
        return Vec::new();
    }
    let end = (start + chunk_size).min(items.len());
    items[start..end].to_vec()
}

fn delta_envelope(delta: IdentityDelta) -> IdentityDeltaEnvelope {
    IdentityDeltaEnvelope {
        version: IDENTITY_SYNC_VERSION,
        operation: SYNC_OP_DELTA.to_string(),
        seq: 0,
        delta,
    }
}

fn assign_delta_seqs(deltas: &mut [IdentityDeltaEnvelope], next_seq: &mut u64) {
    for delta in deltas.iter_mut() {
        delta.seq = *next_seq;
        *next_seq = next_seq.saturating_add(1);
    }
}

/// Replica side: forward locally-originated deltas to the upstream publish task
/// so the primary (motherbee) learns this hive's ilk changes. Tenants are
/// primary-authoritative and skipped. Deltas received downstream are NOT routed
/// here (they go through `delta_event_rx`, which only applies) — so a delta is
/// never echoed back upstream and there is no loop.
fn push_local_deltas_upstream(
    deltas: &[IdentityDeltaEnvelope],
    upstream_tx: &mpsc::UnboundedSender<UpstreamFrame>,
) {
    for envelope in deltas {
        if matches!(envelope.delta, IdentityDelta::TenantUpsert { .. }) {
            continue;
        }
        let _ = upstream_tx.send(UpstreamFrame::Delta(envelope.delta.clone()));
    }
}

fn broadcast_deltas(
    subscribers: &mut Vec<mpsc::Sender<IdentityDeltaEnvelope>>,
    deltas: &[IdentityDeltaEnvelope],
) {
    if deltas.is_empty() {
        return;
    }
    subscribers.retain(|tx| {
        for delta in deltas {
            // Non-blocking (G-5): a subscriber whose bounded queue is full (slow
            // or stuck) is DROPPED rather than blocking the main loop or letting
            // memory grow without bound. It recovers via full-sync on reconnect.
            if tx.try_send(delta.clone()).is_err() {
                return false;
            }
        }
        true
    });
}

async fn run_delta_subscription_loop(
    upstream: String,
    sink: mpsc::UnboundedSender<IdentityDeltaEnvelope>,
    auth_key: Option<json_router::mesh_hmac::MeshHmacKey>,
    self_hive: String,
) {
    loop {
        match stream_deltas_from_primary(&upstream, &sink, auth_key.as_ref(), &self_hive).await {
            Ok(()) => {
                tracing::warn!(upstream = %upstream, "identity delta stream closed; reconnecting")
            }
            Err(err) => {
                tracing::warn!(upstream = %upstream, error = %err, "identity delta stream failed; reconnecting")
            }
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

async fn stream_deltas_from_primary(
    upstream: &str,
    sink: &mpsc::UnboundedSender<IdentityDeltaEnvelope>,
    auth_key: Option<&json_router::mesh_hmac::MeshHmacKey>,
    self_hive: &str,
) -> Result<(), IdentityError> {
    let (mut reader, mut write_half) = connect_and_auth(upstream, auth_key, self_hive).await?;
    let request = IdentitySyncRequest {
        operation: SYNC_OP_DELTA_SUBSCRIBE.to_string(),
        hive_id: None,
    };
    let encoded = serde_json::to_string(&request)?;
    write_half.write_all(encoded.as_bytes()).await?;
    write_half.write_all(b"\n").await?;
    write_half.flush().await?;

    let mut line = String::new();
    let mut last_seq: Option<u64> = None;
    loop {
        line.clear();
        // Size-capped (G-4) but NO idle timeout: this is a long-poll for deltas
        // that may be sparse on a quiet mesh; a timeout would churn reconnects.
        let n = read_sync_line(&mut reader, &mut line, MAX_SYNC_LINE_BYTES, None).await?;
        if n == 0 {
            return Ok(());
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }

        if let Ok(err_payload) = serde_json::from_str::<IdentitySyncError>(raw) {
            if err_payload.status == "error" {
                return Err(format!(
                    "delta subscribe rejected: {} ({})",
                    err_payload.error_code, err_payload.message
                )
                .into());
            }
        }

        if let Ok(envelope) = serde_json::from_str::<IdentityDeltaEnvelope>(raw) {
            if envelope.operation == SYNC_OP_DELTA {
                if envelope.seq == 0 {
                    return Err("delta stream payload with seq=0 is invalid".into());
                }
                if let Some(last) = last_seq {
                    if envelope.seq == last {
                        send_delta_ack(&mut write_half, envelope.seq).await?;
                        continue;
                    }
                    if envelope.seq != last.saturating_add(1) {
                        // F-05: a sequence gap means we missed one or more deltas
                        // — possibly a revocation/demotion, which vault trusts
                        // (lost => privilege RETENTION on this replica). The
                        // replica store lives on the main loop and cannot be
                        // swapped from this task, so recover the same way the
                        // vault-secret path does: exit(0) and let systemd restart
                        // us, which re-runs the boot full-sync and rebuilds a
                        // consistent store. Silently re-subscribing (the old
                        // behavior) would adopt the gap as a fresh baseline and
                        // keep serving stale identity indefinitely.
                        tracing::error!(
                            prev = last,
                            current = envelope.seq,
                            "identity delta stream sequence gap; exiting for a clean full-sync \
                             re-bootstrap on systemd-managed restart"
                        );
                        std::process::exit(0);
                    }
                }
                if sink.send(envelope.clone()).is_err() {
                    return Err("delta sink dropped".into());
                }
                last_seq = Some(envelope.seq);
                send_delta_ack(&mut write_half, envelope.seq).await?;
            }
        }
    }
}

async fn wait_for_delta_ack(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    expected_seq: u64,
) -> Result<(), IdentityError> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = read_sync_line(
            reader,
            &mut line,
            MAX_SYNC_LINE_BYTES,
            Some(Duration::from_millis(IDENTITY_DELTA_ACK_TIMEOUT_MS)),
        )
        .await?;
        if read == 0 {
            return Err("delta subscriber closed while waiting ack".into());
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        let ack: IdentityDeltaAck = serde_json::from_str(raw)?;
        if ack.operation != SYNC_OP_DELTA_ACK {
            return Err(format!("unexpected delta ack operation '{}'", ack.operation).into());
        }
        if ack.seq != expected_seq {
            return Err(format!(
                "unexpected delta ack seq {} (expected {})",
                ack.seq, expected_seq
            )
            .into());
        }
        return Ok(());
    }
}

async fn send_delta_ack(
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    seq: u64,
) -> Result<(), IdentityError> {
    let ack = IdentityDeltaAck {
        operation: SYNC_OP_DELTA_ACK.to_string(),
        seq,
    };
    let encoded = serde_json::to_string(&ack)?;
    write_half.write_all(encoded.as_bytes()).await?;
    write_half.write_all(b"\n").await?;
    write_half.flush().await?;
    Ok(())
}

// ===========================================================================
// Upstream push (replica → primary): bidirectional ilk sync. A replica publishes
// its own `@hive` ilks so the primary (motherbee) converges to the additive
// union of the whole mesh. See docs/onworking COA/ilk-bidirectional-sync.md.
// ===========================================================================

/// A frame the replica's main loop hands to its publish task: an incremental
/// local delta (low latency) or a full self-owned snapshot (periodic
/// reconciliation that recovers upserts AND hard-deletes after a reconnect gap).
enum UpstreamFrame {
    Delta(IdentityDelta),
    Snapshot(Vec<IlkRecord>),
}

/// A frame the primary's publish-connection reader hands to the main loop, tagged
/// with the authenticated publishing hive for the authority check.
enum IngestFrame {
    Delta {
        publisher_hive: String,
        delta: IdentityDelta,
    },
    Snapshot {
        publisher_hive: String,
        ilks: Vec<IlkRecord>,
    },
}

/// Replica side: keep an upstream publish connection to the primary alive,
/// reconnecting on failure. Owns the receiver so the same backlog is drained
/// across reconnects.
async fn run_delta_publish_loop(
    upstream: String,
    hive_id: String,
    mut rx: mpsc::UnboundedReceiver<UpstreamFrame>,
    auth_key: Option<json_router::mesh_hmac::MeshHmacKey>,
) {
    loop {
        match publish_to_primary(&upstream, &hive_id, &mut rx, auth_key.as_ref()).await {
            Ok(()) => {
                tracing::warn!(upstream = %upstream, "identity publish channel closed; reconnecting")
            }
            Err(err) => {
                tracing::warn!(upstream = %upstream, error = %err, "identity publish channel failed; reconnecting")
            }
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

/// One publish-connection lifetime: handshake `DELTA_PUBLISH{hive_id}`, then
/// drain frames, writing each + waiting for the primary's ACK. A frame consumed
/// when the link drops is lost, but the periodic snapshot reconciles it.
async fn publish_to_primary(
    upstream: &str,
    hive_id: &str,
    rx: &mut mpsc::UnboundedReceiver<UpstreamFrame>,
    auth_key: Option<&json_router::mesh_hmac::MeshHmacKey>,
) -> Result<(), IdentityError> {
    let (mut reader, mut write_half) = connect_and_auth(upstream, auth_key, hive_id).await?;
    let request = IdentitySyncRequest {
        operation: SYNC_OP_DELTA_PUBLISH.to_string(),
        hive_id: Some(hive_id.to_string()),
    };
    write_half
        .write_all(serde_json::to_string(&request)?.as_bytes())
        .await?;
    write_half.write_all(b"\n").await?;
    write_half.flush().await?;

    // Handshake ack (the primary confirms it accepted the publish channel).
    let mut line = String::new();
    let n = read_sync_line(
        &mut reader,
        &mut line,
        MAX_SYNC_LINE_BYTES,
        Some(Duration::from_secs(AUTH_HANDSHAKE_READ_TIMEOUT_SECS)),
    )
    .await?;
    if n == 0 {
        return Err("publish handshake connection closed".into());
    }
    if let Ok(err_payload) = serde_json::from_str::<IdentitySyncError>(line.trim()) {
        if err_payload.status == "error" {
            return Err(format!(
                "delta publish rejected: {} ({})",
                err_payload.error_code, err_payload.message
            )
            .into());
        }
    }

    let mut next_seq: u64 = 1;
    while let Some(frame) = rx.recv().await {
        let seq = next_seq;
        next_seq = next_seq.saturating_add(1);
        let encoded = match frame {
            UpstreamFrame::Delta(delta) => serde_json::to_string(&IdentityDeltaEnvelope {
                version: IDENTITY_SYNC_VERSION,
                operation: SYNC_OP_DELTA.to_string(),
                seq,
                delta,
            })?,
            UpstreamFrame::Snapshot(ilks) => serde_json::to_string(&IdentityPublishSnapshot {
                operation: SYNC_OP_PUBLISH_SNAPSHOT.to_string(),
                seq,
                hive_id: hive_id.to_string(),
                ilks,
            })?,
        };
        // Retry write+ack on ack timeout: the primary dedups by seq, so a
        // re-sent frame is idempotent. A write error (dead link) propagates
        // immediately to trigger a reconnect.
        let mut acked = false;
        for attempt in 1..=IDENTITY_DELTA_MAX_RETRIES {
            write_half.write_all(encoded.as_bytes()).await?;
            write_half.write_all(b"\n").await?;
            write_half.flush().await?;
            match wait_for_delta_ack(&mut reader, seq).await {
                Ok(()) => {
                    acked = true;
                    break;
                }
                Err(err) => {
                    tracing::warn!(seq, attempt, error = %err, "identity publish ack not received; retrying");
                }
            }
        }
        if !acked {
            return Err("identity publish ack retries exhausted".into());
        }
    }
    Ok(())
}

/// Primary side: read a replica's publish stream (after the `DELTA_PUBLISH`
/// handshake), forwarding each frame to the main loop via `ingest_tx` and ACKing
/// it. Runs until the replica disconnects.
async fn run_publish_reader(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    publisher_hive: String,
    ingest_tx: mpsc::Sender<IngestFrame>,
) -> Result<(), IdentityError> {
    let mut line = String::new();
    let mut last_seq: Option<u64> = None;
    loop {
        line.clear();
        // Size-capped (G-4) but NO idle timeout: a replica's publish stream is a
        // long-poll (reconcile snapshots + on-change deltas), sparse on a quiet
        // mesh; a timeout would churn reconnects. The cap stops the OOM vector.
        let n = read_sync_line(reader, &mut line, MAX_SYNC_LINE_BYTES, None).await?;
        if n == 0 {
            return Ok(());
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(raw)?;
        let operation = value
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let seq = value.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        if seq == 0 {
            return Err("publish frame with seq=0 is invalid".into());
        }
        // Duplicate (retry) — ack and skip re-ingesting.
        if last_seq == Some(seq) {
            send_delta_ack(write_half, seq).await?;
            continue;
        }
        if let Some(last) = last_seq {
            if seq != last.saturating_add(1) {
                return Err(format!(
                    "publish sequence gap/out-of-order: prev={last} current={seq}"
                )
                .into());
            }
        }
        let frame = if operation == SYNC_OP_PUBLISH_SNAPSHOT {
            let snapshot: IdentityPublishSnapshot = serde_json::from_value(value)?;
            IngestFrame::Snapshot {
                publisher_hive: publisher_hive.clone(),
                ilks: snapshot.ilks,
            }
        } else if operation == SYNC_OP_DELTA {
            let envelope: IdentityDeltaEnvelope = serde_json::from_value(value)?;
            IngestFrame::Delta {
                publisher_hive: publisher_hive.clone(),
                delta: envelope.delta,
            }
        } else {
            return Err(format!("unsupported publish frame operation '{operation}'").into());
        };
        // Bounded send: a full queue backpressures here (we stop reading + the
        // replica's writes block) instead of growing primary memory. We ACK only
        // after the frame is queued, so the replica won't advance past an
        // unconsumed frame.
        if ingest_tx.send(frame).await.is_err() {
            return Err("identity ingest sink dropped".into());
        }
        last_seq = Some(seq);
        send_delta_ack(write_half, seq).await?;
    }
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, IdentityError> {
    let raw = fs::read_to_string(config_dir.join("hive.yaml"))?;
    Ok(serde_yaml::from_str(&raw)?)
}

/// An ILK is well-known and seeded by `ensure_system_ilks_from_hive` when
/// `identification.source == "hive.system_nodes"`. These ILKs are the
/// deterministic SY identities and must not be removable through admin paths:
/// their deletion would silently break vault authorization and node bootstrap
/// the next time the SY node restarts.
fn is_well_known_system_ilk(ilk: &IlkRecord) -> bool {
    ilk.identification
        .get("source")
        .and_then(Value::as_str)
        .map(|value| value == "hive.system_nodes")
        .unwrap_or(false)
}

fn system_nodes_for_hive(hive: &HiveFile) -> Result<Vec<String>, String> {
    let section = hive
        .system_nodes
        .as_ref()
        .ok_or_else(|| "system_nodes section is required".to_string())?;
    let is_primary = is_mother_role(hive.role.as_deref());
    let role_section = if is_primary {
        section.motherbee.as_ref()
    } else {
        section.worker.as_ref()
    }
    .ok_or_else(|| {
        format!(
            "system_nodes.{} section is required",
            if is_primary { "motherbee" } else { "worker" }
        )
    })?;
    let nodes = &role_section.nodes;
    if nodes.is_empty() {
        return Err("system node list is empty".to_string());
    }
    let mut seen_nodes = HashSet::new();
    let mut out = Vec::with_capacity(nodes.len());
    for raw_name in nodes {
        let name = raw_name.trim();
        if !name.starts_with("SY.") {
            return Err(format!("system node '{name}' must use SY.* naming"));
        }
        if !seen_nodes.insert(name.to_string()) {
            return Err(format!("duplicate system node '{name}'"));
        }
        out.push(name.to_string());
    }
    Ok(out)
}

/// Derive the systemd-style service name for a SY base name. Matches the orchestrator's
/// helper of the same name — keep both in sync if either changes.
fn name_to_service(node_name: &str) -> String {
    let trimmed = node_name.trim();
    let base = trimmed.strip_prefix("SY.").unwrap_or(trimmed);
    format!("sy-{}", base.to_ascii_lowercase().replace('.', "-"))
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, hive_id)
    }
}

// Re-exported from the SDK so sy_vault, sy_admin, and any future consumer
// of the well-known SY ILK formula share a single source of truth. Keeping
// a local alias here so existing call sites compile unchanged.
use fluxbee_sdk::deterministic_system_ilk_id;

fn response_name(action: &str) -> &'static str {
    match action {
        MSG_ILK_PROVISION => MSG_ILK_PROVISION_RESPONSE,
        MSG_ILK_LIST => MSG_ILK_LIST_RESPONSE,
        MSG_ILK_GET => MSG_ILK_GET_RESPONSE,
        MSG_TNT_LIST => MSG_TNT_LIST_RESPONSE,
        MSG_TNT_GET => MSG_TNT_GET_RESPONSE,
        MSG_ILK_REGISTER => MSG_ILK_REGISTER_RESPONSE,
        MSG_ILK_ADD_CHANNEL => MSG_ILK_ADD_CHANNEL_RESPONSE,
        MSG_ILK_UPDATE => MSG_ILK_UPDATE_RESPONSE,
        MSG_ILK_SET_DEFINITION => MSG_ILK_SET_DEFINITION_RESPONSE,
        MSG_ILK_DELETE => MSG_ILK_DELETE_RESPONSE,
        MSG_ICH_SET_ENABLED => MSG_ICH_SET_ENABLED_RESPONSE,
        MSG_TNT_CREATE => MSG_TNT_CREATE_RESPONSE,
        MSG_TNT_UPDATE => MSG_TNT_UPDATE_RESPONSE,
        MSG_TNT_SET_SPONSOR => MSG_TNT_SET_SPONSOR_RESPONSE,
        MSG_TNT_APPROVE => MSG_TNT_APPROVE_RESPONSE,
        "IDENTITY_METRICS" => "IDENTITY_METRICS_RESPONSE",
        "CONFIG_GET" | "CONFIG_SET" => "CONFIG_RESPONSE",
        _ => "SYSTEM_ERROR",
    }
}

fn action_requires_primary(action: &str) -> bool {
    matches!(
        action,
        MSG_ILK_PROVISION
            | MSG_ILK_REGISTER
            | MSG_ILK_ADD_CHANNEL
            | MSG_ILK_UPDATE
            | MSG_ILK_SET_DEFINITION
            | MSG_ILK_DELETE
            | MSG_ICH_SET_ENABLED
            | MSG_TNT_CREATE
            | MSG_TNT_UPDATE
            | MSG_TNT_SET_SPONSOR
            | MSG_TNT_APPROVE
    )
}

async fn send_system_response(
    sender: &NodeSender,
    request: &Message,
    msg_name: &str,
    payload: Value,
) -> Result<(), IdentityError> {
    let reply = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(request.routing.src.clone()),
            ttl: 16,
            trace_id: request.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(msg_name.to_string()),
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
    sender.send(reply).await?;
    Ok(())
}

const RPC_CH_SYSTEM: &str = "system";

fn build_identity_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_SYSTEM)
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_SYSTEM),
        )
        .build()
}

fn unauthorized_identity_source_payload(
    action: &str,
    src_uuid: &str,
    src_l2_name: Option<&str>,
) -> Value {
    json!({
        "status": "error",
        "error_code": "UNAUTHORIZED_REGISTRAR",
        "message": "source not authorized for action",
        "action": action,
        "src_uuid": src_uuid,
        "src_l2_name": src_l2_name,
    })
}

fn authorize_ich_enabled_mutation(
    store: &IdentityStore,
    ich_id: &str,
    src_l2_name: Option<&str>,
) -> Result<(), Value> {
    let Some(caller) = src_l2_name
        .map(str::trim)
        .filter(|value| value.starts_with("IO."))
    else {
        return Ok(());
    };
    match store.ich_owner_l2_name(ich_id) {
        Ok(Some(owner)) if owner == caller => Ok(()),
        Ok(_) => Err(error_payload(
            "UNAUTHORIZED",
            "IO callers may only change enabled state for their own ICH",
        )),
        Err(code) => Err(error_payload(&code, "failed to set ich enabled state")),
    }
}

fn error_payload(error_code: &str, message: &str) -> Value {
    json!({
        "status": "error",
        "error_code": error_code,
        "message": message,
    })
}

fn db_write_error_payload(context: &str, err: &(dyn std::error::Error + 'static)) -> Value {
    let code = map_db_write_error_code(err);
    error_payload(code, &format!("{}: {}", context, err))
}

fn map_db_write_error_code(err: &(dyn std::error::Error + 'static)) -> &'static str {
    if let Some(pg_err) = err.downcast_ref::<tokio_postgres::Error>() {
        if let Some(db_err) = pg_err.as_db_error() {
            if db_err.code() == &SqlState::UNIQUE_VIOLATION {
                if let Some(constraint) = db_err.constraint() {
                    return map_unique_constraint_code(constraint);
                }
                return "DUPLICATE_CONSTRAINT";
            }
        }
    }
    let message = err.to_string().to_ascii_lowercase();
    if message.contains("duplicate key value violates unique constraint")
        || message.contains("violates unique constraint")
    {
        if message.contains("idx_identity_ilks_email")
            || message.contains("identity_ilks_email")
            || message.contains("email")
        {
            return "DUPLICATE_EMAIL";
        }
        if message.contains("idx_identity_ilks_node_name")
            || message.contains("identity_ilks_node_name")
            || message.contains("node_name")
        {
            return "DUPLICATE_NODE_NAME";
        }
        if message.contains("identity_ichs_channel_type_address_tenant_id_key")
            || message.contains("identity_ichs")
        {
            return "DUPLICATE_ICH";
        }
        return "DUPLICATE_CONSTRAINT";
    }
    "DB_WRITE_FAILED"
}

fn map_unique_constraint_code(constraint: &str) -> &'static str {
    match constraint {
        "idx_identity_ilks_email" => "DUPLICATE_EMAIL",
        "idx_identity_ilks_node_name" => "DUPLICATE_NODE_NAME",
        "identity_ichs_channel_type_address_tenant_id_key" => "DUPLICATE_ICH",
        _ => "DUPLICATE_CONSTRAINT",
    }
}

fn canonical_ich_key(
    channel_type: &str,
    address: &str,
    tenant_id: &str,
) -> (String, String, String) {
    (
        channel_type.trim().to_ascii_lowercase(),
        address.trim().to_ascii_lowercase(),
        tenant_id.trim().to_ascii_lowercase(),
    )
}

fn normalize_tenant_name(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_tenant_domain(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
}

fn parse_prefixed_uuid(value: &str, prefix: &str) -> Result<Uuid, String> {
    let trimmed = value.trim();
    let expected = format!("{}:", prefix);
    if !trimmed.starts_with(&expected) {
        return Err("INVALID_REQUEST".to_string());
    }
    let raw = &trimmed[expected.len()..];
    Uuid::parse_str(raw).map_err(|_| "INVALID_REQUEST".to_string())
}

/// Validator for `ilk_type` arriving via ILK_REGISTER (frontdesk/orchestrator).
/// External registrars may only mint `human`/`agent` principals. `"system"` is
/// reserved for SY-internal creation (`ensure_system_ilks_from_hive`, driven by
/// hive.yaml system_nodes) and is exactly the type `sy_vault::authorize_read`
/// treats as a root-tenant pool master key — a compromised/buggy frontdesk must
/// not be able to stamp an external identity `system`. (F-08)
fn validate_ilk_type(value: &str) -> Result<(), String> {
    if matches!(value.trim(), "human" | "agent") {
        return Ok(());
    }
    Err("INVALID_REQUEST".to_string())
}

/// Validator for `ilk_type` arriving via ILK_PROVISION. IO nodes can only
/// declare humans or agents; `"system"` is reserved for SY-internal creation.
fn validate_provision_ilk_type(value: &str) -> Result<(), String> {
    if matches!(value.trim(), "human" | "agent") {
        return Ok(());
    }
    Err("INVALID_REQUEST".to_string())
}

fn validate_non_empty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("INVALID_REQUEST: missing {}", field));
    }
    Ok(())
}

fn validate_max_len(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.len() > max {
        return Err(format!("INVALID_REQUEST: {} too long (max {})", field, max));
    }
    Ok(())
}

fn validate_channel_input(channel: &ChannelInput) -> Result<(), String> {
    let _ = parse_prefixed_uuid(&channel.ich_id, "ich")?;
    validate_non_empty("channel.type", &channel.channel_type)?;
    validate_non_empty("channel.address", &channel.address)?;
    validate_max_len(
        "channel.type",
        &channel.channel_type,
        ICH_CHANNEL_TYPE_MAX_LEN,
    )?;
    validate_max_len("channel.address", &channel.address, ICH_ADDRESS_MAX_LEN)?;
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn normalize_hash_array(
    definition: &Value,
    key: &str,
    max_len: usize,
) -> Result<Vec<String>, String> {
    let Some(raw) = definition.get(key) else {
        return Ok(Vec::new());
    };
    let Some(values) = raw.as_array() else {
        return Err("INVALID_DEFINITION".to_string());
    };
    if values.len() > max_len {
        return Err("DEFINITION_TOO_LARGE".to_string());
    }
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let Some(hash) = value.as_str().map(str::trim).filter(|v| !v.is_empty()) else {
            return Err("INVALID_DEFINITION".to_string());
        };
        if !is_sha256_hex(hash) {
            return Err("INVALID_DEFINITION_HASH".to_string());
        }
        out.push(hash.to_ascii_lowercase());
    }
    Ok(out)
}

fn normalize_optional_hash(definition: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(raw) = definition.get(key) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let Some(hash) = raw.as_str().map(str::trim).filter(|v| !v.is_empty()) else {
        return Err("INVALID_DEFINITION".to_string());
    };
    if !is_sha256_hex(hash) {
        return Err("INVALID_DEFINITION_HASH".to_string());
    }
    Ok(Some(hash.to_ascii_lowercase()))
}

fn normalize_agent_definition(definition: &Value) -> Result<Value, String> {
    let Some(obj) = definition.as_object() else {
        return Err("INVALID_DEFINITION".to_string());
    };
    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "role_hash" | "skill_hashes" | "handbook_hashes" | "personality_hash"
        ) {
            return Err("INVALID_DEFINITION".to_string());
        }
    }

    let role_hash = normalize_optional_hash(definition, "role_hash")?;
    let skill_hashes =
        normalize_hash_array(definition, "skill_hashes", AGENT_DEFINITION_MAX_SKILLS)?;
    let handbook_hashes = normalize_hash_array(
        definition,
        "handbook_hashes",
        AGENT_DEFINITION_MAX_HANDBOOKS,
    )?;
    let personality_hash = normalize_optional_hash(definition, "personality_hash")?;

    if role_hash.is_none()
        && skill_hashes.is_empty()
        && handbook_hashes.is_empty()
        && personality_hash.is_none()
    {
        return Ok(json!({}));
    }

    let mut out = Map::new();
    if let Some(role_hash) = role_hash {
        out.insert("role_hash".to_string(), Value::String(role_hash));
    }
    out.insert(
        "skill_hashes".to_string(),
        Value::Array(skill_hashes.into_iter().map(Value::String).collect()),
    );
    out.insert(
        "handbook_hashes".to_string(),
        Value::Array(handbook_hashes.into_iter().map(Value::String).collect()),
    );
    if let Some(personality_hash) = personality_hash {
        out.insert(
            "personality_hash".to_string(),
            Value::String(personality_hash),
        );
    }
    Ok(Value::Object(out))
}

fn normalize_definition_for_ilk_type(ilk_type: &str, definition: Value) -> Value {
    if ilk_type == "agent" {
        normalize_agent_definition(&definition).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    }
}

fn agent_definition_present(definition: &Value) -> bool {
    definition
        .as_object()
        .map(|obj| {
            obj.get("role_hash").is_some()
                || obj
                    .get("skill_hashes")
                    .and_then(Value::as_array)
                    .map(|items| !items.is_empty())
                    .unwrap_or(false)
                || obj
                    .get("handbook_hashes")
                    .and_then(Value::as_array)
                    .map(|items| !items.is_empty())
                    .unwrap_or(false)
                || obj.get("personality_hash").is_some()
        })
        .unwrap_or(false)
}

fn definition_hash_array_for_shm<const N: usize>(
    definition: &Value,
    key: &str,
) -> Result<([[u8; 32]; N], u16), IdentityError> {
    let mut out = [[0u8; 32]; N];
    let Some(values) = definition.get(key).and_then(Value::as_array) else {
        return Ok((out, 0));
    };
    if values.len() > N {
        return Err(format!("definition {} exceeds shm limit {}", key, N).into());
    }
    for (idx, value) in values.iter().enumerate() {
        let hash = value
            .as_str()
            .and_then(sha256_hex_to_bytes)
            .ok_or_else(|| format!("invalid definition hash in {}", key))?;
        out[idx] = hash;
    }
    Ok((out, values.len() as u16))
}

fn apply_definition_to_ilk_entry(
    entry: &mut IlkEntry,
    definition: &Value,
) -> Result<(), IdentityError> {
    entry.role_hash = definition
        .get("role_hash")
        .and_then(Value::as_str)
        .and_then(sha256_hex_to_bytes)
        .unwrap_or([0u8; 32]);
    let (skill_hashes, skill_count) = definition_hash_array_for_shm::<
        IDENTITY_DEFINITION_MAX_SKILLS,
    >(definition, "skill_hashes")?;
    let (handbook_hashes, handbook_count) = definition_hash_array_for_shm::<
        IDENTITY_DEFINITION_MAX_HANDBOOKS,
    >(definition, "handbook_hashes")?;
    entry.skill_hashes = skill_hashes;
    entry.skill_count = skill_count;
    entry.handbook_hashes = handbook_hashes;
    entry.handbook_count = handbook_count;
    entry.personality_hash = definition
        .get("personality_hash")
        .and_then(Value::as_str)
        .and_then(sha256_hex_to_bytes)
        .unwrap_or([0u8; 32]);
    Ok(())
}

fn normalize_optional_owner_l2_name(owner_l2_name: Option<&str>) -> Result<Option<String>, String> {
    let Some(owner_l2_name) = owner_l2_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    validate_max_len("owner_l2_name", owner_l2_name, 128)?;
    Ok(Some(owner_l2_name.to_string()))
}

async fn ensure_primary_schema(database_config: &PgConfig) -> Result<(), IdentityError> {
    let (client, connection) = database_config.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::error!(error = %err, "identity postgres connection closed");
        }
    });

    // F-10: serialize concurrent schema runs (primary + a fast restart) with a
    // session advisory lock, and keep an auditable, ordered version trail in
    // identity_schema_migrations instead of ad-hoc CREATE/ALTER IF NOT EXISTS
    // with no drift/version visibility. The current idempotent DDL is baseline
    // "v1": existing DBs run it as a no-op and simply get stamped. Future
    // breaking/backfill changes get their own numbered, transactional migration.
    const IDENTITY_SCHEMA_LOCK_KEY: i64 = 0x1DEA_0001;
    client
        .execute("SELECT pg_advisory_lock($1)", &[&IDENTITY_SCHEMA_LOCK_KEY])
        .await?;
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS identity_schema_migrations (\n\
             version INTEGER PRIMARY KEY,\n\
             name TEXT NOT NULL,\n\
             applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()\n\
             );",
        )
        .await?;

    client
        .batch_execute(
            r#"
CREATE TABLE IF NOT EXISTS identity_tenants (
    tenant_id UUID PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    domain VARCHAR(128),
    status VARCHAR(16) NOT NULL DEFAULT 'pending',
    settings JSONB NOT NULL DEFAULT '{}',
    sponsor_tenant_id UUID REFERENCES identity_tenants(tenant_id),
    approved_by UUID,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_identity_tenants_sponsor
    ON identity_tenants(sponsor_tenant_id)
    WHERE sponsor_tenant_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS identity_ilks (
    ilk_id UUID PRIMARY KEY,
    ilk_type VARCHAR(16) NOT NULL,
    registration_status VARCHAR(16) NOT NULL DEFAULT 'temporary',
    tenant_id UUID NOT NULL REFERENCES identity_tenants(tenant_id),
    email VARCHAR(256),
    node_name VARCHAR(128),
    identification JSONB NOT NULL DEFAULT '{}',
    association JSONB NOT NULL DEFAULT '{}',
    definition JSONB NOT NULL DEFAULT '{}',
    registered_by UUID,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    deleted_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_identity_ilks_email
    ON identity_ilks(email, tenant_id)
    WHERE email IS NOT NULL AND deleted_at IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_identity_ilks_node_name
    ON identity_ilks(node_name)
    WHERE node_name IS NOT NULL AND deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_identity_ilks_tenant
    ON identity_ilks(tenant_id);

CREATE INDEX IF NOT EXISTS idx_identity_ilks_type
    ON identity_ilks(ilk_type);

CREATE INDEX IF NOT EXISTS idx_identity_ilks_status
    ON identity_ilks(registration_status);

CREATE TABLE IF NOT EXISTS identity_ilk_aliases (
    old_ilk_id UUID PRIMARY KEY,
    canonical_ilk_id UUID NOT NULL REFERENCES identity_ilks(ilk_id),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_identity_ilk_aliases_canonical
    ON identity_ilk_aliases(canonical_ilk_id);

CREATE INDEX IF NOT EXISTS idx_identity_ilk_aliases_expires
    ON identity_ilk_aliases(expires_at);

CREATE TABLE IF NOT EXISTS identity_ichs (
    ich_id UUID PRIMARY KEY,
    ilk_id UUID NOT NULL REFERENCES identity_ilks(ilk_id),
    tenant_id UUID NOT NULL REFERENCES identity_tenants(tenant_id),
    channel_type VARCHAR(32) NOT NULL,
    address VARCHAR(256) NOT NULL,
    owner_l2_name VARCHAR(128),
    is_primary BOOLEAN DEFAULT FALSE,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    added_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(channel_type, address, tenant_id)
);

CREATE INDEX IF NOT EXISTS idx_identity_ichs_lookup
    ON identity_ichs(channel_type, address);

CREATE INDEX IF NOT EXISTS idx_identity_ichs_ilk
    ON identity_ichs(ilk_id);

CREATE INDEX IF NOT EXISTS idx_identity_ichs_owner
    ON identity_ichs(owner_l2_name)
    WHERE owner_l2_name IS NOT NULL;

CREATE TABLE IF NOT EXISTS identity_vocabulary (
    tag VARCHAR(64) PRIMARY KEY,
    category VARCHAR(16) NOT NULL,
    description TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    deprecated_at TIMESTAMPTZ
);
"#,
        )
        .await?;
    client
        .batch_execute(
            r#"
ALTER TABLE identity_tenants
    ADD COLUMN IF NOT EXISTS sponsor_tenant_id UUID REFERENCES identity_tenants(tenant_id);

ALTER TABLE identity_ichs
    ADD COLUMN IF NOT EXISTS owner_l2_name VARCHAR(128);

ALTER TABLE identity_ichs
    ADD COLUMN IF NOT EXISTS enabled BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS idx_identity_tenants_sponsor
    ON identity_tenants(sponsor_tenant_id)
    WHERE sponsor_tenant_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_identity_ichs_owner
    ON identity_ichs(owner_l2_name)
    WHERE owner_l2_name IS NOT NULL;
"#,
        )
        .await?;

    // Stamp the baseline version (idempotent). Future migrations append higher
    // versions, each in its own transaction, keyed off this table.
    client
        .execute(
            "INSERT INTO identity_schema_migrations (version, name) VALUES (1, 'baseline') \
             ON CONFLICT (version) DO NOTHING",
            &[],
        )
        .await?;
    let _ = client
        .execute(
            "SELECT pg_advisory_unlock($1)",
            &[&IDENTITY_SCHEMA_LOCK_KEY],
        )
        .await;

    tracing::info!("identity primary schema ensured (baseline v1)");
    Ok(())
}

async fn gc_aliases_in_db(database_config: &PgConfig) -> Result<u64, IdentityError> {
    let (client, connection) = database_config.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "identity alias gc postgres connection closed");
        }
    });

    let rows = client
        .query(
            r#"
WITH expired AS (
    DELETE FROM identity_ilk_aliases
    WHERE expires_at <= NOW()
    RETURNING old_ilk_id
),
soft_deleted AS (
    UPDATE identity_ilks i
    SET deleted_at = NOW(), updated_at = NOW()
    FROM expired e
    WHERE i.ilk_id = e.old_ilk_id
      AND i.registration_status = 'temporary'
      AND i.deleted_at IS NULL
    RETURNING i.ilk_id
)
SELECT COUNT(*)::BIGINT AS removed_count FROM expired
"#,
            &[],
        )
        .await?;

    let removed = rows
        .first()
        .map(|row| row.get::<_, i64>(0))
        .unwrap_or(0)
        .max(0) as u64;
    Ok(removed)
}

fn optional_identification_string(
    identification: &Value,
    key: &str,
    max_len: usize,
) -> Result<Option<String>, IdentityError> {
    let value = identification
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    if let Some(ref value) = value {
        if value.len() > max_len {
            return Err(format!("identification.{} too long (max {})", key, max_len).into());
        }
    }
    Ok(value)
}

fn association_json_from_ilk(ilk: &IlkRecord) -> Value {
    let channels: Vec<Value> = ilk
        .channels
        .iter()
        .map(|channel| {
            json!({
                "ich_id": channel.ich_id,
                "type": channel.channel_type,
                "address": channel.address,
                "owner_l2_name": channel.owner_l2_name,
                "enabled": channel.enabled,
            })
        })
        .collect();
    json!({
        "tenant_id": ilk.tenant_id,
        "channels": channels,
    })
}

fn definition_json_from_ilk(ilk: &IlkRecord) -> Value {
    normalize_definition_for_ilk_type(&ilk.ilk_type, ilk.definition.clone())
}

async fn persist_ilk_state_in_db(
    database_config: &PgConfig,
    ilk: &IlkRecord,
    alias: Option<&AliasSnapshotRecord>,
) -> Result<(), IdentityError> {
    let ilk_uuid = parse_prefixed_uuid(&ilk.ilk_id, "ilk")?.to_string();
    let tenant_uuid = parse_prefixed_uuid(&ilk.tenant_id, "tnt")?.to_string();
    let email = optional_identification_string(&ilk.identification, "email", 256)?;
    let node_name = optional_identification_string(&ilk.identification, "node_name", 128)?;
    let association = association_json_from_ilk(ilk);
    let definition = definition_json_from_ilk(ilk);
    let deleted_at_ms = ilk
        .deleted_at_ms
        .and_then(|value| i64::try_from(value).ok());
    let registered_by: Option<String> = None;

    let (mut client, connection) = database_config.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "identity ilk persist postgres connection closed");
        }
    });

    let tx = client.transaction().await?;
    tx.execute(
        r#"
INSERT INTO identity_ilks (
    ilk_id,
    ilk_type,
    registration_status,
    tenant_id,
    email,
    node_name,
    identification,
    association,
    definition,
    registered_by,
    deleted_at,
    updated_at
)
VALUES (
    $1::text::uuid,
    $2,
    $3,
    $4::text::uuid,
    $5,
    $6,
    $7::jsonb,
    $8::jsonb,
    $9::jsonb,
    $10::text::uuid,
    CASE WHEN $11::BIGINT IS NULL THEN NULL ELSE to_timestamp(($11::DOUBLE PRECISION) / 1000.0) END,
    NOW()
)
ON CONFLICT (ilk_id) DO UPDATE
SET
    ilk_type = EXCLUDED.ilk_type,
    registration_status = EXCLUDED.registration_status,
    tenant_id = EXCLUDED.tenant_id,
    email = EXCLUDED.email,
    node_name = EXCLUDED.node_name,
    identification = EXCLUDED.identification,
    association = EXCLUDED.association,
    definition = EXCLUDED.definition,
    registered_by = EXCLUDED.registered_by,
    deleted_at = EXCLUDED.deleted_at,
    updated_at = NOW()
"#,
        &[
            &ilk_uuid,
            &ilk.ilk_type,
            &ilk.registration_status,
            &tenant_uuid,
            &email,
            &node_name,
            &ilk.identification,
            &association,
            &definition,
            &registered_by,
            &deleted_at_ms,
        ],
    )
    .await?;

    if ilk.deleted_at_ms.is_some() {
        tx.execute(
            "DELETE FROM identity_ichs WHERE ilk_id = $1::text::uuid",
            &[&ilk_uuid],
        )
        .await?;
    } else {
        for (index, channel) in ilk.channels.iter().enumerate() {
            let ich_uuid = parse_prefixed_uuid(&channel.ich_id, "ich")?.to_string();
            let is_primary = index == 0;
            tx.execute(
                r#"
INSERT INTO identity_ichs (
    ich_id,
    ilk_id,
    tenant_id,
    channel_type,
    address,
    owner_l2_name,
    is_primary,
    enabled,
    added_at
)
VALUES ($1::text::uuid, $2::text::uuid, $3::text::uuid, $4, $5, $6, $7, $8, NOW())
ON CONFLICT (ich_id) DO UPDATE
SET
    ilk_id = EXCLUDED.ilk_id,
    tenant_id = EXCLUDED.tenant_id,
    channel_type = EXCLUDED.channel_type,
    address = EXCLUDED.address,
    owner_l2_name = EXCLUDED.owner_l2_name,
    is_primary = EXCLUDED.is_primary,
    enabled = EXCLUDED.enabled
"#,
                &[
                    &ich_uuid,
                    &ilk_uuid,
                    &tenant_uuid,
                    &channel.channel_type,
                    &channel.address,
                    &channel.owner_l2_name,
                    &is_primary,
                    &channel.enabled,
                ],
            )
            .await?;
        }
    }

    if let Some(alias_record) = alias {
        let old_uuid = parse_prefixed_uuid(&alias_record.old_ilk_id, "ilk")?.to_string();
        let canonical_uuid =
            parse_prefixed_uuid(&alias_record.canonical_ilk_id, "ilk")?.to_string();
        let expires_at_ms = i64::try_from(alias_record.expires_at_ms)
            .map_err(|_| "alias expires_at_ms overflow")?;
        tx.execute(
            r#"
INSERT INTO identity_ilk_aliases (old_ilk_id, canonical_ilk_id, expires_at)
VALUES (
    $1::text::uuid,
    $2::text::uuid,
    to_timestamp(($3::BIGINT)::DOUBLE PRECISION / 1000.0)
)
ON CONFLICT (old_ilk_id) DO UPDATE
SET
    canonical_ilk_id = EXCLUDED.canonical_ilk_id,
    expires_at = EXCLUDED.expires_at
"#,
            &[&old_uuid, &canonical_uuid, &expires_at_ms],
        )
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

async fn delete_ilk_in_db(database_config: &PgConfig, ilk_id: &str) -> Result<(), IdentityError> {
    let ilk_uuid = parse_prefixed_uuid(ilk_id, "ilk")?.to_string();
    let (mut client, connection) = database_config.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "identity ilk delete postgres connection closed");
        }
    });
    let tx = client.transaction().await?;
    tx.execute(
        "DELETE FROM identity_ichs WHERE ilk_id = $1::text::uuid",
        &[&ilk_uuid],
    )
    .await?;
    tx.execute(
        "DELETE FROM identity_ilk_aliases WHERE old_ilk_id = $1::text::uuid OR canonical_ilk_id = $1::text::uuid",
        &[&ilk_uuid],
    )
    .await?;
    tx.execute(
        "DELETE FROM identity_ilks WHERE ilk_id = $1::text::uuid",
        &[&ilk_uuid],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn load_identity_store_from_db(
    database_config: &PgConfig,
) -> Result<IdentityStore, IdentityError> {
    let (client, connection) = database_config.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "identity tenant load postgres connection closed");
        }
    });

    let rows = client
        .query(
            r#"
SELECT
    tenant_id::text AS tenant_id,
    name,
    domain,
    status,
    settings,
    sponsor_tenant_id::text AS sponsor_tenant_id
FROM identity_tenants
ORDER BY created_at ASC
"#,
            &[],
        )
        .await?;

    let mut store = IdentityStore::default();
    for row in rows {
        let tenant_uuid: String = row.get("tenant_id");
        let tenant_id = format!("tnt:{}", tenant_uuid);
        let name: String = row.get("name");
        let domain: Option<String> = row.get("domain");
        let status: String = row.get("status");
        let settings: Value = row.get("settings");
        let sponsor_tenant_uuid: Option<String> = row.get("sponsor_tenant_id");
        store.tenants.insert(
            tenant_id.clone(),
            TenantRecord {
                tenant_id,
                name,
                domain,
                status,
                settings,
                sponsor_tenant_id: sponsor_tenant_uuid.map(|uuid| format!("tnt:{uuid}")),
            },
        );
    }

    let ilk_rows = client
        .query(
            r#"
SELECT
    ilk_id::text AS ilk_id,
    ilk_type,
    registration_status,
    tenant_id::text AS tenant_id,
    identification,
    definition,
    CASE
        WHEN deleted_at IS NULL THEN NULL
        ELSE (EXTRACT(EPOCH FROM deleted_at) * 1000)::BIGINT
    END AS deleted_at_ms
FROM identity_ilks
ORDER BY created_at ASC
"#,
            &[],
        )
        .await?;
    for row in ilk_rows {
        let ilk_uuid: String = row.get("ilk_id");
        let tenant_uuid: String = row.get("tenant_id");
        let definition: Value = row.get("definition");
        let ilk_type: String = row.get("ilk_type");
        let deleted_at_ms: Option<i64> = row.get("deleted_at_ms");
        store.ilks.insert(
            format!("ilk:{}", ilk_uuid),
            IlkRecord {
                ilk_id: format!("ilk:{}", ilk_uuid),
                ilk_type: ilk_type.clone(),
                registration_status: row.get("registration_status"),
                tenant_id: format!("tnt:{}", tenant_uuid),
                identification: row.get("identification"),
                definition: normalize_definition_for_ilk_type(&ilk_type, definition),
                channels: Vec::new(),
                deleted_at_ms: deleted_at_ms.and_then(|value| u64::try_from(value).ok()),
            },
        );
    }

    let ich_rows = client
        .query(
            r#"
SELECT
    ich_id::text AS ich_id,
    ilk_id::text AS ilk_id,
    channel_type,
    address,
    owner_l2_name,
    enabled
FROM identity_ichs
ORDER BY added_at ASC
"#,
            &[],
        )
        .await?;
    for row in ich_rows {
        let ilk_id = format!("ilk:{}", row.get::<_, String>("ilk_id"));
        let channel = ChannelRecord {
            ich_id: format!("ich:{}", row.get::<_, String>("ich_id")),
            channel_type: row.get("channel_type"),
            address: row.get("address"),
            owner_l2_name: row.get("owner_l2_name"),
            enabled: row.get("enabled"),
        };
        if let Some(ilk) = store.ilks.get_mut(&ilk_id) {
            if !ilk
                .channels
                .iter()
                .any(|existing| existing.ich_id == channel.ich_id)
            {
                ilk.channels.push(channel.clone());
            }
            if ilk.deleted_at_ms.is_none() {
                let key =
                    canonical_ich_key(&channel.channel_type, &channel.address, &ilk.tenant_id);
                store.ich_lookup.insert(key, ilk_id.clone());
            }
        }
    }

    let alias_rows = client
        .query(
            r#"
SELECT
    old_ilk_id::text AS old_ilk_id,
    canonical_ilk_id::text AS canonical_ilk_id,
    (EXTRACT(EPOCH FROM expires_at) * 1000)::BIGINT AS expires_at_ms
FROM identity_ilk_aliases
"#,
            &[],
        )
        .await?;
    for row in alias_rows {
        let expires_at_ms: i64 = row.get("expires_at_ms");
        if let Ok(expires_at_ms) = u64::try_from(expires_at_ms) {
            store.aliases.insert(
                format!("ilk:{}", row.get::<_, String>("old_ilk_id")),
                AliasRecord {
                    canonical_ilk_id: format!("ilk:{}", row.get::<_, String>("canonical_ilk_id")),
                    expires_at_ms,
                },
            );
        }
    }

    Ok(store)
}

async fn upsert_tenant_in_db(
    database_config: &PgConfig,
    tenant: &TenantRecord,
) -> Result<(), IdentityError> {
    let tenant_uuid = parse_prefixed_uuid(&tenant.tenant_id, "tnt")?;
    let tenant_uuid = tenant_uuid.to_string();
    let sponsor_tenant_uuid = tenant
        .sponsor_tenant_id
        .as_deref()
        .map(|tenant_id| parse_prefixed_uuid(tenant_id, "tnt").map(|uuid| uuid.to_string()))
        .transpose()?;
    let (client, connection) = database_config.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "identity tenant upsert postgres connection closed");
        }
    });

    client
        .execute(
            r#"
INSERT INTO identity_tenants (
    tenant_id,
    name,
    domain,
    status,
    settings,
    sponsor_tenant_id,
    updated_at
)
VALUES ($1::text::uuid, $2, $3, $4, $5::jsonb, $6::text::uuid, NOW())
ON CONFLICT (tenant_id) DO UPDATE
SET
    name = EXCLUDED.name,
    domain = EXCLUDED.domain,
    status = EXCLUDED.status,
    settings = EXCLUDED.settings,
    sponsor_tenant_id = EXCLUDED.sponsor_tenant_id,
    updated_at = NOW()
"#,
            &[
                &tenant_uuid,
                &tenant.name,
                &tenant.domain,
                &tenant.status,
                &tenant.settings,
                &sponsor_tenant_uuid,
            ],
        )
        .await?;
    Ok(())
}

fn identity_secret_configured(is_primary: bool, control_state: &IdentityControlState) -> bool {
    is_primary && control_state.secret_source != IdentityDbSecretSource::Missing
}

fn identity_effective_source(
    is_primary: bool,
    control_state: &IdentityControlState,
) -> &'static str {
    if is_primary {
        control_state.secret_source.as_str()
    } else {
        "replica_non_primary"
    }
}

fn identity_state_label(is_primary: bool, control_state: &IdentityControlState) -> &'static str {
    if !is_primary {
        "replica_non_primary"
    } else if control_state.db_ready {
        "configured"
    } else if control_state.secret_source == IdentityDbSecretSource::Missing {
        "missing_secret"
    } else {
        "db_not_ready"
    }
}

fn bootstrap_identity_control_state(
    node_name: &str,
    secret_source: IdentityDbSecretSource,
    is_primary: bool,
    db_ready: bool,
    last_error: Option<String>,
) -> Result<IdentityControlState, IdentityError> {
    let persisted = load_identity_config_state(node_name);
    let state = IdentityControlState {
        schema_version: persisted
            .as_ref()
            .map(|value| value.schema_version)
            .unwrap_or(IDENTITY_CONFIG_SCHEMA_VERSION),
        config_version: persisted
            .as_ref()
            .map(|value| value.config_version)
            .unwrap_or(0),
        secret_source,
        db_ready,
        last_error,
    };
    persist_identity_config_state(node_name, is_primary, &state)?;
    Ok(state)
}

fn load_identity_config_state(node_name: &str) -> Option<IdentityConfigStateFile> {
    let path = managed_node_config_path(node_name).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<IdentityConfigStateFile>(&raw).ok()
}

fn persist_identity_config_state(
    node_name: &str,
    is_primary: bool,
    state: &IdentityControlState,
) -> Result<(), IdentityError> {
    let path = managed_node_config_path(node_name)?;
    let payload = IdentityConfigStateFile {
        schema_version: state.schema_version,
        config_version: state.config_version,
        node_name: node_name.to_string(),
        config: identity_public_config(is_primary, node_name, state),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    write_json_atomic(&path, &serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn identity_public_config(
    is_primary: bool,
    _node_name: &str,
    state: &IdentityControlState,
) -> Value {
    json!({
        "database": {
            "mode": "postgres",
            "source": identity_effective_source(is_primary, state),
            "resolved_from": if is_primary {
                Value::String("vault://resource_type=postgres".to_string())
            } else {
                Value::Null
            },
            "db_name": if is_primary {
                Value::String(IDENTITY_DB_NAME.to_string())
            } else {
                Value::Null
            }
        }
    })
}

fn build_identity_config_get_payload(
    is_primary: bool,
    node_name: &str,
    control_state: &IdentityControlState,
) -> Value {
    let configured = identity_secret_configured(is_primary, control_state);
    let mut notes = vec![
        Value::String("SY.identity uses PostgreSQL only on motherbee primary.".to_string()),
        Value::String(
            "Model D': credentials live entirely in SY.vault. Operator loads them with vault_put + resource_type=postgres; identity resolves from the pool at boot.".to_string(),
        ),
        Value::String(
            "VAULT_SECRET_CHANGED broadcasts trigger exit(0) for systemd-managed restart so the pool reconnects with the new secret. CONFIG_SET on SY.identity takes no secret-bearing fields.".to_string(),
        ),
    ];
    let error = if !is_primary {
        notes.push(Value::String(
            "Replica nodes do not use a local PostgreSQL backend.".to_string(),
        ));
        json!({
            "code": "not_primary",
            "message": "SY.identity uses local PostgreSQL only on motherbee primary."
        })
    } else if configured && control_state.db_ready {
        Value::Null
    } else if let Some(message) = control_state.last_error.as_ref() {
        json!({
            "code": if control_state.secret_source == IdentityDbSecretSource::Missing {
                "missing_secret"
            } else {
                "db_not_ready"
            },
            "message": message
        })
    } else {
        json!({
            "code": "missing_secret",
            "message": "postgres secret not resolvable from vault pool; load it via vault_put with resource_type=postgres."
        })
    };
    let resources = if is_primary {
        json!([
            {
                "resource_type": "postgres",
                "required": true,
                "configured": configured,
                "scope": "pool (tenant or root)",
                "consumer_dbname": IDENTITY_DB_NAME
            }
        ])
    } else {
        json!([])
    };
    json!({
        "ok": is_primary && control_state.db_ready,
        "node_name": node_name,
        "state": identity_state_label(is_primary, control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": identity_public_config(is_primary, node_name, control_state),
        "contract": {
            "node_family": "SY",
            "node_kind": "SY.identity",
            "supports": ["CONFIG_GET", "CONFIG_SET"],
            "required_fields": [],
            "optional_fields": [],
            "resources": resources,
            "notes": notes,
        },
        "error": error
    })
}

fn apply_identity_config_set(
    msg: &Message,
    is_primary: bool,
    node_name: &str,
    control_state: &mut IdentityControlState,
) -> Result<Value, IdentityError> {
    if !is_primary {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "not_primary",
            "SY.identity uses local PostgreSQL only on motherbee primary.".to_string(),
        ));
    }
    let Some(requested_node_name) = msg.payload.get("node_name").and_then(Value::as_str) else {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.node_name".to_string(),
        ));
    };
    if requested_node_name != node_name && requested_node_name != IDENTITY_NODE_BASE_NAME {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "invalid_config",
            format!(
                "Invalid payload.node_name: expected '{}' or '{}', got '{}'",
                node_name, IDENTITY_NODE_BASE_NAME, requested_node_name
            ),
        ));
    }
    let Some(schema_version_raw) = msg.payload.get("schema_version").and_then(Value::as_u64) else {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.schema_version".to_string(),
        ));
    };
    let schema_version = schema_version_raw as u32;
    let Some(config_version) = msg.payload.get("config_version").and_then(Value::as_u64) else {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.config_version".to_string(),
        ));
    };
    if config_version < control_state.config_version {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "stale_config_version",
            format!(
                "Stale config_version: received {}, current {}",
                config_version, control_state.config_version
            ),
        ));
    }
    let Some(apply_mode) = msg.payload.get("apply_mode").and_then(Value::as_str) else {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.apply_mode".to_string(),
        ));
    };
    if apply_mode != NODE_CONFIG_APPLY_MODE_REPLACE {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "unsupported_apply_mode",
            format!("Unsupported payload.apply_mode='{apply_mode}'"),
        ));
    }
    let Some(config) = msg.payload.get("config").and_then(Value::as_object) else {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.config".to_string(),
        ));
    };
    // Model D' — sy.identity has no secret-bearing config fields on the
    // CONFIG_SET surface. Postgres credentials live entirely in vault
    // (operator: vault_put + resource_type=postgres). On boot identity
    // discovers them via `resolve_resource`. So CONFIG_SET here accepts
    // only the envelope (schema_version/config_version/apply_mode) and
    // rejects any secret-bearing field with a clear error.
    let database = config.get("database").and_then(Value::as_object);
    if let Some(database) = database {
        for forbidden in ["postgres_url", "postgres_url_ref"] {
            if database.contains_key(forbidden) {
                return Ok(identity_config_error_response(
                    is_primary,
                    node_name,
                    control_state,
                    "invalid_config",
                    format!(
                        "config.database.{forbidden} is no longer accepted; load the postgres secret via vault_put (resource_type=postgres) and identity will discover it from the pool"
                    ),
                ));
            }
        }
    }
    control_state.schema_version = schema_version;
    control_state.config_version = config_version;
    persist_identity_config_state(node_name, is_primary, control_state)?;
    Ok(json!({
        "ok": true,
        "node_name": node_name,
        "state": identity_state_label(is_primary, control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": identity_public_config(is_primary, node_name, control_state),
        "notes": [
            "sy.identity has no secret-bearing CONFIG_SET fields in Model D'.",
            "Load postgres credentials via vault_put with resource_type=postgres; identity will pick them up at boot from the pool."
        ],
        "error": Value::Null
    }))
}

fn identity_config_error_response(
    is_primary: bool,
    node_name: &str,
    control_state: &IdentityControlState,
    code: &str,
    message: String,
) -> Value {
    json!({
        "ok": false,
        "node_name": node_name,
        "state": identity_state_label(is_primary, control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": identity_public_config(is_primary, node_name, control_state),
        "error": {
            "code": code,
            "message": message
        }
    })
}

/// Model D' — resolve identity's Postgres credentials by discovering the
/// `postgres` resource in SY.vault (pool match: dedicated → tenant pool →
/// root-tenant pool). Connects an ephemeral SDK client because identity's own
/// router connection hasn't been built yet at boot. Returns
/// `Ok((None, Missing))` if vault has no postgres secret reachable from
/// our (ilk, tenant) match rules. Returns `Ok((None, last_error))` with
/// the error text if vault is unreachable or denies.
async fn resolve_database_url(
    vault: &VaultClient,
    node_name: &str,
    my_tenant: &str,
) -> (Option<String>, IdentityDbSecretSource, Option<String>) {
    // Option-C fix (race ARCHI-BUG-12): vault lookup uses the caller's
    // PERSISTENT router connection. The caller must already be announced
    // before calling this, so that if vault boots after us its bootstrap
    // VAULT_SECRET_CHANGED broadcast lands in the dispatcher's system
    // channel (rescued by `handle_vault_secret_changed`).
    let mut last_error = None;
    let started_at = Instant::now();
    let result = loop {
        match vault
            .resolve_resource(
                fluxbee_sdk::ResourceType::Postgres,
                my_tenant,
                Duration::from_secs(5),
            )
            .await
        {
            Ok(value) => break Ok(value),
            Err(err) => {
                let err_text = err.to_string();
                last_error = Some(err_text.clone());
                if started_at.elapsed() >= Duration::from_secs(15) {
                    break Err(err_text);
                }
                tracing::warn!(
                    node_name = %node_name,
                    error = %err_text,
                    "identity vault postgres lookup failed during boot; retrying"
                );
                time::sleep(Duration::from_millis(750)).await;
            }
        }
    };
    match result {
        Ok(Some(value)) => match extract_postgres_url_from_vault_value(&value) {
            Some(url) => (Some(url), IdentityDbSecretSource::LocalFile, None),
            None => (
                None,
                IdentityDbSecretSource::Missing,
                Some("vault postgres secret did not carry a usable URL value".to_string()),
            ),
        },
        Ok(None) => (None, IdentityDbSecretSource::Missing, None),
        Err(err) => (
            None,
            IdentityDbSecretSource::Missing,
            Some(format!(
                "vault resource lookup for postgres failed after boot retries: {}",
                last_error.unwrap_or(err)
            )),
        ),
    }
}

/// Accept either a bare string or `{"postgres_url": "..."}` shape.
fn extract_postgres_url_from_vault_value(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
        return Some(s.to_string());
    }
    value
        .get("postgres_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

/// Handle a `VAULT_SECRET_CHANGED` broadcast. Identity reacts only on
/// motherbee primary (replicas don't connect to Postgres) and only for
/// the `postgres` resource scoped to its own (ilk, tenant) match. The
/// reaction is `exit(0)` so systemd reboots the node and identity
/// re-resolves vault with the secret cleanly. See `handle_vault_secret_changed`
/// in `sy_storage.rs` for the rationale (Model D' VA-J'-13).
fn handle_vault_secret_changed(
    msg: &Message,
    is_primary: bool,
    node_name: &str,
    src_l2_name: Option<&str>,
    hive_id: &str,
) {
    tracing::info!(
        node_name = %node_name,
        is_primary = is_primary,
        trace_id = %msg.routing.trace_id,
        "sy.identity handle_vault_secret_changed: entered"
    );
    if !is_primary {
        // Replicas have no local Postgres pool; nothing to reconnect.
        tracing::info!(
            node_name = %node_name,
            "sy.identity handle_vault_secret_changed: not primary, ignoring"
        );
        return;
    }
    // Fail-closed origin check: the reaction is exit(0) (systemd restart), so
    // only the LOCAL SY.vault may trigger it. src_l2_name is stamped
    // authoritatively by the router; a VAULT_SECRET_CHANGED forged by any other
    // node (or another hive's vault) must NOT be able to restart-loop the
    // identity primary. (F-03: this handler runs before is_authorized, and the
    // auth table has no entry for this action, so this is the enforced gate.)
    let expected_origin = format!("SY.vault@{}", hive_id.trim());
    match src_l2_name.map(str::trim) {
        Some(origin) if origin == expected_origin => {}
        other => {
            tracing::warn!(
                node_name = %node_name,
                src_l2_name = %other.unwrap_or("<none>"),
                expected = %expected_origin,
                "VAULT_SECRET_CHANGED from a non-vault / cross-hive origin; refusing to restart identity"
            );
            return;
        }
    }
    let payload: VaultSecretChangedPayload = match serde_json::from_value(msg.payload.clone()) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(error = %err, "ignoring malformed VAULT_SECRET_CHANGED payload");
            return;
        }
    };
    let self_ilk_id = deterministic_system_ilk_id(node_name);
    let interest = VaultSecretInterest {
        resource_type: "postgres",
        my_tenant: fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
        my_ilk: Some(self_ilk_id.as_str()),
        system_caller: true,
    };
    if !payload.matches_interest(&interest) {
        tracing::info!(
            node_name = %node_name,
            resource_type = %payload.resource_type,
            payload_tenant = %payload.tenant_id,
            payload_ilk = %payload.ilk.as_deref().unwrap_or(""),
            my_tenant = %fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
            my_ilk = %self_ilk_id,
            "VAULT_SECRET_CHANGED does not match our interest; ignoring"
        );
        return;
    }
    tracing::warn!(
        node_name = %node_name,
        op = %payload.op.as_str(),
        resource_type = %payload.resource_type,
        version = payload.version,
        key = %payload.key,
        "VAULT_SECRET_CHANGED matches our interest; exiting for systemd-managed restart to reconnect the postgres pool"
    );
    std::process::exit(0);
}

async fn initialize_identity_database_backend(
    database_url: Option<&str>,
) -> (Option<PgConfig>, Option<String>) {
    let Some(database_url) = database_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            None,
            Some(
                "postgres secret not resolvable from SY.vault at boot. Load it with vault_put (resource_type=postgres, value={\"postgres_url\":\"postgresql://user:pass@host:port\"}, no dbname). SY.vault will broadcast VAULT_SECRET_CHANGED and sy-identity will exit(0) to reconnect via systemd-managed restart automatically."
                    .to_string(),
            ),
        );
    };
    let base = match database_url.parse::<PgConfig>() {
        Ok(value) => value,
        Err(err) => return (None, Some(format!("invalid postgres_url: {err}"))),
    };
    if !base.get_dbname().map(str::trim).unwrap_or("").is_empty() {
        return (
            None,
            Some(format!(
                "postgres secret must not include a dbname (got '{}'); load only credentials + host (postgresql://user:pass@host:port)",
                base.get_dbname().unwrap_or("")
            )),
        );
    }
    if let Err(err) = ensure_database_exists(&base, IDENTITY_DB_NAME).await {
        return (
            None,
            Some(format!("failed to ensure identity database exists: {err}")),
        );
    }
    let cfg = with_dbname(&base, IDENTITY_DB_NAME);
    if let Err(err) = ensure_primary_schema(&cfg).await {
        return (
            None,
            Some(format!("failed to ensure identity primary schema: {err}")),
        );
    }
    (Some(cfg), None)
}

fn with_dbname(base: &PgConfig, dbname: &str) -> PgConfig {
    let mut cfg = base.clone();
    cfg.dbname(dbname);
    cfg
}

fn admin_db_config(base: &PgConfig) -> PgConfig {
    with_dbname(base, "postgres")
}

async fn ensure_database_exists(base: &PgConfig, dbname: &str) -> Result<(), IdentityError> {
    let admin_cfg = admin_db_config(base);
    let (client, connection) = admin_cfg.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "identity postgres admin connection closed");
        }
    });
    let exists = client
        .query_opt("SELECT 1 FROM pg_database WHERE datname = $1", &[&dbname])
        .await?
        .is_some();
    if !exists {
        let create_sql = format!("CREATE DATABASE \"{dbname}\"");
        if let Err(err) = client.execute(&create_sql, &[]).await {
            if err.code() == Some(&SqlState::DUPLICATE_DATABASE) {
                tracing::info!(db = dbname, "identity database already exists (race)");
            } else {
                return Err(err.into());
            }
        } else {
            tracing::info!(db = dbname, "created identity database");
        }
    }
    Ok(())
}

fn write_json_atomic(path: &Path, content: &str) -> Result<(), IdentityError> {
    let Some(parent) = path.parent() else {
        return Err("target path has no parent directory".into());
    };
    fs::create_dir_all(parent)?;
    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("state"),
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let tmp_path = parent.join(tmp_name);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)?;
    use std::io::Write;
    file.write_all(content.as_bytes())?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp_path, path)?;
    if let Ok(dir_file) = OpenOptions::new().read(true).open(parent) {
        let _ = dir_file.sync_all();
    }
    Ok(())
}

fn is_mother_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee")
}

fn is_worker_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "worker")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hive(identity_frontdesk: Option<&str>) -> HiveFile {
        HiveFile {
            hive_id: "motherbee".to_string(),
            role: Some("motherbee".to_string()),
            wan: None,
            government: Some(GovernmentSection {
                identity_frontdesk: identity_frontdesk.map(str::to_string),
            }),
            identity: None,
            database: None,
            system_nodes: Some(SystemNodesSection {
                motherbee: Some(RoleSystemNodes {
                    nodes: vec!["SY.identity".to_string(), "SY.architect".to_string()],
                    wait_for: vec!["SY.identity".to_string()],
                }),
                worker: None,
            }),
        }
    }

    #[test]
    fn configured_identity_frontdesk_node_name_normalizes_local_name() {
        let hive = test_hive(Some("SY.frontdesk.gov"));
        assert_eq!(
            configured_identity_frontdesk_node_name(&hive).as_deref(),
            Some("SY.frontdesk.gov@motherbee")
        );
    }

    #[test]
    fn identity_runtime_authorizes_configured_frontdesk_exact_name() {
        let hive = test_hive(Some("SY.frontdesk.gov@motherbee"));
        let runtime = IdentityRuntime::new(&hive, PathBuf::from("/tmp"), true, None);

        assert!(runtime.is_authorized(MSG_ILK_REGISTER, Some("SY.frontdesk.gov@motherbee")));
        assert!(runtime.is_authorized(MSG_ILK_ADD_CHANNEL, Some("SY.frontdesk.gov@motherbee")));
        assert!(runtime.is_authorized(MSG_TNT_CREATE, Some("SY.frontdesk.gov@motherbee")));
        assert!(runtime.is_authorized(MSG_TNT_CREATE, Some("SY.admin@motherbee")));
        assert!(runtime.is_authorized(MSG_TNT_CREATE, Some("SY.architect@motherbee")));
    }

    #[test]
    fn identity_runtime_rejects_missing_src_l2_name_for_protected_actions() {
        let hive = test_hive(Some("SY.frontdesk.gov@motherbee"));
        let runtime = IdentityRuntime::new(&hive, PathBuf::from("/tmp"), true, None);

        assert!(!runtime.is_authorized(MSG_ILK_REGISTER, None));
        assert!(!runtime.is_authorized(MSG_TNT_CREATE, None));
    }

    #[test]
    fn identity_runtime_rejects_orchestrator_relay_for_protected_actions() {
        let hive = test_hive(Some("SY.frontdesk.gov@motherbee"));
        let runtime = IdentityRuntime::new(&hive, PathBuf::from("/tmp"), true, None);

        assert!(!runtime.is_authorized(
            MSG_ILK_REGISTER,
            Some("SY.orchestrator.relay.123@motherbee")
        ));
        assert!(runtime.is_authorized(MSG_ILK_REGISTER, Some("SY.orchestrator@motherbee")));
    }

    #[test]
    fn unauthorized_identity_source_payload_uses_src_l2_name_field() {
        let payload = unauthorized_identity_source_payload(
            MSG_ILK_REGISTER,
            "11111111-1111-1111-1111-111111111111",
            Some("WF.example@motherbee"),
        );

        assert_eq!(
            payload.get("src_uuid").and_then(Value::as_str),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            payload.get("src_l2_name").and_then(Value::as_str),
            Some("WF.example@motherbee")
        );
        assert!(payload.get("source_name").is_none());
    }

    #[test]
    fn identity_store_get_ilk_payload_resolves_alias_and_embeds_tenant() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:11111111-1111-1111-1111-111111111111".to_string(),
            TenantRecord {
                tenant_id: "tnt:11111111-1111-1111-1111-111111111111".to_string(),
                name: "tenant-a".to_string(),
                domain: Some("tenant-a.local".to_string()),
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );
        store.ilks.insert(
            "ilk:22222222-2222-2222-2222-222222222222".to_string(),
            IlkRecord {
                ilk_id: "ilk:22222222-2222-2222-2222-222222222222".to_string(),
                ilk_type: "human".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:11111111-1111-1111-1111-111111111111".to_string(),
                identification: json!({"display_name":"Jane","node_name":"SY.frontdesk.gov@motherbee"}),
                definition: json!({}),
                channels: vec![ChannelRecord {
                    ich_id: "ich:33333333-3333-3333-3333-333333333333".to_string(),
                    channel_type: "slack".to_string(),
                    address: "U123".to_string(),
                    owner_l2_name: Some("IO.slack@motherbee".to_string()),
                    enabled: true,
                }],
                deleted_at_ms: None,
            },
        );
        store.aliases.insert(
            "ilk:44444444-4444-4444-4444-444444444444".to_string(),
            AliasRecord {
                canonical_ilk_id: "ilk:22222222-2222-2222-2222-222222222222".to_string(),
                expires_at_ms: 4_102_444_800_000,
            },
        );

        let payload = store
            .get_ilk_payload("ilk:44444444-4444-4444-4444-444444444444")
            .expect("payload");

        assert_eq!(
            payload.get("canonical_ilk_id").and_then(Value::as_str),
            Some("ilk:22222222-2222-2222-2222-222222222222")
        );
        assert_eq!(
            payload.get("alias_resolved").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            payload
                .get("tenant")
                .and_then(|value| value.get("tenant_id"))
                .and_then(Value::as_str),
            Some("tnt:11111111-1111-1111-1111-111111111111")
        );
    }

    #[test]
    fn get_tenant_payload_returns_tenant_sponsor_and_counts() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            TenantRecord {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                name: "root".to_string(),
                domain: Some("root.local".to_string()),
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );
        store.tenants.insert(
            "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            TenantRecord {
                tenant_id: "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                name: "child".to_string(),
                domain: Some("child.local".to_string()),
                status: "active".to_string(),
                settings: json!({"tier":"pro"}),
                sponsor_tenant_id: Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string()),
            },
        );
        store.ilks.insert(
            "ilk:11111111-1111-1111-1111-111111111111".to_string(),
            IlkRecord {
                ilk_id: "ilk:11111111-1111-1111-1111-111111111111".to_string(),
                ilk_type: "agent".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                identification: json!({}),
                definition: json!({}),
                channels: Vec::new(),
                deleted_at_ms: None,
            },
        );

        let payload = store
            .get_tenant_payload("tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")
            .expect("tenant payload");

        assert_eq!(
            payload.get("tenant_id").and_then(Value::as_str),
            Some("tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")
        );
        assert_eq!(payload.get("child_count").and_then(Value::as_u64), Some(0));
        assert_eq!(payload.get("ilk_count").and_then(Value::as_u64), Some(1));
        assert_eq!(payload.get("is_root").and_then(Value::as_bool), Some(false));
        assert_eq!(
            payload.get("is_sponsor").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            payload
                .get("children")
                .and_then(Value::as_array)
                .map(|items| items.len()),
            Some(0)
        );
        assert_eq!(
            payload
                .get("sponsor")
                .and_then(|value| value.get("tenant_id"))
                .and_then(Value::as_str),
            Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );
    }

    #[test]
    fn list_tenants_payload_sorts_and_includes_summary_counts() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            TenantRecord {
                tenant_id: "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                name: "child".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string()),
            },
        );
        store.tenants.insert(
            "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            TenantRecord {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                name: "root".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );

        let payload = store.list_tenants_payload();
        let tenants = payload
            .get("tenants")
            .and_then(Value::as_array)
            .expect("tenant list");
        assert_eq!(payload.get("count").and_then(Value::as_u64), Some(2));
        assert_eq!(
            tenants[0].get("tenant_id").and_then(Value::as_str),
            Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );
        assert_eq!(
            tenants[0].get("child_count").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            tenants[0].get("is_root").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            tenants[0].get("is_sponsor").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            tenants[1].get("tenant_id").and_then(Value::as_str),
            Some("tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")
        );
        assert_eq!(
            tenants[1].get("is_root").and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn create_tenant_returns_existing_tenant_for_same_name_or_domain() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            TenantRecord {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                name: "4iPlatform".to_string(),
                domain: Some("4iplatform.com".to_string()),
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );

        let by_name = store
            .create_tenant(TntCreateRequest {
                name: "4iplatform".to_string(),
                domain: None,
                status: Some("active".to_string()),
                settings: None,
                sponsor_tenant_id: None,
            })
            .expect("resolve by name");
        assert_eq!(
            by_name.get("tenant_id").and_then(Value::as_str),
            Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );
        assert_eq!(by_name.get("created").and_then(Value::as_bool), Some(false));
        assert_eq!(
            by_name.get("matched_by").and_then(Value::as_str),
            Some("name")
        );

        let by_domain = store
            .create_tenant(TntCreateRequest {
                name: "another-name".to_string(),
                domain: Some("4iplatform.com".to_string()),
                status: Some("active".to_string()),
                settings: None,
                sponsor_tenant_id: None,
            })
            .expect("resolve by domain");
        assert_eq!(
            by_domain.get("tenant_id").and_then(Value::as_str),
            Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );
        assert_eq!(
            by_domain.get("created").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            by_domain.get("matched_by").and_then(Value::as_str),
            Some("domain")
        );
    }

    #[test]
    fn default_root_tenant_is_fixed_by_code() {
        let store = IdentityStore::with_default_tenant();

        let tenant = store
            .tenants
            .get(DEFAULT_ROOT_TENANT_ID)
            .expect("fixed default tenant must exist");
        assert_eq!(tenant.name, DEFAULT_DEFAULT_TENANT_NAME);
        assert_eq!(tenant.status, "active");
        assert_eq!(tenant.sponsor_tenant_id, None);
        assert_eq!(
            store.default_tenant_id().as_deref(),
            Some(DEFAULT_ROOT_TENANT_ID)
        );
    }

    #[test]
    fn ensure_default_root_tenant_repairs_existing_fixed_record() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            DEFAULT_ROOT_TENANT_ID.to_string(),
            TenantRecord {
                tenant_id: DEFAULT_ROOT_TENANT_ID.to_string(),
                name: "other".to_string(),
                domain: Some("example.invalid".to_string()),
                status: "suspended".to_string(),
                settings: json!({"kept": true}),
                sponsor_tenant_id: Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string()),
            },
        );

        assert!(store.ensure_default_root_tenant());
        let tenant = store.tenants.get(DEFAULT_ROOT_TENANT_ID).unwrap();
        assert_eq!(tenant.name, DEFAULT_DEFAULT_TENANT_NAME);
        assert_eq!(tenant.status, "active");
        assert_eq!(tenant.sponsor_tenant_id, None);
        assert_eq!(tenant.domain, None);
        assert_eq!(tenant.settings, json!({}));
    }

    #[test]
    fn default_tenant_id_does_not_fall_back_to_legacy_roots() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            TenantRecord {
                tenant_id: "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                name: "child".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string()),
            },
        );
        store.tenants.insert(
            "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            TenantRecord {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                name: "root".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );

        assert_eq!(store.default_tenant_id().as_deref(), None);

        store.ensure_default_root_tenant();
        assert_eq!(
            store.default_tenant_id().as_deref(),
            Some(DEFAULT_ROOT_TENANT_ID)
        );
    }

    #[test]
    fn system_ilks_are_seeded_deterministically_from_hive() {
        let hive = test_hive(Some("SY.frontdesk.gov@motherbee"));
        let mut store = IdentityStore::default();

        let changed = store
            .ensure_system_ilks_from_hive(&hive)
            .expect("seed system ilks");
        assert_eq!(changed.len(), 2);

        let identity_node = "SY.identity@motherbee";
        let identity_ilk_id = deterministic_system_ilk_id(identity_node);
        let identity_ilk = store
            .ilks
            .get(&identity_ilk_id)
            .expect("identity system ilk");
        assert_eq!(identity_ilk.ilk_type, "system");
        assert_eq!(identity_ilk.registration_status, "complete");
        assert_eq!(identity_ilk.tenant_id, DEFAULT_ROOT_TENANT_ID);
        assert_eq!(
            identity_ilk
                .identification
                .get("node_name")
                .and_then(Value::as_str),
            Some(identity_node)
        );

        let changed_again = store
            .ensure_system_ilks_from_hive(&hive)
            .expect("seed system ilks idempotently");
        assert!(changed_again.is_empty());
    }

    #[test]
    fn set_tenant_sponsor_updates_and_clears_sponsor() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            TenantRecord {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                name: "root".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );
        store.tenants.insert(
            "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            TenantRecord {
                tenant_id: "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                name: "child".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );

        let set_out = store
            .set_tenant_sponsor(TntSetSponsorRequest {
                tenant_id: "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                sponsor_tenant_id: Value::String(
                    "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                ),
            })
            .expect("set sponsor");
        assert_eq!(
            set_out.get("sponsor_tenant_id").and_then(Value::as_str),
            Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );

        let clear_out = store
            .set_tenant_sponsor(TntSetSponsorRequest {
                tenant_id: "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                sponsor_tenant_id: Value::Null,
            })
            .expect("clear sponsor");
        assert!(clear_out.get("sponsor_tenant_id").is_some());
        assert!(clear_out.get("sponsor_tenant_id").unwrap().is_null());
    }

    #[test]
    fn set_tenant_sponsor_rejects_cycles_and_self_sponsorship() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            TenantRecord {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                name: "root".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );
        store.tenants.insert(
            "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            TenantRecord {
                tenant_id: "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                name: "child".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: Some("tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string()),
            },
        );

        let self_err = store
            .set_tenant_sponsor(TntSetSponsorRequest {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                sponsor_tenant_id: Value::String(
                    "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                ),
            })
            .expect_err("self sponsor must fail");
        assert_eq!(self_err, "INVALID_SPONSOR_RELATION");

        let cycle_err = store
            .set_tenant_sponsor(TntSetSponsorRequest {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                sponsor_tenant_id: Value::String(
                    "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ),
            })
            .expect_err("cycle must fail");
        assert_eq!(cycle_err, "INVALID_SPONSOR_RELATION");
    }

    #[test]
    fn set_ich_enabled_updates_existing_channel() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            TenantRecord {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                name: "fluxbee".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );
        store.ilks.insert(
            "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            IlkRecord {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ilk_type: "human".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                identification: json!({}),
                definition: json!({}),
                channels: vec![ChannelRecord {
                    ich_id: "ich:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string(),
                    channel_type: "slack".to_string(),
                    address: "U123".to_string(),
                    owner_l2_name: Some("IO.slack.support@motherbee".to_string()),
                    enabled: false,
                }],
                deleted_at_ms: None,
            },
        );

        let out = store
            .set_ich_enabled("ich:cccccccc-cccc-cccc-cccc-cccccccccccc", true)
            .expect("enable ich");
        assert_eq!(out.get("enabled").and_then(Value::as_bool), Some(true));
        assert_eq!(
            out.get("owner_l2_name").and_then(Value::as_str),
            Some("IO.slack.support@motherbee")
        );
        assert!(store.ilks["ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"].channels[0].enabled);
    }

    #[test]
    fn io_callers_can_enable_only_their_own_ich() {
        let mut store = IdentityStore::default();
        store.ilks.insert(
            "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            IlkRecord {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ilk_type: "agent".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                identification: json!({}),
                definition: json!({}),
                channels: vec![ChannelRecord {
                    ich_id: "ich:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string(),
                    channel_type: "cloud".to_string(),
                    address: "demo".to_string(),
                    owner_l2_name: Some("IO.cloud@motherbee".to_string()),
                    enabled: false,
                }],
                deleted_at_ms: None,
            },
        );

        assert!(authorize_ich_enabled_mutation(
            &store,
            "ich:cccccccc-cccc-cccc-cccc-cccccccccccc",
            Some("IO.cloud@motherbee")
        )
        .is_ok());
        assert!(authorize_ich_enabled_mutation(
            &store,
            "ich:cccccccc-cccc-cccc-cccc-cccccccccccc",
            Some("SY.admin@motherbee")
        )
        .is_ok());
        let denied = authorize_ich_enabled_mutation(
            &store,
            "ich:cccccccc-cccc-cccc-cccc-cccccccccccc",
            Some("IO.other@motherbee"),
        )
        .expect_err("other IO must not toggle this ICH");
        assert_eq!(
            denied.get("error_code").and_then(Value::as_str),
            Some("UNAUTHORIZED")
        );
    }

    #[test]
    fn set_ilk_definition_updates_agent_definition_and_normalizes_hashes() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            TenantRecord {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                name: "fluxbee".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );
        store.ilks.insert(
            "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            IlkRecord {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ilk_type: "agent".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                identification: json!({"node_name":"AI.test@motherbee"}),
                definition: json!({}),
                channels: Vec::new(),
                deleted_at_ms: None,
            },
        );

        let role_hash = "A".repeat(64);
        let skill_hash = "b".repeat(64);
        let handbook_hash = "C".repeat(64);
        let personality_hash = "D".repeat(64);
        let out = store
            .set_ilk_definition(IlkSetDefinitionRequest {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                definition: json!({
                    "role_hash": role_hash,
                    "skill_hashes": [skill_hash],
                    "handbook_hashes": [handbook_hash],
                    "personality_hash": personality_hash,
                }),
            })
            .expect("set definition");

        let definition = out.get("definition").expect("definition");
        let expected_role_hash = "a".repeat(64);
        let expected_skill_hash = "b".repeat(64);
        let expected_handbook_hash = "c".repeat(64);
        let expected_personality_hash = "d".repeat(64);
        assert_eq!(
            definition.get("role_hash").and_then(Value::as_str),
            Some(expected_role_hash.as_str())
        );
        assert_eq!(
            definition
                .get("skill_hashes")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(Value::as_str),
            Some(expected_skill_hash.as_str())
        );
        assert_eq!(
            definition
                .get("handbook_hashes")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(Value::as_str),
            Some(expected_handbook_hash.as_str())
        );
        assert_eq!(
            definition.get("personality_hash").and_then(Value::as_str),
            Some(expected_personality_hash.as_str())
        );
        assert!(agent_definition_present(
            &store.ilks["ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"].definition
        ));
    }

    #[test]
    fn set_ilk_definition_handles_personality_only_and_clears_it() {
        let mut store = IdentityStore::default();
        store.ilks.insert(
            "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            IlkRecord {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ilk_type: "agent".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                identification: json!({}),
                definition: json!({}),
                channels: Vec::new(),
                deleted_at_ms: None,
            },
        );

        let personality_hash = "9".repeat(64);
        let out = store
            .set_ilk_definition(IlkSetDefinitionRequest {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                definition: json!({"personality_hash": personality_hash}),
            })
            .expect("set personality only");
        let definition = out.get("definition").expect("definition");
        assert_eq!(
            definition.get("personality_hash").and_then(Value::as_str),
            Some(personality_hash.as_str())
        );
        assert!(definition.get("role_hash").is_none());

        let cleared = store
            .set_ilk_definition(IlkSetDefinitionRequest {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                definition: json!({}),
            })
            .expect("clear definition");
        assert_eq!(cleared.get("definition"), Some(&json!({})));

        let bad = store.set_ilk_definition(IlkSetDefinitionRequest {
            ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            definition: json!({"personality_hash": "not-a-hash"}),
        });
        assert!(bad.is_err());
    }

    #[test]
    fn set_ilk_definition_accepts_empty_definition_as_clear() {
        let mut store = IdentityStore::default();
        store.ilks.insert(
            "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            IlkRecord {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ilk_type: "agent".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                identification: json!({}),
                definition: json!({"role_hash": "a".repeat(64)}),
                channels: Vec::new(),
                deleted_at_ms: None,
            },
        );

        let out = store
            .set_ilk_definition(IlkSetDefinitionRequest {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                definition: json!({}),
            })
            .expect("clear definition");

        assert_eq!(out.get("definition"), Some(&json!({})));
        assert_eq!(
            store.ilks["ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"].definition,
            json!({})
        );
    }

    #[test]
    fn set_ilk_definition_rejects_malformed_definition() {
        let mut store = IdentityStore::default();
        store.ilks.insert(
            "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            IlkRecord {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ilk_type: "agent".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                identification: json!({}),
                definition: json!({}),
                channels: Vec::new(),
                deleted_at_ms: None,
            },
        );

        let invalid_hash = store
            .set_ilk_definition(IlkSetDefinitionRequest {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                definition: json!({"role_hash": "not-a-hash"}),
            })
            .expect_err("invalid hash");
        assert_eq!(invalid_hash, "INVALID_DEFINITION_HASH");

        let too_many_skills = store
            .set_ilk_definition(IlkSetDefinitionRequest {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                definition: json!({"skill_hashes": vec!["a".repeat(64); 17]}),
            })
            .expect_err("too many skills");
        assert_eq!(too_many_skills, "DEFINITION_TOO_LARGE");
    }

    #[test]
    fn set_ilk_definition_rejects_non_agent_and_not_found() {
        let mut store = IdentityStore::default();
        store.ilks.insert(
            "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            IlkRecord {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ilk_type: "human".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                identification: json!({}),
                definition: json!({}),
                channels: Vec::new(),
                deleted_at_ms: None,
            },
        );

        let non_agent = store
            .set_ilk_definition(IlkSetDefinitionRequest {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                definition: json!({"role_hash": "a".repeat(64)}),
            })
            .expect_err("non-agent");
        assert_eq!(non_agent, "INVALID_ILK_TYPE");

        let not_found = store
            .set_ilk_definition(IlkSetDefinitionRequest {
                ilk_id: "ilk:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string(),
                definition: json!({"role_hash": "a".repeat(64)}),
            })
            .expect_err("not found");
        assert_eq!(not_found, "ILK_NOT_FOUND");
    }

    #[test]
    fn delete_ilk_removes_agent_and_purges_ich_lookup() {
        let mut store = IdentityStore::default();
        let ilk_id = "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string();
        let tenant_id = "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string();
        store.ilks.insert(
            ilk_id.clone(),
            IlkRecord {
                ilk_id: ilk_id.clone(),
                ilk_type: "agent".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: tenant_id.clone(),
                identification: json!({"node_name":"AI.demo@motherbee","source":"orchestrator.run_node"}),
                definition: json!({}),
                channels: Vec::new(),
                deleted_at_ms: None,
            },
        );
        store.ich_lookup.insert(
            ("slack".to_string(), "U123".to_string(), tenant_id.clone()),
            ilk_id.clone(),
        );
        store.aliases.insert(
            ilk_id.clone(),
            AliasRecord {
                canonical_ilk_id: "ilk:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string(),
                expires_at_ms: now_epoch_ms() + 60_000,
            },
        );
        store.aliases.insert(
            "ilk:dddddddd-dddd-dddd-dddd-dddddddddddd".to_string(),
            AliasRecord {
                canonical_ilk_id: ilk_id.clone(),
                expires_at_ms: now_epoch_ms() + 60_000,
            },
        );
        store.aliases.insert(
            "ilk:eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee".to_string(),
            AliasRecord {
                canonical_ilk_id: "ilk:ffffffff-ffff-ffff-ffff-ffffffffffff".to_string(),
                expires_at_ms: now_epoch_ms() + 60_000,
            },
        );

        let ok = store
            .delete_ilk(IlkDeleteRequest {
                ilk_id: ilk_id.clone(),
            })
            .expect("delete ok");
        assert_eq!(ok.get("status").and_then(Value::as_str), Some("ok"));
        assert_eq!(
            ok.get("removed_alias_count").and_then(Value::as_u64),
            Some(2)
        );
        assert!(!store.ilks.contains_key(&ilk_id));
        assert!(!store.ich_lookup.values().any(|mapped| mapped == &ilk_id));
        assert!(!store.aliases.contains_key(&ilk_id));
        assert!(!store
            .aliases
            .values()
            .any(|alias| alias.canonical_ilk_id == ilk_id));
        assert!(store
            .aliases
            .contains_key("ilk:eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"));
    }

    #[test]
    fn alias_old_ids_referencing_captures_both_directions() {
        let mut store = IdentityStore::default();
        let target = "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string();
        store.aliases.insert(
            target.clone(),
            AliasRecord {
                canonical_ilk_id: "ilk:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string(),
                expires_at_ms: now_epoch_ms() + 60_000,
            },
        );
        store.aliases.insert(
            "ilk:dddddddd-dddd-dddd-dddd-dddddddddddd".to_string(),
            AliasRecord {
                canonical_ilk_id: target.clone(),
                expires_at_ms: now_epoch_ms() + 60_000,
            },
        );
        store.aliases.insert(
            "ilk:eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee".to_string(),
            AliasRecord {
                canonical_ilk_id: "ilk:ffffffff-ffff-ffff-ffff-ffffffffffff".to_string(),
                expires_at_ms: now_epoch_ms() + 60_000,
            },
        );

        let mut captured = store.alias_old_ids_referencing(&target);
        captured.sort();
        assert_eq!(
            captured,
            vec![
                "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                "ilk:dddddddd-dddd-dddd-dddd-dddddddddddd".to_string(),
            ]
        );
    }

    #[test]
    fn apply_ilk_delete_delta_removes_aliases_for_replicas() {
        let mut store = IdentityStore::default();
        let ilk_id = "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string();
        store.aliases.insert(
            ilk_id.clone(),
            AliasRecord {
                canonical_ilk_id: "ilk:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string(),
                expires_at_ms: now_epoch_ms() + 60_000,
            },
        );
        store.aliases.insert(
            "ilk:dddddddd-dddd-dddd-dddd-dddddddddddd".to_string(),
            AliasRecord {
                canonical_ilk_id: ilk_id.clone(),
                expires_at_ms: now_epoch_ms() + 60_000,
            },
        );
        store.aliases.insert(
            "ilk:eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee".to_string(),
            AliasRecord {
                canonical_ilk_id: "ilk:ffffffff-ffff-ffff-ffff-ffffffffffff".to_string(),
                expires_at_ms: now_epoch_ms() + 60_000,
            },
        );

        store.apply_delta(IdentityDelta::IlkDelete {
            ilk_id: ilk_id.clone(),
        });

        assert!(!store.aliases.contains_key(&ilk_id));
        assert!(!store
            .aliases
            .values()
            .any(|alias| alias.canonical_ilk_id == ilk_id));
        assert!(store
            .aliases
            .contains_key("ilk:eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"));
    }

    #[test]
    fn delete_ilk_refuses_well_known_system_ilks() {
        let mut store = IdentityStore::default();
        let ilk_id = "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string();
        store.ilks.insert(
            ilk_id.clone(),
            IlkRecord {
                ilk_id: ilk_id.clone(),
                ilk_type: "system".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:00000000-0000-0000-0000-000000000001".to_string(),
                identification: json!({
                    "node_name":"SY.admin@motherbee",
                    "source":"hive.system_nodes"
                }),
                definition: json!({}),
                channels: Vec::new(),
                deleted_at_ms: None,
            },
        );

        let err = store
            .delete_ilk(IlkDeleteRequest {
                ilk_id: ilk_id.clone(),
            })
            .expect_err("must refuse system ilk");
        assert_eq!(err, "SYSTEM_ILK_PROTECTED");
        assert!(store.ilks.contains_key(&ilk_id));
    }

    #[test]
    fn delete_ilk_not_found_returns_specific_error() {
        let mut store = IdentityStore::default();
        let err = store
            .delete_ilk(IlkDeleteRequest {
                ilk_id: "ilk:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string(),
            })
            .expect_err("not found");
        assert_eq!(err, "ILK_NOT_FOUND");
    }

    #[test]
    fn list_ilks_payload_returns_compact_identity_rows() {
        let mut store = IdentityStore::default();
        store.tenants.insert(
            "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            TenantRecord {
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                name: "fluxbee".to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
                sponsor_tenant_id: None,
            },
        );
        store.ilks.insert(
            "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            IlkRecord {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ilk_type: "human".to_string(),
                registration_status: "temporary".to_string(),
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                identification: json!({"display_name":"Jane","node_name":"SY.frontdesk.gov@motherbee"}),
                definition: json!({}),
                channels: vec![ChannelRecord {
                    ich_id: "ich:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string(),
                    channel_type: "slack".to_string(),
                    address: "U123".to_string(),
                    owner_l2_name: Some("IO.slack.support@motherbee".to_string()),
                    enabled: false,
                }],
                deleted_at_ms: None,
            },
        );

        let payload = store.list_ilks_payload();
        assert_eq!(payload.get("count").and_then(Value::as_u64), Some(1));
        let rows = payload
            .get("ilks")
            .and_then(Value::as_array)
            .expect("ilks array");
        assert_eq!(
            rows[0].get("tenant_name").and_then(Value::as_str),
            Some("fluxbee")
        );
        assert_eq!(
            rows[0].get("channel_count").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            rows[0].get("registration_status").and_then(Value::as_str),
            Some("temporary")
        );
    }

    fn ilk_for(node_name: &str, ilk_id: &str) -> IlkRecord {
        let hive = node_name.rsplit_once('@').map(|(_, h)| h).unwrap_or("");
        IlkRecord {
            ilk_id: ilk_id.to_string(),
            // Non-privileged default so per-hive OWNERSHIP tests are not entangled
            // with the system-type authority binding (tested separately). Tests
            // that need a `system` ilk set it explicitly.
            ilk_type: "agent".to_string(),
            registration_status: "complete".to_string(),
            tenant_id: "tnt:00000000-0000-0000-0000-000000000001".to_string(),
            identification: json!({ "node_name": node_name, "hive_id": hive }),
            definition: json!({}),
            channels: vec![],
            deleted_at_ms: None,
        }
    }

    #[test]
    fn ilk_owning_hive_uses_node_name_suffix_then_hive_id() {
        assert_eq!(
            ilk_owning_hive(&ilk_for("SY.storage@worker1", "ilk:a")).as_deref(),
            Some("worker1")
        );
        // Fallback to identification.hive_id when node_name carries no `@`.
        let mut ilk = ilk_for("plain", "ilk:b");
        ilk.identification = json!({ "node_name": "plain", "hive_id": "motherbee" });
        assert_eq!(ilk_owning_hive(&ilk).as_deref(), Some("motherbee"));
    }

    #[test]
    fn delta_authority_blocks_foreign_forge_and_tenants() {
        let store = IdentityStore::default();
        // A replica may push its own `@hive` ilk.
        assert!(delta_authorized_for_hive(
            &IdentityDelta::IlkUpsert {
                ilk: ilk_for("SY.storage@worker1", "ilk:w")
            },
            "worker1",
            &store
        ));
        // It may NOT forge a `@motherbee` or another hive's ilk.
        assert!(!delta_authorized_for_hive(
            &IdentityDelta::IlkUpsert {
                ilk: ilk_for("SY.storage@motherbee", "ilk:m")
            },
            "worker1",
            &store
        ));
        assert!(!delta_authorized_for_hive(
            &IdentityDelta::IlkUpsert {
                ilk: ilk_for("SY.x@worker2", "ilk:w2")
            },
            "worker1",
            &store
        ));
        // Tenants are primary-authoritative — never accepted from a replica.
        assert!(!delta_authorized_for_hive(
            &IdentityDelta::TenantUpsert {
                tenant: TenantRecord {
                    tenant_id: "tnt:x".into(),
                    name: "x".into(),
                    domain: None,
                    status: "active".into(),
                    settings: json!({}),
                    sponsor_tenant_id: None,
                }
            },
            "worker1",
            &store
        ));
        // IlkDelete authority resolves against the existing ilk's owner; an
        // unknown id is rejected (cannot verify ownership).
        let mut store2 = IdentityStore::default();
        store2
            .ilks
            .insert("ilk:w".into(), ilk_for("SY.storage@worker1", "ilk:w"));
        assert!(delta_authorized_for_hive(
            &IdentityDelta::IlkDelete {
                ilk_id: "ilk:w".into()
            },
            "worker1",
            &store2
        ));
        assert!(!delta_authorized_for_hive(
            &IdentityDelta::IlkDelete {
                ilk_id: "ilk:w".into()
            },
            "worker2",
            &store2
        ));
        assert!(!delta_authorized_for_hive(
            &IdentityDelta::IlkDelete {
                ilk_id: "ilk:unknown".into()
            },
            "worker1",
            &store2
        ));
    }

    #[test]
    fn authority_rejects_ilk_id_collision_with_other_hive() {
        let mut store = IdentityStore::default();
        // motherbee owns ilk_id "ilk:shared".
        store.ilks.insert(
            "ilk:shared".into(),
            ilk_for("SY.storage@motherbee", "ilk:shared"),
        );
        // worker1 tries to hijack that ilk_id with a forged `@worker1` node_name.
        let collide = IdentityDelta::IlkUpsert {
            ilk: ilk_for("SY.evil@worker1", "ilk:shared"),
        };
        assert!(
            !delta_authorized_for_hive(&collide, "worker1", &store),
            "delta path must reject ilk_id collision"
        );
        // The snapshot/reconcile path must also skip it, leaving motherbee intact.
        let deltas =
            store.reconcile_hive_ilks("worker1", vec![ilk_for("SY.evil@worker1", "ilk:shared")]);
        assert!(
            deltas.is_empty(),
            "reconcile must not overwrite foreign ilk_id"
        );
        assert_eq!(
            ilk_owning_hive(store.ilks.get("ilk:shared").unwrap()).as_deref(),
            Some("motherbee")
        );
    }

    #[test]
    fn reconcile_and_apply_never_delete_well_known_system_ilk() {
        let mut store = IdentityStore::default();
        // worker1's well-known system ilk (source = hive.system_nodes).
        let mut sys = ilk_for("SY.identity@worker1", "ilk:sys");
        sys.identification = json!({
            "node_name": "SY.identity@worker1", "hive_id": "worker1", "source": "hive.system_nodes"
        });
        store.ilks.insert("ilk:sys".into(), sys);
        // A forged snapshot that OMITS the system ilk must NOT reconcile-delete it
        // (the mesh-DOS the reviewer flagged).
        let deltas =
            store.reconcile_hive_ilks("worker1", vec![ilk_for("SY.other@worker1", "ilk:other")]);
        assert!(
            store.ilks.contains_key("ilk:sys"),
            "system ilk must survive reconcile"
        );
        assert!(store.ilks.contains_key("ilk:other"));
        assert!(!deltas
            .iter()
            .any(|d| matches!(d, IdentityDelta::IlkDelete { ilk_id } if ilk_id == "ilk:sys")));
        // apply_delta is the net: a direct IlkDelete of a well-known ilk is a no-op.
        store.apply_delta(IdentityDelta::IlkDelete {
            ilk_id: "ilk:sys".into(),
        });
        assert!(
            store.ilks.contains_key("ilk:sys"),
            "apply_delta must not remove a system ilk"
        );
    }

    #[test]
    fn reconcile_hive_ilks_is_additive_and_handles_hard_delete() {
        let mut store = IdentityStore::default();
        // motherbee's own ilk must survive a worker1 reconcile untouched.
        store
            .ilks
            .insert("ilk:mb".into(), ilk_for("SY.storage@motherbee", "ilk:mb"));
        // worker1 had two; now publishes one unchanged + one new (the other was
        // hard-deleted on the replica).
        store
            .ilks
            .insert("ilk:w1a".into(), ilk_for("SY.a@worker1", "ilk:w1a"));
        store
            .ilks
            .insert("ilk:w1b".into(), ilk_for("SY.b@worker1", "ilk:w1b"));
        let incoming = vec![
            ilk_for("SY.a@worker1", "ilk:w1a"),
            ilk_for("SY.c@worker1", "ilk:w1c"),
        ];
        let deltas = store.reconcile_hive_ilks("worker1", incoming);

        assert!(store.ilks.contains_key("ilk:mb"), "other hive untouched");
        assert!(store.ilks.contains_key("ilk:w1a"));
        assert!(store.ilks.contains_key("ilk:w1c"), "new ilk added");
        assert!(
            !store.ilks.contains_key("ilk:w1b"),
            "absent ilk hard-removed"
        );
        // w1a is byte-identical -> no-op skipped; only w1c upsert + w1b delete.
        let upserts = deltas
            .iter()
            .filter(|d| matches!(d, IdentityDelta::IlkUpsert { .. }))
            .count();
        let deletes = deltas
            .iter()
            .filter(|d| matches!(d, IdentityDelta::IlkDelete { .. }))
            .count();
        assert_eq!(upserts, 1);
        assert_eq!(deletes, 1);
        // A second identical reconcile is a pure no-op (no churn).
        let again = store.reconcile_hive_ilks(
            "worker1",
            vec![
                ilk_for("SY.a@worker1", "ilk:w1a"),
                ilk_for("SY.c@worker1", "ilk:w1c"),
            ],
        );
        assert!(again.is_empty(), "stable state produces no deltas");
    }

    #[test]
    fn self_owned_ilks_includes_tombstones_and_excludes_other_hives() {
        let mut store = IdentityStore::default();
        store
            .ilks
            .insert("ilk:mb".into(), ilk_for("SY.storage@motherbee", "ilk:mb"));
        store
            .ilks
            .insert("ilk:w".into(), ilk_for("SY.a@worker1", "ilk:w"));
        let mut tomb = ilk_for("SY.b@worker1", "ilk:wt");
        tomb.deleted_at_ms = Some(123);
        store.ilks.insert("ilk:wt".into(), tomb);

        let owned = store.self_owned_ilks("worker1");
        assert_eq!(owned.len(), 2, "active + tombstone, both @worker1");
        assert!(owned
            .iter()
            .all(|i| ilk_owning_hive(i).as_deref() == Some("worker1")));
    }

    // ---- F-01: fail-closed identity.sync.auth ----
    #[test]
    fn auth_mode_required_is_fail_closed() {
        // Absent / empty / whitespace / typo => REQUIRED.
        assert!(auth_mode_required(None));
        assert!(auth_mode_required(Some("")));
        assert!(auth_mode_required(Some("   ")));
        assert!(auth_mode_required(Some("require")));
        assert!(auth_mode_required(Some("requird")));
        assert!(auth_mode_required(Some("enabled")));
        assert!(auth_mode_required(Some("true")));
        // Explicit required (any case / padding) => required.
        assert!(auth_mode_required(Some("required")));
        assert!(auth_mode_required(Some("REQUIRED")));
        assert!(auth_mode_required(Some("  Required  ")));
        // Only the explicit opt-out tokens disable auth.
        assert!(!auth_mode_required(Some("disabled")));
        assert!(!auth_mode_required(Some("off")));
        assert!(!auth_mode_required(Some("none")));
        assert!(!auth_mode_required(Some("  DISABLED ")));
    }

    // ---- G-3: fail-safe SHM ilk_type default ----
    #[test]
    fn parse_ilk_type_for_shm_fails_safe_to_human() {
        assert_eq!(parse_ilk_type_for_shm("human"), SHM_ILK_TYPE_HUMAN);
        assert_eq!(parse_ilk_type_for_shm("agent"), SHM_ILK_TYPE_AGENT);
        assert_eq!(parse_ilk_type_for_shm("system"), SHM_ILK_TYPE_SYSTEM);
        // Unknown / empty / mis-cased must NOT become the privileged system type.
        assert_eq!(parse_ilk_type_for_shm(""), SHM_ILK_TYPE_HUMAN);
        assert_eq!(parse_ilk_type_for_shm("System"), SHM_ILK_TYPE_HUMAN);
        assert_eq!(parse_ilk_type_for_shm("worker"), SHM_ILK_TYPE_HUMAN);
    }

    // ---- G-1: replica may assert ilk_type=system only for its deterministic SY ilk ----
    #[test]
    fn ingest_system_ilk_type_bound_to_deterministic_shape() {
        // Non-system types are always allowed for an owned ilk.
        assert!(ingest_ilk_type_authorized(
            &ilk_for("AI.bot@worker1", "ilk:a"),
            "worker1"
        ));
        // A genuine deterministic SY.* system ilk of the publisher is allowed.
        let node = "SY.vault@worker1";
        let mut sys = ilk_for(node, &deterministic_system_ilk_id(node));
        sys.ilk_type = "system".to_string();
        assert!(ingest_ilk_type_authorized(&sys, "worker1"));
        // Relabelling an arbitrary owned (non-SY) ilk as system is rejected.
        let mut evil = ilk_for("AI.evil@worker1", "ilk:evil");
        evil.ilk_type = "system".to_string();
        assert!(!ingest_ilk_type_authorized(&evil, "worker1"));
        // SY-named but non-deterministic ilk_id is rejected (id must match shape).
        let mut fake = ilk_for("SY.vault@worker1", "ilk:not-the-real-id");
        fake.ilk_type = "system".to_string();
        assert!(!ingest_ilk_type_authorized(&fake, "worker1"));
        // Ownership still required: a deterministic system ilk of ANOTHER hive.
        let node2 = "SY.vault@worker2";
        let mut other = ilk_for(node2, &deterministic_system_ilk_id(node2));
        other.ilk_type = "system".to_string();
        assert!(!ingest_ilk_type_authorized(&other, "worker1"));
    }

    #[test]
    fn delta_authority_rejects_forged_system_ilk_type() {
        let store = IdentityStore::default();
        // A replica cannot mint a system-typed identity for an arbitrary owned ilk.
        let mut evil = ilk_for("AI.evil@worker1", "ilk:evil");
        evil.ilk_type = "system".to_string();
        assert!(!delta_authorized_for_hive(
            &IdentityDelta::IlkUpsert { ilk: evil },
            "worker1",
            &store
        ));
        // It CAN push its own agent ilk and its genuine deterministic system ilk.
        assert!(delta_authorized_for_hive(
            &IdentityDelta::IlkUpsert {
                ilk: ilk_for("AI.ok@worker1", "ilk:ok")
            },
            "worker1",
            &store
        ));
        let node = "SY.vault@worker1";
        let mut sys = ilk_for(node, &deterministic_system_ilk_id(node));
        sys.ilk_type = "system".to_string();
        assert!(delta_authorized_for_hive(
            &IdentityDelta::IlkUpsert { ilk: sys },
            "worker1",
            &store
        ));
    }

    // ---- G-2: AliasUpsert authority must own BOTH endpoints, never shadow system ----
    #[test]
    fn alias_upsert_authority_requires_both_endpoints_owned() {
        let mut store = IdentityStore::default();
        store
            .ilks
            .insert("ilk:mb".into(), ilk_for("SY.vault@motherbee", "ilk:mb"));
        store
            .ilks
            .insert("ilk:w-old".into(), ilk_for("AI.old@worker1", "ilk:w-old"));
        store.ilks.insert(
            "ilk:w-canon".into(),
            ilk_for("AI.canon@worker1", "ilk:w-canon"),
        );
        let alias = |old: &str, canon: &str| IdentityDelta::AliasUpsert {
            alias: AliasSnapshotRecord {
                old_ilk_id: old.to_string(),
                canonical_ilk_id: canon.to_string(),
                expires_at_ms: 9_999_999_999_999,
            },
        };
        // Legit merge: both endpoints owned by worker1.
        assert!(delta_authorized_for_hive(
            &alias("ilk:w-old", "ilk:w-canon"),
            "worker1",
            &store
        ));
        // Hijack: redirect motherbee's ilk onto a worker1 ilk — old_ilk_id not
        // owned by worker1 => rejected (G-2 core).
        assert!(!delta_authorized_for_hive(
            &alias("ilk:mb", "ilk:w-canon"),
            "worker1",
            &store
        ));
        // Unknown source, and canonical-not-owned, both rejected.
        assert!(!delta_authorized_for_hive(
            &alias("ilk:nope", "ilk:w-canon"),
            "worker1",
            &store
        ));
        assert!(!delta_authorized_for_hive(
            &alias("ilk:w-old", "ilk:mb"),
            "worker1",
            &store
        ));
        // Even an OWNED well-known system ilk may not be an alias source.
        let mut wk = ilk_for("SY.identity@worker1", "ilk:wk");
        wk.identification = json!({
            "node_name": "SY.identity@worker1", "hive_id": "worker1", "source": "hive.system_nodes"
        });
        store.ilks.insert("ilk:wk".into(), wk);
        assert!(!delta_authorized_for_hive(
            &alias("ilk:wk", "ilk:w-canon"),
            "worker1",
            &store
        ));
    }

    // ---- F-07: privileged SY roles are same-hive-only; IO/orchestrator cross-hive ----
    #[test]
    fn is_authorized_scopes_privileged_sy_roles_to_own_hive() {
        let hive = test_hive(Some("SY.frontdesk.gov@motherbee"));
        let runtime = IdentityRuntime::new(&hive, PathBuf::from("/tmp"), true, None);
        // Same-hive privileged roles allowed.
        assert!(runtime.is_authorized(MSG_TNT_CREATE, Some("SY.admin@motherbee")));
        assert!(runtime.is_authorized("CONFIG_SET", Some("SY.admin@motherbee")));
        assert!(runtime.is_authorized(MSG_ILK_SET_DEFINITION, Some("SY.architect@motherbee")));
        // Cross-hive privileged SY roles REJECTED.
        assert!(!runtime.is_authorized(MSG_TNT_CREATE, Some("SY.admin@worker1")));
        assert!(!runtime.is_authorized("CONFIG_SET", Some("SY.admin@worker1")));
        assert!(!runtime.is_authorized(MSG_ILK_SET_DEFINITION, Some("SY.architect@worker1")));
        assert!(!runtime.is_authorized(MSG_TNT_CREATE, Some("SY.frontdesk.gov@worker1")));
        // IO provisioning stays legitimately cross-hive.
        assert!(runtime.is_authorized(MSG_ILK_PROVISION, Some("IO.api@worker1")));
        // Orchestrator control-plane stays legitimately cross-hive.
        assert!(runtime.is_authorized(MSG_ILK_REGISTER, Some("SY.orchestrator@worker1")));
        assert!(runtime.is_authorized(MSG_ILK_DELETE, Some("SY.orchestrator@worker1")));
    }

    // ---- F-08: ILK_REGISTER may not mint 'system' ----
    #[test]
    fn register_ilk_type_rejects_system() {
        assert!(validate_ilk_type("human").is_ok());
        assert!(validate_ilk_type("agent").is_ok());
        assert!(validate_ilk_type("system").is_err());
        assert!(validate_ilk_type("System").is_err());
        assert!(validate_ilk_type(" system ").is_err());
    }
}
