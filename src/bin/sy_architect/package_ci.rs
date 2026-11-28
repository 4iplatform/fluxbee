//! Archi's CI of deployable packages (docs/digital-executive-v1-stage-3.md §2, §3, §11): the
//! packages SY.deploy deploys, in its format, of three kinds (`executive`, `capability`,
//! `infrastructure`).
//!
//! - **Two stores.** The packages Fluxbee ships, installed read-only by the .deb at
//!   `/usr/share/fluxbee/solutions/<id>/<version>.json` (`system`): Archi reads them and never
//!   writes, edits or removes one. Archi's own released packages, at
//!   `<state dir>/packages/<id>/<version>.json` (`own`), each written once.
//! - **The publish.** Every package goes to SY.deploy with `cd_publish`, through SY.admin, one by
//!   one: at Archi's start, once SY.admin answers, and then any package new or changed since. A
//!   package SY.deploy refuses is not sent again until its file changes. SY.deploy checks the
//!   package; Archi checks only what it needs to keep the package (its place, its size, its
//!   origin).
//! - **A release** (`release_package`, from the chat): staged for the operator's CONFIRM, then
//!   written once to the own store and published at once; a package SY.deploy refuses is removed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::future::Future;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use json_router::agent_assets::sha256_hex;
use json_router::mesh_reply::MAX_FRAME_SIZE;
use serde::Serialize;
use serde_json::{json, Value};

/// Where the .deb installs the packages Fluxbee ships (contracts §2), read-only for Archi.
pub const SYSTEM_PACKAGES_DIR: &str = "/usr/share/fluxbee/solutions";

/// Archi's own released packages, under its state dir (`/var/lib/fluxbee/state/packages`).
pub const OWN_PACKAGES_SUBDIR: &str = "packages";

/// What a `cd_publish` message carries beside its package: the admin command's envelope, the
/// `_caller` SY.admin stamps and the router's reserve.
const PUBLISH_ENVELOPE_RESERVE_BYTES: usize = 8 * 1024;

/// The most bytes a package may take, serialized compact: a message is at most 128 KiB (the
/// frame), and a package larger is `PACKAGE_TOO_LARGE` (contracts §3). Archi never sends one: the
/// SDK would refuse the frame, and the publish would never be answered.
pub const PACKAGE_MAX_BYTES: usize = MAX_FRAME_SIZE - PUBLISH_ENVELOPE_RESERVE_BYTES;

/// The most bytes Archi reads of a package's file.
const PACKAGE_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// The kinds a release takes (contracts §1). `ed`, a read alias for the copies already on disk,
/// is never released.
pub const RELEASE_KINDS: &[&str] = &["executive", "capability", "infrastructure"];

/// The origins a release takes: `system` packages ship in the .deb, never from Archi.
pub const RELEASE_ORIGINS: &[&str] = &["custom", "catalog"];

/// The error code of a package too large to send (contracts §3).
pub const PACKAGE_TOO_LARGE: &str = "PACKAGE_TOO_LARGE";

/// SY.admin's answer when SY.deploy does not answer, or cannot be reached: retryable.
const DEPLOY_UNAVAILABLE: &str = "DEPLOY_UNAVAILABLE";

/// Which store a package lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageStore {
    /// Shipped with Fluxbee, read-only.
    System,
    /// Released by Archi, written once.
    Own,
}

/// A package as Archi read it from its file.
#[derive(Debug, Clone)]
pub struct CiPackage {
    pub store: PackageStore,
    pub path: PathBuf,
    /// Its id and version as the file names them, or as its place does when it names none.
    pub id: String,
    pub version: String,
    /// The SHA-256 of the file's bytes: a change of the file is a new publish.
    pub file_sha256: String,
    /// The package, when it reads as one.
    pub content: Option<Value>,
    /// Why Archi cannot publish it as it is, before SY.deploy reads it.
    pub error: Option<String>,
}

type PackageKey = (PackageStore, String, String);

impl CiPackage {
    fn key(&self) -> PackageKey {
        (self.store, self.id.clone(), self.version.clone())
    }

    fn text(&self, field: &str) -> Option<&str> {
        self.content.as_ref()?.get(field)?.as_str()
    }

    /// The package as the listing shows it: what it is and where it lives, never its content.
    pub fn listing(&self, publish: Option<&PublishRecord>) -> Value {
        let mut entry = json!({
            "id": self.id,
            "version": self.version,
            "kind": self.text("kind"),
            "origin": self.text("origin"),
            "title": self.text("title"),
            "store": self.store,
            "path": self.path.display().to_string(),
        });
        if let Some(offered_to) = self.content.as_ref().and_then(|c| c.get("offered_to")) {
            entry["offered_to"] = offered_to.clone();
        }
        if let Some(error) = &self.error {
            entry["error"] = json!(error);
        }
        if let Some(publish) = publish {
            entry["publish"] = json!(publish);
        }
        entry
    }
}

/// An id or a version, as SY.deploy reads them: 1 to 64 of `[a-z0-9._-]`, never `.` or `..`, so
/// each is a plain name in the store.
pub fn is_package_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value != "."
        && value != ".."
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.' | b'_')
        })
}

/// The bytes a package takes serialized compact, as it goes in a `cd_publish`.
fn compact_len(package: &Value) -> usize {
    serde_json::to_vec(package)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

fn too_large(bytes: usize) -> String {
    format!(
        "{PACKAGE_TOO_LARGE}: the package takes {bytes} bytes, more than the {PACKAGE_MAX_BYTES} \
         a publish carries (a message is at most {MAX_FRAME_SIZE})"
    )
}

/// Reads one package's file at `<store dir>/<dir_id>/<stem>.json` and checks what Archi needs to
/// keep and send it: a JSON object whose `id` and `version` are its place's, its origin its
/// store's (`system` for the shipped ones, `custom` or `catalog` for Archi's own) and its size one
/// a publish carries. SY.deploy checks the rest.
fn read_package(store: PackageStore, dir_id: &str, stem: &str, path: &Path) -> CiPackage {
    let mut package = CiPackage {
        store,
        path: path.to_path_buf(),
        id: dir_id.to_string(),
        version: stem.to_string(),
        file_sha256: String::new(),
        content: None,
        error: None,
    };
    let len = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if len > PACKAGE_FILE_MAX_BYTES {
        package.file_sha256 = format!("len:{len}");
        package.error = Some(too_large(len as usize));
        return package;
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            package.file_sha256 = "unreadable".to_string();
            package.error = Some(format!("the file cannot be read: {err}"));
            return package;
        }
    };
    package.file_sha256 = sha256_hex(&bytes);
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(err) => {
            package.error = Some(format!("the file is not JSON: {err}"));
            return package;
        }
    };
    if !value.is_object() {
        package.error = Some("a package is a JSON object".to_string());
        return package;
    }
    package.content = Some(value);
    package.error = package_place_error(&package);
    package
}

/// Why a package read from its store cannot be published as it is (`read_package`).
fn package_place_error(package: &CiPackage) -> Option<String> {
    let content = package.content.as_ref()?;
    let named = |field: &str| content.get(field).and_then(Value::as_str);
    if named("id") != Some(package.id.as_str())
        || named("version") != Some(package.version.as_str())
    {
        return Some(format!(
            "its id and version are not its place's: a package lives at <id>/<version>.json, \
             this one at {}/{}.json",
            package.id, package.version
        ));
    }
    if !is_package_token(&package.id) || !is_package_token(&package.version) {
        return Some("a package's id and version are 1 to 64 of [a-z0-9._-]".to_string());
    }
    let origin = named("origin").unwrap_or_default();
    match package.store {
        PackageStore::System if origin != "system" => {
            return Some(format!(
                "a package Fluxbee ships is of origin system, not '{origin}'"
            ))
        }
        PackageStore::Own if !RELEASE_ORIGINS.contains(&origin) => {
            return Some(format!(
                "a package Archi released is of origin custom or catalog, not '{origin}': \
                 system packages ship in the .deb"
            ))
        }
        _ => {}
    }
    let bytes = compact_len(content);
    (bytes > PACKAGE_MAX_BYTES).then(|| too_large(bytes))
}

/// Every package of a store dir, each `<id>/<version>.json`, by id and version. A hidden file or
/// dir, a file that is not `.json` and a dir that cannot be read are not packages.
fn read_store(store: PackageStore, dir: &Path) -> Vec<CiPackage> {
    let visible = |name: &str| !name.is_empty() && !name.starts_with('.');
    let mut packages = Vec::new();
    let Ok(ids) = fs::read_dir(dir) else {
        return packages;
    };
    let mut id_dirs = ids
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .filter(|(name, _)| visible(name))
        .collect::<Vec<_>>();
    id_dirs.sort();
    for (id, id_dir) in id_dirs {
        let Ok(files) = fs::read_dir(&id_dir) else {
            continue;
        };
        let mut versions = files
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let stem = name.strip_suffix(".json")?.to_string();
                visible(&stem).then(|| (stem, entry.path()))
            })
            .collect::<Vec<_>>();
        versions.sort();
        for (version, path) in versions {
            packages.push(read_package(store, &id, &version, &path));
        }
    }
    packages
}

/// What became of a package's publish this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishOutcome {
    /// SY.deploy kept a new copy.
    New,
    /// SY.deploy had it already.
    Already,
    /// Refused, by SY.deploy or before it: not sent again until its file changes.
    Refused,
    /// Not answered yet: sent again.
    Pending,
}

/// A package's last publish this run.
#[derive(Debug, Clone, Serialize)]
pub struct PublishRecord {
    pub outcome: PublishOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at_ms: u64,
    /// The file it was read from, by its bytes.
    #[serde(skip)]
    file_sha256: String,
    /// A release whose publish got no answer: removed when SY.deploy refuses it.
    #[serde(skip)]
    released: bool,
}

/// SY.admin's answer to a `cd_publish`, as Archi takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishAnswer {
    /// Kept, `new` or `already`.
    Kept(PublishOutcome),
    /// Refused: its code and why.
    Refused(String),
    /// No verdict: no answer, or SY.deploy did not answer SY.admin.
    Unanswered(String),
}

/// Reads SY.admin's answer to a `cd_publish` (contracts §3): `ok` with SY.deploy's `kept` (`new`
/// or `already`) in its payload; an error SY.deploy or SY.admin gave on the package
/// (`PACKAGE_INVALID`, `PACKAGE_CHANGED`, `PACKAGE_TOO_LARGE`, or any other that is not
/// retryable) refuses it; no answer, `DEPLOY_UNAVAILABLE` or one marked `retryable` is no
/// verdict.
pub fn classify_publish_answer(answer: Result<Value, String>) -> PublishAnswer {
    let answer = match answer {
        Ok(answer) => answer,
        Err(err) => return PublishAnswer::Unanswered(err),
    };
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let payload = answer.get("payload");
    let status = text(answer.get("status")).unwrap_or_else(|| "ok".to_string());
    if status.eq_ignore_ascii_case("ok") {
        let kept = text(payload.and_then(|payload| payload.get("kept")))
            .or_else(|| text(answer.get("kept")));
        return PublishAnswer::Kept(match kept.as_deref() {
            Some("already") => PublishOutcome::Already,
            _ => PublishOutcome::New,
        });
    }
    let code = text(answer.get("error_code"))
        .or_else(|| text(payload.and_then(|payload| payload.get("error_code"))))
        .unwrap_or_else(|| status.to_ascii_uppercase());
    let detail = text(answer.get("error_detail"))
        .or_else(|| text(payload.and_then(|payload| payload.get("message"))))
        .or_else(|| text(payload.and_then(|payload| payload.get("error_detail"))));
    let reason = match detail {
        Some(detail) => format!("{code}: {detail}"),
        None => code.clone(),
    };
    let retryable = payload
        .and_then(|payload| payload.get("retryable"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if retryable || code == DEPLOY_UNAVAILABLE {
        PublishAnswer::Unanswered(reason)
    } else {
        PublishAnswer::Refused(reason)
    }
}

/// What one pass of the publish did (`PackageCi::publish_pass`).
#[derive(Debug, Clone, Default)]
pub struct PassReport {
    /// The packages it sent.
    pub sent: usize,
    /// Whether a package's outcome changed: a new verdict, a package gone, one that stopped being
    /// answered.
    pub changed: bool,
    /// Why the pass stopped before the end: SY.admin or SY.deploy did not answer.
    pub unanswered: Option<String>,
    /// The released packages SY.deploy refused on a later publish, removed (their paths).
    pub removed: Vec<String>,
}

/// A release staged for the operator's CONFIRM (`PackageCi::stage_release`).
#[derive(Debug, Clone)]
pub struct PendingRelease {
    pub package: Value,
    pub id: String,
    pub version: String,
    pub kind: String,
    pub origin: String,
    pub bytes: usize,
    /// Where CONFIRM writes it.
    pub path: PathBuf,
}

impl PendingRelease {
    /// What the release is, for the chat: its id, version, kind, origin, title, whom it is
    /// offered to, its nodes and where it goes.
    pub fn view(&self) -> Value {
        let nodes = self
            .package
            .get("nodes")
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .map(|node| {
                        json!({
                            "role": node.get("role"),
                            "kind": node.get("kind"),
                            "work_role": node.get("work_role"),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        json!({
            "id": self.id,
            "version": self.version,
            "kind": self.kind,
            "origin": self.origin,
            "title": self.package.get("title"),
            "offered_to": self.package.get("offered_to"),
            "nodes": nodes,
            "bytes": self.bytes,
            "path": self.path.display().to_string(),
        })
    }

    /// The chat's line for the staged release, ending with how to confirm it.
    pub fn staged_message(&self) -> String {
        let offered = match self.package.get("offered_to") {
            Some(offered) => offered.to_string(),
            None => "nobody named".to_string(),
        };
        let nodes = self
            .package
            .get("nodes")
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .map(|node| {
                        let field = |name: &str| node.get(name).and_then(Value::as_str);
                        format!(
                            "{} ({} {})",
                            field("role").unwrap_or("?"),
                            field("kind").unwrap_or("?"),
                            field("work_role").unwrap_or("?")
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        format!(
            "Package {id} {version} is ready to release: a {kind} of origin {origin}, offered to \
             {offered}, with {count} node(s): {nodes}. CONFIRM writes it once to {path} and \
             publishes it to SY.deploy, which checks it; if SY.deploy refuses it, the file is \
             removed and nothing is released. Reply CONFIRM to release it or CANCEL to discard.",
            id = self.id,
            version = self.version,
            kind = self.kind,
            origin = self.origin,
            count = nodes.len(),
            nodes = if nodes.is_empty() {
                "none".to_string()
            } else {
                nodes.join(", ")
            },
            path = self.path.display(),
        )
    }
}

/// What a confirmed release did (`PackageCi::confirm_release`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseResult {
    /// Not written: nothing was released.
    NotWritten(String),
    /// Written and kept by SY.deploy.
    Published { path: PathBuf, kept: PublishOutcome },
    /// SY.deploy refused it: the file is removed (or why it could not be).
    Refused {
        reason: String,
        removal_error: Option<String>,
    },
    /// Written; the publish got no answer, and the CI's next pass sends it again.
    Unanswered { path: PathBuf, reason: String },
}

impl ReleaseResult {
    /// The chat's status and line for it.
    pub fn message(&self, id: &str, version: &str) -> (&'static str, String) {
        match self {
            ReleaseResult::NotWritten(why) => (
                "error",
                format!("Package {id} {version} was not released: {why}."),
            ),
            ReleaseResult::Published { path, kept } => (
                "ok",
                format!(
                    "Package {id} {version} is released: written to {} and published to \
                     SY.deploy ({}), which offers it in its catalog.",
                    path.display(),
                    match kept {
                        PublishOutcome::Already => "it had this version already",
                        _ => "a new copy kept",
                    }
                ),
            ),
            ReleaseResult::Refused {
                reason,
                removal_error: None,
            } => (
                "error",
                format!(
                    "SY.deploy refused package {id} {version}: {reason}. Its file was removed: \
                     nothing is released. Fix the package and release it again."
                ),
            ),
            ReleaseResult::Refused {
                reason,
                removal_error: Some(err),
            } => (
                "error",
                format!(
                    "SY.deploy refused package {id} {version}: {reason}. Its file could not be \
                     removed ({err}): the CI lists it as refused."
                ),
            ),
            ReleaseResult::Unanswered { path, reason } => (
                "error",
                format!(
                    "Package {id} {version} is written to {}, but its publish got no answer \
                     ({reason}): Archi sends it again with its CI within a minute, and removes it \
                     if SY.deploy refuses it. Its outcome is in the CI's listing.",
                    path.display()
                ),
            ),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Archi's CI: its two stores, what each package's publish did this run, and the releases staged
/// in the chat.
pub struct PackageCi {
    system_dir: PathBuf,
    own_dir: PathBuf,
    /// One publish at a time: a pass and a release's never cross.
    publish_lock: tokio::sync::Mutex<()>,
    ledger: std::sync::Mutex<BTreeMap<PackageKey, PublishRecord>>,
    /// A release staged for the operator's CONFIRM, by chat session.
    pending: std::sync::Mutex<HashMap<String, PendingRelease>>,
}

impl PackageCi {
    pub fn new(system_dir: PathBuf, own_dir: PathBuf) -> Self {
        Self {
            system_dir,
            own_dir,
            publish_lock: tokio::sync::Mutex::new(()),
            ledger: std::sync::Mutex::new(BTreeMap::new()),
            pending: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The CI of an Archi whose state dir is `state_dir`: the shipped packages where the .deb
    /// installs them.
    pub fn for_state_dir(state_dir: &Path) -> Self {
        Self::new(
            PathBuf::from(SYSTEM_PACKAGES_DIR),
            state_dir.join(OWN_PACKAGES_SUBDIR),
        )
    }

    /// Every package, the shipped ones first.
    pub fn read(&self) -> Vec<CiPackage> {
        let mut packages = read_store(PackageStore::System, &self.system_dir);
        packages.extend(read_store(PackageStore::Own, &self.own_dir));
        packages
    }

    fn record_of(&self, key: &PackageKey) -> Option<PublishRecord> {
        self.ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned()
    }

    /// Records a package's outcome; true when it changed (a pending one stays the same while it
    /// is not answered, whatever the reason).
    fn record(
        &self,
        key: PackageKey,
        outcome: PublishOutcome,
        reason: Option<String>,
        file_sha256: &str,
        released: bool,
    ) -> bool {
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let changed = ledger.get(&key).is_none_or(|before| {
            before.outcome != outcome
                || before.file_sha256 != file_sha256
                || (outcome != PublishOutcome::Pending && before.reason != reason)
        });
        ledger.insert(
            key,
            PublishRecord {
                outcome,
                reason,
                at_ms: now_ms(),
                file_sha256: file_sha256.to_string(),
                released,
            },
        );
        changed
    }

    fn forget(&self, key: &PackageKey) {
        self.ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
    }

    /// The CI's listing (the tool's and the SCMD's): both stores, each package with its publish.
    pub fn listing(&self) -> Value {
        let packages = self
            .read()
            .iter()
            .map(|package| package.listing(self.record_of(&package.key()).as_ref()))
            .collect::<Vec<_>>();
        json!({
            "status": "ok",
            "system_dir": self.system_dir.display().to_string(),
            "own_dir": self.own_dir.display().to_string(),
            "count": packages.len(),
            "packages": packages,
        })
    }

    /// One package of the CI, whole, by id and version: the shipped one first.
    pub fn package(&self, id: &str, version: &str) -> Option<Value> {
        let package = self
            .read()
            .into_iter()
            .find(|package| package.id == id && package.version == version)?;
        let mut entry = package.listing(self.record_of(&package.key()).as_ref());
        entry["content"] = package.content.clone().unwrap_or(Value::Null);
        Some(entry)
    }

    /// One pass of the publish (contracts §3): every package not yet answered this run, or whose
    /// file changed, sent with `publish` (`{"package": ...}` to SY.admin's `cd_publish`), one by
    /// one, in the listing's order. A package Archi cannot send as it is (`CiPackage::error`) is
    /// refused without a send. The pass stops at the first publish that gets no verdict
    /// (SY.admin or SY.deploy not answering): the packages after it wait for the next pass. A
    /// package whose file is gone is forgotten.
    pub async fn publish_pass<F, Fut>(&self, mut publish: F) -> PassReport
    where
        F: FnMut(Value) -> Fut,
        Fut: Future<Output = Result<Value, String>>,
    {
        let _one_at_a_time = self.publish_lock.lock().await;
        let packages = self.read();
        let mut report = PassReport::default();
        {
            let keys = packages.iter().map(CiPackage::key).collect::<BTreeSet<_>>();
            let mut ledger = self
                .ledger
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let before = ledger.len();
            ledger.retain(|key, _| keys.contains(key));
            report.changed |= ledger.len() != before;
        }
        for package in &packages {
            let key = package.key();
            let before = self.record_of(&key);
            let released = before.as_ref().is_some_and(|record| record.released);
            let settled = before.as_ref().is_some_and(|record| {
                record.outcome != PublishOutcome::Pending
                    && record.file_sha256 == package.file_sha256
            });
            if settled {
                continue;
            }
            if report.unanswered.is_some() {
                if before.is_none() {
                    self.record(
                        key,
                        PublishOutcome::Pending,
                        None,
                        &package.file_sha256,
                        false,
                    );
                    report.changed = true;
                }
                continue;
            }
            let content = match (&package.error, &package.content) {
                (None, Some(content)) => content.clone(),
                (error, _) => {
                    let reason = error
                        .clone()
                        .unwrap_or_else(|| "the package does not read".to_string());
                    report.changed |= self.record(
                        key,
                        PublishOutcome::Refused,
                        Some(reason),
                        &package.file_sha256,
                        false,
                    );
                    continue;
                }
            };
            report.sent += 1;
            match classify_publish_answer(publish(content).await) {
                PublishAnswer::Kept(outcome) => {
                    report.changed |= self.record(key, outcome, None, &package.file_sha256, false);
                }
                PublishAnswer::Refused(reason)
                    if released && package.store == PackageStore::Own =>
                {
                    // A release whose publish got no answer, refused now: removed, as a release
                    // SY.deploy refuses is.
                    if fs::remove_file(&package.path).is_ok() {
                        self.forget(&key);
                        report
                            .removed
                            .push(format!("{}: {reason}", package.path.display()));
                    } else {
                        self.record(
                            key,
                            PublishOutcome::Refused,
                            Some(reason),
                            &package.file_sha256,
                            false,
                        );
                    }
                    report.changed = true;
                }
                PublishAnswer::Refused(reason) => {
                    report.changed |= self.record(
                        key,
                        PublishOutcome::Refused,
                        Some(reason),
                        &package.file_sha256,
                        false,
                    );
                }
                PublishAnswer::Unanswered(reason) => {
                    report.changed |= self.record(
                        key,
                        PublishOutcome::Pending,
                        Some(reason.clone()),
                        &package.file_sha256,
                        released,
                    );
                    report.unanswered = Some(reason);
                }
            }
        }
        report
    }

    /// What Archi's status keeps of the CI's publish: how many packages SY.deploy kept new and
    /// had already, and each refused or not yet answered with why.
    pub fn summary(&self) -> Value {
        let ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = |outcome: PublishOutcome| {
            ledger
                .values()
                .filter(|record| record.outcome == outcome)
                .count()
        };
        let listed = |outcome: PublishOutcome| {
            ledger
                .iter()
                .filter(|(_, record)| record.outcome == outcome)
                .map(|((store, id, version), record)| {
                    json!({
                        "store": store,
                        "id": id,
                        "version": version,
                        "reason": record.reason,
                    })
                })
                .collect::<Vec<_>>()
        };
        json!({
            "new": count(PublishOutcome::New),
            "already": count(PublishOutcome::Already),
            "refused": listed(PublishOutcome::Refused),
            "pending": listed(PublishOutcome::Pending),
            "updated_at_ms": ledger.values().map(|record| record.at_ms).max(),
        })
    }

    /// The log's one line for the CI's publish: new, already, and each refused with why.
    pub fn summary_line(&self) -> String {
        let summary = self.summary();
        let entries = |field: &str| {
            summary[field]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| {
                            format!(
                                "{} {} ({}): {}",
                                entry["id"].as_str().unwrap_or_default(),
                                entry["version"].as_str().unwrap_or_default(),
                                entry["store"].as_str().unwrap_or_default(),
                                entry["reason"].as_str().unwrap_or("not sent yet")
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let refused = entries("refused");
        let pending = entries("pending");
        let mut line = format!(
            "{} new, {} already, {} refused",
            summary["new"],
            summary["already"],
            refused.len()
        );
        if !refused.is_empty() {
            line.push_str(&format!(" [{}]", refused.join("; ")));
        }
        if !pending.is_empty() {
            line.push_str(&format!(
                ", {} not answered yet [{}]",
                pending.len(),
                pending.join("; ")
            ));
        }
        line
    }

    /// Checks a package for a release and stages it for `session_id`'s CONFIRM, in place of any
    /// release staged there before. Archi checks what it needs to keep it: an object whose id and
    /// version are 1 to 64 of `[a-z0-9._-]`, its kind one of the three, its origin `custom` or
    /// `catalog` (a `system` package ships in the .deb), a size a publish carries, and a version
    /// neither store holds: a version is released once. SY.deploy checks the rest on the publish.
    pub fn stage_release(
        &self,
        session_id: &str,
        package: Value,
    ) -> Result<PendingRelease, String> {
        self.discard_release(session_id);
        let release = self.release_candidate(package)?;
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id.to_string(), release.clone());
        Ok(release)
    }

    fn release_candidate(&self, package: Value) -> Result<PendingRelease, String> {
        let Some(fields) = package.as_object() else {
            return Err("a package is a JSON object".to_string());
        };
        let text = |field: &str| {
            fields
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let (id, version, kind, origin) =
            (text("id"), text("version"), text("kind"), text("origin"));
        if !is_package_token(id) || !is_package_token(version) {
            return Err("a package's id and version are each 1 to 64 of [a-z0-9._-]".to_string());
        }
        if !RELEASE_KINDS.contains(&kind) {
            return Err(format!(
                "a package's kind is one of {}, not '{kind}'",
                RELEASE_KINDS.join(", ")
            ));
        }
        if !RELEASE_ORIGINS.contains(&origin) {
            return Err(format!(
                "a release is of origin custom (one tenant) or catalog (every tenant), not \
                 '{origin}': system packages ship in the .deb, and Archi derives a custom copy \
                 of one under its own id"
            ));
        }
        let bytes = compact_len(&package);
        if bytes > PACKAGE_MAX_BYTES {
            return Err(too_large(bytes));
        }
        let file = format!("{version}.json");
        if self.system_dir.join(id).join(&file).exists() {
            return Err(format!(
                "Fluxbee ships package {id} {version}: a release takes another id"
            ));
        }
        let path = self.own_dir.join(id).join(&file);
        if path.exists() {
            return Err(format!(
                "package {id} {version} is released already: a version is released once, and a \
                 new content takes a new version"
            ));
        }
        Ok(PendingRelease {
            id: id.to_string(),
            version: version.to_string(),
            kind: kind.to_string(),
            origin: origin.to_string(),
            bytes,
            path,
            package,
        })
    }

    /// The release staged for `session_id`, if any, taken out for its CONFIRM.
    pub fn take_release(&self, session_id: &str) -> Option<PendingRelease> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id)
    }

    /// Drops the release staged for `session_id`; true when there was one.
    pub fn discard_release(&self, session_id: &str) -> bool {
        self.take_release(session_id).is_some()
    }

    /// The release staged for `session_id`, as the chat shows it.
    pub fn staged_release(&self, session_id: &str) -> Option<PendingRelease> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session_id)
            .cloned()
    }

    /// Writes a release once at its place, atomically: the whole file under a hidden name, then
    /// linked in, which fails when the place holds one. Returns the bytes' SHA-256.
    fn write_once(&self, release: &PendingRelease) -> Result<String, String> {
        if self
            .system_dir
            .join(&release.id)
            .join(format!("{}.json", release.version))
            .exists()
        {
            return Err(format!(
                "Fluxbee ships package {} {}: a release takes another id",
                release.id, release.version
            ));
        }
        let dir = release
            .path
            .parent()
            .ok_or_else(|| "the package's place has no directory".to_string())?;
        fs::create_dir_all(dir)
            .map_err(|err| format!("{} cannot be made: {err}", dir.display()))?;
        let mut bytes = serde_json::to_vec_pretty(&release.package)
            .map_err(|err| format!("the package does not serialize: {err}"))?;
        bytes.push(b'\n');
        let temp = dir.join(format!(
            ".{}.json.{}.tmp",
            release.version,
            uuid::Uuid::new_v4().simple()
        ));
        let written = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o644))?;
            fs::hard_link(&temp, &release.path)
        })();
        let _ = fs::remove_file(&temp);
        match written {
            Ok(()) => Ok(sha256_hex(&bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
                "package {} {} is released already: a version is released once",
                release.id, release.version
            )),
            Err(err) => Err(format!(
                "{} cannot be written: {err}",
                release.path.display()
            )),
        }
    }

    /// A confirmed release: written once to the own store, then published with `publish` at
    /// once. SY.deploy's refusal removes the file; a publish with no verdict leaves it for the
    /// next pass, which removes it if SY.deploy refuses it then.
    pub async fn confirm_release<F, Fut>(
        &self,
        release: &PendingRelease,
        publish: F,
    ) -> ReleaseResult
    where
        F: FnOnce(Value) -> Fut,
        Fut: Future<Output = Result<Value, String>>,
    {
        let _one_at_a_time = self.publish_lock.lock().await;
        let file_sha256 = match self.write_once(release) {
            Ok(sha) => sha,
            Err(why) => return ReleaseResult::NotWritten(why),
        };
        let key = (
            PackageStore::Own,
            release.id.clone(),
            release.version.clone(),
        );
        match classify_publish_answer(publish(release.package.clone()).await) {
            PublishAnswer::Kept(kept) => {
                self.record(key, kept, None, &file_sha256, false);
                ReleaseResult::Published {
                    path: release.path.clone(),
                    kept,
                }
            }
            PublishAnswer::Refused(reason) => {
                let removal_error = fs::remove_file(&release.path)
                    .err()
                    .map(|err| err.to_string());
                if removal_error.is_none() {
                    self.forget(&key);
                } else {
                    self.record(
                        key,
                        PublishOutcome::Refused,
                        Some(reason.clone()),
                        &file_sha256,
                        false,
                    );
                }
                ReleaseResult::Refused {
                    reason,
                    removal_error,
                }
            }
            PublishAnswer::Unanswered(reason) => {
                self.record(
                    key,
                    PublishOutcome::Pending,
                    Some(reason.clone()),
                    &file_sha256,
                    true,
                );
                ReleaseResult::Unanswered {
                    path: release.path.clone(),
                    reason,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A scratch dir with a system store and an own store.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "fluxbee-package-ci-{}",
                uuid::Uuid::new_v4().simple()
            ));
            fs::create_dir_all(root.join("system")).expect("system dir");
            fs::create_dir_all(root.join("state")).expect("state dir");
            Self { root }
        }
        fn system(&self) -> PathBuf {
            self.root.join("system")
        }
        fn own(&self) -> PathBuf {
            self.root.join("state").join(OWN_PACKAGES_SUBDIR)
        }
        fn ci(&self) -> PackageCi {
            PackageCi::new(self.system(), self.own())
        }
        fn put(&self, dir: &Path, id: &str, version: &str, package: &Value) -> PathBuf {
            let path = dir.join(id).join(format!("{version}.json"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, serde_json::to_vec_pretty(package).unwrap()).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn package(id: &str, version: &str, kind: &str, origin: &str) -> Value {
        json!({
            "schema": 1,
            "id": id,
            "version": version,
            "kind": kind,
            "origin": origin,
            "offered_to": {"tenants": "all"},
            "title": format!("The {id}"),
            "description": "",
            "blanks": [],
            "nodes": [{"role": "door", "kind": "WF", "middle": id, "work_role": "capability",
                       "definition": {"start_from": [{"$ref": "grouping.conscious"}]}}],
            "vault": [],
        })
    }

    fn ok(kept: &str) -> Result<Value, String> {
        Ok(json!({"status": "ok", "action": "cd_publish",
                  "payload": {"status": "ok", "kept": kept, "id": "x", "version": "1",
                              "hash": "sha256:00"}}))
    }

    fn refused(code: &str, message: &str) -> Result<Value, String> {
        Ok(
            json!({"status": "error", "action": "cd_publish", "error_code": code,
                  "error_detail": message,
                  "payload": {"status": "error", "error_code": code, "message": message}}),
        )
    }

    fn unavailable() -> Result<Value, String> {
        Ok(
            json!({"status": "error", "action": "cd_publish", "error_code": "DEPLOY_UNAVAILABLE",
                  "error_detail": "SY.deploy@motherbee did not answer within 10 s",
                  "payload": {"retryable": true}}),
        )
    }

    fn tree_bytes(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.insert(path.clone(), fs::read(&path).unwrap());
                }
            }
        }
        files
    }

    /// The listing (contracts §2): the shipped packages and Archi's own, each with its id,
    /// version, kind, origin, title and where it lives; a file in the wrong place, of the wrong
    /// origin for its store, not JSON or too large is listed with why it cannot be published; a
    /// hidden file, a non-JSON one and a release being written are not packages. Reading the CI,
    /// whole or one package, never writes to the system store.
    #[test]
    fn the_listing_reads_both_stores_and_never_writes_the_system_one() {
        let scratch = Scratch::new();
        let system = scratch.system();
        scratch.put(
            &system,
            "plain-ed",
            "1",
            &package("plain-ed", "1", "executive", "system"),
        );
        scratch.put(
            &system,
            "plain-ed",
            "2",
            &package("plain-ed", "2", "ed", "system"),
        );
        scratch.put(
            &system,
            "rogue",
            "1",
            &package("rogue", "1", "capability", "custom"),
        );
        scratch.put(
            &system,
            "moved",
            "1",
            &package("other", "1", "capability", "system"),
        );
        fs::write(system.join("plain-ed").join("README.md"), "not a package").unwrap();
        fs::write(system.join("plain-ed").join(".3.json.tmp"), "{").unwrap();
        fs::create_dir_all(system.join(".hidden")).unwrap();
        fs::write(system.join(".hidden").join("1.json"), "{}").unwrap();
        let own = scratch.own();
        scratch.put(
            &own,
            "billing",
            "1",
            &package("billing", "1", "capability", "custom"),
        );
        scratch.put(
            &own,
            "faked",
            "1",
            &package("faked", "1", "executive", "system"),
        );
        fs::create_dir_all(own.join("broken")).unwrap();
        fs::write(own.join("broken").join("1.json"), "{not json").unwrap();
        let mut huge = package("huge", "1", "infrastructure", "catalog");
        huge["description"] = json!("x".repeat(PACKAGE_MAX_BYTES));
        scratch.put(&own, "huge", "1", &huge);
        let before = tree_bytes(&system);

        let ci = scratch.ci();
        let listing = ci.listing();
        let packages = listing["packages"].as_array().unwrap();
        let named = |id: &str, version: &str| {
            packages
                .iter()
                .find(|entry| entry["id"] == id && entry["version"] == version)
                .unwrap_or_else(|| panic!("{id} {version} in {listing}"))
        };
        assert_eq!(listing["count"], 8, "{listing}");
        let plain = named("plain-ed", "1");
        assert_eq!(plain["store"], "system");
        assert_eq!(plain["kind"], "executive");
        assert_eq!(plain["origin"], "system");
        assert_eq!(plain["title"], "The plain-ed");
        assert_eq!(plain["offered_to"], json!({"tenants": "all"}));
        assert_eq!(
            plain["path"],
            system.join("plain-ed/1.json").display().to_string()
        );
        assert!(plain.get("error").is_none(), "{plain}");
        // The read alias of the copies on disk is listed as written.
        assert_eq!(named("plain-ed", "2")["kind"], "ed");
        let billing = named("billing", "1");
        assert_eq!(billing["store"], "own");
        assert_eq!(billing["origin"], "custom");
        assert!(billing.get("error").is_none(), "{billing}");
        for (id, why) in [
            ("rogue", "a package Fluxbee ships is of origin system"),
            ("moved", "its id and version are not its place's"),
            ("faked", "system packages ship in the .deb"),
            ("broken", "the file is not JSON"),
            ("huge", PACKAGE_TOO_LARGE),
        ] {
            let entry = named(id, "1");
            let error = entry["error"].as_str().unwrap_or_default();
            assert!(error.contains(why), "{id}: {entry}");
        }

        let one = ci.package("billing", "1").expect("billing 1");
        assert_eq!(
            one["content"],
            package("billing", "1", "capability", "custom")
        );
        assert!(ci.package("billing", "2").is_none());
        assert_eq!(
            tree_bytes(&system),
            before,
            "the system store is never written"
        );
    }

    /// SY.admin's answer to a publish, as Archi takes it (contracts §3): kept new or already, a
    /// refusal with its code and reason, and no verdict when nothing or SY.admin's
    /// `DEPLOY_UNAVAILABLE` (retryable) answered.
    #[test]
    fn a_publish_answer_is_kept_refused_or_unanswered() {
        assert_eq!(
            classify_publish_answer(ok("new")),
            PublishAnswer::Kept(PublishOutcome::New)
        );
        assert_eq!(
            classify_publish_answer(ok("already")),
            PublishAnswer::Kept(PublishOutcome::Already)
        );
        assert_eq!(
            classify_publish_answer(refused("PACKAGE_INVALID", "a capability has one door")),
            PublishAnswer::Refused("PACKAGE_INVALID: a capability has one door".to_string())
        );
        assert_eq!(
            classify_publish_answer(refused(
                "PACKAGE_CHANGED",
                "billing 1 is kept with another hash"
            )),
            PublishAnswer::Refused(
                "PACKAGE_CHANGED: billing 1 is kept with another hash".to_string()
            )
        );
        assert!(matches!(
            classify_publish_answer(unavailable()),
            PublishAnswer::Unanswered(reason) if reason.starts_with("DEPLOY_UNAVAILABLE")
        ));
        assert_eq!(
            classify_publish_answer(Err("timeout waiting response".to_string())),
            PublishAnswer::Unanswered("timeout waiting response".to_string())
        );
    }

    /// The publish at start over a fake admin (contracts §3): every package once, the shipped
    /// ones first; the summary says new, already and each refused with why; a package Archi
    /// cannot send is refused without a send. A later pass sends nothing: a refused package is not
    /// sent again until its file changes, and then it is; a pass that gets no verdict stops and
    /// the next one goes on where it stopped.
    #[tokio::test]
    async fn the_publish_at_start_sends_each_package_once_and_never_retries_an_unchanged_refusal() {
        let scratch = Scratch::new();
        let system = scratch.system();
        let own = scratch.own();
        scratch.put(
            &system,
            "plain-ed",
            "1",
            &package("plain-ed", "1", "executive", "system"),
        );
        scratch.put(
            &own,
            "billing",
            "1",
            &package("billing", "1", "capability", "custom"),
        );
        let bad = scratch.put(
            &own,
            "bad",
            "1",
            &package("bad", "1", "capability", "custom"),
        );
        scratch.put(
            &own,
            "faked",
            "1",
            &package("faked", "1", "executive", "system"),
        );
        let ci = scratch.ci();

        let sent = RefCell::new(Vec::<String>::new());
        let fake_admin = |package: Value| {
            let id = package["id"].as_str().unwrap_or_default().to_string();
            sent.borrow_mut().push(id.clone());
            async move {
                match id.as_str() {
                    "plain-ed" => ok("already"),
                    "billing" => ok("new"),
                    _ => refused("PACKAGE_INVALID", "a capability has exactly one door"),
                }
            }
        };
        let report = ci.publish_pass(fake_admin).await;
        assert_eq!(*sent.borrow(), ["plain-ed", "bad", "billing"]);
        assert_eq!(report.sent, 3);
        assert!(report.changed);
        assert!(report.unanswered.is_none());
        let summary = ci.summary();
        assert_eq!(summary["new"], 1, "{summary}");
        assert_eq!(summary["already"], 1, "{summary}");
        let refusals = summary["refused"].as_array().unwrap();
        assert_eq!(refusals.len(), 2, "{summary}");
        assert_eq!(refusals[0]["id"], "bad");
        assert_eq!(
            refusals[0]["reason"],
            "PACKAGE_INVALID: a capability has exactly one door"
        );
        assert_eq!(refusals[1]["id"], "faked");
        assert!(refusals[1]["reason"]
            .as_str()
            .unwrap()
            .contains("system packages ship in the .deb"));
        let line = ci.summary_line();
        assert!(line.starts_with("1 new, 1 already, 2 refused"), "{line}");
        assert!(line.contains("bad 1 (own): PACKAGE_INVALID"), "{line}");
        assert_eq!(ci.listing()["packages"][0]["publish"]["outcome"], "already");

        // Nothing changed: nothing is sent, the refusals included.
        sent.borrow_mut().clear();
        let report = ci.publish_pass(fake_admin).await;
        assert!(sent.borrow().is_empty(), "{:?}", sent.borrow());
        assert_eq!(report.sent, 0);
        assert!(!report.changed);

        // The refused file changes: it is sent again, alone.
        let mut fixed = package("bad", "1", "capability", "custom");
        fixed["title"] = json!("Fixed");
        fs::write(&bad, serde_json::to_vec(&fixed).unwrap()).unwrap();
        let report = ci
            .publish_pass(|package: Value| {
                sent.borrow_mut()
                    .push(package["id"].as_str().unwrap_or_default().to_string());
                async { ok("new") }
            })
            .await;
        assert_eq!(*sent.borrow(), ["bad"]);
        assert!(report.changed);
        assert_eq!(ci.summary()["new"], 2);

        // A new package and SY.deploy not answering: the pass stops at the first, and the next
        // one sends it again.
        scratch.put(
            &own,
            "agenda",
            "1",
            &package("agenda", "1", "capability", "catalog"),
        );
        scratch.put(
            &own,
            "zeta",
            "1",
            &package("zeta", "1", "infrastructure", "catalog"),
        );
        sent.borrow_mut().clear();
        let report = ci
            .publish_pass(|package: Value| {
                sent.borrow_mut()
                    .push(package["id"].as_str().unwrap_or_default().to_string());
                async { unavailable() }
            })
            .await;
        assert_eq!(*sent.borrow(), ["agenda"], "the pass stops at the first");
        assert!(report.unanswered.is_some());
        assert_eq!(ci.summary()["pending"].as_array().unwrap().len(), 2);
        // Still not answered: no change to log.
        sent.borrow_mut().clear();
        let report = ci
            .publish_pass(|package: Value| {
                sent.borrow_mut()
                    .push(package["id"].as_str().unwrap_or_default().to_string());
                async { Err::<Value, String>("timeout waiting response".to_string()) }
            })
            .await;
        assert_eq!(*sent.borrow(), ["agenda"]);
        assert!(!report.changed);
        sent.borrow_mut().clear();
        ci.publish_pass(|package: Value| {
            sent.borrow_mut()
                .push(package["id"].as_str().unwrap_or_default().to_string());
            async { ok("new") }
        })
        .await;
        assert_eq!(*sent.borrow(), ["agenda", "zeta"]);
        assert!(ci.summary()["pending"].as_array().unwrap().is_empty());

        // A package whose file is gone is forgotten.
        fs::remove_file(own.join("zeta/1.json")).unwrap();
        let report = ci.publish_pass(fake_admin).await;
        assert!(report.changed);
        assert_eq!(ci.summary()["new"], 3);
    }

    /// A release (contracts §1, §3): staged with what Archi checks (its place, its kind, its
    /// origin, its size, a version never released), written once on CONFIRM and published at
    /// once; SY.deploy's refusal removes the file; a publish with no answer leaves it for the
    /// next pass, which removes it when SY.deploy refuses it then. The system store is never
    /// written.
    #[tokio::test]
    async fn a_release_is_staged_written_once_published_and_removed_when_refused() {
        let scratch = Scratch::new();
        let system = scratch.system();
        scratch.put(
            &system,
            "plain-ed",
            "1",
            &package("plain-ed", "1", "executive", "system"),
        );
        let before = tree_bytes(&system);
        let ci = scratch.ci();

        // What a release cannot be.
        let mut huge = package("huge", "1", "capability", "custom");
        huge["description"] = json!("x".repeat(PACKAGE_MAX_BYTES));
        for (candidate, why) in [
            (json!([]), "a package is a JSON object"),
            (package("../up", "1", "capability", "custom"), "[a-z0-9._-]"),
            (
                package("billing", "..", "capability", "custom"),
                "[a-z0-9._-]",
            ),
            (
                package("billing", "1", "ed", "custom"),
                "a package's kind is one of",
            ),
            (
                package("billing", "1", "capability", "system"),
                "system packages ship in the .deb",
            ),
            (
                package("plain-ed", "1", "executive", "catalog"),
                "Fluxbee ships package plain-ed 1",
            ),
            (huge, PACKAGE_TOO_LARGE),
        ] {
            let err = ci.stage_release("s1", candidate).expect_err(why);
            assert!(err.contains(why), "{why}: {err}");
        }
        assert!(ci.staged_release("s1").is_none());

        // Staged, then confirmed: written once and published.
        let billing = package("billing", "1", "capability", "custom");
        let staged = ci.stage_release("s1", billing.clone()).expect("staged");
        assert_eq!(staged.path, scratch.own().join("billing/1.json"));
        assert!(!staged.path.exists(), "staging writes nothing");
        let message = staged.staged_message();
        assert!(
            message.contains("Package billing 1 is ready to release"),
            "{message}"
        );
        assert!(message.contains("door (WF capability)"), "{message}");
        assert!(message.ends_with("Reply CONFIRM to release it or CANCEL to discard."));
        assert_eq!(staged.view()["nodes"][0]["work_role"], "capability");
        let release = ci.take_release("s1").expect("the staged release");
        assert!(ci.take_release("s1").is_none(), "taken once");
        let sent = RefCell::new(Vec::<Value>::new());
        let result = ci
            .confirm_release(&release, |package: Value| {
                sent.borrow_mut().push(package);
                async { ok("new") }
            })
            .await;
        assert_eq!(
            result,
            ReleaseResult::Published {
                path: release.path.clone(),
                kept: PublishOutcome::New
            }
        );
        assert_eq!(*sent.borrow(), std::slice::from_ref(&billing));
        let written: Value = serde_json::from_slice(&fs::read(&release.path).unwrap()).unwrap();
        assert_eq!(written, billing);
        assert_eq!(
            fs::metadata(&release.path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(ci.summary()["new"], 1);
        // Published by the release: the next pass sends the shipped package alone.
        let report = ci
            .publish_pass(|package: Value| {
                assert_eq!(
                    package["id"], "plain-ed",
                    "the release is published already"
                );
                async { ok("already") }
            })
            .await;
        assert_eq!(report.sent, 1);
        // A version is released once.
        let err = ci.stage_release("s1", billing.clone()).expect_err("once");
        assert!(err.contains("released already"), "{err}");
        let again = ci
            .confirm_release(&release, |_| async { panic!("never sent") })
            .await;
        assert!(
            matches!(again, ReleaseResult::NotWritten(why) if why.contains("released already"))
        );
        assert_eq!(
            written,
            serde_json::from_slice::<Value>(&fs::read(&release.path).unwrap()).unwrap()
        );

        // Refused by SY.deploy: the file is removed and the reason told.
        let release = ci
            .stage_release("s2", package("crm", "1", "capability", "custom"))
            .expect("staged");
        let result = ci
            .confirm_release(&release, |_| async {
                refused(
                    "PACKAGE_INVALID",
                    "a custom package names exactly one tenant",
                )
            })
            .await;
        assert_eq!(
            result,
            ReleaseResult::Refused {
                reason: "PACKAGE_INVALID: a custom package names exactly one tenant".to_string(),
                removal_error: None
            }
        );
        assert!(!release.path.exists(), "a refused release is removed");
        let (status, message) = result.message("crm", "1");
        assert_eq!(status, "error");
        assert!(
            message.contains("a custom package names exactly one tenant"),
            "{message}"
        );
        assert!(message.contains("Its file was removed"), "{message}");
        assert!(ci.listing()["packages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["id"] != "crm"));

        // No answer: written and pending; the next pass sends it, and its refusal removes it.
        let release = ci
            .stage_release("s3", package("agenda", "1", "capability", "catalog"))
            .expect("staged");
        let result = ci
            .confirm_release(&release, |_| async { unavailable() })
            .await;
        assert!(
            matches!(result, ReleaseResult::Unanswered { .. }),
            "{result:?}"
        );
        assert!(release.path.exists());
        assert_eq!(ci.summary()["pending"][0]["id"], "agenda");
        let report = ci
            .publish_pass(|_| async { refused("PACKAGE_INVALID", "no door") })
            .await;
        assert_eq!(report.sent, 1);
        assert_eq!(report.removed.len(), 1, "{:?}", report.removed);
        assert!(
            !release.path.exists(),
            "a release refused later is removed too"
        );
        assert!(ci.summary()["pending"].as_array().unwrap().is_empty());

        // A staged release is dropped on CANCEL, and a new one takes its place.
        ci.stage_release("s4", package("one", "1", "capability", "custom"))
            .expect("staged");
        ci.stage_release("s4", package("two", "1", "capability", "custom"))
            .expect("staged");
        assert_eq!(ci.staged_release("s4").unwrap().id, "two");
        assert!(ci.discard_release("s4"));
        assert!(!ci.discard_release("s4"));

        assert_eq!(
            tree_bytes(&system),
            before,
            "the system store is never written"
        );
    }
}
