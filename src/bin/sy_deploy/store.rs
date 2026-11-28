//! SY.deploy's state, in plain files (docs/digital-executive-v1-stage-2.md §3):
//! - `deployments/<deployment_id>.json`, each deployment's record, written to a temporary file and
//!   renamed;
//! - `journal/<deployment_id>.jsonl`, appended before and after each step;
//! - `sources/<source_id>/<version>.json`, the packages' immutable copies (§3.3);
//! - `published/<source_id>/<version>.json`, whether each kept version is published, and its offer
//!   (stage 3 §3: a publish writes it, a withdraw marks it not published).
//!
//! No secret in any: a credential is its key's name.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A grouping as a deployment names it: its kind (`ed` or `tenant`), its id (an ED's ILK, or the
/// tenant's id) and its short id; for a capability of one ED, the node name of the ED's conscious,
/// which its door starts from (`grouping.conscious`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupingRef {
    pub kind: String,
    pub id: String,
    pub short_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conscious: Option<String>,
}

/// The source a deployment runs, pinned with its content's hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceRef {
    pub id: String,
    pub version: String,
    pub hash: String,
}

/// A node a deployment made, with its ILK and its pinned runtime version (§3.2). A WF node's ILK
/// is the orchestrator's: empty until SY.deploy reads it from the node's status, after its
/// `wf_rules_compile_apply`; its runtime is `wf.<workflow>`, which SY.wf-rules versions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordNode {
    pub role: String,
    pub node_name: String,
    pub ilk_id: String,
    pub runtime: String,
    pub runtime_version: String,
    pub work_role: String,
}

impl RecordNode {
    pub fn is_wf(&self) -> bool {
        self.node_name.starts_with("WF.")
    }
}

/// The assets of one agent's definition, by hash.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordAssets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handbooks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
}

/// A key a deployment assigned, by name, and to which node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordVault {
    pub key: String,
    pub resource_type: String,
    pub assigned_to: String,
}

/// A deployment's state (§2.2): `deploying` → `active` or `failed`; `active` or `failed` →
/// `removing` → `removed`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentState {
    Deploying,
    Active,
    Failed,
    Removing,
    Removed,
}

impl DeploymentState {
    pub fn as_str(self) -> &'static str {
        match self {
            DeploymentState::Deploying => "deploying",
            DeploymentState::Active => "active",
            DeploymentState::Failed => "failed",
            DeploymentState::Removing => "removing",
            DeploymentState::Removed => "removed",
        }
    }
}

/// Why a deployment is `failed`: the operation, the step, its code and its detail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Failure {
    /// `deploy` or `remove`.
    pub operation: String,
    pub step: String,
    pub error_code: String,
    pub error_detail: String,
    pub at_ms: u64,
}

/// A removal under way: whether it ends the whole grouping (and marks its person).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Removal {
    pub whole_grouping: bool,
    pub by: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub at_ms: u64,
    pub what: String,
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A deployment's record (§3.1): what SY.deploy did, and with what. The ED's names and channels
/// are in its document; its parts, in their `works_for`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeploymentRecord {
    pub deployment_id: String,
    pub grouping: GroupingRef,
    pub tenant_id: String,
    pub request_key: String,
    /// The hash of the order's content, for the request key's check (§2.1).
    pub request_hash: String,
    pub source: SourceRef,
    /// Whether this deployment founded its grouping (made its person).
    pub founding: bool,
    pub blanks: Value,
    pub hive: String,
    pub nodes: Vec<RecordNode>,
    /// The assets of each agent, by its role.
    #[serde(default)]
    pub assets: BTreeMap<String, RecordAssets>,
    #[serde(default)]
    pub vault: Vec<RecordVault>,
    pub state: DeploymentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal: Option<Removal>,
    pub requested_by: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}

impl DeploymentRecord {
    /// The key of its grouping's lock: its short id, unique for every grouping.
    pub fn grouping_key(&self) -> &str {
        &self.grouping.short_id
    }

    pub fn node(&self, role: &str) -> Option<&RecordNode> {
        self.nodes.iter().find(|node| node.role == role)
    }

    pub fn note(&mut self, at_ms: u64, what: &str, by: &str, detail: Option<String>) {
        self.updated_at_ms = at_ms;
        self.history.push(HistoryEntry {
            at_ms,
            what: what.to_string(),
            by: by.to_string(),
            detail,
        });
    }
}

/// SY.deploy's files under its state directory, `/var/lib/fluxbee/state/sy-deploy`.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

/// Writes `bytes` to `path` whole: to a temporary file beside it, synced, then renamed.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp"));
    let written = fs::File::create(&tmp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
        && !name.starts_with('.')
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn deployments_dir(&self) -> PathBuf {
        self.root.join("deployments")
    }

    fn journal_dir(&self) -> PathBuf {
        self.root.join("journal")
    }

    fn sources_dir(&self) -> PathBuf {
        self.root.join("sources")
    }

    fn published_dir(&self) -> PathBuf {
        self.root.join("published")
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        fs::create_dir_all(self.deployments_dir())?;
        fs::create_dir_all(self.journal_dir())?;
        fs::create_dir_all(self.sources_dir())?;
        fs::create_dir_all(self.published_dir())
    }

    /// Every record on disk; one that does not read is named in the second list and left alone.
    pub fn load_deployments(&self) -> (Vec<DeploymentRecord>, Vec<String>) {
        let mut records = Vec::new();
        let mut unreadable = Vec::new();
        let Ok(entries) = fs::read_dir(self.deployments_dir()) else {
            return (records, unreadable);
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().and_then(|ext| ext.to_str()) == Some("json")
                    && !path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with('.'))
            })
            .collect();
        paths.sort();
        for path in paths {
            match fs::read(&path)
                .map_err(|err| err.to_string())
                .and_then(|raw| {
                    serde_json::from_slice::<DeploymentRecord>(&raw).map_err(|err| err.to_string())
                }) {
                Ok(record) => records.push(record),
                Err(err) => unreadable.push(format!("{}: {err}", path.display())),
            }
        }
        (records, unreadable)
    }

    pub fn save_deployment(&self, record: &DeploymentRecord) -> Result<(), String> {
        if !safe_name(&record.deployment_id) {
            return Err("a deployment id names no file".to_string());
        }
        let bytes = serde_json::to_vec_pretty(record).map_err(|err| err.to_string())?;
        write_whole(
            &self
                .deployments_dir()
                .join(format!("{}.json", record.deployment_id)),
            &bytes,
        )
        .map_err(|err| format!("the deployment's record could not be written: {err}"))
    }

    /// Appends one line to a deployment's journal.
    pub fn append_journal(&self, deployment_id: &str, entry: &Value) -> Result<(), String> {
        if !safe_name(deployment_id) {
            return Err("a deployment id names no file".to_string());
        }
        fs::create_dir_all(self.journal_dir()).map_err(|err| err.to_string())?;
        let mut line = serde_json::to_vec(entry).map_err(|err| err.to_string())?;
        line.push(b'\n');
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.journal_dir().join(format!("{deployment_id}.jsonl")))
            .and_then(|mut file| file.write_all(&line).and_then(|()| file.sync_data()))
            .map_err(|err| format!("the deployment's journal could not be written: {err}"))
    }

    /// A deployment's journal, its lines in order; a line that does not read is left out.
    pub fn read_journal(&self, deployment_id: &str) -> Vec<Value> {
        if !safe_name(deployment_id) {
            return Vec::new();
        }
        fs::read_to_string(self.journal_dir().join(format!("{deployment_id}.jsonl")))
            .map(|raw| {
                raw.lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn source_path(&self, id: &str, version: &str) -> Option<PathBuf> {
        (safe_name(id) && safe_name(version))
            .then(|| self.sources_dir().join(id).join(format!("{version}.json")))
    }

    /// Keeps a source's copy: written once, never changed. A copy already there with other bytes
    /// is an error, and stays as it is.
    pub fn keep_source_copy(
        &self,
        id: &str,
        version: &str,
        canonical: &[u8],
    ) -> Result<bool, String> {
        let path = self
            .source_path(id, version)
            .ok_or("a source's id or version names no file")?;
        match fs::read(&path) {
            Ok(existing) if existing == canonical => Ok(false),
            Ok(_) => Err(format!(
                "the copy of {id} version {version} differs from the source of the same version: it is kept as it is"
            )),
            Err(_) => write_whole(&path, canonical)
                .map(|()| true)
                .map_err(|err| format!("the source's copy could not be written: {err}")),
        }
    }

    /// Every source copy: its id, its version and its bytes.
    pub fn source_copies(&self) -> Vec<(String, String, Vec<u8>)> {
        versioned_files(&self.sources_dir())
    }

    pub fn remove_source_copy(&self, id: &str, version: &str) -> Result<(), String> {
        let path = self
            .source_path(id, version)
            .ok_or("a source's id or version names no file")?;
        fs::remove_file(path).map_err(|err| err.to_string())
    }

    fn published_path(&self, id: &str, version: &str) -> Option<PathBuf> {
        (safe_name(id) && safe_name(version)).then(|| {
            self.published_dir()
                .join(id)
                .join(format!("{version}.json"))
        })
    }

    /// Writes whether a kept version is published, and its offer.
    pub fn save_publication(&self, id: &str, version: &str, state: &Value) -> Result<(), String> {
        let path = self
            .published_path(id, version)
            .ok_or("a package's id or version names no file")?;
        let bytes = serde_json::to_vec_pretty(state).map_err(|err| err.to_string())?;
        write_whole(&path, &bytes)
            .map_err(|err| format!("the package's publication could not be written: {err}"))
    }

    /// Every version's publication: its id, its version and its state; one that does not read is
    /// left out.
    pub fn publications(&self) -> Vec<(String, String, Value)> {
        versioned_files(&self.published_dir())
            .into_iter()
            .filter_map(|(id, version, bytes)| {
                serde_json::from_slice(&bytes)
                    .ok()
                    .map(|state| (id, version, state))
            })
            .collect()
    }

    pub fn remove_publication(&self, id: &str, version: &str) {
        if let Some(path) = self.published_path(id, version) {
            let _ = fs::remove_file(path);
        }
    }
}

/// Every `<dir>/<id>/<version>.json`: its id, its version and its bytes.
fn versioned_files(dir: &Path) -> Vec<(String, String, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(ids) = fs::read_dir(dir) else {
        return out;
    };
    for id_entry in ids.flatten() {
        let id = id_entry.file_name().to_string_lossy().into_owned();
        if !safe_name(&id) || !id_entry.path().is_dir() {
            continue;
        }
        let Ok(versions) = fs::read_dir(id_entry.path()) else {
            continue;
        };
        for version_entry in versions.flatten() {
            let file = version_entry.file_name().to_string_lossy().into_owned();
            let Some(version) = file.strip_suffix(".json") else {
                continue;
            };
            if !safe_name(version) {
                continue;
            }
            if let Ok(bytes) = fs::read(version_entry.path()) {
                out.push((id.clone(), version.to_string(), bytes));
            }
        }
    }
    out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    out
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;

    pub fn temp_store() -> Store {
        let root =
            std::env::temp_dir().join(format!("fb-sy-deploy-{}", uuid::Uuid::new_v4().simple()));
        let store = Store::new(root);
        store.ensure_dirs().expect("dirs");
        store
    }

    #[test]
    fn the_journal_is_appended_and_read_in_order() {
        let store = temp_store();
        for n in 0..3 {
            store
                .append_journal("dp2abcdefg", &json!({"n": n}))
                .expect("appended");
        }
        let lines = store.read_journal("dp2abcdefg");
        assert_eq!(lines, [json!({"n": 0}), json!({"n": 1}), json!({"n": 2})]);
        assert!(store.read_journal("../etc").is_empty());
        assert!(store.append_journal("../x", &json!({})).is_err());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn a_source_copy_is_immutable() {
        let store = temp_store();
        assert_eq!(store.keep_source_copy("plain-ed", "1", b"{}"), Ok(true));
        assert_eq!(store.keep_source_copy("plain-ed", "1", b"{}"), Ok(false));
        assert!(store
            .keep_source_copy("plain-ed", "1", b"{\"x\":1}")
            .is_err());
        assert_eq!(
            store.source_copies(),
            [("plain-ed".to_string(), "1".to_string(), b"{}".to_vec())]
        );
        store.remove_source_copy("plain-ed", "1").expect("removed");
        assert!(store.source_copies().is_empty());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn a_publication_is_written_again_and_removed() {
        let store = temp_store();
        store
            .save_publication("billing", "1", &json!({"published": true}))
            .expect("saved");
        store
            .save_publication("billing", "1", &json!({"published": false}))
            .expect("saved again");
        assert_eq!(
            store.publications(),
            [(
                "billing".to_string(),
                "1".to_string(),
                json!({"published": false})
            )]
        );
        assert!(store.save_publication("../x", "1", &json!({})).is_err());
        store.remove_publication("billing", "1");
        assert!(store.publications().is_empty());
        let _ = fs::remove_dir_all(store.root());
    }
}
