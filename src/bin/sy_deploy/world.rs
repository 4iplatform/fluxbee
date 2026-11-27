//! What SY.deploy reads and calls, behind traits so the orders and the runner are tested without a
//! router: SY.admin's actions (`AdminApi`), and what it reads on the motherbee's own disk and
//! memory (`World`): the identity SHM, the hives' room, the runtimes' current versions, the agent
//! assets and the people folder.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

/// One SY.admin action: its name, the hive it is for, its arguments and how long SY.deploy waits.
#[derive(Debug, Clone, PartialEq)]
pub struct AdminCall {
    pub action: String,
    pub target: String,
    pub params: Value,
    pub timeout: Duration,
}

/// An SY.admin answer, as SY.deploy reads it.
#[derive(Debug, Clone, PartialEq)]
pub enum AdminOutcome {
    /// Done: the action's payload.
    Ok(Value),
    /// Refused or failed, with its code; `retryable` when the answer says it changed nothing and
    /// may pass when asked again.
    Refused {
        code: String,
        detail: String,
        retryable: bool,
        payload: Value,
    },
    /// No answer: a timeout, the router down, or an answer that could not be read. Whether the
    /// action ran is unknown, so the next try reads the state first (§5.2).
    Unanswered(String),
}

#[async_trait]
pub trait AdminApi: Send + Sync {
    async fn call(&self, call: &AdminCall) -> AdminOutcome;
}

/// An active ILK as the identity SHM holds it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IlkView {
    pub ilk_id: String,
    pub ilk_type: String,
    pub tenant_id: String,
    pub display_name: Option<String>,
    pub handler_node: Option<String>,
    pub is_virtual: bool,
    pub works_for: Option<String>,
    pub work_role: Option<String>,
    /// The 16-hex prefix of its document's hash; `None` for none.
    pub document_hash: Option<String>,
    pub role_hash: Option<String>,
    pub skill_hashes: Vec<String>,
    pub handbook_hashes: Vec<String>,
    pub personality_hash: Option<String>,
}

/// A worker's room, as the motherbee's router sees it (design §5.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiveRoom {
    pub hive_id: String,
    /// Connected, its join done, and its router's link state fresh.
    pub alive: bool,
    /// The nodes its router holds.
    pub node_count: u32,
}

/// The workers, and the remote nodes the installation holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capacity {
    pub workers: Vec<HiveRoom>,
    pub total_remote_nodes: u32,
}

/// A person's document, read from the people folder.
#[derive(Debug, Clone, PartialEq)]
pub enum DocumentRead {
    Found(Value),
    /// The SHM names a file that has not arrived, or no longer is (A-338): "not yet".
    NotYet,
    Unreadable(String),
}

pub trait World: Send + Sync {
    /// The identity SHM's active ILKs.
    fn identity(&self) -> Result<Vec<IlkView>, String>;
    /// A tenant's status in the identity SHM (`active`, `pending`, `suspended`); `None` when it is
    /// not there.
    fn tenant_status(&self, tenant_id: &str) -> Result<Option<String>, String>;
    fn capacity(&self) -> Result<Capacity, String>;
    /// A runtime's `current` version in the runtimes manifest.
    fn runtime_current(&self, runtime: &str) -> Result<Option<String>, String>;
    /// Whether `<blob>/agent-assets/<hash>.json` is there with its bytes.
    fn asset_present(&self, hash: &str) -> bool;
    /// Writes an asset into `<blob>/agent-assets/` as Archi does.
    fn write_asset(&self, asset: &Value) -> Result<String, String>;
    /// A person's document: `<blob>/people/person-<uuid>_<hash16>.json`.
    fn person_document(&self, ilk_id: &str, hash16: &str) -> DocumentRead;
    fn now_ms(&self) -> u64;
}

/// The router's most nodes (`MAX_NODES`) and the installation's most remote nodes
/// (`MAX_REMOTE_NODES`), `src/shm/mod.rs`.
pub const MAX_NODES_PER_HIVE: u32 = json_router::shm::MAX_NODES;
pub const MAX_REMOTE_NODES: u32 = json_router::shm::MAX_REMOTE_NODES;

#[cfg(test)]
pub mod fake {
    //! A world and an SY.admin in memory: the admin's writes change the world as SY.admin's,
    //! identity's and the orchestrator's would, so a deployment can be run end to end.

    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use json_router::agent_assets::prepare_agent_asset;
    use serde_json::json;

    #[derive(Debug, Clone, Default)]
    pub struct FakeNode {
        pub hive: String,
        pub lifecycle: String,
        pub ilk_id: String,
        pub tenant_id: String,
        pub config: Value,
    }

    #[derive(Debug, Default)]
    pub struct FakeState {
        pub ilks: BTreeMap<String, IlkView>,
        pub tenants: BTreeMap<String, String>,
        pub workers: Vec<HiveRoom>,
        pub runtimes: BTreeMap<String, String>,
        pub assets: BTreeMap<String, Value>,
        pub documents: BTreeMap<String, Value>,
        pub nodes: BTreeMap<String, FakeNode>,
        /// key -> (tenant, resource_type, owner ILK, assigned ILKs)
        pub secrets: BTreeMap<String, (String, String, Option<String>, Vec<String>)>,
        /// Every call, in order.
        pub calls: Vec<AdminCall>,
        /// Actions that answer no answer, as many times as named.
        pub unanswered: BTreeMap<String, usize>,
        /// Actions refused, with their code, every time.
        pub refused: BTreeMap<String, String>,
        /// Whether a retire marks the node's ILK (`ilk_retired`).
        pub retire_marks: bool,
        pub now_ms: u64,
    }

    pub const TENANT: &str = "tnt:43d576a3-d712-4d91-9245-5d5463dd693e";
    pub const OTHER_TENANT: &str = "tnt:11111111-1111-4111-8111-111111111111";

    #[derive(Default)]
    pub struct Fake {
        pub state: Mutex<FakeState>,
    }

    impl Fake {
        /// The motherbee with one live worker, `worker1`, the tenant `pruebas` with an anthropic
        /// key and a WhatsApp credential, and the runtimes current.
        pub fn installation() -> Fake {
            let fake = Fake::default();
            {
                let mut state = fake.state.lock().expect("fake");
                state
                    .tenants
                    .insert(TENANT.to_string(), "active".to_string());
                state
                    .tenants
                    .insert(OTHER_TENANT.to_string(), "active".to_string());
                state.workers = vec![HiveRoom {
                    hive_id: "worker1".to_string(),
                    alive: true,
                    node_count: 20,
                }];
                state
                    .runtimes
                    .insert("ai.generic".to_string(), "0.1.159".to_string());
                state
                    .runtimes
                    .insert("io.wapp".to_string(), "0.1.159".to_string());
                state.secrets.insert(
                    "pruebas-anthropic".to_string(),
                    (
                        TENANT.to_string(),
                        "anthropic".to_string(),
                        None,
                        Vec::new(),
                    ),
                );
                state.secrets.insert(
                    "acme-laura-wapp".to_string(),
                    (TENANT.to_string(), "whatsapp".to_string(), None, Vec::new()),
                );
                state.retire_marks = true;
                state.now_ms = 1_700_000_000_000;
            }
            fake
        }

        pub fn calls(&self) -> Vec<AdminCall> {
            self.state.lock().expect("fake").calls.clone()
        }

        /// The calls that change something, by action and the role they are for.
        pub fn writes(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter(|call| !matches!(call.action.as_str(), "get_node_status" | "vault_list"))
                .map(|call| {
                    let what = call
                        .params
                        .get("node_name")
                        .or_else(|| call.params.get("ilk_id"))
                        .or_else(|| call.params.get("key"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    format!("{} {what}", call.action)
                })
                .collect()
        }

        pub fn clear_calls(&self) {
            self.state.lock().expect("fake").calls.clear();
        }

        fn answer(&self, call: &AdminCall) -> AdminOutcome {
            let mut state = self.state.lock().expect("fake");
            state.calls.push(call.clone());
            if let Some(left) = state.unanswered.get_mut(&call.action) {
                if *left > 0 {
                    *left -= 1;
                    return AdminOutcome::Unanswered(format!("{} timed out", call.action));
                }
            }
            if let Some(code) = state.refused.get(&call.action) {
                return AdminOutcome::Refused {
                    code: code.clone(),
                    detail: format!("{} refused", call.action),
                    retryable: false,
                    payload: Value::Null,
                };
            }
            let params = &call.params;
            let text = |field: &str| {
                params
                    .get(field)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let refused = |code: &str| AdminOutcome::Refused {
                code: code.to_string(),
                detail: code.to_string(),
                retryable: false,
                payload: Value::Null,
            };
            match call.action.as_str() {
                "create_virtual_person" => {
                    let ilk_id = text("ilk_id");
                    state.ilks.entry(ilk_id.clone()).or_insert_with(|| IlkView {
                        ilk_id: ilk_id.clone(),
                        ilk_type: "human".to_string(),
                        tenant_id: text("tenant_id"),
                        display_name: Some(text("display_name")),
                        is_virtual: true,
                        ..IlkView::default()
                    });
                    AdminOutcome::Ok(json!({"status": "ok", "ilk_id": ilk_id}))
                }
                "set_person_document" => {
                    let ilk_id = text("ilk_id");
                    let document = params["document"].clone();
                    let hash = json_router::agent_assets::sha256_hex(
                        &crate::source::canonical_bytes(&document),
                    );
                    let Some(ilk) = state.ilks.get_mut(&ilk_id) else {
                        return refused("PERSON_NOT_FOUND");
                    };
                    ilk.document_hash = Some(hash[..16].to_string());
                    state
                        .documents
                        .insert(format!("{ilk_id}/{}", &hash[..16]), document);
                    AdminOutcome::Ok(
                        json!({"status": "ok", "ilk_id": ilk_id, "document_hash": hash}),
                    )
                }
                "run_node" => {
                    let node_name = text("node_name");
                    if state.nodes.contains_key(&node_name) {
                        return refused("NODE_ALREADY_EXISTS");
                    }
                    let ilk_id = text("ilk_id");
                    state.nodes.insert(
                        node_name.clone(),
                        FakeNode {
                            hive: call.target.clone(),
                            lifecycle: "RUNNING".to_string(),
                            ilk_id: ilk_id.clone(),
                            tenant_id: text("tenant_id"),
                            config: params["config"].clone(),
                        },
                    );
                    state.ilks.insert(
                        ilk_id.clone(),
                        IlkView {
                            ilk_id,
                            ilk_type: "agent".to_string(),
                            tenant_id: text("tenant_id"),
                            handler_node: Some(node_name.clone()),
                            ..IlkView::default()
                        },
                    );
                    AdminOutcome::Ok(json!({"status": "ok", "node_name": node_name}))
                }
                "start_node" => {
                    let node_name = text("node_name");
                    match state.nodes.get_mut(&node_name) {
                        Some(node) => {
                            node.lifecycle = "RUNNING".to_string();
                            AdminOutcome::Ok(json!({"status": "ok"}))
                        }
                        None => refused("NODE_NOT_FOUND"),
                    }
                }
                "get_node_status" => {
                    let node_name = text("node_name");
                    match state.nodes.get(&node_name) {
                        Some(node) => AdminOutcome::Ok(json!({
                            "status": "ok",
                            "node_name": node_name,
                            "node_status": {"lifecycle_state": node.lifecycle, "health_state": "HEALTHY"}
                        })),
                        None => refused("NODE_NOT_FOUND"),
                    }
                }
                "set_ilk_works_for" => {
                    let ilk_id = text("ilk_id");
                    let works_for = params["works_for"].as_str().map(str::to_string);
                    let work_role = params["work_role"].as_str().map(str::to_string);
                    match state.ilks.get_mut(&ilk_id) {
                        Some(ilk) => {
                            ilk.works_for = works_for;
                            ilk.work_role = work_role;
                            AdminOutcome::Ok(json!({"status": "ok"}))
                        }
                        None => refused("ILK_NOT_FOUND"),
                    }
                }
                "set_ilk_definition" => {
                    let ilk_id = text("ilk_id");
                    let definition = params["definition"].clone();
                    let strings = |field: &str| -> Vec<String> {
                        definition[field]
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    let skill_hashes = strings("skill_hashes");
                    let handbook_hashes = strings("handbook_hashes");
                    match state.ilks.get_mut(&ilk_id) {
                        Some(ilk) => {
                            ilk.role_hash = definition["role_hash"].as_str().map(str::to_string);
                            ilk.personality_hash =
                                definition["personality_hash"].as_str().map(str::to_string);
                            ilk.skill_hashes = skill_hashes;
                            ilk.handbook_hashes = handbook_hashes;
                            AdminOutcome::Ok(json!({"status": "ok"}))
                        }
                        None => refused("ILK_NOT_FOUND"),
                    }
                }
                "vault_list" => {
                    let filter = &params["filter"];
                    let tenant = filter["tenant_id"].as_str().unwrap_or_default();
                    let resource = filter["resource_type"].as_str();
                    let prefix = filter["prefix"].as_str().unwrap_or_default();
                    let secrets: Vec<Value> = state
                        .secrets
                        .iter()
                        .filter(|(key, (t, r, _, _))| {
                            t == tenant
                                && resource.is_none_or(|resource| resource == r)
                                && key.starts_with(prefix)
                        })
                        .map(|(key, (t, r, owner, assigned))| {
                            let mut metadata = json!({"tenant_id": t, "resource_type": r});
                            if let Some(owner) = owner {
                                metadata["ilk"] = json!(owner);
                            }
                            if !assigned.is_empty() {
                                metadata["assigned_ilks"] = json!(assigned);
                            }
                            json!({"key": key, "metadata": metadata, "version": 1, "access_count": 0})
                        })
                        .collect();
                    AdminOutcome::Ok(json!({"status": "ok", "secrets": secrets}))
                }
                "vault_assign" => {
                    let key = text("key");
                    let assigned: Vec<String> = params["assigned_ilks"]
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    match state.secrets.get_mut(&key) {
                        Some(secret) => {
                            secret.3 = assigned.clone();
                            AdminOutcome::Ok(
                                json!({"status": "ok", "key": key, "assigned_ilks": assigned}),
                            )
                        }
                        None => refused("KEY_NOT_FOUND"),
                    }
                }
                "kill_node" => {
                    let node_name = text("node_name");
                    let Some(node) = state.nodes.remove(&node_name) else {
                        return refused("NODE_NOT_FOUND");
                    };
                    let marks = state.retire_marks;
                    if marks {
                        state.ilks.remove(&node.ilk_id);
                    }
                    AdminOutcome::Ok(json!({
                        "status": "ok",
                        "node_name": node_name,
                        "ilk_retired": marks,
                        "vault_secrets_kept": true
                    }))
                }
                "delete_ilk" => {
                    let ilk_id = text("ilk_id");
                    match state.ilks.remove(&ilk_id) {
                        Some(_) => {
                            AdminOutcome::Ok(json!({"status": "ok", "already_deleted": false}))
                        }
                        None => refused("ILK_NOT_FOUND"),
                    }
                }
                other => refused(&format!("UNEXPECTED_{other}")),
            }
        }
    }

    #[async_trait]
    impl AdminApi for Fake {
        async fn call(&self, call: &AdminCall) -> AdminOutcome {
            self.answer(call)
        }
    }

    impl World for Fake {
        fn identity(&self) -> Result<Vec<IlkView>, String> {
            Ok(self
                .state
                .lock()
                .expect("fake")
                .ilks
                .values()
                .cloned()
                .collect())
        }
        fn tenant_status(&self, tenant_id: &str) -> Result<Option<String>, String> {
            Ok(self
                .state
                .lock()
                .expect("fake")
                .tenants
                .get(tenant_id)
                .cloned())
        }
        fn capacity(&self) -> Result<Capacity, String> {
            let state = self.state.lock().expect("fake");
            Ok(Capacity {
                workers: state.workers.clone(),
                total_remote_nodes: state.workers.iter().map(|w| w.node_count).sum(),
            })
        }
        fn runtime_current(&self, runtime: &str) -> Result<Option<String>, String> {
            Ok(self
                .state
                .lock()
                .expect("fake")
                .runtimes
                .get(runtime)
                .cloned())
        }
        fn asset_present(&self, hash: &str) -> bool {
            self.state.lock().expect("fake").assets.contains_key(hash)
        }
        fn write_asset(&self, asset: &Value) -> Result<String, String> {
            let prepared = prepare_agent_asset(asset).map_err(|err| err.to_string())?;
            self.state
                .lock()
                .expect("fake")
                .assets
                .insert(prepared.hash.clone(), asset.clone());
            Ok(prepared.hash)
        }
        fn person_document(&self, ilk_id: &str, hash16: &str) -> DocumentRead {
            match self
                .state
                .lock()
                .expect("fake")
                .documents
                .get(&format!("{ilk_id}/{hash16}"))
            {
                Some(document) => DocumentRead::Found(document.clone()),
                None => DocumentRead::NotYet,
            }
        }
        fn now_ms(&self) -> u64 {
            let mut state = self.state.lock().expect("fake");
            state.now_ms += 1;
            state.now_ms
        }
    }
}
