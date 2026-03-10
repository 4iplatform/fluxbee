use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time;
use tokio_postgres::NoTls;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{connect, NodeConfig, NodeReceiver, NodeSender};
use json_router::shm::{
    now_epoch_ms, LsaRegionReader, LsaSnapshot, NodeEntry, RouterRegionReader, ShmSnapshot,
};

type IdentityError = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_GATEWAY_NAME: &str = "RT.gateway";
const DEFAULT_DEFAULT_TENANT_NAME: &str = "fluxbee";
const DEFAULT_MERGE_ALIAS_TTL_SECS: u64 = 3600;
const ALIAS_GC_INTERVAL_SECS: u64 = 30;

const MSG_ILK_PROVISION: &str = "ILK_PROVISION";
const MSG_ILK_PROVISION_RESPONSE: &str = "ILK_PROVISION_RESPONSE";
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

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    wan: Option<WanSection>,
    #[serde(default)]
    identity: Option<IdentitySection>,
    #[serde(default)]
    database: Option<DatabaseSection>,
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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Clone)]
struct TenantRecord {
    tenant_id: String,
    name: String,
    domain: Option<String>,
    status: String,
    settings: Value,
}

#[derive(Debug, Clone)]
struct ChannelRecord {
    ich_id: String,
    channel_type: String,
    address: String,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
struct AliasRecord {
    canonical_ilk_id: String,
    expires_at_ms: u64,
}

#[derive(Debug, Default)]
struct IdentityStore {
    tenants: HashMap<String, TenantRecord>,
    ilks: HashMap<String, IlkRecord>,
    // (channel_type_lower, address_lower) -> ilk_id
    ich_lookup: HashMap<(String, String), String>,
    aliases: HashMap<String, AliasRecord>,
}

impl IdentityStore {
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

        let key = canonical_ich_key(&req.channel_type, &req.address);
        if let Some(existing) = self.ich_lookup.get(&key) {
            return Ok(json!({
                "status": "ok",
                "ilk_id": existing,
                "registration_status": "temporary",
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

        match self.ilks.get_mut(&req.ilk_id) {
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
                    req.ilk_id.clone(),
                    IlkRecord {
                        ilk_id: req.ilk_id.clone(),
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
            "ilk_id": req.ilk_id,
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
        let status = req
            .status
            .unwrap_or_else(|| "pending".to_string())
            .trim()
            .to_ascii_lowercase();
        if !matches!(status.as_str(), "pending" | "active" | "suspended") {
            return Err("INVALID_REQUEST".to_string());
        }

        let tenant_id = format!("tnt:{}", Uuid::new_v4());
        self.tenants.insert(
            tenant_id.clone(),
            TenantRecord {
                tenant_id: tenant_id.clone(),
                name: req.name,
                domain: req.domain,
                status,
                settings: req.settings.unwrap_or_else(|| json!({})),
            },
        );

        Ok(json!({
            "status": "ok",
            "tenant_id": tenant_id,
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
}

struct IdentityRuntime {
    hive_id: String,
    state_dir: PathBuf,
    gateway_name: String,
    is_primary: bool,
    db_url: Option<String>,
    merge_alias_ttl_secs: u64,
    store: IdentityStore,
    // action -> allowed prefixes by node name
    // e.g. "ILK_PROVISION" -> ["IO."]
    // special full names allowed are represented as exacts in `allowed_exacts`.
    allowed_prefixes: HashMap<&'static str, Vec<&'static str>>,
    allowed_exacts: HashMap<&'static str, HashSet<String>>,
}

impl IdentityRuntime {
    fn new(hive: &HiveFile, state_dir: PathBuf, is_primary: bool, db_url: Option<String>) -> Self {
        let mut allowed_prefixes: HashMap<&'static str, Vec<&'static str>> = HashMap::new();
        allowed_prefixes.insert(MSG_ILK_PROVISION, vec!["IO."]);
        allowed_prefixes.insert(MSG_ILK_REGISTER, vec!["AI.frontdesk@", "SY.orchestrator@"]);
        allowed_prefixes.insert(MSG_ILK_ADD_CHANNEL, vec!["AI.frontdesk@"]);
        allowed_prefixes.insert(MSG_ILK_UPDATE, vec!["SY.orchestrator@"]);
        allowed_prefixes.insert(MSG_TNT_CREATE, vec!["AI.frontdesk@"]);
        allowed_prefixes.insert(MSG_TNT_APPROVE, vec!["SY.admin@"]);

        let mut allowed_exacts: HashMap<&'static str, HashSet<String>> = HashMap::new();
        let mut bootstrap = HashSet::new();
        bootstrap.insert(format!("SY.identity@{}", hive.hive_id));
        allowed_exacts.insert(MSG_ILK_REGISTER, bootstrap.clone());
        allowed_exacts.insert(MSG_ILK_UPDATE, bootstrap);

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
            db_url,
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
    ) -> Result<(), IdentityError> {
        let Some(action) = msg.meta.msg.as_deref() else {
            return Ok(());
        };

        let source_name = self.resolve_source_name_with_retry(&msg.routing.src).await;
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
            return Ok(());
        }

        let payload = match action {
            MSG_ILK_PROVISION => {
                match serde_json::from_value::<IlkProvisionRequest>(msg.payload.clone()) {
                    Ok(req) => match self.store.provision_temporary_ilk(req) {
                        Ok(ok) => ok,
                        Err(code) => error_payload(&code, "failed to provision ilk"),
                    },
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_ILK_REGISTER => {
                match serde_json::from_value::<IlkRegisterRequest>(msg.payload.clone()) {
                    Ok(req) => match self.store.register_ilk(req) {
                        Ok(ok) => ok,
                        Err(code) => error_payload(&code, "failed to register ilk"),
                    },
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_ILK_ADD_CHANNEL => {
                match serde_json::from_value::<IlkAddChannelRequest>(msg.payload.clone()) {
                    Ok(req) => match self.store.add_channel(req, self.merge_alias_ttl_secs) {
                        Ok(ok) => ok,
                        Err(code) => error_payload(&code, "failed to add channel"),
                    },
                    Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
                }
            }
            MSG_ILK_UPDATE => match serde_json::from_value::<IlkUpdateRequest>(msg.payload.clone())
            {
                Ok(req) => match self.store.update_ilk(req) {
                    Ok(ok) => ok,
                    Err(code) => error_payload(&code, "failed to update ilk"),
                },
                Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
            },
            MSG_TNT_CREATE => match serde_json::from_value::<TntCreateRequest>(msg.payload.clone())
            {
                Ok(req) => match self.store.create_tenant(req) {
                    Ok(ok) => ok,
                    Err(code) => error_payload(&code, "failed to create tenant"),
                },
                Err(err) => error_payload("INVALID_REQUEST", &err.to_string()),
            },
            MSG_TNT_APPROVE => {
                match serde_json::from_value::<TntApproveRequest>(msg.payload.clone()) {
                    Ok(req) => match self.store.approve_tenant(req) {
                        Ok(ok) => ok,
                        Err(code) => error_payload(&code, "failed to approve tenant"),
                    },
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

        send_system_response(sender, msg, response_name(action), payload).await
    }

    async fn run_alias_gc(&mut self) -> Result<(), IdentityError> {
        let now_ms = now_epoch_ms();
        let removed_local = self.store.gc_expired_aliases(now_ms);
        if removed_local > 0 {
            tracing::info!(removed = removed_local, "identity alias gc applied locally");
        }

        if self.is_primary {
            if let Some(db_url) = self.db_url.as_deref() {
                let removed_db = gc_aliases_in_db(db_url).await?;
                if removed_db > 0 {
                    tracing::info!(
                        removed = removed_db,
                        "identity alias gc applied in database"
                    );
                }
            }
        }
        Ok(())
    }

    fn is_authorized(&self, action: &str, source_name: Option<&str>) -> bool {
        if matches!(
            action,
            "IDENTITY_METRICS" | "IDENTITY_METRICS_RESPONSE" | "PING" | "PING_RESPONSE"
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
        prefixes.iter().any(|prefix| name.starts_with(prefix))
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
    if is_primary {
        ensure_primary_schema(&hive).await?;
    } else {
        tracing::info!(
            role = %hive.role.clone().unwrap_or_else(|| "unknown".to_string()),
            "sy.identity running without local DB (replica/non-primary mode)"
        );
    }
    let node_config = NodeConfig {
        name: "SY.identity".to_string(),
        router_socket: socket_dir,
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir: config_dir.clone(),
        version: "2.0".to_string(),
    };

    let db_url = if is_primary {
        Some(database_url(&hive)?)
    } else {
        None
    };
    let mut runtime = IdentityRuntime::new(&hive, state_dir.clone(), is_primary, db_url);
    let (mut sender, mut receiver) =
        connect_with_retry(&node_config, Duration::from_secs(1)).await?;

    tracing::info!(
        hive = %hive.hive_id,
        role = %hive.role.clone().unwrap_or_else(|| "unknown".to_string()),
        "sy.identity started"
    );

    let mut heartbeat = time::interval(Duration::from_secs(5));
    let mut alias_gc_tick = time::interval(Duration::from_secs(ALIAS_GC_INTERVAL_SECS));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                tracing::debug!(metrics = %runtime.store.metrics(), "identity heartbeat");
            }
            _ = alias_gc_tick.tick() => {
                if let Err(err) = runtime.run_alias_gc().await {
                    tracing::warn!(error = %err, "identity alias gc failed");
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

                if let Err(err) = runtime.process_system_message(&sender, &msg).await {
                    tracing::warn!(error = %err, action = ?msg.meta.msg, "failed to process system message");
                }
            }
        }
    }
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
        MSG_ILK_REGISTER => MSG_ILK_REGISTER_RESPONSE,
        MSG_ILK_ADD_CHANNEL => MSG_ILK_ADD_CHANNEL_RESPONSE,
        MSG_ILK_UPDATE => MSG_ILK_UPDATE_RESPONSE,
        MSG_TNT_CREATE => MSG_TNT_CREATE_RESPONSE,
        MSG_TNT_APPROVE => MSG_TNT_APPROVE_RESPONSE,
        "IDENTITY_METRICS" => "IDENTITY_METRICS_RESPONSE",
        _ => "SYSTEM_ERROR",
    }
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

fn canonical_ich_key(channel_type: &str, address: &str) -> (String, String) {
    (
        channel_type.trim().to_ascii_lowercase(),
        address.trim().to_ascii_lowercase(),
    )
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

fn validate_channel_input(channel: &ChannelInput) -> Result<(), String> {
    let _ = parse_prefixed_uuid(&channel.ich_id, "ich")?;
    validate_non_empty("channel.type", &channel.channel_type)?;
    validate_non_empty("channel.address", &channel.address)?;
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

async fn ensure_primary_schema(hive: &HiveFile) -> Result<(), IdentityError> {
    let url = database_url(hive)?;
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await?;
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

async fn gc_aliases_in_db(database_url: &str) -> Result<u64, IdentityError> {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls).await?;
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

fn database_url(hive: &HiveFile) -> Result<String, IdentityError> {
    if let Ok(url) = std::env::var("FLUXBEE_DATABASE_URL") {
        if !url.trim().is_empty() {
            return Ok(url);
        }
    }
    if let Ok(url) = std::env::var("JSR_DATABASE_URL") {
        if !url.trim().is_empty() {
            return Ok(url);
        }
    }
    let Some(db) = hive.database.as_ref() else {
        return Err("database.url missing in hive.yaml and env".into());
    };
    let Some(url) = db.url.as_ref() else {
        return Err("database.url missing in hive.yaml and env".into());
    };
    if url.trim().is_empty() {
        return Err("database.url empty".into());
    }
    Ok(url.clone())
}

fn is_mother_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee" || r == "mother")
}
