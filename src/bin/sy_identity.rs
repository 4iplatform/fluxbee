use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time;
use tokio_postgres::{error::SqlState, Config as PgConfig, NoTls};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    build_node_config_response_message, build_node_secret_record, connect, load_node_secret_record,
    managed_node_config_path, save_node_secret_record, try_handle_default_node_status, NodeConfig,
    NodeReceiver, NodeSecretDescriptor, NodeSecretWriteOptions, NodeSender,
    NODE_CONFIG_APPLY_MODE_REPLACE, NODE_SECRET_REDACTION_TOKEN,
};
use json_router::shm::{
    copy_bytes_with_len, now_epoch_ms, IchEntry, IdentityRegionLimits, IdentityRegionWriter,
    IlkAliasEntry, IlkEntry, LsaRegionReader, LsaSnapshot, NodeEntry, RouterRegionReader,
    ShmSnapshot, TenantEntry, VocabularyEntry, FLAG_ACTIVE, ICH_ADDRESS_MAX_LEN,
    ICH_CHANNEL_TYPE_MAX_LEN,
};

type IdentityError = Box<dyn std::error::Error + Send + Sync>;
const PRIMARY_HIVE_ID: &str = "motherbee";

const DEFAULT_GATEWAY_NAME: &str = "RT.gateway";
const DEFAULT_DEFAULT_TENANT_NAME: &str = "fluxbee";
const DEFAULT_MERGE_ALIAS_TTL_SECS: u64 = 3600;
const ALIAS_GC_INTERVAL_SECS: u64 = 30;
const DEFAULT_IDENTITY_SYNC_PORT: u16 = 9100;
const IDENTITY_DB_NAME: &str = "fluxbee_identity";
const IDENTITY_NODE_BASE_NAME: &str = "SY.identity";
const IDENTITY_NODE_VERSION: &str = "2.0";
const IDENTITY_LOCAL_SECRET_KEY_POSTGRES_URL: &str = "postgres_url";
const IDENTITY_CONFIG_SCHEMA_VERSION: u32 = 1;
const IDENTITY_FULL_SYNC_CHUNK_ITEMS: usize = 256;
const IDENTITY_SYNC_VERSION: u32 = 1;
const SYNC_OP_FULL_SYNC_REQUEST: &str = "IDENTITY_FULL_SYNC_REQUEST";
const SYNC_OP_FULL_SYNC: &str = "full_sync";
const SYNC_OP_DELTA_SUBSCRIBE: &str = "IDENTITY_DELTA_SUBSCRIBE";
const SYNC_OP_DELTA: &str = "IDENTITY_DELTA";
const SYNC_OP_DELTA_ACK: &str = "IDENTITY_DELTA_ACK";
const IDENTITY_DELTA_ACK_TIMEOUT_MS: u64 = 2_000;
const IDENTITY_DELTA_MAX_RETRIES: u32 = 3;
const DEFAULT_IDENTITY_SHM_MAX_ILKS: u32 = 8_192;
const DEFAULT_IDENTITY_SHM_MAX_TENANTS: u32 = 1_024;
const DEFAULT_IDENTITY_SHM_MAX_VOCABULARY: u32 = 4_096;
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
const MSG_TNT_CREATE: &str = "TNT_CREATE";
const MSG_TNT_CREATE_RESPONSE: &str = "TNT_CREATE_RESPONSE";
const MSG_TNT_APPROVE: &str = "TNT_APPROVE";
const MSG_TNT_APPROVE_RESPONSE: &str = "TNT_APPROVE_RESPONSE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityDbSecretSource {
    LocalFile,
    EnvCompat,
    Missing,
}

impl IdentityDbSecretSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "local_file",
            Self::EnvCompat => "env_compat",
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
    default_tenant: Option<String>,
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
}

#[derive(Debug, Deserialize)]
struct DatabaseSection {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RouterIdentityFile {
    shm: RouterIdentityShm,
}

#[derive(Debug, Deserialize)]
struct RouterIdentityShm {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IlkProvisionRequest {
    ich_id: String,
    channel_type: String,
    address: String,
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
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    capabilities: Vec<String>,
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
struct IlkUpdateRequest {
    ilk_id: String,
    #[serde(default)]
    add_channels: Vec<ChannelInput>,
    #[serde(default)]
    add_roles: Vec<String>,
    #[serde(default)]
    remove_roles: Vec<String>,
    #[serde(default)]
    add_capabilities: Vec<String>,
    #[serde(default)]
    remove_capabilities: Vec<String>,
    #[serde(default)]
    change_reason: Option<String>,
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TntApproveRequest {
    tenant_id: String,
    approved_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TenantRecord {
    tenant_id: String,
    name: String,
    domain: Option<String>,
    status: String,
    settings: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChannelRecord {
    ich_id: String,
    channel_type: String,
    address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IlkRecord {
    ilk_id: String,
    ilk_type: String,
    registration_status: String,
    tenant_id: String,
    identification: Value,
    roles: Vec<String>,
    capabilities: Vec<String>,
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
    // (channel_type_lower, address_lower) -> ilk_id
    ich_lookup: HashMap<(String, String), String>,
    aliases: HashMap<String, AliasRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IdentitySyncRequest {
    operation: String,
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
                    "channel_count": ilk.channels.len(),
                    "channels": ilk.channels.iter().map(|channel| json!({
                        "ich_id": channel.ich_id,
                        "channel_type": channel.channel_type,
                        "address": channel.address,
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

    fn with_default_tenant(name: &str) -> Self {
        let mut out = Self::default();
        let tenant_id = format!("tnt:{}", Uuid::new_v4());
        out.tenants.insert(
            tenant_id.clone(),
            TenantRecord {
                tenant_id,
                name: name.trim().to_string(),
                domain: None,
                status: "active".to_string(),
                settings: json!({}),
            },
        );
        out
    }

    fn default_tenant_id(&self) -> Option<String> {
        self.tenants.keys().next().cloned()
    }

    fn provision_temporary_ilk(&mut self, req: IlkProvisionRequest) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.ich_id, "ich")?;
        validate_non_empty("channel_type", &req.channel_type)?;
        validate_non_empty("address", &req.address)?;
        validate_max_len("channel_type", &req.channel_type, ICH_CHANNEL_TYPE_MAX_LEN)?;
        validate_max_len("address", &req.address, ICH_ADDRESS_MAX_LEN)?;

        let key = canonical_ich_key(&req.channel_type, &req.address);
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
        let tenant_id = self
            .default_tenant_id()
            .ok_or_else(|| "missing default tenant".to_string())?;

        let ilk = IlkRecord {
            ilk_id: ilk_id.clone(),
            ilk_type: "human".to_string(),
            registration_status: "temporary".to_string(),
            tenant_id,
            identification: json!({}),
            roles: Vec::new(),
            capabilities: Vec::new(),
            channels: vec![ChannelRecord {
                ich_id: req.ich_id,
                channel_type: req.channel_type,
                address: req.address,
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

        let roles = dedup_lowercase_tags(req.roles)?;
        let capabilities = dedup_lowercase_tags(req.capabilities)?;

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
                let tenant_change = existing.tenant_id != req.tenant_id;
                if tenant_change && !existing.registration_status.eq("temporary") {
                    return Err("INVALID_TENANT_TRANSITION".to_string());
                }
                existing.ilk_type = req.ilk_type;
                existing.tenant_id = req.tenant_id;
                existing.registration_status = "complete".to_string();
                existing.identification = req.identification;
                existing.roles = roles;
                existing.capabilities = capabilities;
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
                        roles,
                        capabilities,
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
        merge_alias_ttl_secs: u64,
    ) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.ilk_id, "ilk")?;
        validate_channel_input(&req.channel)?;

        let canonical_ilk_id = req.ilk_id.clone();
        let target = self
            .ilks
            .get_mut(&canonical_ilk_id)
            .ok_or_else(|| "ILK_NOT_FOUND".to_string())?;
        if target.deleted_at_ms.is_some() {
            return Err("ILK_NOT_FOUND".to_string());
        }

        let key = canonical_ich_key(&req.channel.channel_type, &req.channel.address);
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

            let source_channels = source.channels.clone();
            let source_keys: Vec<(String, String)> = source_channels
                .iter()
                .map(|ch| canonical_ich_key(&ch.channel_type, &ch.address))
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
            "change_reason": req.change_reason,
        }))
    }

    fn update_ilk(&mut self, req: IlkUpdateRequest) -> Result<Value, String> {
        let _ = parse_prefixed_uuid(&req.ilk_id, "ilk")?;
        let add_roles = dedup_lowercase_tags(req.add_roles)?;
        let remove_roles = dedup_lowercase_tags(req.remove_roles)?;
        let add_caps = dedup_lowercase_tags(req.add_capabilities)?;
        let remove_caps = dedup_lowercase_tags(req.remove_capabilities)?;

        let entry = self
            .ilks
            .get_mut(&req.ilk_id)
            .ok_or_else(|| "ILK_NOT_FOUND".to_string())?;
        if entry.deleted_at_ms.is_some() {
            return Err("ILK_NOT_FOUND".to_string());
        }

        for ch in &req.add_channels {
            validate_channel_input(ch)?;
            let key = canonical_ich_key(&ch.channel_type, &ch.address);
            self.ich_lookup.insert(key, req.ilk_id.clone());
            let exists = entry.channels.iter().any(|c| c.ich_id == ch.ich_id);
            if !exists {
                entry.channels.push(ChannelRecord {
                    ich_id: ch.ich_id.clone(),
                    channel_type: ch.channel_type.clone(),
                    address: ch.address.clone(),
                });
            }
        }

        apply_tag_delta(&mut entry.roles, &add_roles, &remove_roles);
        apply_tag_delta(&mut entry.capabilities, &add_caps, &remove_caps);

        Ok(json!({
            "status": "ok",
            "ilk_id": req.ilk_id,
            "change_reason": req.change_reason,
        }))
    }

    fn create_tenant(&mut self, req: TntCreateRequest) -> Result<Value, String> {
        validate_non_empty("name", &req.name)?;
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
            }));
        }

        let tenant_id = format!("tnt:{}", Uuid::new_v4());
        self.tenants.insert(
            tenant_id.clone(),
            TenantRecord {
                tenant_id: tenant_id.clone(),
                name: normalized_name,
                domain: normalized_domain,
                status,
                settings: req.settings.unwrap_or_else(|| json!({})),
            },
        );

        Ok(json!({
            "status": "ok",
            "tenant_id": tenant_id,
            "created": true,
            "matched_by": serde_json::Value::Null,
        }))
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
                let key = canonical_ich_key(&channel.channel_type, &channel.address);
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
                        let key = canonical_ich_key(&channel.channel_type, &channel.address);
                        self.ich_lookup.insert(key, ilk_id.clone());
                    }
                }
                self.ilks.insert(ilk_id, ilk);
            }
            IdentityDelta::IlkDelete { ilk_id } => {
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
}

struct IdentityRuntime {
    hive_id: String,
    state_dir: PathBuf,
    gateway_name: String,
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
        state_dir: PathBuf,
        is_primary: bool,
        db_config: Option<PgConfig>,
    ) -> Self {
        let mut allowed_prefixes: HashMap<&'static str, Vec<&'static str>> = HashMap::new();
        allowed_prefixes.insert(MSG_ILK_PROVISION, vec!["IO."]);
        allowed_prefixes.insert(MSG_ILK_LIST, vec!["SY.admin@"]);
        allowed_prefixes.insert(MSG_ILK_GET, vec!["SY.admin@"]);
        allowed_prefixes.insert(MSG_ILK_REGISTER, vec!["AI.frontdesk@", "SY.orchestrator@"]);
        allowed_prefixes.insert(MSG_ILK_ADD_CHANNEL, vec!["AI.frontdesk@"]);
        allowed_prefixes.insert(MSG_ILK_UPDATE, vec!["SY.orchestrator@"]);
        allowed_prefixes.insert(MSG_TNT_CREATE, vec!["AI.frontdesk@"]);
        allowed_prefixes.insert(MSG_TNT_APPROVE, vec!["SY.admin@"]);
        allowed_prefixes.insert("CONFIG_GET", vec!["SY.admin@"]);
        allowed_prefixes.insert("CONFIG_SET", vec!["SY.admin@"]);

        let mut allowed_exacts: HashMap<&'static str, HashSet<String>> = HashMap::new();
        let mut bootstrap = HashSet::new();
        bootstrap.insert(format!("SY.identity@{}", hive.hive_id));
        allowed_exacts.insert(MSG_ILK_REGISTER, bootstrap.clone());
        allowed_exacts.insert(MSG_ILK_UPDATE, bootstrap);
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
                .entry(MSG_TNT_CREATE)
                .or_default()
                .insert(frontdesk_node);
        }

        let default_tenant = hive
            .identity
            .as_ref()
            .and_then(|cfg| cfg.default_tenant.as_deref())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(DEFAULT_DEFAULT_TENANT_NAME);
        let merge_alias_ttl_secs = hive
            .identity
            .as_ref()
            .and_then(|cfg| cfg.merge_alias_ttl_secs)
            .unwrap_or(DEFAULT_MERGE_ALIAS_TTL_SECS);

        let gateway_name = hive
            .wan
            .as_ref()
            .and_then(|wan| wan.gateway_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_GATEWAY_NAME.to_string());

        Self {
            hive_id: hive.hive_id.clone(),
            state_dir,
            gateway_name,
            is_primary,
            db_config,
            merge_alias_ttl_secs,
            store: IdentityStore::with_default_tenant(default_tenant),
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

        let source_name = self.resolve_source_name_with_retry(&msg.routing.src).await;
        if action == MSG_ILK_PROVISION {
            tracing::info!(
                action,
                trace_id = %trace_id,
                source_uuid = %msg.routing.src,
                source_name = source_name.as_deref().unwrap_or("<unknown>"),
                payload = %msg.payload,
                "identity received ILK_PROVISION request"
            );
        }
        if !self.is_authorized(action, source_name.as_deref()) {
            let payload = json!({
                "status": "error",
                "error_code": "UNAUTHORIZED_REGISTRAR",
                "message": "source not authorized for action",
                "action": action,
                "source_uuid": msg.routing.src,
                "source_name": source_name,
            });
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
                        match self
                            .store
                            .add_channel(req.clone(), self.merge_alias_ttl_secs)
                        {
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
            MSG_TNT_CREATE => match serde_json::from_value::<TntCreateRequest>(msg.payload.clone())
            {
                Ok(req) => match self.store.create_tenant(req) {
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
                                            self.store.tenants.remove(&tenant_id);
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
                                        deltas.push(delta_envelope(IdentityDelta::TenantUpsert {
                                            tenant,
                                        }));
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
                },
                Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
            },
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
                apply_identity_shm_deltas(writer, &self.store, action, &deltas)?;
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
        let removed_local = self.store.gc_expired_aliases(now_ms);
        if removed_local > 0 {
            tracing::info!(removed = removed_local, "identity alias gc applied locally");
        }

        if self.is_primary {
            if let Some(database_config) = self.db_config.as_ref() {
                let removed_db = gc_aliases_in_db(database_config).await?;
                if removed_db > 0 {
                    tracing::info!(
                        removed = removed_db,
                        "identity alias gc applied in database"
                    );
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

        let variants = authorized_name_variants(name);

        if let Some(exacts) = self.allowed_exacts.get(action) {
            if variants.iter().any(|candidate| exacts.contains(candidate)) {
                return true;
            }
        }

        let Some(prefixes) = self.allowed_prefixes.get(action) else {
            return false;
        };
        variants
            .iter()
            .any(|candidate| prefixes.iter().any(|prefix| candidate.starts_with(prefix)))
    }

    async fn resolve_source_name_with_retry(&self, source_uuid: &str) -> Option<String> {
        let uuid = Uuid::parse_str(source_uuid).ok()?;
        let started = Instant::now();

        loop {
            if let Ok(snapshot) = self.load_router_snapshot() {
                if let Some(name) = source_name_from_snapshot(&snapshot, uuid) {
                    return Some(name);
                }
            }
            if let Ok(snapshot) = self.load_lsa_snapshot() {
                if let Some(name) = source_name_from_lsa_snapshot(&snapshot, uuid) {
                    return Some(name);
                }
            }
            if started.elapsed() >= Duration::from_secs(2) {
                return None;
            }
            time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn load_router_snapshot(&self) -> Result<ShmSnapshot, IdentityError> {
        let router_l2_name = ensure_l2_name(&self.gateway_name, &self.hive_id);
        let identity_path = self.state_dir.join(router_l2_name).join("identity.yaml");
        let data = fs::read_to_string(identity_path)?;
        let identity: RouterIdentityFile = serde_yaml::from_str(&data)?;
        let reader = RouterRegionReader::open_read_only(&identity.shm.name)?;
        reader
            .read_snapshot()
            .ok_or_else(|| "router shm snapshot unavailable".into())
    }

    fn load_lsa_snapshot(&self) -> Result<LsaSnapshot, IdentityError> {
        let shm_name = format!("/jsr-lsa-{}", self.hive_id);
        let reader = LsaRegionReader::open_read_only(&shm_name)?;
        reader
            .read_snapshot()
            .ok_or_else(|| "lsa shm snapshot unavailable".into())
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

fn authorized_name_variants(name: &str) -> Vec<String> {
    let mut out = vec![name.to_string()];
    if let Some((local, hive)) = name.split_once('@') {
        if local.starts_with("SY.orchestrator.relay.") {
            out.push(format!("SY.orchestrator@{hive}"));
        }
    }
    out
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
    let (database_url, db_secret_source) = resolve_database_url(&node_name);
    let (db_config, db_init_error) = if is_primary {
        initialize_identity_database_backend(database_url.as_deref()).await
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
    let node_config = NodeConfig {
        name: IDENTITY_NODE_BASE_NAME.to_string(),
        router_socket: socket_dir,
        uuid_persistence_dir: state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: IDENTITY_NODE_VERSION.to_string(),
    };

    let mut runtime = IdentityRuntime::new(&hive, state_dir.clone(), is_primary, db_config);
    if is_primary {
        if let Some(database_config) = runtime.db_config.clone() {
            match load_identity_store_from_db(&database_config).await {
                Ok(store) if store.tenants.is_empty() => {
                    if let Some(default_tenant_id) = runtime.store.default_tenant_id() {
                        if let Some(default_tenant) =
                            runtime.store.tenants.get(&default_tenant_id).cloned()
                        {
                            if let Err(err) =
                                upsert_tenant_in_db(&database_config, &default_tenant).await
                            {
                                tracing::warn!(error = %err, "failed to persist default tenant in primary db bootstrap");
                            } else {
                                tracing::info!(tenant_id = %default_tenant.tenant_id, "persisted default tenant in primary db bootstrap");
                            }
                        }
                    }
                }
                Ok(store) => {
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
            match fetch_full_sync_from_primary(upstream).await {
                Ok(store) => {
                    let metrics = store.metrics();
                    runtime.store = store;
                    tracing::info!(upstream = %upstream, metrics = %metrics, "identity full sync bootstrap applied");
                }
                Err(err) => {
                    tracing::warn!(upstream = %upstream, error = %err, "identity full sync bootstrap failed; starting with local in-memory state");
                }
            }
        } else {
            tracing::warn!("identity replica mode without identity.sync.upstream; starting with local in-memory state");
        }
    }
    if let Some(writer) = identity_shm.as_mut() {
        if let Err(err) = sync_identity_shm_mappings(writer, &runtime.store) {
            tracing::warn!(error = %err, "initial identity shm sync failed");
        }
    }
    let (delta_event_tx, mut delta_event_rx) = mpsc::unbounded_channel::<IdentityDeltaEnvelope>();
    if !is_primary {
        if let Some(upstream) = sync_upstream.clone() {
            tokio::spawn(async move {
                run_delta_subscription_loop(upstream, delta_event_tx).await;
            });
        }
    }
    let (mut sender, mut receiver) =
        connect_with_retry(&node_config, Duration::from_secs(1)).await?;

    tracing::info!(
        hive = %hive.hive_id,
        role = %hive.role.clone().unwrap_or_else(|| "unknown".to_string()),
        "sy.identity started"
    );

    let mut heartbeat = time::interval(Duration::from_secs(5));
    let mut alias_gc_tick = time::interval(Duration::from_secs(ALIAS_GC_INTERVAL_SECS));
    let sync_listener = sync_listener;
    let mut delta_subscribers: Vec<mpsc::UnboundedSender<IdentityDeltaEnvelope>> = Vec::new();
    let mut next_delta_seq: u64 = 1;
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                tracing::debug!(metrics = %runtime.store.metrics(), "identity heartbeat");
                if let Some(writer) = identity_shm.as_mut() {
                    writer.update_heartbeat();
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
                    let chunks = runtime.store.build_full_sync_chunks(IDENTITY_FULL_SYNC_CHUNK_ITEMS);
                    match handle_sync_connection(stream, chunks).await {
                        Ok(Some(subscriber)) => {
                            tracing::info!(remote = %remote_addr, "identity delta subscriber connected");
                            delta_subscribers.push(subscriber);
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::warn!(remote = %remote_addr, error = %err, "identity sync request failed");
                        }
                    }
                }
            }
            received = receiver.recv() => {
                let msg = match received {
                    Ok(msg) => msg,
                    Err(err) => {
                        tracing::warn!(error = %err, "recv error; reconnecting");
                        let (new_sender, new_receiver) = connect_with_retry(&node_config, Duration::from_secs(1)).await?;
                        sender = new_sender;
                        receiver = new_receiver;
                        tracing::info!("reconnected to router");
                        continue;
                    }
                };

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
        _ => SHM_ILK_TYPE_SYSTEM,
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
            created_at: now_ms,
            updated_at: now_ms,
            _reserved: [0u8; 8],
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

    let mut channel_to_ich: HashMap<(String, String), [u8; 16]> = HashMap::new();
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
                channel_type: [0u8; ICH_CHANNEL_TYPE_MAX_LEN],
                address: [0u8; ICH_ADDRESS_MAX_LEN],
                flags: FLAG_ACTIVE,
                is_primary: if idx == 0 { 1 } else { 0 },
                _pad0: [0u8; 5],
                added_at: now_ms,
                _reserved: [0u8; 16],
            };
            copy_bytes_with_len(&mut ich_entry.channel_type, &channel_type);
            copy_bytes_with_len(&mut ich_entry.address, &address);
            ich_entries.push(ich_entry);
            ich_count = ich_count.saturating_add(1);
            channel_to_ich.insert((channel_type, address), *ich_uuid.as_bytes());
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
            roles_offset: 0,
            roles_len: 0,
            _pad1: [0u8; 2],
            capabilities_offset: 0,
            capabilities_len: 0,
            _pad2: [0u8; 2],
            created_at: now_ms,
            updated_at: now_ms,
            _reserved: [0u8; 8],
        };
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
    let mut lookup_keys: Vec<(String, String)> = store.ich_lookup.keys().cloned().collect();
    lookup_keys.sort_unstable();
    for (channel_type, address) in lookup_keys {
        let Some(ilk_id) = store
            .ich_lookup
            .get(&(channel_type.clone(), address.clone()))
        else {
            continue;
        };
        let Ok(ilk_uuid) = parse_prefixed_uuid(ilk_id, "ilk") else {
            tracing::warn!(ilk_id = %ilk_id, "skipping invalid ilk_id during identity shm sync");
            continue;
        };
        let Some(ich_id) = channel_to_ich
            .get(&(channel_type.clone(), address.clone()))
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
        if let Err(err) =
            writer.upsert_ich_mapping(&channel_type, &address, ich_id, *ilk_uuid.as_bytes())
        {
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
        created_at: now_ms,
        updated_at: now_ms,
        _reserved: [0u8; 8],
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
        roles_offset: 0,
        roles_len: 0,
        _pad1: [0u8; 2],
        capabilities_offset: 0,
        capabilities_len: 0,
        _pad2: [0u8; 2],
        created_at: now_ms,
        updated_at: now_ms,
        _reserved: [0u8; 8],
    };
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
            channel_type: [0u8; ICH_CHANNEL_TYPE_MAX_LEN],
            address: [0u8; ICH_ADDRESS_MAX_LEN],
            flags: FLAG_ACTIVE,
            is_primary: if idx == 0 { 1 } else { 0 },
            _pad0: [0u8; 5],
            added_at: now_ms,
            _reserved: [0u8; 16],
        };
        copy_bytes_with_len(&mut entry.channel_type, &channel_type);
        copy_bytes_with_len(&mut entry.address, &address);
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

async fn handle_sync_connection(
    stream: TcpStream,
    chunks: Vec<IdentityFullSyncChunk>,
) -> Result<Option<mpsc::UnboundedSender<IdentityDeltaEnvelope>>, IdentityError> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut request_line = String::new();
    let read = reader.read_line(&mut request_line).await?;
    if read == 0 {
        return Err("full sync request connection closed".into());
    }
    let request: IdentitySyncRequest = serde_json::from_str(request_line.trim())?;
    match request.operation.as_str() {
        SYNC_OP_FULL_SYNC_REQUEST => {
            for chunk in chunks {
                let encoded = serde_json::to_string(&chunk)?;
                write_half.write_all(encoded.as_bytes()).await?;
                write_half.write_all(b"\n").await?;
            }
            write_half.flush().await?;
            Ok(None)
        }
        SYNC_OP_DELTA_SUBSCRIBE => {
            let (tx, mut rx) = mpsc::unbounded_channel::<IdentityDeltaEnvelope>();
            let ack = json!({
                "status": "ok",
                "operation": "IDENTITY_DELTA_SUBSCRIBED"
            });
            write_half
                .write_all(serde_json::to_string(&ack)?.as_bytes())
                .await?;
            write_half.write_all(b"\n").await?;
            write_half.flush().await?;
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
                        if write_half.write_all(encoded.as_bytes()).await.is_err() {
                            return;
                        }
                        if write_half.write_all(b"\n").await.is_err() {
                            return;
                        }
                        if write_half.flush().await.is_err() {
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

async fn fetch_full_sync_from_primary(upstream: &str) -> Result<IdentityStore, IdentityError> {
    let stream = TcpStream::connect(upstream).await?;
    let (read_half, mut write_half) = stream.into_split();
    let request = IdentitySyncRequest {
        operation: SYNC_OP_FULL_SYNC_REQUEST.to_string(),
    };
    let encoded = serde_json::to_string(&request)?;
    write_half.write_all(encoded.as_bytes()).await?;
    write_half.write_all(b"\n").await?;
    write_half.flush().await?;

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let mut expected_chunks: Option<usize> = None;
    let mut received: Vec<Option<IdentityFullSyncChunk>> = Vec::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
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

fn broadcast_deltas(
    subscribers: &mut Vec<mpsc::UnboundedSender<IdentityDeltaEnvelope>>,
    deltas: &[IdentityDeltaEnvelope],
) {
    if deltas.is_empty() {
        return;
    }
    subscribers.retain(|tx| {
        for delta in deltas {
            if tx.send(delta.clone()).is_err() {
                return false;
            }
        }
        true
    });
}

async fn run_delta_subscription_loop(
    upstream: String,
    sink: mpsc::UnboundedSender<IdentityDeltaEnvelope>,
) {
    loop {
        match stream_deltas_from_primary(&upstream, &sink).await {
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
) -> Result<(), IdentityError> {
    let stream = TcpStream::connect(upstream).await?;
    let (read_half, mut write_half) = stream.into_split();
    let request = IdentitySyncRequest {
        operation: SYNC_OP_DELTA_SUBSCRIBE.to_string(),
    };
    let encoded = serde_json::to_string(&request)?;
    write_half.write_all(encoded.as_bytes()).await?;
    write_half.write_all(b"\n").await?;
    write_half.flush().await?;

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let mut last_seq: Option<u64> = None;
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
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
                        return Err(format!(
                            "delta stream sequence gap/out-of-order: prev={} current={}",
                            last, envelope.seq
                        )
                        .into());
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
        let read = time::timeout(
            Duration::from_millis(IDENTITY_DELTA_ACK_TIMEOUT_MS),
            reader.read_line(&mut line),
        )
        .await
        .map_err(|_| "delta ack timeout".to_string())??;
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

fn load_hive(config_dir: &Path) -> Result<HiveFile, IdentityError> {
    let raw = fs::read_to_string(config_dir.join("hive.yaml"))?;
    Ok(serde_yaml::from_str(&raw)?)
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, hive_id)
    }
}

fn response_name(action: &str) -> &'static str {
    match action {
        MSG_ILK_PROVISION => MSG_ILK_PROVISION_RESPONSE,
        MSG_ILK_LIST => MSG_ILK_LIST_RESPONSE,
        MSG_ILK_GET => MSG_ILK_GET_RESPONSE,
        MSG_ILK_REGISTER => MSG_ILK_REGISTER_RESPONSE,
        MSG_ILK_ADD_CHANNEL => MSG_ILK_ADD_CHANNEL_RESPONSE,
        MSG_ILK_UPDATE => MSG_ILK_UPDATE_RESPONSE,
        MSG_TNT_CREATE => MSG_TNT_CREATE_RESPONSE,
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
            | MSG_TNT_CREATE
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
        },
        payload,
    };
    sender.send(reply).await?;
    Ok(())
}

async fn connect_with_retry(
    config: &NodeConfig,
    delay: Duration,
) -> Result<(NodeSender, NodeReceiver), fluxbee_sdk::NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                tracing::warn!(error = %err, "connect failed; retrying");
                time::sleep(delay).await;
            }
        }
    }
}

fn source_name_from_snapshot(snapshot: &ShmSnapshot, source_uuid: Uuid) -> Option<String> {
    for entry in &snapshot.nodes {
        if entry.name_len == 0 {
            continue;
        }
        let Ok(entry_uuid) = Uuid::from_slice(&entry.uuid) else {
            continue;
        };
        if entry_uuid == source_uuid {
            return Some(node_name(entry));
        }
    }
    None
}

fn source_name_from_lsa_snapshot(snapshot: &LsaSnapshot, source_uuid: Uuid) -> Option<String> {
    for entry in &snapshot.nodes {
        if entry.name_len == 0 {
            continue;
        }
        let Ok(entry_uuid) = Uuid::from_slice(&entry.uuid) else {
            continue;
        };
        if entry_uuid == source_uuid {
            let len = entry.name_len as usize;
            return Some(String::from_utf8_lossy(&entry.name[..len]).into_owned());
        }
    }
    None
}

fn node_name(entry: &NodeEntry) -> String {
    let len = entry.name_len as usize;
    String::from_utf8_lossy(&entry.name[..len]).into_owned()
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

fn canonical_ich_key(channel_type: &str, address: &str) -> (String, String) {
    (
        channel_type.trim().to_ascii_lowercase(),
        address.trim().to_ascii_lowercase(),
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

fn validate_ilk_type(value: &str) -> Result<(), String> {
    if matches!(value.trim(), "human" | "agent" | "system") {
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

fn dedup_lowercase_tags(tags: Vec<String>) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in tags {
        let tag = raw.trim().to_ascii_lowercase();
        if tag.is_empty() {
            return Err("INVALID_REQUEST".to_string());
        }
        if !tag
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err("VOCABULARY_INVALID".to_string());
        }
        if seen.insert(tag.clone()) {
            out.push(tag);
        }
    }
    Ok(out)
}

fn apply_tag_delta(current: &mut Vec<String>, add: &[String], remove: &[String]) {
    let mut set: HashSet<String> = current.iter().cloned().collect();
    for value in add {
        set.insert(value.clone());
    }
    for value in remove {
        set.remove(value);
    }
    let mut out: Vec<String> = set.into_iter().collect();
    out.sort();
    *current = out;
}

async fn ensure_primary_schema(database_config: &PgConfig) -> Result<(), IdentityError> {
    let (client, connection) = database_config.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::error!(error = %err, "identity postgres connection closed");
        }
    });

    client
        .batch_execute(
            r#"
CREATE TABLE IF NOT EXISTS identity_tenants (
    tenant_id UUID PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    domain VARCHAR(128),
    status VARCHAR(16) NOT NULL DEFAULT 'pending',
    settings JSONB NOT NULL DEFAULT '{}',
    approved_by UUID,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

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
    is_primary BOOLEAN DEFAULT FALSE,
    added_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(channel_type, address, tenant_id)
);

CREATE INDEX IF NOT EXISTS idx_identity_ichs_lookup
    ON identity_ichs(channel_type, address);

CREATE INDEX IF NOT EXISTS idx_identity_ichs_ilk
    ON identity_ichs(ilk_id);

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

    tracing::info!("identity primary schema ensured");
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

fn definition_tags(definition: &Value, key: &str) -> Vec<String> {
    let from_current = definition
        .get("current")
        .and_then(Value::as_object)
        .and_then(|current| current.get(key))
        .and_then(Value::as_array);
    let from_root = definition.get(key).and_then(Value::as_array);
    from_current
        .or(from_root)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
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
            })
        })
        .collect();
    json!({
        "tenant_id": ilk.tenant_id,
        "channels": channels,
    })
}

fn definition_json_from_ilk(ilk: &IlkRecord) -> Value {
    json!({
        "current": {
            "roles": ilk.roles,
            "capabilities": ilk.capabilities,
        }
    })
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
INSERT INTO identity_ichs (ich_id, ilk_id, tenant_id, channel_type, address, is_primary, added_at)
VALUES ($1::text::uuid, $2::text::uuid, $3::text::uuid, $4, $5, $6, NOW())
ON CONFLICT (ich_id) DO UPDATE
SET
    ilk_id = EXCLUDED.ilk_id,
    tenant_id = EXCLUDED.tenant_id,
    channel_type = EXCLUDED.channel_type,
    address = EXCLUDED.address,
    is_primary = EXCLUDED.is_primary
"#,
                &[
                    &ich_uuid,
                    &ilk_uuid,
                    &tenant_uuid,
                    &channel.channel_type,
                    &channel.address,
                    &is_primary,
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
    settings
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
        store.tenants.insert(
            tenant_id.clone(),
            TenantRecord {
                tenant_id,
                name,
                domain,
                status,
                settings,
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
        let roles = definition_tags(&definition, "roles");
        let capabilities = definition_tags(&definition, "capabilities");
        let deleted_at_ms: Option<i64> = row.get("deleted_at_ms");
        store.ilks.insert(
            format!("ilk:{}", ilk_uuid),
            IlkRecord {
                ilk_id: format!("ilk:{}", ilk_uuid),
                ilk_type: row.get("ilk_type"),
                registration_status: row.get("registration_status"),
                tenant_id: format!("tnt:{}", tenant_uuid),
                identification: row.get("identification"),
                roles,
                capabilities,
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
    address
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
                let key = canonical_ich_key(&channel.channel_type, &channel.address);
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
    let (client, connection) = database_config.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "identity tenant upsert postgres connection closed");
        }
    });

    client
        .execute(
            r#"
INSERT INTO identity_tenants (tenant_id, name, domain, status, settings, updated_at)
VALUES ($1::text::uuid, $2, $3, $4, $5::jsonb, NOW())
ON CONFLICT (tenant_id) DO UPDATE
SET
    name = EXCLUDED.name,
    domain = EXCLUDED.domain,
    status = EXCLUDED.status,
    settings = EXCLUDED.settings,
    updated_at = NOW()
"#,
            &[
                &tenant_uuid,
                &tenant.name,
                &tenant.domain,
                &tenant.status,
                &tenant.settings,
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
        config: identity_public_config(is_primary, state),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    write_json_atomic(&path, &serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn identity_public_config(is_primary: bool, state: &IdentityControlState) -> Value {
    json!({
        "database": {
            "mode": "postgres",
            "postgres_url": if identity_secret_configured(is_primary, state) {
                Value::String(NODE_SECRET_REDACTION_TOKEN.to_string())
            } else {
                Value::Null
            },
            "source": identity_effective_source(is_primary, state),
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
    let mut secret_descriptor = NodeSecretDescriptor::new(
        "config.database.postgres_url",
        IDENTITY_LOCAL_SECRET_KEY_POSTGRES_URL,
    );
    secret_descriptor.required = is_primary;
    secret_descriptor.configured = configured;
    secret_descriptor.persistence = if is_primary {
        control_state.secret_source.as_str().to_string()
    } else {
        "replica_non_primary".to_string()
    };
    let mut notes = vec![
        Value::String("SY.identity uses PostgreSQL only on motherbee primary.".to_string()),
        Value::String(
            "Secret values are persisted in local secrets.json and always returned redacted."
                .to_string(),
        ),
        Value::String(
            "Applying a new DB secret persists locally and requires sy-identity restart to affect live connections."
                .to_string(),
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
            "message": "Missing database secret in local secrets.json or env overrides."
        })
    };
    json!({
        "ok": is_primary && control_state.db_ready,
        "node_name": node_name,
        "state": identity_state_label(is_primary, control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": identity_public_config(is_primary, control_state),
        "contract": {
            "node_family": "SY",
            "node_kind": "SY.identity",
            "supports": ["CONFIG_GET", "CONFIG_SET"],
            "required_fields": if is_primary {
                json!(["config.database.postgres_url"])
            } else {
                json!([])
            },
            "optional_fields": [],
            "secrets": [secret_descriptor],
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
    let Some(postgres_url) = config
        .get("database")
        .and_then(Value::as_object)
        .and_then(|value| value.get("postgres_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "invalid_config",
            "config.database.postgres_url is required".to_string(),
        ));
    };
    if let Err(err) = postgres_url.parse::<PgConfig>() {
        return Ok(identity_config_error_response(
            is_primary,
            node_name,
            control_state,
            "invalid_config",
            format!("invalid postgres_url: {err}"),
        ));
    }
    persist_local_identity_postgres_url(
        node_name,
        postgres_url,
        &build_secret_write_options_from_message(msg),
    )?;
    control_state.schema_version = schema_version;
    control_state.config_version = config_version;
    control_state.secret_source = IdentityDbSecretSource::LocalFile;
    if control_state.db_ready {
        control_state.last_error = None;
    } else {
        control_state.last_error = Some(
            "restart_required: sy-identity must be restarted to apply new DB connection settings"
                .to_string(),
        );
    }
    persist_identity_config_state(node_name, is_primary, control_state)?;
    Ok(json!({
        "ok": true,
        "node_name": node_name,
        "state": identity_state_label(is_primary, control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": identity_public_config(is_primary, control_state),
        "notes": [
            "postgres_url persisted in local secrets.json",
            "restart_required: sy-identity must be restarted to apply new DB connection settings"
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
        "config": identity_public_config(is_primary, control_state),
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn persist_local_identity_postgres_url(
    node_name: &str,
    postgres_url: &str,
    options: &NodeSecretWriteOptions,
) -> Result<(), fluxbee_sdk::NodeSecretError> {
    let mut secrets = load_node_secret_record(node_name)
        .map(|record| record.secrets)
        .unwrap_or_else(|_| Map::new());
    secrets.insert(
        IDENTITY_LOCAL_SECRET_KEY_POSTGRES_URL.to_string(),
        Value::String(postgres_url.to_string()),
    );
    let record = build_node_secret_record(secrets, options);
    save_node_secret_record(node_name, &record)?;
    Ok(())
}

fn load_local_identity_postgres_url(node_name: &str) -> Option<String> {
    load_node_secret_record(node_name)
        .ok()
        .and_then(|record| {
            record
                .secrets
                .get(IDENTITY_LOCAL_SECRET_KEY_POSTGRES_URL)
                .cloned()
        })
        .and_then(|value| value.as_str().map(ToString::to_string))
        .filter(|value| !value.trim().is_empty() && value != NODE_SECRET_REDACTION_TOKEN)
}

fn build_secret_write_options_from_message(msg: &Message) -> NodeSecretWriteOptions {
    let updated_by_label = msg
        .payload
        .get("requested_by")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| msg.meta.action.clone());
    NodeSecretWriteOptions {
        updated_by_ilk: msg.meta.src_ilk.clone(),
        updated_by_label,
        trace_id: Some(msg.routing.trace_id.clone()),
    }
}

fn resolve_database_url(node_name: &str) -> (Option<String>, IdentityDbSecretSource) {
    if let Some(url) = load_local_identity_postgres_url(node_name) {
        return (Some(url), IdentityDbSecretSource::LocalFile);
    }
    if let Ok(url) = std::env::var("FLUXBEE_DATABASE_URL") {
        if !url.trim().is_empty() {
            return (Some(url), IdentityDbSecretSource::EnvCompat);
        }
    }
    if let Ok(url) = std::env::var("JSR_DATABASE_URL") {
        if !url.trim().is_empty() {
            return (Some(url), IdentityDbSecretSource::EnvCompat);
        }
    }
    (None, IdentityDbSecretSource::Missing)
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
            Some("Missing database secret in local secrets.json or env overrides.".to_string()),
        );
    };
    let base = match database_url.parse::<PgConfig>() {
        Ok(value) => value,
        Err(err) => return (None, Some(format!("invalid postgres_url: {err}"))),
    };
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
        }
    }

    #[test]
    fn configured_identity_frontdesk_node_name_normalizes_local_name() {
        let hive = test_hive(Some("AI.frontdesk.gov"));
        assert_eq!(
            configured_identity_frontdesk_node_name(&hive).as_deref(),
            Some("AI.frontdesk.gov@motherbee")
        );
    }

    #[test]
    fn identity_runtime_authorizes_configured_frontdesk_exact_name() {
        let hive = test_hive(Some("AI.frontdesk.gov@motherbee"));
        let runtime = IdentityRuntime::new(&hive, PathBuf::from("/tmp"), true, None);

        assert!(runtime.is_authorized(MSG_ILK_REGISTER, Some("AI.frontdesk.gov@motherbee")));
        assert!(runtime.is_authorized(MSG_ILK_ADD_CHANNEL, Some("AI.frontdesk.gov@motherbee")));
        assert!(runtime.is_authorized(MSG_TNT_CREATE, Some("AI.frontdesk.gov@motherbee")));
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
            },
        );
        store.ilks.insert(
            "ilk:22222222-2222-2222-2222-222222222222".to_string(),
            IlkRecord {
                ilk_id: "ilk:22222222-2222-2222-2222-222222222222".to_string(),
                ilk_type: "human".to_string(),
                registration_status: "complete".to_string(),
                tenant_id: "tnt:11111111-1111-1111-1111-111111111111".to_string(),
                identification: json!({"display_name":"Jane","node_name":"AI.frontdesk.gov@motherbee"}),
                roles: vec!["user".to_string()],
                capabilities: vec!["chat".to_string()],
                channels: vec![ChannelRecord {
                    ich_id: "ich:33333333-3333-3333-3333-333333333333".to_string(),
                    channel_type: "slack".to_string(),
                    address: "U123".to_string(),
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
            },
        );

        let by_name = store
            .create_tenant(TntCreateRequest {
                name: "4iplatform".to_string(),
                domain: None,
                status: Some("active".to_string()),
                settings: None,
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
            },
        );
        store.ilks.insert(
            "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            IlkRecord {
                ilk_id: "ilk:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
                ilk_type: "human".to_string(),
                registration_status: "temporary".to_string(),
                tenant_id: "tnt:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                identification: json!({"display_name":"Jane","node_name":"AI.frontdesk.gov@motherbee"}),
                roles: vec![],
                capabilities: vec![],
                channels: vec![ChannelRecord {
                    ich_id: "ich:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string(),
                    channel_type: "slack".to_string(),
                    address: "U123".to_string(),
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
}
