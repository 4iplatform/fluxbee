//! SY.deploy on the motherbee: SY.admin's actions through the SDK's admin RPC (`MeshAdmin`), as
//! `SY.deploy@motherbee`; what it reads locally (`MotherbeeWorld`): the identity SHM with the
//! SDK's reader, the hive registry and the router's link state for the workers' room, the runtimes
//! manifest, the agent assets and the people folder; and its answers to SY.admin.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use fluxbee_sdk::mesh::{HiveRegistry, HiveRole};
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use fluxbee_sdk::rpc::AdminCommandRequest;
use fluxbee_sdk::{NodeSender, RouterDispatcher};
use json_router::agent_assets::{
    agent_asset_file_matches, prepare_agent_asset, store_prepared_agent_asset,
};
use json_router::mesh_reply;
use json_router::runtime_manifest::{load_runtime_manifest_from_paths, RuntimeManifestEntry};
use json_router::runtime_package::DIST_RUNTIME_MANIFEST_PATH;
use json_router::shm::{
    now_epoch_ms, LsaRegionReader, RemoteHiveEntry, FLAG_DELETED, FLAG_STALE, HEARTBEAT_STALE_MS,
    HIVE_FLAG_SELF,
};

use crate::world::{
    AdminApi, AdminCall, AdminOutcome, Capacity, DocumentRead, HiveRoom, IlkView, World,
};

/// The admin SY.deploy asks for every step, and the only one it takes orders from.
pub const ADMIN_NODE: &str = "SY.admin@motherbee";

pub struct MeshAdmin {
    pub rpc: Arc<RouterDispatcher>,
}

fn detail_text(detail: Option<Value>) -> String {
    match detail {
        Some(Value::String(text)) => text,
        Some(Value::Null) | None => String::new(),
        Some(other) => other
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| other.to_string()),
    }
}

#[async_trait]
impl AdminApi for MeshAdmin {
    async fn call(&self, call: &AdminCall) -> AdminOutcome {
        let result = self
            .rpc
            .send_admin_rpc(AdminCommandRequest {
                admin_target: ADMIN_NODE,
                action: &call.action,
                target: Some(&call.target),
                params: call.params.clone(),
                request_id: None,
                timeout: call.timeout,
            })
            .await;
        match result {
            Ok(result) if result.status == "ok" => AdminOutcome::Ok(result.payload),
            Ok(result) => {
                let retryable =
                    result.payload.get("retryable").and_then(Value::as_bool) == Some(true);
                AdminOutcome::Refused {
                    code: result
                        .error_code
                        .clone()
                        .unwrap_or_else(|| format!("ADMIN_{}", result.status.to_ascii_uppercase())),
                    detail: detail_text(result.error_detail),
                    retryable,
                    payload: result.payload,
                }
            }
            Err(err) => AdminOutcome::Unanswered(err.to_string()),
        }
    }
}

/// What SY.deploy reads on the motherbee.
pub struct MotherbeeWorld {
    pub hive_id: String,
    pub blob_root: PathBuf,
}

impl MotherbeeWorld {
    fn agent_assets_dir(&self) -> PathBuf {
        self.blob_root.join("agent-assets")
    }

    fn people_dir(&self) -> PathBuf {
        self.blob_root.join("people")
    }
}

fn hive_name(entry: &RemoteHiveEntry) -> Option<String> {
    let len = (entry.hive_id_len as usize).min(entry.hive_id.len());
    (len > 0).then(|| String::from_utf8_lossy(&entry.hive_id[..len]).into_owned())
}

/// A remote hive whose link state is gone or older than the router's heartbeat window, as SY.admin
/// reads it (`remote_hive_is_stale`).
fn hive_is_stale(entry: &RemoteHiveEntry, now: u64) -> bool {
    (entry.flags & (FLAG_DELETED | FLAG_STALE)) != 0
        || now.saturating_sub(entry.last_updated) > HEARTBEAT_STALE_MS
}

impl World for MotherbeeWorld {
    fn identity(&self) -> Result<Vec<IlkView>, String> {
        let snapshot = fluxbee_sdk::identity::list_ilks_from_hive_id(&self.hive_id)
            .map_err(|err| err.to_string())?;
        Ok(snapshot
            .ilks
            .into_iter()
            .map(|ilk| IlkView {
                ilk_id: ilk.ilk_id,
                ilk_type: ilk.ilk_type,
                tenant_id: ilk.tenant_id,
                display_name: ilk.display_name,
                handler_node: ilk.handler_node,
                is_virtual: ilk.is_virtual,
                works_for: ilk.works_for,
                work_role: ilk.work_role,
                document_hash: ilk.document_hash,
                role_hash: ilk.role_hash,
                skill_hashes: ilk.skill_hashes,
                handbook_hashes: ilk.handbook_hashes,
                personality_hash: ilk.personality_hash,
            })
            .collect())
    }

    fn tenant_status(&self, tenant_id: &str) -> Result<Option<String>, String> {
        fluxbee_sdk::identity::tenant_status_in_hive_id(&self.hive_id, tenant_id)
            .map_err(|err| err.to_string())
    }

    fn capacity(&self) -> Result<Capacity, String> {
        let registry = HiveRegistry::motherbee();
        let entries = registry
            .entries()
            .map_err(|err| format!("the hive registry cannot be read: {err}"))?;
        let workers: Vec<String> = entries
            .iter()
            .filter(|entry| entry.role() == Some(HiveRole::Worker))
            .map(|entry| entry.name().to_string())
            .collect();
        let snapshot = LsaRegionReader::open_read_only(&format!("/jsr-lsa-{}", self.hive_id))
            .map_err(|err| format!("the router's link state cannot be opened: {err}"))?
            .read_snapshot()
            .ok_or("the router's link state cannot be read now")?;
        let now = now_epoch_ms();
        let mut capacity = Capacity::default();
        for entry in &snapshot.hives {
            if entry.flags & HIVE_FLAG_SELF != 0 || entry.flags & FLAG_DELETED != 0 {
                continue;
            }
            capacity.total_remote_nodes += entry.node_count;
        }
        for worker in workers {
            let entry = snapshot
                .hives
                .iter()
                .find(|entry| hive_name(entry).as_deref() == Some(worker.as_str()));
            capacity.workers.push(HiveRoom {
                alive: entry.is_some_and(|entry| !hive_is_stale(entry, now)),
                node_count: entry.map(|entry| entry.node_count).unwrap_or(0),
                hive_id: worker,
            });
        }
        Ok(capacity)
    }

    fn runtime_current(&self, runtime: &str) -> Result<Option<String>, String> {
        let manifest =
            load_runtime_manifest_from_paths(&[PathBuf::from(DIST_RUNTIME_MANIFEST_PATH)])?
                .ok_or("the runtimes manifest is not there")?;
        let Some(entry) = manifest.runtimes.get(runtime) else {
            return Ok(None);
        };
        let entry: RuntimeManifestEntry = serde_json::from_value(entry.clone())
            .map_err(|err| format!("the runtime's entry: {err}"))?;
        Ok(entry
            .current
            .map(|current| current.trim().to_string())
            .filter(|current| !current.is_empty()))
    }

    fn asset_present(&self, hash: &str) -> bool {
        agent_asset_file_matches(&self.agent_assets_dir(), hash)
    }

    fn write_asset(&self, asset: &Value) -> Result<String, String> {
        let prepared = prepare_agent_asset(asset).map_err(|err| err.to_string())?;
        store_prepared_agent_asset(&self.agent_assets_dir(), &prepared)
            .map_err(|err| err.to_string())?;
        Ok(prepared.hash)
    }

    fn person_document(&self, ilk_id: &str, hash16: &str) -> DocumentRead {
        let Some(uuid) = ilk_id
            .strip_prefix("ilk:")
            .filter(|uuid| uuid::Uuid::parse_str(uuid).is_ok())
        else {
            return DocumentRead::Unreadable("the person's id is not an ILK".to_string());
        };
        if hash16.len() != 16
            || !hash16
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return DocumentRead::Unreadable("the document's hash is not 16 hex".to_string());
        }
        let path = self
            .people_dir()
            .join(format!("person-{uuid}_{hash16}.json"));
        match std::fs::read(&path) {
            Ok(raw) => serde_json::from_slice(&raw)
                .map(DocumentRead::Found)
                .unwrap_or_else(|err| DocumentRead::Unreadable(err.to_string())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => DocumentRead::NotYet,
            Err(err) => DocumentRead::Unreadable(err.to_string()),
        }
    }

    fn now_ms(&self) -> u64 {
        now_epoch_ms()
    }
}

/// The blob root: hive.yaml's `blob.path`, else the default.
pub fn blob_root(configured: Option<&str>) -> PathBuf {
    configured
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("/var/lib/fluxbee/blob").to_path_buf())
}

/// SY.deploy's answer to an order of SY.admin's: the order's kind, `<ACTION>_RESPONSE`, its action,
/// to the asker, on the order's trace. One past the frame is answered with `REPLY_TOO_LARGE` in its
/// place, so SY.admin learns why at once.
pub async fn answer(
    sender: &NodeSender,
    request: &Message,
    kind: &str,
    action: &str,
    payload: Value,
) {
    let build = |payload: Value| Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(request.routing.src.clone()),
            ttl: 16,
            trace_id: request.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: kind.to_string(),
            msg: Some(format!("{}_RESPONSE", action.to_ascii_uppercase())),
            action: Some(action.to_string()),
            ..Meta::default()
        },
        payload,
    };
    let outcome = mesh_reply::reply_outcome(&payload);
    let rows = mesh_reply::listing_rows(&payload);
    let rows = rows.or_else(|| {
        payload
            .get("deployments")
            .and_then(Value::as_array)
            .map(Vec::len)
    });
    let mut reply = build(payload);
    if let Some(size) = mesh_reply::oversized(&reply) {
        reply = build(mesh_reply::reply_too_large_payload(
            action,
            size,
            mesh_reply::REPLY_MAX_BYTES,
            outcome,
            rows,
            "ask for fewer: cd_list takes a limit, a tenant or a grouping",
        ));
    }
    if let Err(err) = sender.send(reply).await {
        tracing::warn!(action, error = %err, "an answer to SY.admin could not be sent");
    }
}

/// The refusal of an order from anyone but SY.admin.
pub fn not_from_admin(caller: &str) -> Value {
    json!({
        "status": "error",
        "error_code": "UNAUTHORIZED",
        "message": format!("SY.deploy takes orders from {ADMIN_NODE} only, not '{caller}'"),
    })
}
