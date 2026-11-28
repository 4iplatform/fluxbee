//! SY.deploy's orders (docs/digital-executive-v1-stage-2.md §2, §3, §5.1, §9; stage 3 §3, §4): the
//! eight `cd_*` actions SY.admin forwards, each checked and answered at once. A write that is taken
//! is recorded and journaled first, answered *accepted*, and run in the background by the runner
//! (`runner.rs`), one deployment at a time, with a lock per grouping. Its catalog is what Archi
//! published (`cd_publish`, `cd_unpublish`): each package's copy, and whether it is offered.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use json_router::agent_assets::sha256_hex;

use json_router::solution_package::{
    agent_providers, catalog_blanks, catalog_parts, check_blanks, compare_versions,
    key_requirements, parse_package, parse_package_value, versions_kept, whatsapp_numbers,
    without_nulls, Offer, PackageKind, ParsedPackage, Reading, GROUPING_ED, GROUPING_TENANT,
    PACKAGE_MAX_BYTES, WORK_ROLE_CONSCIOUS,
};

use crate::desired::{desired_state, record_parts, DesiredState};
use crate::ids;
use crate::steps::MOTHERBEE;
use crate::store::{
    DeploymentRecord, DeploymentState, GroupingRef, RecordNode, Removal, SourceRef, Store,
};
use crate::tree::assemble_tree;
use crate::world::{
    AdminApi, AdminCall, AdminOutcome, World, MAX_NODES_PER_HIVE, MAX_REMOTE_NODES,
};

/// The error codes SY.deploy answers with (docs: the 2D contracts §1); SY.admin maps each to HTTP.
pub const NOT_FOUND: &str = "NOT_FOUND";
pub const REQUEST_KEY_REUSED: &str = "REQUEST_KEY_REUSED";
pub const SOURCE_NOT_FOUND: &str = "SOURCE_NOT_FOUND";
pub const INVALID_REQUEST: &str = "INVALID_REQUEST";
pub const BLANK_INVALID: &str = "BLANK_INVALID";
pub const AI_KEY_MISSING: &str = "AI_KEY_MISSING";
pub const CREDENTIAL_MISSING: &str = "CREDENTIAL_MISSING";
pub const NUMBER_TAKEN: &str = "NUMBER_TAKEN";
pub const HIVE_UNAVAILABLE: &str = "HIVE_UNAVAILABLE";
pub const BUSY: &str = "BUSY";
pub const DEPLOYMENT_NOT_REMOVABLE: &str = "DEPLOYMENT_NOT_REMOVABLE";
/// No package is published yet (stage 3 §3): retryable, as Archi publishes its packages at its
/// start.
pub const CATALOG_NOT_READY: &str = "CATALOG_NOT_READY";
/// A publish's codes (stage 3 §3).
pub const PACKAGE_INVALID: &str = "PACKAGE_INVALID";
pub const PACKAGE_CHANGED: &str = "PACKAGE_CHANGED";
pub const PACKAGE_TOO_LARGE: &str = "PACKAGE_TOO_LARGE";

/// The actions, by kind: SY.admin sends a write as a `command`, a read as a `query`.
pub const COMMANDS: &[&str] = &["cd_deploy", "cd_remove", "cd_publish", "cd_unpublish"];
pub const QUERIES: &[&str] = &["cd_get", "cd_list", "cd_catalog", "cd_tree"];

/// The versions of each package kept (§3.3): the last three, and every one a deployment runs.
pub const SOURCE_VERSIONS_KEPT: usize = 3;

/// A listing's page: 100 deployments without a limit, 200 at most.
const LIST_DEFAULT_LIMIT: usize = 100;
const LIST_MAX_LIMIT: usize = 200;

/// A deployment id's form, as a refusal names it.
const DEPLOYMENT_ID_FORM: &str =
    "deployment_id is a deployment's id: dp, a digit from 2 to 7 and 7 of [a-z2-7]";

/// Whether `id` names a grouping: its ED's ILK, its tenant's id (the tenant's grouping), or a short
/// id of any kind.
fn is_grouping_id(id: &str) -> bool {
    ids::is_ilk_id(id)
        || ids::is_tenant_id(id)
        || ["ed", "tenant", "test"]
            .iter()
            .any(|kind| ids::is_short_id(id, kind))
}

/// How many times an id is minted again when the SHM or a record holds it.
const MINT_TRIES: usize = 8;

/// An error answer: `{"status":"error","error_code","message"}`, and `retryable` for the ones
/// that changed nothing and may pass when asked again.
pub fn error_answer(code: &str, message: impl Into<String>) -> Value {
    let mut answer = json!({"status": "error", "error_code": code, "message": message.into()});
    if matches!(code, BUSY | HIVE_UNAVAILABLE | CATALOG_NOT_READY) {
        answer["retryable"] = json!(true);
    }
    answer
}

/// Who asked: what SY.admin adds to every order (`_caller`), never a caller's own field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub by: String,
    /// The tenant Fluxbee Cloud claims: with one, nothing of another tenant is found.
    pub tenant_id: Option<String>,
}

/// The timings of the runner and of the reads behind an answer.
#[derive(Debug, Clone)]
pub struct Timings {
    /// The waits between a step's tries; one more try than waits.
    pub retry_backoff: Vec<Duration>,
    /// What a listing gives its nodes' status reads in all, within SY.admin's 10 s.
    pub status_budget: Duration,
    /// One status read's wait.
    pub status_call: Duration,
    /// One key read's wait in an order's checks.
    pub check_call: Duration,
}

impl Timings {
    pub fn production() -> Timings {
        Timings {
            retry_backoff: vec![
                Duration::from_secs(3),
                Duration::from_secs(10),
                Duration::from_secs(30),
            ],
            status_budget: Duration::from_secs(5),
            status_call: Duration::from_secs(3),
            check_call: Duration::from_secs(3),
        }
    }
}

/// A run the runner takes, one at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Deploy(String),
    /// A removal: its deployments newest first, and the person it marks when it ends the whole
    /// grouping.
    Remove {
        grouping_key: String,
        deployment_ids: Vec<String>,
        person: Option<String>,
        tenant_id: String,
    },
}

/// A version's publication (stage 3 §3): whether it is offered, and its offer, as the last publish
/// or withdraw left it.
#[derive(Debug, Clone, PartialEq)]
pub struct Publication {
    pub published: bool,
    pub offer: Offer,
    pub by: String,
    pub at_ms: u64,
}

impl Publication {
    fn to_value(&self, kind: PackageKind) -> Value {
        json!({
            "published": self.published,
            "origin": self.offer.origin.as_str(),
            "offered_to": self.offer.offered_to_value(kind),
            "by": self.by,
            "updated_at_ms": self.at_ms,
        })
    }

    fn from_value(kind: PackageKind, value: &Value) -> Result<Publication, String> {
        Ok(Publication {
            published: value["published"].as_bool().unwrap_or(false),
            offer: Offer::parse(kind, &value["origin"], &value["offered_to"])?,
            by: value["by"].as_str().unwrap_or_default().to_string(),
            at_ms: value["updated_at_ms"].as_u64().unwrap_or_default(),
        })
    }
}

/// A version SY.deploy keeps: its package, and its publication; a copy never published (one kept
/// before stage 3) has none, and serves only the deployments that run it.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub parsed: ParsedPackage,
    pub publication: Option<Publication>,
}

impl CatalogEntry {
    /// Its offer, while it is published.
    pub fn offered(&self) -> Option<&Offer> {
        self.publication
            .as_ref()
            .filter(|publication| publication.published)
            .map(|publication| &publication.offer)
    }
}

/// A grouping a deployment goes into that is there already, and its hive.
struct Existing {
    grouping: GroupingRef,
    hive: String,
}

#[derive(Debug, Default)]
pub struct Inner {
    pub records: BTreeMap<String, DeploymentRecord>,
    /// The groupings with a run queued or under way: their lock.
    pub busy: BTreeSet<String>,
    /// The versions SY.deploy keeps, by id and version.
    pub catalog: BTreeMap<(String, String), CatalogEntry>,
}

pub struct Deployer {
    pub admin: Arc<dyn AdminApi>,
    pub world: Arc<dyn World>,
    pub store: Store,
    pub timings: Timings,
    pub inner: Mutex<Inner>,
    jobs: mpsc::UnboundedSender<Job>,
}

type Answer = Result<Value, Value>;

fn text_field(fields: &Map<String, Value>, name: &str) -> Result<Option<String>, Value> {
    match fields.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            Ok((!text.is_empty()).then(|| text.to_string()))
        }
        Some(_) => Err(error_answer(
            INVALID_REQUEST,
            format!("{name} must be a string"),
        )),
    }
}

fn only_fields(fields: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), Value> {
    match fields.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(extra) => Err(error_answer(
            INVALID_REQUEST,
            format!("{what} takes only {allowed:?}, not '{extra}'"),
        )),
        None => Ok(()),
    }
}

/// Splits an order into who asked and its own fields.
fn split_order(payload: Value) -> Result<(Caller, Map<String, Value>), Value> {
    let mut fields = match payload {
        Value::Object(fields) => fields,
        Value::Null => Map::new(),
        _ => return Err(error_answer(INVALID_REQUEST, "an order is a JSON object")),
    };
    let caller = match fields.remove("_caller") {
        None | Some(Value::Null) => Caller {
            by: "SY.admin".to_string(),
            tenant_id: None,
        },
        Some(Value::Object(caller)) => Caller {
            by: caller
                .get("by")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|by| !by.is_empty())
                .unwrap_or("SY.admin")
                .to_string(),
            tenant_id: caller
                .get("tenant_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|tenant| !tenant.is_empty())
                .map(str::to_string),
        },
        Some(_) => return Err(error_answer(INVALID_REQUEST, "_caller is an object")),
    };
    Ok((caller, fields))
}

/// The tenant an order is scoped to: the one Cloud claims, or the one the order names; when both
/// are there they must be one, or nothing is found.
fn scoped_tenant(caller: &Caller, named: Option<String>) -> Result<Option<String>, Value> {
    match (&caller.tenant_id, named) {
        (Some(claimed), Some(named)) if *claimed != named => {
            Err(error_answer(NOT_FOUND, "nothing of that tenant is found"))
        }
        (Some(claimed), _) => Ok(Some(claimed.clone())),
        (None, named) => Ok(named),
    }
}

fn record_view(record: &DeploymentRecord) -> Value {
    let mut view = serde_json::to_value(record).unwrap_or(Value::Null);
    if let Some(fields) = view.as_object_mut() {
        fields.remove("request_hash");
    }
    view
}

fn summary_view(record: &DeploymentRecord) -> Value {
    let mut view = json!({
        "deployment_id": record.deployment_id,
        "grouping": record.grouping,
        "tenant_id": record.tenant_id,
        "source": record.source,
        "state": record.state.as_str(),
        "hive": record.hive,
        "nodes": record.nodes,
        "created_at_ms": record.created_at_ms,
        "updated_at_ms": record.updated_at_ms,
    });
    if let Some(failure) = &record.failure {
        view["failure"] = json!(failure);
    }
    view
}

/// A node's status as an answer shows it: what `get_node_status` says of its lifecycle, its health,
/// its runtime and its extensions (the conscious's `speaks_as`, the IO's `person_ilk`).
fn status_view(outcome: &AdminOutcome) -> Value {
    match outcome {
        AdminOutcome::Ok(payload) => {
            let status = &payload["node_status"];
            let mut view = json!({
                "lifecycle_state": status["lifecycle_state"],
                "health_state": status["health_state"],
                "runtime_version": status["runtime"]["resolved_version"],
            });
            for field in ["extensions", "last_error", "launched_by"] {
                if let Some(value) = status.get(field).filter(|value| !value.is_null()) {
                    view[field] = value.clone();
                }
            }
            view
        }
        AdminOutcome::Refused { code, .. } if code == "NODE_NOT_FOUND" => {
            json!({"lifecycle_state": "NOT_FOUND"})
        }
        AdminOutcome::Refused { code, detail, .. } => {
            json!({"lifecycle_state": null, "error_code": code, "error": detail})
        }
        AdminOutcome::Unanswered(why) => json!({"lifecycle_state": null, "error": why}),
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A secret as `vault_list` shows it: its key and the metadata SY.deploy reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretView {
    pub key: String,
    pub resource_type: String,
    pub owner: Option<String>,
    pub assigned_ilks: Vec<String>,
}

impl SecretView {
    /// A tenant's general secret: no owner and no node assigned, which any node of the tenant
    /// reads.
    pub fn is_general(&self) -> bool {
        self.owner.is_none() && self.assigned_ilks.is_empty()
    }
}

pub fn secrets_of(payload: &Value) -> Vec<SecretView> {
    payload["secrets"]
        .as_array()
        .map(|secrets| {
            secrets
                .iter()
                .filter_map(|secret| {
                    let metadata = &secret["metadata"];
                    Some(SecretView {
                        key: secret["key"].as_str()?.to_string(),
                        resource_type: metadata["resource_type"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        owner: metadata["ilk"]
                            .as_str()
                            .map(str::to_string)
                            .filter(|ilk| !ilk.is_empty()),
                        assigned_ilks: metadata["assigned_ilks"]
                            .as_array()
                            .map(|ilks| {
                                ilks.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `vault_list` of one tenant's keys of one type, the ones that start with `prefix`.
pub fn vault_list_call(
    tenant_id: &str,
    resource_type: &str,
    prefix: Option<&str>,
    timeout: Duration,
) -> AdminCall {
    let mut filter = json!({"tenant_id": tenant_id, "resource_type": resource_type, "limit": 200});
    if let Some(prefix) = prefix {
        filter["prefix"] = json!(prefix);
    }
    AdminCall {
        action: "vault_list".to_string(),
        target: MOTHERBEE.to_string(),
        params: json!({"filter": filter}),
        timeout,
    }
}

impl Deployer {
    /// Opens SY.deploy's state: its records, its catalog (the copies and their publications, the
    /// versions beyond what is kept removed), and the runs to resume: every deployment left
    /// `deploying` or `removing`.
    pub fn open(
        admin: Arc<dyn AdminApi>,
        world: Arc<dyn World>,
        store: Store,
        timings: Timings,
        jobs: mpsc::UnboundedSender<Job>,
    ) -> Result<Arc<Deployer>, String> {
        store
            .ensure_dirs()
            .map_err(|err| format!("SY.deploy's state directory: {err}"))?;
        let (records, unreadable) = store.load_deployments();
        for what in unreadable {
            tracing::error!(record = %what, "a deployment's record does not read; it is left alone");
        }
        let deployer = Arc::new(Deployer {
            admin,
            world,
            store,
            timings,
            inner: Mutex::new(Inner {
                records: records
                    .into_iter()
                    .map(|record| (record.deployment_id.clone(), record))
                    .collect(),
                ..Inner::default()
            }),
            jobs,
        });
        deployer.refresh_catalog()?;
        deployer.resume();
        Ok(deployer)
    }

    /// The catalog: the copies and their publications, the versions kept of each (§3.3), the
    /// copies beyond them removed with their publications.
    pub fn refresh_catalog(&self) -> Result<(), String> {
        let mut known: BTreeMap<(String, String), ParsedPackage> = BTreeMap::new();
        let copies = self.store.source_copies();
        let on_disk: BTreeSet<(String, String)> = copies
            .iter()
            .map(|(id, version, _)| (id.clone(), version.clone()))
            .collect();
        for (id, version, bytes) in copies {
            match std::str::from_utf8(&bytes)
                .map_err(|err| err.to_string())
                .and_then(|raw| parse_package(raw, Reading::KeptCopy))
            {
                Ok(parsed) if parsed.package.id == id && parsed.package.version == version => {
                    known.insert((id, version), parsed);
                }
                Ok(_) => {
                    tracing::error!(package = %id, version = %version, "a package copy names another package; it is left out")
                }
                Err(err) => {
                    tracing::error!(package = %id, version = %version, error = %err, "a package copy does not read; it is left out")
                }
            }
        }
        let mut publications: BTreeMap<(String, String), Value> = self
            .store
            .publications()
            .into_iter()
            .map(|(id, version, state)| ((id, version), state))
            .collect();
        let mut inner = lock(&self.inner);
        let in_use: BTreeSet<(String, String)> = inner
            .records
            .values()
            .filter(|record| record.state != DeploymentState::Removed)
            .map(|record| (record.source.id.clone(), record.source.version.clone()))
            .collect();
        let ids: BTreeSet<String> = known.keys().map(|(id, _)| id.clone()).collect();
        let mut catalog = BTreeMap::new();
        for id in ids {
            let versions: Vec<String> = known
                .keys()
                .filter(|(of, _)| *of == id)
                .map(|(_, version)| version.clone())
                .collect();
            let used: BTreeSet<String> = in_use
                .iter()
                .filter(|(of, _)| *of == id)
                .map(|(_, version)| version.clone())
                .collect();
            let kept = versions_kept(&versions, &used, SOURCE_VERSIONS_KEPT);
            for version in versions {
                let key = (id.clone(), version.clone());
                let Some(parsed) = known.remove(&key) else {
                    continue;
                };
                let state = publications.remove(&key);
                if kept.contains(&version) {
                    let publication = match state
                        .map(|state| Publication::from_value(parsed.package.kind, &state))
                        .transpose()
                    {
                        Ok(publication) => publication,
                        Err(err) => {
                            tracing::error!(package = %id, version = %version, error = %err, "a version's publication does not read: it is not offered");
                            None
                        }
                    };
                    catalog.insert(
                        key,
                        CatalogEntry {
                            parsed,
                            publication,
                        },
                    );
                } else if self.store.remove_source_copy(&id, &version).is_ok() {
                    self.store.remove_publication(&id, &version);
                    tracing::info!(package = %id, version = %version, "a package version beyond the ones kept was removed");
                }
            }
        }
        // A publication whose copy is gone goes too; one whose copy does not read stays with it.
        for (id, version) in publications.into_keys() {
            if !on_disk.contains(&(id.clone(), version.clone())) {
                self.store.remove_publication(&id, &version);
            }
        }
        inner.catalog = catalog;
        Ok(())
    }

    /// Whether SY.deploy takes deployments: it holds a published package (stage 3 §3).
    pub fn catalog_ready(&self) -> bool {
        lock(&self.inner)
            .catalog
            .values()
            .any(|entry| entry.offered().is_some())
    }

    fn resume(&self) {
        let mut jobs = Vec::new();
        {
            let mut inner = lock(&self.inner);
            let mut removals: BTreeMap<String, Vec<DeploymentRecord>> = BTreeMap::new();
            for record in inner.records.values() {
                match record.state {
                    DeploymentState::Deploying => jobs.push((
                        record.grouping_key().to_string(),
                        Job::Deploy(record.deployment_id.clone()),
                    )),
                    DeploymentState::Removing => removals
                        .entry(record.grouping_key().to_string())
                        .or_default()
                        .push(record.clone()),
                    _ => {}
                }
            }
            for (key, mut records) in removals {
                records.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
                let whole = records.iter().any(|record| {
                    record
                        .removal
                        .as_ref()
                        .is_some_and(|removal| removal.whole_grouping)
                });
                let person = records
                    .iter()
                    .find(|record| record.founding && whole)
                    .map(|record| record.grouping.id.clone());
                jobs.push((
                    key.clone(),
                    Job::Remove {
                        grouping_key: key,
                        tenant_id: records[0].tenant_id.clone(),
                        deployment_ids: records
                            .iter()
                            .map(|record| record.deployment_id.clone())
                            .collect(),
                        person,
                    },
                ));
            }
            for (key, _) in &jobs {
                inner.busy.insert(key.clone());
            }
        }
        for (_, job) in jobs {
            tracing::info!(job = ?job, "a run left under way resumes");
            let _ = self.jobs.send(job);
        }
    }

    pub fn record(&self, deployment_id: &str) -> Option<DeploymentRecord> {
        lock(&self.inner).records.get(deployment_id).cloned()
    }

    /// Changes a record and writes it.
    pub fn update_record(
        &self,
        deployment_id: &str,
        change: impl FnOnce(&mut DeploymentRecord),
    ) -> Option<DeploymentRecord> {
        let mut inner = lock(&self.inner);
        let record = inner.records.get_mut(deployment_id)?;
        change(record);
        if let Err(err) = self.store.save_deployment(record) {
            tracing::error!(deployment_id = %deployment_id, error = %err, "a deployment's record could not be written");
        }
        Some(record.clone())
    }

    pub fn journal(&self, deployment_id: &str, mut entry: Value) {
        entry["at_ms"] = json!(self.world.now_ms());
        if let Err(err) = self.store.append_journal(deployment_id, &entry) {
            tracing::error!(deployment_id = %deployment_id, error = %err, "a journal line could not be written");
        }
    }

    pub fn release(&self, grouping_key: &str) {
        lock(&self.inner).busy.remove(grouping_key);
    }

    /// The package a deployment runs, published or not: its copy, pinned by its hash.
    pub fn source_of(&self, record: &DeploymentRecord) -> Result<ParsedPackage, String> {
        let inner = lock(&self.inner);
        let parsed = inner
            .catalog
            .get(&(record.source.id.clone(), record.source.version.clone()))
            .map(|entry| entry.parsed.clone())
            .ok_or_else(|| {
                format!(
                    "SY.deploy holds no copy of the source {} version {}",
                    record.source.id, record.source.version
                )
            })?;
        if parsed.hash != record.source.hash {
            return Err(format!(
                "the copy of the source {} version {} is not the one the deployment pinned",
                record.source.id, record.source.version
            ));
        }
        Ok(parsed)
    }

    pub fn desired_for(&self, record: &DeploymentRecord) -> Result<DesiredState, String> {
        desired_state(&self.source_of(record)?, record)
    }

    /// Serves one order: a write as a `command`, a read as a `query` (the 2D contracts §1, §2).
    pub async fn handle(&self, kind: &str, action: &str, payload: Value) -> Value {
        let answer = match (kind, action) {
            ("command", "cd_deploy") => self.cd_deploy(payload).await,
            ("command", "cd_remove") => self.cd_remove(payload),
            ("command", "cd_publish") => self.cd_publish(payload),
            ("command", "cd_unpublish") => self.cd_unpublish(payload),
            ("query", "cd_get") => self.cd_get(payload).await,
            ("query", "cd_list") => self.cd_list(payload).await,
            ("query", "cd_catalog") => self.cd_catalog(payload),
            ("query", "cd_tree") => self.cd_tree(payload).await,
            (_, action) if COMMANDS.contains(&action) => Err(error_answer(
                INVALID_REQUEST,
                format!("{action} is a command"),
            )),
            (_, action) if QUERIES.contains(&action) => Err(error_answer(
                INVALID_REQUEST,
                format!("{action} is a query"),
            )),
            _ => Err(error_answer(
                INVALID_REQUEST,
                format!("SY.deploy has no action '{action}'"),
            )),
        };
        answer.unwrap_or_else(|refusal| refusal)
    }

    // --- cd_deploy (§2.1, §5.1) -----------------------------------------------------------------

    async fn cd_deploy(&self, payload: Value) -> Answer {
        let (caller, fields) = split_order(payload)?;
        only_fields(
            &fields,
            &[
                "tenant_id",
                "request_key",
                "source",
                "grouping",
                "blanks",
                "hive",
            ],
            "cd_deploy",
        )?;
        // The tenant: a user tenant, active, in the identity SHM.
        let tenant_id = text_field(&fields, "tenant_id")?
            .ok_or_else(|| error_answer(INVALID_REQUEST, "tenant_id is required, tnt:<uuid>"))?;
        if !ids::is_tenant_id(&tenant_id) {
            return Err(error_answer(
                INVALID_REQUEST,
                "tenant_id is a tenant, tnt:<uuid>",
            ));
        }
        scoped_tenant(&caller, Some(tenant_id.clone()))?;
        if tenant_id == fluxbee_sdk::identity::DEFAULT_ROOT_TENANT_ID {
            return Err(error_answer(
                INVALID_REQUEST,
                "a deployment is a user tenant's, never the root tenant's",
            ));
        }
        match self.world.tenant_status(&tenant_id) {
            Ok(Some(status)) if status == "active" => {}
            Ok(Some(status)) => {
                return Err(error_answer(
                    INVALID_REQUEST,
                    format!("the tenant is {status}, not active"),
                ))
            }
            Ok(None) => return Err(error_answer(NOT_FOUND, "no such tenant")),
            Err(err) => {
                return Err(error_answer(
                    BUSY,
                    format!("the identity SHM cannot be read now: {err}"),
                ))
            }
        }
        let request_key = text_field(&fields, "request_key")?
            .ok_or_else(|| error_answer(INVALID_REQUEST, "request_key is required"))?;
        if !ids::is_request_key(&request_key) {
            return Err(error_answer(
                INVALID_REQUEST,
                "request_key is 1 to 64 of [A-Za-z0-9._:-]",
            ));
        }
        let source_ask = fields
            .get("source")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                error_answer(INVALID_REQUEST, "source is required: {\"id\", \"version\"}")
            })?;
        only_fields(source_ask, &["id", "version"], "source")?;
        let source_id = text_field(source_ask, "id")?
            .ok_or_else(|| error_answer(INVALID_REQUEST, "source.id is required"))?;
        let source_version = text_field(source_ask, "version")?;
        let grouping_ask = fields
            .get("grouping")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                error_answer(INVALID_REQUEST, "grouping is required: {\"kind\", \"id\"}")
            })?;
        only_fields(grouping_ask, &["kind", "id"], "grouping")?;
        let grouping_kind = text_field(grouping_ask, "kind")?
            .ok_or_else(|| error_answer(INVALID_REQUEST, "grouping.kind is required"))?;
        let grouping_id = text_field(grouping_ask, "id")?;
        let blanks = match fields.get("blanks") {
            // A null is an absent field, at every depth: SY.admin drops the top-level ones.
            Some(Value::Object(blanks)) => without_nulls(&Value::Object(blanks.clone())),
            _ => {
                return Err(error_answer(
                    INVALID_REQUEST,
                    "blanks is required, an object",
                ))
            }
        };
        let hive_ask = text_field(&fields, "hive")?;
        // The order's content as read, so a field sent as null and one left out are the same.
        let request_hash = sha256_hex(&json_router::solution_package::canonical_bytes(&json!({
            "tenant_id": tenant_id,
            "source": {"id": source_id, "version": source_version},
            "grouping": {"kind": grouping_kind, "id": grouping_id},
            "blanks": blanks,
            "hive": hive_ask,
        })));

        // The request key: new in this tenant, or the same content as before.
        let existing = lock(&self.inner)
            .records
            .values()
            .find(|record| record.tenant_id == tenant_id && record.request_key == request_key)
            .cloned();
        if let Some(existing) = existing {
            if existing.request_hash != request_hash {
                return Err(error_answer(
                    REQUEST_KEY_REUSED,
                    "the request_key names another order of this tenant",
                ));
            }
            return self.replay(existing, &caller);
        }

        // The catalog: what Archi published, or the copies kept before a restart.
        if !self.catalog_ready() {
            return Err(error_answer(
                CATALOG_NOT_READY,
                "SY.deploy holds no published package yet: Archi publishes its packages at its start",
            ));
        }
        // The package: a published version, offered to the tenant.
        let (parsed, offer) = {
            let inner = lock(&self.inner);
            let mut versions: Vec<&CatalogEntry> = inner
                .catalog
                .iter()
                .filter(|((id, _), entry)| {
                    *id == source_id
                        && entry
                            .offered()
                            .is_some_and(|offer| offer.tenants.admits(&tenant_id))
                })
                .map(|(_, entry)| entry)
                .collect();
            versions.sort_by(|a, b| {
                compare_versions(&b.parsed.package.version, &a.parsed.package.version)
            });
            match &source_version {
                Some(version) => versions
                    .into_iter()
                    .find(|entry| entry.parsed.package.version == *version),
                None => versions.into_iter().next(),
            }
            .and_then(|entry| {
                entry
                    .offered()
                    .cloned()
                    .map(|offer| (entry.parsed.clone(), offer))
            })
        }
        .ok_or_else(|| {
            error_answer(
                SOURCE_NOT_FOUND,
                "SY.deploy offers the tenant no such package, or no such version of it",
            )
        })?;
        let package = &parsed.package;

        // The grouping: of the kind the package deploys into. An executive makes a new ED's; a
        // capability of one ED goes into that ED's; infrastructure and a capability of every ED
        // into the tenant's, made by the first deployment that needs it.
        let into = package.deploys_into();
        if grouping_kind != into {
            return Err(error_answer(
                INVALID_REQUEST,
                format!(
                    "the package {} ({}) deploys into a grouping of kind {into}",
                    package.id,
                    package.kind.as_str()
                ),
            ));
        }
        let mut hive_of_grouping = None;
        let grouping = if package.kind == PackageKind::Executive {
            if grouping_id.is_some() {
                return Err(error_answer(
                    INVALID_REQUEST,
                    format!(
                        "the package {} founds a new grouping: name no grouping id",
                        package.id
                    ),
                ));
            }
            None
        } else {
            let existing = if into == GROUPING_ED {
                let id = grouping_id.as_ref().ok_or_else(|| {
                    error_answer(
                        INVALID_REQUEST,
                        "a capability of one ED names its ED: grouping.id, its ILK or its short id",
                    )
                })?;
                Some(self.ed_grouping(&tenant_id, id, &offer)?)
            } else {
                self.tenant_grouping(&tenant_id, grouping_id.as_deref())?
            };
            match existing {
                Some(existing) => {
                    if hive_ask.as_ref().is_some_and(|hive| *hive != existing.hive) {
                        return Err(error_answer(
                            INVALID_REQUEST,
                            "a grouping's deployments go to its hive",
                        ));
                    }
                    if let Some(running) = lock(&self.inner).records.values().find(|record| {
                        record.grouping_key() == existing.grouping.short_id
                            && record.state != DeploymentState::Removed
                            && record.source.id == package.id
                    }) {
                        return Err(error_answer(
                            INVALID_REQUEST,
                            format!(
                                "the grouping runs the package {} already, in the deployment {}",
                                package.id, running.deployment_id
                            ),
                        ));
                    }
                    hive_of_grouping = Some(existing.hive);
                    Some(existing.grouping)
                }
                None => None,
            }
        };

        // The blanks: exactly the package's, each by its type.
        check_blanks(package, &blanks)
            .map_err(|err| error_answer(BLANK_INVALID, err.to_string()))?;
        for number in whatsapp_numbers(package, &blanks) {
            let held = {
                let inner = lock(&self.inner);
                inner
                    .records
                    .values()
                    .filter(|record| record.state != DeploymentState::Removed)
                    .any(|record| record_numbers(record, &inner.catalog).contains(&number))
            };
            if held {
                return Err(error_answer(
                    NUMBER_TAKEN,
                    "another deployment holds that WhatsApp number",
                ));
            }
        }

        // The hive: a live worker with room under the router's 1024.
        let hive = match hive_of_grouping {
            Some(hive) => {
                self.room_on(&hive, package.nodes.len())?;
                hive
            }
            None => self.pick_hive(hive_ask.as_deref(), package.nodes.len())?,
        };
        // The runtimes: each pinned to its current version (§3.2); a WF's is SY.wf-rules'.
        let mut runtime_versions = BTreeMap::new();
        for node in package.nodes.iter().filter(|node| !node.is_wf()) {
            match self.world.runtime_current(&node.runtime) {
                Ok(Some(version)) => {
                    runtime_versions.insert(node.role.clone(), version);
                }
                Ok(None) => {
                    return Err(error_answer(
                        HIVE_UNAVAILABLE,
                        format!("the runtime {} has no current version", node.runtime),
                    ))
                }
                Err(err) => {
                    return Err(error_answer(
                        HIVE_UNAVAILABLE,
                        format!("the runtimes manifest cannot be read: {err}"),
                    ))
                }
            }
        }

        // The keys: each agent's provider key, and each credential the blanks name.
        self.check_keys(&tenant_id, &parsed, &blanks).await?;

        // The ids: minted, and held by no SHM entry and no record.
        let (deployment_id, grouping, nodes) =
            self.mint_ids(&parsed, &hive, grouping, &tenant_id, &runtime_versions)?;
        let now = self.world.now_ms();
        let mut record = DeploymentRecord {
            deployment_id: deployment_id.clone(),
            grouping,
            tenant_id: tenant_id.clone(),
            request_key,
            request_hash,
            source: SourceRef {
                id: package.id.clone(),
                version: package.version.clone(),
                hash: parsed.hash.clone(),
            },
            founding: package.founds.is_some(),
            blanks,
            hive,
            nodes,
            assets: BTreeMap::new(),
            vault: Vec::new(),
            state: DeploymentState::Deploying,
            failure: None,
            removal: None,
            requested_by: caller.by.clone(),
            created_at_ms: now,
            updated_at_ms: now,
            history: Vec::new(),
        };
        let desired =
            desired_state(&parsed, &record).map_err(|err| error_answer(BLANK_INVALID, err))?;
        let (assets, vault) = record_parts(&desired);
        record.assets = assets;
        record.vault = vault;
        record.note(now, "accepted", &caller.by, None);
        self.store
            .save_deployment(&record)
            .map_err(|err| error_answer(BUSY, err))?;
        self.journal(
            &deployment_id,
            json!({
                "event": "accepted",
                "by": caller.by,
                "source": record.source,
                "grouping": record.grouping,
                "hive": record.hive,
                "nodes": record.nodes,
            }),
        );
        let answer = accepted_answer(&record);
        {
            let mut inner = lock(&self.inner);
            inner.busy.insert(record.grouping_key().to_string());
            inner.records.insert(deployment_id.clone(), record);
        }
        let _ = self.jobs.send(Job::Deploy(deployment_id));
        Ok(answer)
    }

    /// An ED's grouping a capability goes into: its founding deployment, active, of the tenant;
    /// the package offered to that ED; and its conscious, which the door starts from.
    fn ed_grouping(&self, tenant_id: &str, id: &str, offer: &Offer) -> Result<Existing, Value> {
        let inner = lock(&self.inner);
        let founding = inner
            .records
            .values()
            .find(|record| {
                record.founding
                    && record.tenant_id == tenant_id
                    && record.state != DeploymentState::Removed
                    && record.grouping.kind == GROUPING_ED
                    && (record.grouping.id == id || record.grouping.short_id == id)
            })
            .cloned()
            .ok_or_else(|| error_answer(NOT_FOUND, "no such ED in the tenant"))?;
        if inner.busy.contains(founding.grouping_key()) {
            return Err(error_answer(BUSY, "the grouping has a run under way"));
        }
        drop(inner);
        if founding.state != DeploymentState::Active {
            return Err(error_answer(
                INVALID_REQUEST,
                format!(
                    "the ED's founding deployment is {}, not active",
                    founding.state.as_str()
                ),
            ));
        }
        if !offer.eds.admits(&founding.grouping.short_id) {
            return Err(error_answer(
                SOURCE_NOT_FOUND,
                "SY.deploy offers that ED no such package",
            ));
        }
        let conscious = founding
            .nodes
            .iter()
            .find(|node| node.work_role == WORK_ROLE_CONSCIOUS)
            .map(|node| node.node_name.clone())
            .ok_or_else(|| error_answer(INVALID_REQUEST, "the ED has no conscious"))?;
        Ok(Existing {
            grouping: GroupingRef {
                conscious: Some(conscious),
                ..founding.grouping.clone()
            },
            hive: founding.hive,
        })
    }

    /// The tenant's grouping (stage 3 §4): the one its deployments that are not removed name, or
    /// none yet, when the first deployment that needs it makes it. An id names it: the tenant's
    /// id or its short id.
    fn tenant_grouping(
        &self,
        tenant_id: &str,
        id: Option<&str>,
    ) -> Result<Option<Existing>, Value> {
        let inner = lock(&self.inner);
        let live = inner
            .records
            .values()
            .find(|record| {
                record.tenant_id == tenant_id
                    && record.grouping.kind == GROUPING_TENANT
                    && record.state != DeploymentState::Removed
            })
            .cloned();
        match (live, id) {
            (Some(record), Some(id)) if record.grouping.id != id && record.grouping.short_id != id => {
                Err(error_answer(NOT_FOUND, "no such grouping in the tenant"))
            }
            (None, Some(_)) => Err(error_answer(
                NOT_FOUND,
                "the tenant has no grouping of its own yet: name no grouping id, and the first deployment makes it",
            )),
            (Some(record), _) => {
                if inner.busy.contains(record.grouping_key()) {
                    return Err(error_answer(BUSY, "the grouping has a run under way"));
                }
                Ok(Some(Existing {
                    grouping: GroupingRef {
                        conscious: None,
                        ..record.grouping.clone()
                    },
                    hive: record.hive,
                }))
            }
            (None, None) => Ok(None),
        }
    }

    /// The same order again (§2.1, §2.2): the same deployment; a failed one resumes where it
    /// stopped.
    fn replay(&self, existing: DeploymentRecord, caller: &Caller) -> Answer {
        match existing.state {
            DeploymentState::Deploying => Ok(accepted_answer(&existing)),
            DeploymentState::Failed => {
                if existing
                    .failure
                    .as_ref()
                    .is_some_and(|failure| failure.operation == "remove")
                {
                    return Err(error_answer(
                        INVALID_REQUEST,
                        "the deployment's removal failed: ask for its removal again",
                    ));
                }
                if lock(&self.inner).busy.contains(existing.grouping_key()) {
                    return Err(error_answer(BUSY, "the grouping has a run under way"));
                }
                let now = self.world.now_ms();
                let by = caller.by.clone();
                let resumed = self
                    .update_record(&existing.deployment_id, |record| {
                        record.state = DeploymentState::Deploying;
                        let step = record.failure.take().map(|failure| failure.step);
                        record.note(now, "resumed", &by, step);
                    })
                    .ok_or_else(|| error_answer(NOT_FOUND, "no such deployment"))?;
                self.journal(
                    &resumed.deployment_id,
                    json!({"event": "resumed", "by": caller.by}),
                );
                lock(&self.inner)
                    .busy
                    .insert(resumed.grouping_key().to_string());
                let _ = self.jobs.send(Job::Deploy(resumed.deployment_id.clone()));
                Ok(accepted_answer(&resumed))
            }
            _ => {
                let mut answer = accepted_answer(&existing);
                answer["accepted"] = json!(false);
                Ok(answer)
            }
        }
    }

    /// The nodes a hive's deployments under way will add, not counted by its router yet.
    fn pending_nodes(&self, hive: Option<&str>) -> u32 {
        lock(&self.inner)
            .records
            .values()
            .filter(|record| record.state == DeploymentState::Deploying)
            .filter(|record| hive.is_none_or(|hive| record.hive == hive))
            .map(|record| record.nodes.len() as u32)
            .sum()
    }

    fn room_on(&self, hive: &str, nodes: usize) -> Result<(), Value> {
        let capacity = self.world.capacity().map_err(|err| {
            error_answer(
                HIVE_UNAVAILABLE,
                format!("the hives cannot be read now: {err}"),
            )
        })?;
        let nodes = nodes as u32;
        if capacity.total_remote_nodes + self.pending_nodes(None) + nodes > MAX_REMOTE_NODES {
            return Err(error_answer(
                HIVE_UNAVAILABLE,
                format!("the installation holds its {MAX_REMOTE_NODES} remote nodes"),
            ));
        }
        let worker = capacity
            .workers
            .iter()
            .find(|worker| worker.hive_id == hive && worker.alive)
            .ok_or_else(|| {
                error_answer(HIVE_UNAVAILABLE, format!("{hive} is not a live worker"))
            })?;
        if worker.node_count + self.pending_nodes(Some(hive)) + nodes > MAX_NODES_PER_HIVE {
            return Err(error_answer(
                HIVE_UNAVAILABLE,
                format!("{hive} has no room for {nodes} more nodes under its router's {MAX_NODES_PER_HIVE}"),
            ));
        }
        Ok(())
    }

    /// The hive of a new grouping: the one asked, if it is a live worker with room, else the
    /// live worker with the most room.
    fn pick_hive(&self, asked: Option<&str>, nodes: usize) -> Result<String, Value> {
        if let Some(hive) = asked {
            self.room_on(hive, nodes)?;
            return Ok(hive.to_string());
        }
        let capacity = self.world.capacity().map_err(|err| {
            error_answer(
                HIVE_UNAVAILABLE,
                format!("the hives cannot be read now: {err}"),
            )
        })?;
        let mut best: Option<(u32, String)> = None;
        for worker in capacity.workers.iter().filter(|worker| worker.alive) {
            let used = worker.node_count + self.pending_nodes(Some(&worker.hive_id));
            let room = MAX_NODES_PER_HIVE.saturating_sub(used);
            if best.as_ref().is_none_or(|(most, hive)| {
                room > *most || (room == *most && worker.hive_id < *hive)
            }) {
                best = Some((room, worker.hive_id.clone()));
            }
        }
        let (_, hive) = best.ok_or_else(|| error_answer(HIVE_UNAVAILABLE, "no worker is alive"))?;
        self.room_on(&hive, nodes)?;
        Ok(hive)
    }

    /// §5.1's key checks, through SY.admin's `vault_list` (metadata only): for each agent, a
    /// general key of its provider in the tenant (`AI_KEY_MISSING`); each credential the blanks
    /// name, of its resource type, in the tenant (`CREDENTIAL_MISSING`).
    async fn check_keys(
        &self,
        tenant_id: &str,
        parsed: &ParsedPackage,
        blanks: &Value,
    ) -> Result<(), Value> {
        let providers = agent_providers(&parsed.package);
        let credentials = key_requirements(&parsed.package, blanks);
        let mut reads = Vec::new();
        for provider in &providers {
            reads.push(vault_list_call(
                tenant_id,
                provider,
                None,
                self.timings.check_call,
            ));
        }
        for credential in &credentials {
            reads.push(vault_list_call(
                tenant_id,
                &credential.resource_type,
                Some(&credential.key),
                self.timings.check_call,
            ));
        }
        let answers =
            futures::future::join_all(reads.iter().map(|call| self.admin.call(call))).await;
        let mut answers = answers.into_iter();
        for provider in &providers {
            let secrets = match answers.next() {
                Some(AdminOutcome::Ok(payload)) => secrets_of(&payload),
                Some(other) => return Err(key_read_refusal(&other)),
                None => Vec::new(),
            };
            if !secrets
                .iter()
                .any(|secret| secret.resource_type == *provider && secret.is_general())
            {
                return Err(error_answer(
                    AI_KEY_MISSING,
                    format!("the tenant holds no general key of resource_type {provider} for its agents"),
                ));
            }
        }
        for credential in &credentials {
            let secrets = match answers.next() {
                Some(AdminOutcome::Ok(payload)) => secrets_of(&payload),
                Some(other) => return Err(key_read_refusal(&other)),
                None => Vec::new(),
            };
            if !secrets.iter().any(|secret| {
                secret.key == credential.key && secret.resource_type == credential.resource_type
            }) {
                return Err(error_answer(
                    CREDENTIAL_MISSING,
                    format!(
                        "the tenant holds no key of resource_type {} by the name the blanks give",
                        credential.resource_type
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The ids of a new deployment: its id, its grouping's (for a new ED's: its short id and its
    /// person's ILK; for a new tenant's: its short id), and each node's name and ILK (a WF's is
    /// the orchestrator's), none held by an SHM entry or a record. In a grouping that is there
    /// already the names are fixed: one that is taken refuses the deployment.
    fn mint_ids(
        &self,
        parsed: &ParsedPackage,
        hive: &str,
        grouping: Option<GroupingRef>,
        tenant_id: &str,
        runtime_versions: &BTreeMap<String, String>,
    ) -> Result<(String, GroupingRef, Vec<RecordNode>), Value> {
        let ilks = self.world.identity().map_err(|err| {
            error_answer(BUSY, format!("the identity SHM cannot be read now: {err}"))
        })?;
        let held_ilks: BTreeSet<String> = ilks
            .iter()
            .map(|ilk| ilk.ilk_id.to_ascii_lowercase())
            .collect();
        let held_names: BTreeSet<String> = ilks
            .iter()
            .filter_map(|ilk| ilk.handler_node.clone())
            .collect();
        let inner = lock(&self.inner);
        let recorded_ids: BTreeSet<String> = inner.records.keys().cloned().collect();
        let recorded_names: BTreeSet<String> = inner
            .records
            .values()
            .flat_map(|record| record.nodes.iter().map(|node| node.node_name.clone()))
            .collect();
        let recorded_ilks: BTreeSet<String> = inner
            .records
            .values()
            .flat_map(|record| {
                record
                    .nodes
                    .iter()
                    .map(|node| node.ilk_id.clone())
                    .chain(std::iter::once(record.grouping.id.clone()))
            })
            .collect();
        let recorded_shorts: BTreeSet<String> = inner
            .records
            .values()
            .map(|record| record.grouping.short_id.clone())
            .collect();
        drop(inner);
        let ilk_free = |ilk: &str| !held_ilks.contains(ilk) && !recorded_ilks.contains(ilk);
        let package = &parsed.package;
        let kind = package.deploys_into();
        let fixed = grouping.is_some();
        let mut rng = rand::thread_rng();
        let deployment_id = (0..MINT_TRIES)
            .map(|_| ids::mint_deployment_id(&mut rng))
            .find(|id| !recorded_ids.contains(id))
            .ok_or_else(|| error_answer(BUSY, "no free deployment id was minted"))?;
        for _ in 0..MINT_TRIES {
            let grouping = match &grouping {
                Some(grouping) => grouping.clone(),
                None => {
                    let short_id = ids::mint_short_id(&mut rng, kind).ok_or_else(|| {
                        error_answer(INVALID_REQUEST, "the grouping kind has no short id")
                    })?;
                    if recorded_shorts.contains(&short_id) {
                        continue;
                    }
                    // An ED's grouping is its person; a tenant's is the tenant.
                    let id = if kind == GROUPING_ED {
                        let person = ids::mint_ilk_id();
                        if !ilk_free(&person) {
                            continue;
                        }
                        person
                    } else {
                        tenant_id.to_string()
                    };
                    GroupingRef {
                        kind: kind.to_string(),
                        id,
                        short_id,
                        conscious: None,
                    }
                }
            };
            let nodes: Vec<RecordNode> = package
                .nodes
                .iter()
                .map(|node| {
                    let node_name = ids::node_name(
                        &node.kind,
                        node.middle.as_deref(),
                        &grouping.short_id,
                        hive,
                    );
                    match ids::workflow_name(&node_name).map(str::to_string) {
                        Some(workflow) if node.is_wf() => RecordNode {
                            role: node.role.clone(),
                            node_name,
                            ilk_id: String::new(),
                            runtime: format!("wf.{workflow}"),
                            runtime_version: String::new(),
                            work_role: node.work_role.clone(),
                        },
                        _ => RecordNode {
                            role: node.role.clone(),
                            node_name,
                            ilk_id: ids::mint_ilk_id(),
                            runtime: node.runtime.clone(),
                            runtime_version: runtime_versions
                                .get(&node.role)
                                .cloned()
                                .unwrap_or_default(),
                            work_role: node.work_role.clone(),
                        },
                    }
                })
                .collect();
            let taken = nodes.iter().find(|node| {
                held_names.contains(&node.node_name) || recorded_names.contains(&node.node_name)
            });
            if let Some(taken) = taken {
                if fixed {
                    return Err(error_answer(
                        INVALID_REQUEST,
                        format!(
                            "the node name {} is taken in the grouping: by a node of another deployment, or by one a removed deployment left marked",
                            taken.node_name
                        ),
                    ));
                }
                continue;
            }
            if nodes
                .iter()
                .all(|node| node.ilk_id.is_empty() || ilk_free(&node.ilk_id))
            {
                return Ok((deployment_id, grouping, nodes));
            }
        }
        Err(error_answer(BUSY, "no free names were minted: ask again"))
    }

    // --- cd_remove (§9) ------------------------------------------------------------------------

    fn cd_remove(&self, payload: Value) -> Answer {
        let (caller, fields) = split_order(payload)?;
        only_fields(
            &fields,
            &["deployment_id", "grouping_id", "tenant_id"],
            "cd_remove",
        )?;
        let tenant = scoped_tenant(&caller, text_field(&fields, "tenant_id")?)?;
        let deployment_id = text_field(&fields, "deployment_id")?;
        let grouping_id = text_field(&fields, "grouping_id")?;
        if deployment_id
            .as_deref()
            .is_some_and(|id| !ids::is_deployment_id(id))
        {
            return Err(error_answer(INVALID_REQUEST, DEPLOYMENT_ID_FORM));
        }
        if grouping_id.as_deref().is_some_and(|id| !is_grouping_id(id)) {
            return Err(error_answer(
                INVALID_REQUEST,
                "grouping_id is a grouping's id (its ED's ILK, ilk:<uuid>, or its tenant's, tnt:<uuid>) or its short id",
            ));
        }
        let in_scope = |record: &DeploymentRecord| {
            tenant
                .as_ref()
                .is_none_or(|tenant| record.tenant_id == *tenant)
        };
        let mut inner = lock(&self.inner);
        let (named, grouping_key) = match (&deployment_id, &grouping_id) {
            (Some(id), None) => {
                let record = inner
                    .records
                    .get(id)
                    .filter(|record| in_scope(record))
                    .ok_or_else(|| error_answer(NOT_FOUND, "no such deployment"))?;
                (Some(record.clone()), record.grouping_key().to_string())
            }
            (None, Some(id)) => {
                // A tenant's id names every grouping the tenant had: the one not removed first.
                let record = inner
                    .records
                    .values()
                    .filter(|record| in_scope(record))
                    .filter(|record| record.grouping.id == *id || record.grouping.short_id == *id)
                    .max_by_key(|record| {
                        (
                            record.state != DeploymentState::Removed,
                            record.created_at_ms,
                        )
                    })
                    .ok_or_else(|| error_answer(NOT_FOUND, "no such grouping"))?;
                (None, record.grouping_key().to_string())
            }
            _ => {
                return Err(error_answer(
                    INVALID_REQUEST,
                    "cd_remove takes exactly one of deployment_id and grouping_id",
                ))
            }
        };
        let mut live: Vec<DeploymentRecord> = inner
            .records
            .values()
            .filter(|record| {
                record.grouping_key() == grouping_key && record.state != DeploymentState::Removed
            })
            .cloned()
            .collect();
        live.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
        if inner.busy.contains(&grouping_key)
            || live
                .iter()
                .any(|record| record.state == DeploymentState::Deploying)
        {
            return Err(error_answer(BUSY, "the grouping has a run under way"));
        }
        let (targets, whole) = match &named {
            Some(record) if record.state == DeploymentState::Removed => {
                return Ok(json!({
                    "status": "ok",
                    "accepted": false,
                    "deployment_id": record.deployment_id,
                    "grouping": record.grouping,
                    "state": "removed",
                }))
            }
            Some(record) if record.founding => {
                if live
                    .iter()
                    .any(|other| other.deployment_id != record.deployment_id)
                {
                    return Err(error_answer(
                        DEPLOYMENT_NOT_REMOVABLE,
                        "the grouping's founding deployment goes only with the whole grouping, while it has others",
                    ));
                }
                (vec![record.clone()], true)
            }
            Some(record) => (vec![record.clone()], false),
            None => (live.clone(), true),
        };
        let Some(first) = targets.first().cloned() else {
            let record = inner
                .records
                .values()
                .find(|record| record.grouping_key() == grouping_key)
                .cloned();
            return Ok(json!({
                "status": "ok",
                "accepted": false,
                "grouping": record.map(|record| record.grouping),
                "state": "removed",
            }));
        };
        let now = self.world.now_ms();
        let removal = Removal {
            whole_grouping: whole,
            by: caller.by.clone(),
            at_ms: now,
        };
        for target in &targets {
            if let Some(record) = inner.records.get_mut(&target.deployment_id) {
                record.state = DeploymentState::Removing;
                record.failure = None;
                record.removal = Some(removal.clone());
                record.note(now, "removing", &caller.by, None);
                if let Err(err) = self.store.save_deployment(record) {
                    return Err(error_answer(BUSY, err));
                }
            }
        }
        inner.busy.insert(grouping_key.clone());
        drop(inner);
        let ids: Vec<String> = targets
            .iter()
            .map(|record| record.deployment_id.clone())
            .collect();
        for id in &ids {
            self.journal(
                id,
                json!({"event": "removing", "by": caller.by, "whole_grouping": whole}),
            );
        }
        let person = (whole && targets.iter().any(|record| record.founding))
            .then(|| first.grouping.id.clone());
        let _ = self.jobs.send(Job::Remove {
            grouping_key,
            deployment_ids: ids.clone(),
            person,
            tenant_id: first.tenant_id.clone(),
        });
        Ok(json!({
            "status": "ok",
            "accepted": true,
            "deployment_id": named.as_ref().map(|record| record.deployment_id.clone()).unwrap_or_else(|| first.deployment_id.clone()),
            "deployment_ids": ids,
            "grouping": first.grouping,
            "state": "removing",
        }))
    }

    // --- the reads ------------------------------------------------------------------------------

    /// Each node's status, read through `get_node_status` within the reads' budget: what does not
    /// come in time is named so.
    pub async fn node_statuses(&self, nodes: Vec<(String, String)>) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        let deadline = tokio::time::Instant::now() + self.timings.status_budget;
        let mut reads: FuturesUnordered<_> = nodes
            .iter()
            .map(|(hive, node_name)| {
                let call = AdminCall {
                    action: "get_node_status".to_string(),
                    target: hive.clone(),
                    params: json!({"node_name": node_name}),
                    timeout: self.timings.status_call,
                };
                let node_name = node_name.clone();
                async move { (node_name, self.admin.call(&call).await) }
            })
            .collect();
        loop {
            match tokio::time::timeout_at(deadline, reads.next()).await {
                Ok(Some((node_name, outcome))) => {
                    out.insert(node_name, status_view(&outcome));
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        for (_, node_name) in nodes {
            out.entry(node_name).or_insert_with(
                || json!({"lifecycle_state": null, "error": "not read within the answer's time"}),
            );
        }
        out
    }

    fn with_statuses(view: &mut Value, statuses: &BTreeMap<String, Value>) {
        if let Some(nodes) = view["nodes"].as_array_mut() {
            for node in nodes {
                if let Some(status) = node["node_name"]
                    .as_str()
                    .and_then(|name| statuses.get(name))
                {
                    node["status"] = status.clone();
                }
            }
        }
    }

    /// A deployment's steps as its journal's last run shows them: when each started, how long it
    /// took, and how it ended.
    pub fn progress(&self, deployment_id: &str) -> Value {
        let journal = self.store.read_journal(deployment_id);
        // A run that continues another (a read again after a WF's compile) is part of its try.
        let start = journal
            .iter()
            .rposition(|entry| entry["event"] == "run" && entry["continues"] != json!(true))
            .unwrap_or(0);
        let steps: Vec<Value> = journal[start..]
            .iter()
            .filter(|entry| entry["event"] == "step" && entry["phase"] != "start")
            .map(|entry| {
                json!({
                    "step": entry["step"],
                    "outcome": entry["phase"],
                    "at_ms": entry["at_ms"],
                    "duration_ms": entry["duration_ms"],
                })
            })
            .collect();
        json!(steps)
    }

    async fn cd_get(&self, payload: Value) -> Answer {
        let (caller, fields) = split_order(payload)?;
        only_fields(&fields, &["deployment_id", "tenant_id"], "cd_get")?;
        let tenant = scoped_tenant(&caller, text_field(&fields, "tenant_id")?)?;
        let deployment_id = text_field(&fields, "deployment_id")?
            .ok_or_else(|| error_answer(INVALID_REQUEST, "deployment_id is required"))?;
        if !ids::is_deployment_id(&deployment_id) {
            return Err(error_answer(INVALID_REQUEST, DEPLOYMENT_ID_FORM));
        }
        let record = self
            .record(&deployment_id)
            .filter(|record| {
                tenant
                    .as_ref()
                    .is_none_or(|tenant| record.tenant_id == *tenant)
            })
            .ok_or_else(|| error_answer(NOT_FOUND, "no such deployment"))?;
        let mut view = record_view(&record);
        if record.state != DeploymentState::Removed {
            let statuses = self
                .node_statuses(
                    record
                        .nodes
                        .iter()
                        .map(|node| (record.hive.clone(), node.node_name.clone()))
                        .collect(),
                )
                .await;
            Self::with_statuses(&mut view, &statuses);
        }
        view["progress"] = self.progress(&record.deployment_id);
        Ok(json!({"status": "ok", "deployment": view}))
    }

    async fn cd_list(&self, payload: Value) -> Answer {
        let (caller, fields) = split_order(payload)?;
        only_fields(
            &fields,
            &[
                "tenant_id",
                "grouping_kind",
                "grouping_id",
                "limit",
                "cursor",
            ],
            "cd_list",
        )?;
        let tenant = match scoped_tenant(&caller, text_field(&fields, "tenant_id")?) {
            Ok(tenant) => tenant,
            // Another tenant than the one claimed lists nothing.
            Err(_) => return Ok(json!({"status": "ok", "deployments": [], "count": 0})),
        };
        let kind = text_field(&fields, "grouping_kind")?;
        let grouping = text_field(&fields, "grouping_id")?;
        let limit = match fields.get("limit") {
            None | Some(Value::Null) => LIST_DEFAULT_LIMIT,
            Some(Value::Number(n)) => n.as_u64().map(|n| n as usize).unwrap_or(LIST_DEFAULT_LIMIT),
            Some(Value::String(text)) => text
                .trim()
                .parse::<usize>()
                .map_err(|_| error_answer(INVALID_REQUEST, "limit is a whole number"))?,
            Some(_) => return Err(error_answer(INVALID_REQUEST, "limit is a whole number")),
        }
        .clamp(1, LIST_MAX_LIMIT);
        let cursor = text_field(&fields, "cursor")?;
        let mut records: Vec<DeploymentRecord> = lock(&self.inner)
            .records
            .values()
            .filter(|record| {
                tenant
                    .as_ref()
                    .is_none_or(|tenant| record.tenant_id == *tenant)
            })
            .filter(|record| {
                kind.as_ref()
                    .is_none_or(|kind| record.grouping.kind == *kind)
            })
            .filter(|record| {
                grouping
                    .as_ref()
                    .is_none_or(|id| record.grouping.id == *id || record.grouping.short_id == *id)
            })
            .cloned()
            .collect();
        records.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| a.deployment_id.cmp(&b.deployment_id))
        });
        if let Some(cursor) = &cursor {
            let at = records
                .iter()
                .position(|record| record.deployment_id == *cursor)
                .ok_or_else(|| {
                    error_answer(
                        INVALID_REQUEST,
                        "the cursor is not a next_cursor of cd_list",
                    )
                })?;
            records.drain(..=at);
        }
        let more = records.len() > limit;
        records.truncate(limit);
        let nodes: Vec<(String, String)> = records
            .iter()
            .filter(|record| record.state != DeploymentState::Removed)
            .flat_map(|record| {
                record
                    .nodes
                    .iter()
                    .map(|node| (record.hive.clone(), node.node_name.clone()))
            })
            .collect();
        let statuses = self.node_statuses(nodes).await;
        let deployments: Vec<Value> = records
            .iter()
            .map(|record| {
                let mut view = summary_view(record);
                Self::with_statuses(&mut view, &statuses);
                view
            })
            .collect();
        let mut answer =
            json!({"status": "ok", "count": deployments.len(), "deployments": deployments});
        if more {
            if let Some(last) = records.last() {
                answer["next_cursor"] = json!(last.deployment_id);
            }
        }
        Ok(answer)
    }

    /// The published packages the caller's tenant is offered (stage 3 §4): system and catalog ones
    /// to every tenant, a custom one to its tenant; by kind; with a grouping (an ED), only the
    /// capabilities its `eds` offers that ED.
    fn cd_catalog(&self, payload: Value) -> Answer {
        let (caller, fields) = split_order(payload)?;
        only_fields(&fields, &["tenant_id", "kind", "grouping_id"], "cd_catalog")?;
        let tenant = match scoped_tenant(&caller, text_field(&fields, "tenant_id")?) {
            Ok(tenant) => tenant,
            // Another tenant than the one claimed is offered nothing.
            Err(_) => return Ok(json!({"status": "ok", "sources": []})),
        };
        let kind = text_field(&fields, "kind")?
            .map(|kind| {
                PackageKind::parse(&kind, true).ok_or_else(|| {
                    error_answer(
                        INVALID_REQUEST,
                        "kind is one of executive, capability and infrastructure",
                    )
                })
            })
            .transpose()?;
        let inner = lock(&self.inner);
        let ed = match text_field(&fields, "grouping_id")? {
            Some(id) => Some(
                inner
                    .records
                    .values()
                    .find(|record| {
                        record.founding
                            && record.state != DeploymentState::Removed
                            && record.grouping.kind == GROUPING_ED
                            && tenant
                                .as_ref()
                                .is_none_or(|tenant| record.tenant_id == *tenant)
                            && (record.grouping.id == id || record.grouping.short_id == id)
                    })
                    .map(|record| record.grouping.short_id.clone())
                    .ok_or_else(|| error_answer(NOT_FOUND, "no such ED in the tenant"))?,
            ),
            None => None,
        };
        let mut by_id: BTreeMap<String, Vec<&CatalogEntry>> = BTreeMap::new();
        for ((id, _), entry) in &inner.catalog {
            let Some(offer) = entry.offered() else {
                continue;
            };
            let package = &entry.parsed.package;
            let offered = tenant
                .as_ref()
                .is_none_or(|tenant| offer.tenants.admits(tenant))
                && kind.is_none_or(|kind| package.kind == kind)
                && ed.as_ref().is_none_or(|ed| {
                    package.kind != PackageKind::Capability
                        || package.deploys_into() != GROUPING_ED
                        || offer.eds.admits(ed)
                });
            if offered {
                by_id.entry(id.clone()).or_default().push(entry);
            }
        }
        let sources: Vec<Value> = by_id
            .into_iter()
            .map(|(id, mut versions)| {
                versions.sort_by(|a, b| {
                    compare_versions(&b.parsed.package.version, &a.parsed.package.version)
                });
                let newest = &versions[0].parsed.package;
                let offer = versions[0].offered();
                let in_use = |version: &str| {
                    inner
                        .records
                        .values()
                        .filter(|record| {
                            record.state != DeploymentState::Removed
                                && record.source.id == id
                                && record.source.version == version
                        })
                        .count()
                };
                json!({
                    "id": id,
                    "kind": newest.kind.as_str(),
                    "origin": offer.map(|offer| offer.origin.as_str()),
                    "offered_to": offer.map(|offer| offer.offered_to_value(newest.kind)),
                    "deploys_into": newest.deploys_into(),
                    "title": newest.title,
                    "description": newest.description,
                    "founds": newest.founds.is_some(),
                    "newest": newest.version,
                    "blanks": catalog_blanks(newest),
                    "versions": versions.iter().map(|entry| json!({
                        "version": entry.parsed.package.version,
                        "hash": entry.parsed.hash,
                        "blanks": catalog_blanks(&entry.parsed.package),
                        "parts": catalog_parts(&entry.parsed.package),
                        "deployments": in_use(&entry.parsed.package.version),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({"status": "ok", "sources": sources}))
    }

    // --- cd_publish, cd_unpublish (stage 3 §3) -------------------------------------------------

    /// A publish's caller: Archi or the operator, through SY.admin; never a tenant's claim.
    fn publisher(caller: &Caller) -> Result<(), Value> {
        match caller.tenant_id {
            Some(_) => Err(error_answer(
                INVALID_REQUEST,
                "a package is published by Archi or the operator, never for a claimed tenant",
            )),
            None => Ok(()),
        }
    }

    /// Takes a released package: checks it, keeps its copy (written once), and offers it. The same
    /// version again is `already`; the same version with another content, `PACKAGE_CHANGED`. A
    /// copy kept before stage 3 (kind `ed`, no offer) takes the offer of the publish of its
    /// content, its bytes and the hash its deployments pin unchanged.
    fn cd_publish(&self, payload: Value) -> Answer {
        let (caller, fields) = split_order(payload)?;
        only_fields(&fields, &["package"], "cd_publish")?;
        Self::publisher(&caller)?;
        let package = match fields.get("package") {
            Some(package @ Value::Object(_)) => package,
            _ => {
                return Err(error_answer(
                    INVALID_REQUEST,
                    "package is required, the package's JSON object",
                ))
            }
        };
        let size = serde_json::to_vec(package)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        if size > PACKAGE_MAX_BYTES {
            return Err(error_answer(
                PACKAGE_TOO_LARGE,
                format!("the package is {size} bytes: a package is at most {PACKAGE_MAX_BYTES}"),
            ));
        }
        let parsed = parse_package_value(package, Reading::Publish)
            .map_err(|why| error_answer(PACKAGE_INVALID, why))?;
        let id = parsed.package.id.clone();
        let version = parsed.package.version.clone();
        let changed = || {
            error_answer(
                PACKAGE_CHANGED,
                format!(
                    "SY.deploy keeps {id} version {version} with another content: a change is a new version"
                ),
            )
        };
        let existing = lock(&self.inner)
            .catalog
            .get(&(id.clone(), version.clone()))
            .map(|entry| entry.parsed.clone());
        let (kept, hash) = match existing {
            Some(copy) if copy.hash == parsed.hash => ("already", copy.hash),
            Some(copy)
                if copy.package.offer.is_none() && copy.content_hash == parsed.content_hash =>
            {
                ("already", copy.hash)
            }
            Some(_) => return Err(changed()),
            None => match self
                .store
                .keep_source_copy(&id, &version, &parsed.canonical)
            {
                Ok(true) => ("new", parsed.hash.clone()),
                Ok(false) => ("already", parsed.hash.clone()),
                Err(_) => return Err(changed()),
            },
        };
        let offer = parsed
            .package
            .offer
            .clone()
            .ok_or_else(|| error_answer(PACKAGE_INVALID, "a package names its offer"))?;
        let publication = Publication {
            published: true,
            offer,
            by: caller.by.clone(),
            at_ms: self.world.now_ms(),
        };
        self.store
            .save_publication(&id, &version, &publication.to_value(parsed.package.kind))
            .map_err(|err| error_answer(BUSY, err))?;
        self.refresh_catalog()
            .map_err(|err| error_answer(BUSY, err))?;
        tracing::info!(package = %id, version = %version, kept, by = %caller.by, "a package was published");
        Ok(json!({
            "status": "ok",
            "kept": kept,
            "id": id,
            "version": version,
            "hash": hash,
        }))
    }

    /// Withdraws a version: it is no longer offered; the deployments that run it keep it.
    fn cd_unpublish(&self, payload: Value) -> Answer {
        let (caller, fields) = split_order(payload)?;
        only_fields(&fields, &["id", "version"], "cd_unpublish")?;
        Self::publisher(&caller)?;
        let id = text_field(&fields, "id")?
            .ok_or_else(|| error_answer(INVALID_REQUEST, "id is required"))?;
        let version = text_field(&fields, "version")?
            .ok_or_else(|| error_answer(INVALID_REQUEST, "version is required"))?;
        let (entry, deployments) = {
            let inner = lock(&self.inner);
            let entry = inner
                .catalog
                .get(&(id.clone(), version.clone()))
                .cloned()
                .ok_or_else(|| {
                    error_answer(SOURCE_NOT_FOUND, "SY.deploy keeps no such package version")
                })?;
            let deployments = inner
                .records
                .values()
                .filter(|record| {
                    record.state != DeploymentState::Removed
                        && record.source.id == id
                        && record.source.version == version
                })
                .count();
            (entry, deployments)
        };
        if let Some(mut publication) = entry.publication {
            if publication.published {
                publication.published = false;
                publication.by = caller.by.clone();
                publication.at_ms = self.world.now_ms();
                self.store
                    .save_publication(
                        &id,
                        &version,
                        &publication.to_value(entry.parsed.package.kind),
                    )
                    .map_err(|err| error_answer(BUSY, err))?;
                self.refresh_catalog()
                    .map_err(|err| error_answer(BUSY, err))?;
                tracing::info!(package = %id, version = %version, by = %caller.by, deployments, "a package was withdrawn");
            }
        }
        Ok(json!({
            "status": "ok",
            "id": id,
            "version": version,
            "published": false,
            "deployments": deployments,
        }))
    }

    async fn cd_tree(&self, payload: Value) -> Answer {
        let (caller, fields) = split_order(payload)?;
        only_fields(&fields, &["tenant_id"], "cd_tree")?;
        let tenant = scoped_tenant(&caller, text_field(&fields, "tenant_id")?)?
            .ok_or_else(|| error_answer(INVALID_REQUEST, "tenant_id is required"))?;
        let ilks = self.world.identity().map_err(|err| {
            error_answer(BUSY, format!("the identity SHM cannot be read now: {err}"))
        })?;
        let records: Vec<DeploymentRecord> = lock(&self.inner).records.values().cloned().collect();
        let mut tree = assemble_tree(&tenant, &ilks, &records, |ilk_id, hash16| {
            self.world.person_document(ilk_id, hash16)
        });
        let nodes: Vec<(String, String)> = tree["eds"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|ed| ed["parts"].as_array().cloned().unwrap_or_default())
            .filter_map(|part| {
                let name = part["node_name"].as_str()?.to_string();
                let hive = name.rsplit_once('@')?.1.to_string();
                Some((hive, name))
            })
            .collect();
        let statuses = self.node_statuses(nodes).await;
        if let Some(eds) = tree["eds"].as_array_mut() {
            for ed in eds {
                if let Some(parts) = ed["parts"].as_array_mut() {
                    for part in parts {
                        if let Some(status) = part["node_name"]
                            .as_str()
                            .and_then(|name| statuses.get(name))
                        {
                            part["status"] = status.clone();
                        }
                    }
                }
            }
        }
        tree["status"] = json!("ok");
        Ok(tree)
    }
}

/// The WhatsApp numbers a deployment holds: its blanks' of its package's number type, or, for a
/// package the catalog no longer holds, any `phone_number_id` among its blanks.
fn record_numbers(
    record: &DeploymentRecord,
    catalog: &BTreeMap<(String, String), CatalogEntry>,
) -> Vec<String> {
    match catalog.get(&(record.source.id.clone(), record.source.version.clone())) {
        Some(entry) => whatsapp_numbers(&entry.parsed.package, &record.blanks),
        None => record
            .blanks
            .as_object()
            .into_iter()
            .flat_map(|blanks| blanks.values())
            .filter_map(|blank| blank.get("phone_number_id"))
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    }
}

fn key_read_refusal(outcome: &AdminOutcome) -> Value {
    let why = match outcome {
        AdminOutcome::Refused { code, detail, .. } => format!("{code}: {detail}"),
        AdminOutcome::Unanswered(why) => why.clone(),
        AdminOutcome::Ok(_) => String::new(),
    };
    error_answer(
        BUSY,
        format!("the tenant's keys could not be read through SY.admin's vault_list: {why}"),
    )
}

/// The answer to a write that is taken: SY.admin turns `accepted` into HTTP 202.
pub fn accepted_answer(record: &DeploymentRecord) -> Value {
    json!({
        "status": "ok",
        "accepted": true,
        "deployment_id": record.deployment_id,
        "grouping": record.grouping,
        "source": record.source,
        "state": record.state.as_str(),
    })
}
