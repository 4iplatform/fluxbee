//! The installation's update (docs/hive-config-update-v1.md §6, "As built (stage 4)"): every hive
//! brought to what the motherbee holds, its core and its installation file, by one update at a time
//! that SY.admin keeps on disk.
//!
//! - **The target** is the motherbee's now ([`target_of`]): its installed core manifest hash and
//!   version, the core's reference, and its installation file's version and sha256.
//! - **A hive is on it** when its installed core hash is the target's with the update settled and
//!   nothing open, and its `config.applied` names the target's version and sha256.
//! - **Per hive** ([`decide`], a pure function of the hive's `/versions`, the target and the run's
//!   kind): the core first (`SYSTEM_UPDATE` category core), then the config (category config), or
//!   an exclusion with its reason and the per-hive action that moves it.
//! - **The state** ([`Updates`]): `installation-update.json` in SY.admin's state directory,
//!   written atomically: the open update, each hive's state, last outcome and next step, and the
//!   last [`HISTORY_KEPT`] closed ones. A run whose target differs closes the open update as
//!   superseded; one with the same target resumes it.
//! - **The background task** ([`run_background`]), while an update is open: it follows a hive in
//!   flight (a core restarting or settling) to its settle and then applies its config, re-checks a
//!   hive not on the target, and finishes the update once for a hive that did not answer and now
//!   does (the automatic step, §6), never for an excluded one.
//!
//! Every outcome is one log line, `installation update: outcome`, with the update's id, the hive,
//! the category, the outcome and its duration (`ops.py timeline` reads it).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    apply_installation_on_hive, env_timeout_secs, installation_apply_bound, now_epoch_ms,
    AdminContext, ConnectedNodes, InstallationApplied, InstallationApplyOutcome, RouterDispatcher,
};

/// The closed updates the state keeps, newest first.
pub(crate) const HISTORY_KEPT: usize = 20;

const UPDATES_SCHEMA_VERSION: u32 = 1;

/// The longest text a record keeps (a reason, a detail, a next step): the history at its bound
/// stays well inside one mesh frame (128 KiB).
const TEXT_MAX: usize = 400;

/// Where SY.admin keeps the installation's update.
pub(crate) fn updates_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sy-admin").join("installation-update.json")
}

// ---------------------------------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------------------------------

/// An update's own state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UpdateState {
    /// Some hive is not on the target yet.
    Open,
    /// Every hive reached the target.
    Done,
    /// A run with another target closed it.
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Category {
    Core,
    Config,
}

impl Category {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Category::Core => "core",
            Category::Config => "config",
        }
    }
}

/// Where one hive stands in one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Standing {
    OnTarget,
    /// Not on the target: nothing tried yet, or what was tried did not get it there (its outcome
    /// says what, and `next` what moves it).
    Pending,
    /// Under way: an update being sent, or a core restarting or settling, which the background
    /// task follows.
    InFlight,
    /// Left as it is, with its reason and the per-hive action that moves it.
    Excluded,
    /// Not known: its `/versions` was not read yet, or did not answer.
    Unknown,
}

impl Standing {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Standing::OnTarget => "on_target",
            Standing::Pending => "pending",
            Standing::InFlight => "in_flight",
            Standing::Excluded => "excluded",
            Standing::Unknown => "unknown",
        }
    }
}

/// What an attempt (or an exclusion) left. Never "not applied" (P-13): a hive that did not answer
/// may hold the target already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutcomeStatus {
    Ok,
    /// A core update whose binaries are in place, its services restarting.
    Restarting,
    /// A core update installed and settled by the new orchestrator, nothing open.
    Settled,
    /// The hive's copy had not arrived at the bound.
    SyncPending,
    Error,
    NotAnswered,
    Excluded,
}

impl OutcomeStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            OutcomeStatus::Ok => "ok",
            OutcomeStatus::Restarting => "restarting",
            OutcomeStatus::Settled => "settled",
            OutcomeStatus::SyncPending => "sync_pending",
            OutcomeStatus::Error => "error",
            OutcomeStatus::NotAnswered => "not_answered",
            OutcomeStatus::Excluded => "excluded",
        }
    }
}

/// Who ran an attempt: an explicit run (`update_installation`, `set_installation_ai`, and the
/// background task following what it started), or the automatic step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunKind {
    Explicit,
    Automatic,
}

impl RunKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RunKind::Explicit => "explicit",
            RunKind::Automatic => "automatic",
        }
    }
}

/// One attempt's outcome, with its times (ms since the epoch on the motherbee's clock; the
/// durations in ms after `started_at_ms`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Outcome {
    pub(crate) status: OutcomeStatus,
    pub(crate) by: RunKind,
    pub(crate) started_at_ms: u64,
    /// When it reached `status`.
    pub(crate) at_ms: u64,
    #[serde(default)]
    pub(crate) attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    /// A core update: its answer, the target installed (`/versions`), and the new orchestrator's
    /// settle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) answered_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) installed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) settled_ms: Option<u64>,
    /// A config update: the readers it restarted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) restarted: Option<Vec<String>>,
    /// A core update: the hive's last core update before it (`core.last_update.started_at_ms`), so
    /// the watch tells this update's record from an older one. None for an update this run did not
    /// send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) previous_update: Option<String>,
}

impl Outcome {
    fn new(status: OutcomeStatus, by: RunKind, started_at_ms: u64, at_ms: u64) -> Self {
        Outcome {
            status,
            by,
            started_at_ms,
            at_ms,
            attempts: 0,
            error_code: None,
            detail: None,
            answered_ms: None,
            installed_ms: None,
            settled_ms: None,
            restarted: None,
            previous_update: None,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.at_ms.saturating_sub(self.started_at_ms)
    }
}

/// One hive's state in one category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CategoryEntry {
    pub(crate) state: Standing,
    /// Why it stands so (an exclusion's reason, what it waits for).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<Outcome>,
    /// What moves it, when it is not on the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next: Option<String>,
}

impl Default for CategoryEntry {
    fn default() -> Self {
        CategoryEntry {
            state: Standing::Unknown,
            reason: Some("not checked yet".to_string()),
            outcome: None,
            next: None,
        }
    }
}

/// The automatic step's one attempt on a hive, in this update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AutomaticAttempt {
    pub(crate) started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at_ms: Option<u64>,
    /// What it left, in a few words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HiveEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<String>,
    #[serde(default)]
    pub(crate) core: CategoryEntry,
    #[serde(default)]
    pub(crate) config: CategoryEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) automatic: Option<AutomaticAttempt>,
}

impl HiveEntry {
    fn category(&self, category: Category) -> &CategoryEntry {
        match category {
            Category::Core => &self.core,
            Category::Config => &self.config,
        }
    }

    fn category_mut(&mut self, category: Category) -> &mut CategoryEntry {
        match category {
            Category::Core => &mut self.core,
            Category::Config => &mut self.config,
        }
    }

    fn on_target(&self) -> bool {
        self.core.state == Standing::OnTarget && self.config.state == Standing::OnTarget
    }

    /// The automatic step finishes the update for this hive: it did not answer an attempt of this
    /// update, it is not on the target, and the step has not tried it yet (once per hive and
    /// target).
    pub(crate) fn awaits_automatic_step(&self) -> bool {
        self.automatic.is_none()
            && [&self.core, &self.config].into_iter().any(|entry| {
                matches!(entry.state, Standing::Pending | Standing::Unknown)
                    && entry
                        .outcome
                        .as_ref()
                        .is_some_and(|outcome| outcome.status == OutcomeStatus::NotAnswered)
            })
    }
}

/// The motherbee's core, the reference: its installed manifest hash, and its version (the
/// manifest's components').
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CoreTarget {
    pub(crate) manifest_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
}

/// The motherbee's installation file: its version and sha256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConfigTarget {
    pub(crate) version: u64,
    pub(crate) sha256: String,
}

/// What every hive is brought to: what the motherbee holds now. A part it cannot name is None,
/// with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Target {
    #[serde(default)]
    pub(crate) core: Option<CoreTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) core_note: Option<String>,
    #[serde(default)]
    pub(crate) config: Option<ConfigTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) config_note: Option<String>,
}

impl Target {
    /// The same target: the same core hash and the same file version and hash (the notes and the
    /// version's name do not count).
    pub(crate) fn same_as(&self, other: &Target) -> bool {
        let core = |target: &Target| {
            target
                .core
                .as_ref()
                .map(|core| normalize_hash(&core.manifest_hash))
        };
        let config = |target: &Target| {
            target
                .config
                .as_ref()
                .map(|config| (config.version, normalize_hash(&config.sha256)))
        };
        core(self) == core(other) && config(self) == config(other)
    }

    fn describe(&self) -> String {
        let core = match &self.core {
            Some(core) => format!(
                "core {} ({})",
                core.version.as_deref().unwrap_or("version unknown"),
                short_hash(&core.manifest_hash)
            ),
            None => "core unknown".to_string(),
        };
        let config = match &self.config {
            Some(config) => format!(
                "installation file v{} ({})",
                config.version,
                short_hash(&config.sha256)
            ),
            None => "no installation file to apply".to_string(),
        };
        format!("{core}, {config}")
    }
}

/// One installation update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UpdateRecord {
    pub(crate) id: String,
    pub(crate) state: UpdateState,
    pub(crate) opened_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) closed_at_ms: Option<u64>,
    /// Who opened it: `http` (loopback), `executor` (a confirmed plan), `set_installation_ai`.
    pub(crate) origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    pub(crate) target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) superseded_by: Option<String>,
    #[serde(default)]
    pub(crate) hives: BTreeMap<String, HiveEntry>,
    /// The registry's hives whose join did not finish: the update does not reach them.
    #[serde(default)]
    pub(crate) skipped: Vec<Value>,
}

impl UpdateRecord {
    fn all_on_target(&self) -> bool {
        !self.hives.is_empty() && self.hives.values().all(HiveEntry::on_target)
    }
}

/// `installation-update.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UpdatesFile {
    pub(crate) schema_version: u32,
    #[serde(default)]
    pub(crate) open: Option<UpdateRecord>,
    /// The closed updates, newest first, at most [`HISTORY_KEPT`].
    #[serde(default)]
    pub(crate) closed: Vec<UpdateRecord>,
}

impl Default for UpdatesFile {
    fn default() -> Self {
        UpdatesFile {
            schema_version: UPDATES_SCHEMA_VERSION,
            open: None,
            closed: Vec::new(),
        }
    }
}

/// What a run did to the open update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Opening {
    Opened,
    Resumed,
    /// It closed the open update, with another target, as superseded.
    Superseded(String),
}

impl UpdatesFile {
    fn open_mut(&mut self, id: &str) -> Option<&mut UpdateRecord> {
        self.open.as_mut().filter(|update| update.id == id)
    }

    /// The open update or, with none open, the last closed one.
    pub(crate) fn current(&self) -> Option<&UpdateRecord> {
        self.open.as_ref().or_else(|| self.closed.first())
    }

    /// Into the history, newest first, the oldest past [`HISTORY_KEPT`] dropped.
    fn close(&mut self, update: UpdateRecord) {
        self.closed.insert(0, update);
        self.closed.truncate(HISTORY_KEPT);
    }

    /// One update at a time (§6): the open one when its target is `target`; otherwise a new one,
    /// the open one closed as superseded and kept in the history. A new update's hives start
    /// unknown.
    pub(crate) fn open_or_resume(
        &mut self,
        target: &Target,
        origin: &str,
        reason: Option<&str>,
        now_ms: u64,
    ) -> (String, Opening) {
        if let Some(open) = &self.open {
            if open.target.same_as(target) {
                return (open.id.clone(), Opening::Resumed);
            }
        }
        let id = format!("upd-{now_ms}");
        let id = if self.open.as_ref().is_some_and(|open| open.id == id)
            || self.closed.iter().any(|closed| closed.id == id)
        {
            format!("{id}-{}", uuid::Uuid::new_v4().simple())
        } else {
            id
        };
        let opening = match self.open.take() {
            Some(mut previous) => {
                let previous_id = previous.id.clone();
                previous.state = UpdateState::Superseded;
                previous.closed_at_ms = Some(now_ms);
                previous.superseded_by = Some(id.clone());
                self.close(previous);
                Opening::Superseded(previous_id)
            }
            None => Opening::Opened,
        };
        self.open = Some(UpdateRecord {
            id: id.clone(),
            state: UpdateState::Open,
            opened_at_ms: now_ms,
            closed_at_ms: None,
            origin: origin.to_string(),
            reason: reason.map(cut),
            target: target.clone(),
            superseded_by: None,
            hives: BTreeMap::new(),
            skipped: Vec::new(),
        });
        (id, opening)
    }

    /// The hives an update reaches now: an entry for each (a new one unknown), none for a hive
    /// that left; the registry's hives whose join did not finish, named in `skipped`.
    pub(crate) fn refresh_hives(&mut self, id: &str, hives: &[String], skipped: Vec<Value>) {
        let Some(update) = self.open_mut(id) else {
            return;
        };
        update.hives.retain(|hive, _| hives.contains(hive));
        for hive in hives {
            update.hives.entry(hive.clone()).or_default();
        }
        update.skipped = skipped;
    }

    /// Closes the open update as done once every hive is on the target: its id.
    pub(crate) fn close_if_done(&mut self, now_ms: u64) -> Option<String> {
        if !self.open.as_ref().is_some_and(UpdateRecord::all_on_target) {
            return None;
        }
        let mut update = self.open.take()?;
        update.state = UpdateState::Done;
        update.closed_at_ms = Some(now_ms);
        let id = update.id.clone();
        self.close(update);
        Some(id)
    }

    /// The core versions SY.admin has seen as targets, by hash: how it tells a hive ahead of the
    /// motherbee (a hive's installed record names no version).
    pub(crate) fn known_cores(&self) -> BTreeMap<String, String> {
        self.open
            .iter()
            .chain(self.closed.iter().rev())
            .filter_map(|update| update.target.core.as_ref())
            .filter_map(|core| Some((normalize_hash(&core.manifest_hash), core.version.clone()?)))
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// The decision, from a hive's /versions
// ---------------------------------------------------------------------------------------------

/// A sha256 as every record compares it: no `sha256:` prefix, lowercase.
pub(crate) fn normalize_hash(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("sha256:")
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

fn short_hash(raw: &str) -> String {
    normalize_hash(raw).chars().take(16).collect()
}

/// A text cut to [`TEXT_MAX`] characters.
fn cut(text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    if text.chars().count() <= TEXT_MAX {
        return text.to_string();
    }
    let mut out: String = text.chars().take(TEXT_MAX - 1).collect();
    out.push('…');
    out
}

fn text_of<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// The hash of the core a hive runs (`ops.py`'s `core_of`): what it recorded installing when it
/// says so, otherwise what dist delivered (a package install leaves no record). None for an install
/// recorded without its hash (a rollback that went wrong): never the dist hash then.
pub(crate) fn installed_core_hash(core: &Value) -> Option<String> {
    match core
        .get("installed")
        .filter(|installed| !installed.is_null())
    {
        Some(installed) => text_of(Some(installed), "manifest_hash").map(normalize_hash),
        None => text_of(Some(core), "manifest_hash").map(normalize_hash),
    }
}

/// The operator rolled this hive's core back (`core_rollback`, A-5): the one allowed divergence.
pub(crate) fn core_rolled_back(core: &Value) -> bool {
    text_of(core.get("installed"), "source")
        .is_some_and(|source| source.starts_with("core_rollback"))
}

/// The version of a core manifest: its components' version, one in a release. Several (a
/// manifest put together by hand) are named together, and compare as no version.
pub(crate) fn core_version(core: &Value) -> Option<String> {
    let mut versions: Vec<&str> = core
        .get("components")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|components| components.values())
        .filter_map(|component| text_of(Some(component), "version"))
        .collect();
    versions.sort_unstable();
    versions.dedup();
    (!versions.is_empty()).then(|| versions.join(","))
}

/// Whether release `a` is newer than `b` (dotted numbers, `0.1.123`); None when either is not one.
pub(crate) fn version_newer(a: &str, b: &str) -> Option<bool> {
    let parse = |version: &str| -> Option<Vec<u64>> {
        version
            .trim()
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect()
    };
    Some(parse(a)? > parse(b)?)
}

/// How a core update a hive ran last stands (`ops.py`'s `update_open_items`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Settle {
    /// Not over: its services restarting, or the orchestrator it installed has not settled it.
    NotOver,
    /// Over, with something open: components left out, a settle pending or with errors, binaries
    /// awaiting their update.
    Open(Vec<String>),
    Settled,
}

pub(crate) fn core_settle(core: &Value) -> Settle {
    let last = core.get("last_update").filter(|last| last.is_object());
    let settled = last
        .and_then(|last| last.get("settled"))
        .filter(|settled| settled.is_object());
    if text_of(last, "phase") == Some("restarting") {
        return Settle::NotOver;
    }
    let updated_orchestrator = last
        .and_then(|last| last.get("updated"))
        .and_then(Value::as_array)
        .is_some_and(|updated| updated.iter().any(|name| name == "sy-orchestrator"));
    if updated_orchestrator && settled.is_none() {
        return Settle::NotOver;
    }
    let names = |value: Option<&Value>| -> Vec<String> {
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .or_else(|| text_of(Some(item), "component").map(str::to_string))
            })
            .collect()
    };
    let mut items = Vec::new();
    let left_out = names(last.and_then(|last| last.get("left_out")));
    if !left_out.is_empty() {
        items.push(format!("left_out={}", left_out.join(",")));
    }
    if let Some(settled) = settled {
        if settled.get("settled").and_then(Value::as_bool) == Some(false) {
            items.push(format!(
                "settle pending={} errors={}",
                names(settled.get("pending")).join(","),
                names(settled.get("errors")).join("; ")
            ));
        }
    }
    let awaiting = core.get("awaiting_update").filter(|value| !value.is_null());
    if let Some(awaiting) = awaiting {
        let components = names(awaiting.get("components"));
        items.push(format!(
            "awaiting_update={}",
            if components.is_empty() {
                "?".to_string()
            } else {
                components.join(",")
            }
        ));
    }
    if items.is_empty() {
        Settle::Settled
    } else {
        Settle::Open(items)
    }
}

/// The hive's services are restarting onto a core update.
fn core_restarting(core: &Value) -> bool {
    text_of(core.get("last_update"), "phase") == Some("restarting")
}

/// The orchestrator takes category config: a release from 0.1.122 on, whose `/versions` names the
/// config update the hive applied last (`config.applied`, null before its first).
pub(crate) fn takes_category_config(snapshot: &Value) -> bool {
    snapshot
        .get("config")
        .and_then(Value::as_object)
        .is_some_and(|config| config.contains_key("applied"))
}

fn config_applied(snapshot: &Value, target: &ConfigTarget) -> bool {
    let applied = snapshot
        .get("config")
        .and_then(|config| config.get("applied"))
        .filter(|applied| applied.is_object());
    applied
        .and_then(|applied| applied.get("version"))
        .and_then(Value::as_u64)
        == Some(target.version)
        && text_of(applied, "sha256").map(normalize_hash) == Some(normalize_hash(&target.sha256))
}

fn config_edited_by_hand(snapshot: &Value) -> bool {
    text_of(snapshot.get("config"), "state") == Some("edited_by_hand")
}

/// The target from the motherbee's `/versions` (its `hive`): its core, the reference, and its
/// installation file while that file is a version it recorded (`current`).
pub(crate) fn target_of(motherbee: &Value) -> Target {
    let core = motherbee.get("core").filter(|core| core.is_object());
    let (core_target, core_note) = match core {
        Some(core) if text_of(Some(core), "status") == Some("ok") => {
            match installed_core_hash(core) {
                Some(hash) => (
                    Some(CoreTarget {
                        manifest_hash: hash,
                        version: core_version(core),
                    }),
                    None,
                ),
                None => (
                    None,
                    Some(
                        "the motherbee records a core install without its manifest hash"
                            .to_string(),
                    ),
                ),
            }
        }
        Some(core) => (
            None,
            Some(format!(
                "the motherbee's core manifest cannot be read: {}",
                text_of(Some(core), "message").unwrap_or("no reason given")
            )),
        ),
        None => (
            None,
            Some("the motherbee's /versions has no core section".to_string()),
        ),
    };
    let config = motherbee.get("config").filter(|config| config.is_object());
    let (config_target, config_note) = match config {
        None => (
            None,
            Some(
                "the motherbee's orchestrator reports no installation file: nothing to apply"
                    .to_string(),
            ),
        ),
        Some(config) => match (
            text_of(Some(config), "state"),
            config.get("version").and_then(Value::as_u64),
            text_of(Some(config), "sha256"),
        ) {
            (Some("current"), Some(version), Some(sha256)) => (
                Some(ConfigTarget {
                    version,
                    sha256: normalize_hash(sha256),
                }),
                None,
            ),
            (Some("edited_by_hand"), _, _) => (
                None,
                Some(
                    "the motherbee's installation file is edited by hand: restore it, or make it \
                     a version with set_installation_ai and replace_hand_edit (the operator's \
                     choice); until then no hive gets a config update"
                        .to_string(),
                ),
            ),
            (Some("pending"), _, _) => (
                None,
                Some(format!(
                    "the motherbee holds no installation file{}: nothing to apply",
                    text_of(Some(config), "not_written")
                        .map(|why| format!(" ({why})"))
                        .unwrap_or_default()
                )),
            ),
            (state, _, _) => (
                None,
                Some(format!(
                    "the motherbee's installation file is {}: no version to apply",
                    state.unwrap_or("not described")
                )),
            ),
        },
    };
    Target {
        core: core_target,
        core_note: core_note.map(cut),
        config: config_target,
        config_note: config_note.map(cut),
    }
}

/// The target after `set_installation_ai` wrote `version` (`sha256`): the motherbee's core, from
/// its `/versions` read before the write, and the version just written.
pub(crate) fn target_after_write(motherbee: &Value, version: u64, sha256: &str) -> Target {
    Target {
        config: Some(ConfigTarget {
            version,
            sha256: normalize_hash(sha256),
        }),
        config_note: None,
        ..target_of(motherbee)
    }
}

/// What a run does with one category of one hive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    OnTarget,
    /// Send the update.
    Act,
    /// Under way already (a core restarting or settling): followed, not sent again.
    InFlight(String),
    /// The config waits for the core: it goes once the core is on the target.
    AfterCore(String),
    /// Left as it is: why, and the per-hive action that moves it.
    Excluded {
        reason: String,
        next: String,
    },
    /// Not known: why.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decision {
    pub(crate) core: Step,
    pub(crate) config: Step,
}

/// What a run knows of one hive.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HiveFacts<'a> {
    pub(crate) hive: &'a str,
    pub(crate) is_motherbee: bool,
    /// Its `/versions` (`hive`), None when it did not answer.
    pub(crate) snapshot: Option<&'a Value>,
}

fn core_update_step(hive: &str, target: &Target) -> String {
    let hash = target
        .core
        .as_ref()
        .map(|core| normalize_hash(&core.manifest_hash))
        .unwrap_or_else(|| "<the motherbee's core manifest hash>".to_string());
    format!(
        "POST /hives/{hive}/update with {{\"category\": \"core\", \"manifest_version\": 0, \
         \"manifest_hash\": \"{hash}\"}}"
    )
}

fn config_update_step(hive: &str, target: &Target) -> String {
    match &target.config {
        Some(config) => format!(
            "POST /hives/{hive}/update with {{\"category\": \"config\", \"manifest_version\": {}, \
             \"manifest_hash\": \"{}\"}}",
            config.version, config.sha256
        ),
        None => format!(
            "POST /hives/{hive}/update with category config, the version and sha256 \
             get_installation shows"
        ),
    }
}

fn rolled_back_next(hive: &str, target: &Target) -> String {
    format!(
        "the operator's call: {} moves its core, or update_installation with include_rolled_back: \
         [\"{hive}\"]",
        core_update_step(hive, target)
    )
}

/// The decision for one hive (§6, the brief's exclusions), a pure function of its `/versions`, the
/// target, the run's kind and the hives an explicit run names in `include_rolled_back`. `known`
/// holds the core versions SY.admin saw as targets, by hash.
///
/// - The automatic step leaves alone, both categories, a hive the operator rolled back, one ahead
///   of the motherbee and one whose copy is edited by hand.
/// - An installation-wide run changes no rolled-back hive's core unless it names it; it still
///   applies the config (a config apply replaces no binary).
/// - No run applies config to a copy edited by hand.
/// - A hive whose orchestrator takes no category config (before 0.1.122) gets its core first.
/// - The motherbee is the core's reference: its core is always on the target.
pub(crate) fn decide(
    facts: HiveFacts<'_>,
    target: &Target,
    kind: RunKind,
    include_rolled_back: &[String],
    known: &BTreeMap<String, String>,
) -> Decision {
    let Some(snapshot) = facts.snapshot else {
        let why = "its /versions did not answer".to_string();
        return Decision {
            core: Step::Unknown(why.clone()),
            config: Step::Unknown(why),
        };
    };
    let hive = facts.hive;
    let core = snapshot.get("core").unwrap_or(&Value::Null);
    let installed = installed_core_hash(core);
    let rolled_back = !facts.is_motherbee && core_rolled_back(core);
    let installed_version = installed.as_ref().and_then(|hash| known.get(hash));
    let ahead = !facts.is_motherbee
        && installed.as_deref()
            != target
                .core
                .as_ref()
                .map(|core| normalize_hash(&core.manifest_hash))
                .as_deref()
        && match (
            installed_version,
            target.core.as_ref().and_then(|core| core.version.as_ref()),
        ) {
            (Some(version), Some(target_version)) => {
                version_newer(version, target_version) == Some(true)
            }
            _ => false,
        };
    let edited = config_edited_by_hand(snapshot);

    if kind == RunKind::Automatic {
        let left_alone = if rolled_back {
            Some((
                "the operator rolled its core back (core_rollback, the one allowed divergence): the \
                 automatic step leaves it alone"
                    .to_string(),
                rolled_back_next(hive, target),
            ))
        } else if ahead {
            Some((
                format!(
                    "its core ({}) is newer than the motherbee's ({}): the automatic step leaves \
                     it alone",
                    installed_version.map(String::as_str).unwrap_or("?"),
                    target
                        .core
                        .as_ref()
                        .and_then(|core| core.version.as_deref())
                        .unwrap_or("?")
                ),
                "update_installation moves it to the motherbee's release (the installation's \
                 rollback), or install its release on the motherbee"
                    .to_string(),
            ))
        } else if edited {
            Some((
                "its copy of the installation file is edited by hand: the automatic step leaves \
                 it alone"
                    .to_string(),
                format!(
                    "restore its copy to the motherbee's bytes (GET /versions shows it), then \
                     update_installation, or {}",
                    config_update_step(hive, target)
                ),
            ))
        } else {
            None
        };
        if let Some((reason, next)) = left_alone {
            let step = Step::Excluded { reason, next };
            return Decision {
                core: step.clone(),
                config: step,
            };
        }
    }

    let core_step = decide_core(
        facts,
        core,
        target,
        kind,
        include_rolled_back,
        &installed,
        rolled_back,
    );
    let config_step = decide_config(facts, snapshot, target, &core_step, edited);
    Decision {
        core: core_step,
        config: config_step,
    }
}

fn decide_core(
    facts: HiveFacts<'_>,
    core: &Value,
    target: &Target,
    kind: RunKind,
    include_rolled_back: &[String],
    installed: &Option<String>,
    rolled_back: bool,
) -> Step {
    if facts.is_motherbee {
        return Step::OnTarget;
    }
    let Some(core_target) = &target.core else {
        return Step::Unknown(
            target
                .core_note
                .clone()
                .unwrap_or_else(|| "the motherbee's core is not known".to_string()),
        );
    };
    if text_of(Some(core), "status") != Some("ok") {
        return Step::Unknown(format!(
            "its core manifest cannot be read: {}",
            text_of(Some(core), "message").unwrap_or("its /versions has no core section")
        ));
    }
    let left_alone = rolled_back
        && (kind == RunKind::Automatic
            || !include_rolled_back.iter().any(|named| named == facts.hive));
    let rolled_back_step = || Step::Excluded {
        reason:
            "the operator rolled its core back (core_rollback, the one allowed divergence): an \
                 installation-wide update leaves its core as it is unless it names the hive in \
                 include_rolled_back"
                .to_string(),
        next: rolled_back_next(facts.hive, target),
    };
    if installed.as_deref() == Some(normalize_hash(&core_target.manifest_hash).as_str()) {
        return match core_settle(core) {
            Settle::Settled => Step::OnTarget,
            Settle::NotOver => Step::InFlight(
                "its core update is not over: its services restart, or the orchestrator it \
                 installed has not settled it yet"
                    .to_string(),
            ),
            // On the target with something open: a core update installs what is pending.
            Settle::Open(_) if left_alone => rolled_back_step(),
            Settle::Open(_) => Step::Act,
        };
    }
    if left_alone {
        return rolled_back_step();
    }
    if core_restarting(core) {
        return Step::InFlight("its services are restarting onto a core update".to_string());
    }
    Step::Act
}

fn decide_config(
    facts: HiveFacts<'_>,
    snapshot: &Value,
    target: &Target,
    core_step: &Step,
    edited: bool,
) -> Step {
    let Some(config_target) = &target.config else {
        return Step::Excluded {
            reason: target
                .config_note
                .clone()
                .unwrap_or_else(|| "the motherbee holds no installation file to apply".to_string()),
            next: "get_installation shows the motherbee's file; once it is a version, \
                   update_installation applies it"
                .to_string(),
        };
    };
    if !takes_category_config(snapshot) {
        let why = "its orchestrator takes no category config (a release before 0.1.122)";
        return match core_step {
            Step::Act | Step::InFlight(_) => {
                Step::AfterCore(format!("{why}: its core update comes first"))
            }
            Step::Excluded { reason, next } => Step::Excluded {
                reason: format!("{why}, and its core is left as it is: {reason}"),
                next: next.clone(),
            },
            Step::Unknown(why) => Step::Unknown(why.clone()),
            Step::OnTarget | Step::AfterCore(_) => Step::Act,
        };
    }
    if edited {
        return Step::Excluded {
            reason: "its copy of the installation file is edited by hand (config.state \
                     edited_by_hand): no update applies config to it"
                .to_string(),
            next: format!(
                "restore its copy to the motherbee's bytes (GET /versions shows it), then \
                 update_installation, or {}",
                config_update_step(facts.hive, target)
            ),
        };
    }
    if config_applied(snapshot, config_target) {
        return Step::OnTarget;
    }
    match core_step {
        Step::Act | Step::InFlight(_) => Step::AfterCore(
            "its core first: the config goes once the core is on the target".to_string(),
        ),
        _ => Step::Act,
    }
}

// ---------------------------------------------------------------------------------------------
// The watch of a core update, from a hive's /versions
// ---------------------------------------------------------------------------------------------

/// Where a core update under way stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoreWatch {
    /// Still under way. `installed`: the hive runs the target; `open`: what its settle leaves open.
    Waiting { installed: bool, open: Vec<String> },
    /// On the target, settled, nothing open.
    Settled,
    /// The update ended without the target installed.
    Failed { code: String, detail: String },
}

/// The watch: `previous` is the hive's last core update before this one, so an older record is
/// never read as this one's (None: any record that ended counts).
pub(crate) fn watch_core(snapshot: &Value, target_hash: &str, previous: Option<&str>) -> CoreWatch {
    let core = snapshot.get("core").unwrap_or(&Value::Null);
    if installed_core_hash(core).as_deref() == Some(normalize_hash(target_hash).as_str()) {
        return match core_settle(core) {
            Settle::Settled => CoreWatch::Settled,
            Settle::NotOver => CoreWatch::Waiting {
                installed: true,
                open: Vec::new(),
            },
            Settle::Open(open) => CoreWatch::Waiting {
                installed: true,
                open,
            },
        };
    }
    let last = core.get("last_update").filter(|last| last.is_object());
    let this_one = previous.is_none_or(|previous| {
        last.and_then(|last| last.get("started_at_ms"))
            .map(|started| {
                started
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| started.to_string())
            })
            .as_deref()
            != Some(previous)
    });
    if this_one && text_of(last, "phase") == Some("done") {
        let errors: Vec<String> = last
            .and_then(|last| last.get("errors"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        return match text_of(last, "status") {
            Some("rollback") => CoreWatch::Failed {
                code: "CORE_UPDATE_ROLLED_BACK".to_string(),
                detail: format!(
                    "the core update's health gate failed and its binaries were rolled back: {}",
                    if errors.is_empty() {
                        "no reason recorded".to_string()
                    } else {
                        errors.join("; ")
                    }
                ),
            },
            _ => CoreWatch::Failed {
                code: "CORE_NOT_ON_TARGET".to_string(),
                detail: format!(
                    "its last core update ended and it runs {}, not the target {}",
                    installed_core_hash(core).unwrap_or_else(|| "an install it cannot name".into()),
                    short_hash(target_hash)
                ),
            },
        };
    }
    CoreWatch::Waiting {
        installed: false,
        open: Vec::new(),
    }
}

/// The hive's last core update before this one, as its record names it.
fn previous_update_of(snapshot: &Value) -> Option<String> {
    snapshot
        .get("core")
        .and_then(|core| core.get("last_update"))
        .and_then(|last| last.get("started_at_ms"))
        .map(|started| {
            started
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| started.to_string())
        })
}

// ---------------------------------------------------------------------------------------------
// The state, on disk
// ---------------------------------------------------------------------------------------------

/// The bounds of the machinery (environment overrides in the style of `JSR_ADMIN_*`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bounds {
    /// How long a run waits for a hive's orchestrator to answer before its core update
    /// (`JSR_ADMIN_INSTALLATION_ANSWER_TIMEOUT_SECS`, 60): the motherbee's router may not have the
    /// spokes back right after an install.
    pub(crate) answer: Duration,
    /// Between two reads while it waits.
    pub(crate) answer_poll: Duration,
    /// How long one update is asked again while `sync_pending` or unreachable
    /// (`JSR_ADMIN_INSTALLATION_APPLY_TIMEOUT_SECS`, 90).
    pub(crate) apply: Duration,
    /// The first wait before asking again, growing half again each time up to `max_wait`.
    pub(crate) first_wait: Duration,
    pub(crate) max_wait: Duration,
    /// One update's answer (`JSR_ADMIN_UPDATE_TIMEOUT_SECS`, 60).
    pub(crate) update_call: Duration,
    /// How long a core update is followed to its settle (`JSR_ADMIN_INSTALLATION_SETTLE_TIMEOUT_SECS`,
    /// 300).
    pub(crate) settle: Duration,
    /// The background task's look at a hive in flight (`JSR_ADMIN_INSTALLATION_FLIGHT_CHECK_SECS`,
    /// 3).
    pub(crate) flight_check: Duration,
    /// Its look at a hive not on the target, an unreachable one with a cheap read first
    /// (`JSR_ADMIN_INSTALLATION_RETRY_CHECK_SECS`, 30).
    pub(crate) retry_check: Duration,
    /// Its `/versions` read (`JSR_ADMIN_INSTALLATION_READ_TIMEOUT_SECS`, 10).
    pub(crate) read: Duration,
}

impl Bounds {
    pub(crate) fn from_env() -> Self {
        let secs = |name: &str, default: u64| {
            Duration::from_secs(env_timeout_secs(name).unwrap_or(default))
        };
        Bounds {
            answer: secs("JSR_ADMIN_INSTALLATION_ANSWER_TIMEOUT_SECS", 60),
            answer_poll: Duration::from_secs(3),
            apply: installation_apply_bound(),
            first_wait: super::INSTALLATION_APPLY_FIRST_WAIT,
            max_wait: super::INSTALLATION_APPLY_MAX_WAIT,
            update_call: super::admin_action_timeout("update"),
            settle: secs("JSR_ADMIN_INSTALLATION_SETTLE_TIMEOUT_SECS", 300),
            flight_check: secs("JSR_ADMIN_INSTALLATION_FLIGHT_CHECK_SECS", 3)
                .max(Duration::from_secs(1)),
            retry_check: secs("JSR_ADMIN_INSTALLATION_RETRY_CHECK_SECS", 30)
                .max(Duration::from_secs(1)),
            read: secs("JSR_ADMIN_INSTALLATION_READ_TIMEOUT_SECS", 10),
        }
    }

    /// The longest an explicit run's own pass takes on one hive: the wait for its answer, then a
    /// core update and a config update, each asked again up to the apply bound and its last
    /// answer an update's.
    pub(crate) fn explicit_pass(&self) -> Duration {
        self.answer + 2 * (self.apply + self.update_call)
    }
}

/// The installation's update: its state on disk, and what is being acted on now.
pub(crate) struct Updates {
    path: PathBuf,
    file: Mutex<UpdatesFile>,
    /// The hives a task acts on now: another run, or the background task, leaves them.
    busy: Mutex<HashSet<String>>,
    /// When the background task looks at each hive next (ms), in memory: a restart looks at once.
    due: Mutex<HashMap<String, u64>>,
    /// Wakes the background task: an update opened, or something to follow.
    wake: tokio::sync::Notify,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write `installation-update.json` atomically: a temporary file in the same directory, then a
/// rename.
fn persist(path: &Path, file: &UpdatesFile) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "installation-update.json has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    let temp = parent.join(format!(
        ".installation-update-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<(), String> {
        let data = serde_json::to_vec_pretty(file).map_err(|err| err.to_string())?;
        let mut out = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o640)
            .open(&temp)
            .map_err(|err| format!("create {}: {err}", temp.display()))?;
        out.write_all(&data)
            .map_err(|err| format!("write {}: {err}", temp.display()))?;
        out.sync_all()
            .map_err(|err| format!("sync {}: {err}", temp.display()))?;
        drop(out);
        fs::rename(&temp, path).map_err(|err| format!("replace {}: {err}", path.display()))?;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o640));
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// The state on disk: none is an empty one; one that does not parse is moved aside, with why.
fn load_file(path: &Path, now_ms: u64) -> (UpdatesFile, Option<String>) {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return (UpdatesFile::default(), None)
        }
        Err(err) => {
            return (
                UpdatesFile::default(),
                Some(format!("{} cannot be read ({err})", path.display())),
            )
        }
    };
    match serde_json::from_slice::<UpdatesFile>(&raw) {
        Ok(file) => (file, None),
        Err(err) => {
            let aside = path.with_extension(format!("json.unreadable-{now_ms}"));
            let moved = match fs::rename(path, &aside) {
                Ok(()) => aside.display().to_string(),
                Err(err) => format!("nowhere ({err})"),
            };
            (
                UpdatesFile::default(),
                Some(format!(
                    "{} does not parse ({err}); moved to {moved}",
                    path.display()
                )),
            )
        }
    }
}

/// A hive's busy mark, released when the task that holds it ends.
pub(crate) struct Claim<'a> {
    updates: &'a Updates,
    hive: String,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        lock(&self.updates.busy).remove(&self.hive);
    }
}

impl Updates {
    /// The state SY.admin's start finds, the open update resumed, every hive looked at at once. A
    /// core whose update answered stays in flight, and is followed; an update a previous process
    /// sent with no answer recorded (a config apply, a core update not answered yet) is
    /// `not_answered`: the hive may hold it, and the automatic step finishes it once.
    pub(crate) fn load(state_dir: &Path) -> Arc<Updates> {
        let path = updates_path(state_dir);
        let now = now_epoch_ms();
        let (mut file, note) = load_file(&path, now);
        if let Some(note) = note {
            tracing::warn!(note = %note, "installation update: its state starts empty");
        }
        if let Some(open) = file.open.as_mut() {
            for entry in open.hives.values_mut() {
                for slot in [&mut entry.core, &mut entry.config] {
                    let answered = slot
                        .outcome
                        .as_ref()
                        .is_some_and(|outcome| outcome.answered_ms.is_some());
                    if slot.state != Standing::InFlight || answered {
                        continue;
                    }
                    let mut outcome = Outcome::new(
                        OutcomeStatus::NotAnswered,
                        slot.outcome
                            .as_ref()
                            .map_or(RunKind::Explicit, |outcome| outcome.by),
                        slot.outcome
                            .as_ref()
                            .map_or(now, |outcome| outcome.started_at_ms),
                        now,
                    );
                    outcome.detail = Some(
                        "SY.admin restarted before its update answered; the hive may hold it"
                            .to_string(),
                    );
                    slot.state = Standing::Unknown;
                    slot.reason = Some("SY.admin restarted while its update ran".to_string());
                    slot.outcome = Some(outcome);
                }
            }
            tracing::info!(
                update = %open.id,
                target = %open.target.describe(),
                hives = open.hives.len(),
                "installation update: an open update resumed at SY.admin's start; its hives are \
                 checked again"
            );
        }
        Arc::new(Updates {
            path,
            file: Mutex::new(file),
            busy: Mutex::new(HashSet::new()),
            due: Mutex::new(HashMap::new()),
            wake: tokio::sync::Notify::new(),
        })
    }

    pub(crate) fn read<R>(&self, read: impl FnOnce(&UpdatesFile) -> R) -> R {
        read(&lock(&self.file))
    }

    /// A change of the state, written to disk at once when it changed anything.
    pub(crate) fn change<R>(&self, change: impl FnOnce(&mut UpdatesFile) -> R) -> R {
        let mut file = lock(&self.file);
        let before = file.clone();
        let result = change(&mut file);
        if *file != before {
            if let Err(err) = persist(&self.path, &file) {
                tracing::warn!(error = %err, "installation update: its state could not be written");
            }
        }
        result
    }

    fn claim(&self, hive: &str) -> Option<Claim<'_>> {
        lock(&self.busy).insert(hive.to_string()).then(|| Claim {
            updates: self,
            hive: hive.to_string(),
        })
    }

    /// The hive's mark, waited for up to `wait`: the background task holds it for a read; a task
    /// that acts on the hive holds it longer, and the caller then leaves the hive to it.
    async fn claim_within(&self, hive: &str, wait: Duration) -> Option<Claim<'_>> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(claim) = self.claim(hive) {
                return Some(claim);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn is_busy(&self, hive: &str) -> bool {
        lock(&self.busy).contains(hive)
    }

    fn due_now(&self, hive: &str, now_ms: u64) -> bool {
        lock(&self.due).get(hive).is_none_or(|due| *due <= now_ms)
    }

    fn look_again(&self, hive: &str, after: Duration) {
        lock(&self.due).insert(
            hive.to_string(),
            now_epoch_ms().saturating_add(after.as_millis() as u64),
        );
    }

    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Sets one hive's category in the update `id`, while it is the open one; logs an outcome on
    /// one line. False when the update is no longer open (a run with another target closed it).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(
        &self,
        id: &str,
        hive: &str,
        category: Category,
        state: Standing,
        reason: Option<String>,
        outcome: Option<Outcome>,
        next: Option<String>,
    ) -> bool {
        if let Some(outcome) = &outcome {
            log_outcome(id, hive, category, outcome);
        }
        let recorded = self.change(|file| {
            let Some(update) = file.open_mut(id) else {
                return false;
            };
            let slot = update
                .hives
                .entry(hive.to_string())
                .or_default()
                .category_mut(category);
            slot.state = state;
            slot.reason = reason.map(cut);
            if let Some(outcome) = outcome {
                slot.outcome = Some(Outcome {
                    detail: outcome.detail.map(cut),
                    ..outcome
                });
            }
            slot.next = if state == Standing::OnTarget {
                None
            } else {
                next.map(cut)
            };
            true
        });
        if !recorded {
            tracing::info!(
                update = %id,
                hive = %hive,
                category = category.as_str(),
                "installation update: an outcome of an update no longer open, not recorded"
            );
        }
        recorded
    }

    fn set_role(&self, id: &str, hive: &str, role: Option<&str>) {
        let Some(role) = role else {
            return;
        };
        self.change(|file| {
            if let Some(update) = file.open_mut(id) {
                update.hives.entry(hive.to_string()).or_default().role = Some(role.to_string());
            }
        });
    }

    /// The state of the update `id`, open or in the history.
    pub(crate) fn state_of(&self, id: &str) -> Option<UpdateState> {
        self.read(|file| {
            file.open
                .iter()
                .chain(file.closed.iter())
                .find(|update| update.id == id)
                .map(|update| update.state)
        })
    }

    /// Closes the open update once every hive is on the target, and says so.
    pub(crate) fn close_if_done(&self) -> Option<String> {
        let closed = self.change(|file| file.close_if_done(now_epoch_ms()));
        if let Some(id) = &closed {
            tracing::info!(update = %id, "installation update: done, every hive on the target");
        }
        closed
    }

    /// The summary `/versions` and `get_installation` carry: the current or last update, its
    /// target and each hive's state.
    pub(crate) fn summary(&self) -> Value {
        self.read(|file| match file.current() {
            Some(update) => json!({
                "id": update.id,
                "state": update.state,
                "opened_at_ms": update.opened_at_ms,
                "closed_at_ms": update.closed_at_ms,
                "target": target_view(&update.target),
                "hives": update
                    .hives
                    .iter()
                    .map(|(hive, entry)| {
                        (
                            hive.clone(),
                            json!({
                                "core": entry.core.state.as_str(),
                                "config": entry.config.state.as_str(),
                            }),
                        )
                    })
                    .collect::<serde_json::Map<String, Value>>(),
            }),
            None => Value::Null,
        })
    }

    /// `get_installation_update`'s answer.
    pub(crate) fn view(&self, history: bool) -> Value {
        self.read(|file| {
            let mut view = json!({
                "update": file.current().map(record_view),
                "open": file.open.is_some(),
            });
            if history {
                view["history"] = Value::Array(file.closed.iter().map(history_view).collect());
            }
            view
        })
    }
}

fn log_outcome(id: &str, hive: &str, category: Category, outcome: &Outcome) {
    let elapsed_ms = outcome.elapsed_ms();
    match outcome.status {
        OutcomeStatus::Error | OutcomeStatus::NotAnswered | OutcomeStatus::SyncPending => {
            tracing::warn!(
                update = %id,
                hive = %hive,
                category = category.as_str(),
                outcome = outcome.status.as_str(),
                by = outcome.by.as_str(),
                elapsed_ms,
                attempts = outcome.attempts,
                error_code = ?outcome.error_code,
                detail = ?outcome.detail,
                "installation update: outcome"
            )
        }
        _ => tracing::info!(
            update = %id,
            hive = %hive,
            category = category.as_str(),
            outcome = outcome.status.as_str(),
            by = outcome.by.as_str(),
            elapsed_ms,
            attempts = outcome.attempts,
            "installation update: outcome"
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// The views
// ---------------------------------------------------------------------------------------------

pub(crate) fn target_view(target: &Target) -> Value {
    json!({
        "core": target.core,
        "core_note": target.core_note,
        "config": target.config,
        "config_note": target.config_note,
    })
}

fn category_view(entry: &CategoryEntry) -> Value {
    let mut view = serde_json::to_value(entry).unwrap_or(Value::Null);
    if let (Some(outcome), Some(out)) = (&entry.outcome, view.get_mut("outcome")) {
        out["elapsed_ms"] = json!(outcome.elapsed_ms());
        if let Some(fields) = out.as_object_mut() {
            fields.remove("previous_update");
        }
    }
    view
}

/// The hives of an update not on the target, each with where it stands.
pub(crate) fn pending_of(update: &UpdateRecord) -> Vec<String> {
    update
        .hives
        .iter()
        .filter(|(_, entry)| !entry.on_target())
        .map(|(hive, entry)| {
            let part = |category: Category| {
                let slot = entry.category(category);
                (slot.state != Standing::OnTarget).then(|| {
                    match slot
                        .outcome
                        .as_ref()
                        .filter(|_| slot.state != Standing::Excluded)
                    {
                        Some(outcome) => format!(
                            "{} {} ({})",
                            category.as_str(),
                            slot.state.as_str(),
                            outcome.status.as_str()
                        ),
                        None => format!("{} {}", category.as_str(), slot.state.as_str()),
                    }
                })
            };
            let parts: Vec<String> = [part(Category::Core), part(Category::Config)]
                .into_iter()
                .flatten()
                .collect();
            format!("{hive} ({})", parts.join(", "))
        })
        .collect()
}

/// The full view of one update.
pub(crate) fn record_view(update: &UpdateRecord) -> Value {
    json!({
        "id": update.id,
        "state": update.state,
        "opened_at_ms": update.opened_at_ms,
        "closed_at_ms": update.closed_at_ms,
        "origin": update.origin,
        "reason": update.reason,
        "superseded_by": update.superseded_by,
        "target": target_view(&update.target),
        "hives": update
            .hives
            .iter()
            .map(|(hive, entry)| {
                (
                    hive.clone(),
                    json!({
                        "role": entry.role,
                        "core": category_view(&entry.core),
                        "config": category_view(&entry.config),
                        "automatic": entry.automatic,
                    }),
                )
            })
            .collect::<serde_json::Map<String, Value>>(),
        "pending": pending_of(update),
        "skipped": update.skipped,
    })
}

/// A closed update in the history: what it was and which hives it left off the target.
pub(crate) fn history_view(update: &UpdateRecord) -> Value {
    json!({
        "id": update.id,
        "state": update.state,
        "opened_at_ms": update.opened_at_ms,
        "closed_at_ms": update.closed_at_ms,
        "origin": update.origin,
        "reason": update.reason,
        "superseded_by": update.superseded_by,
        "target": {
            "core": update.target.core,
            "config": update.target.config,
        },
        "hives": update.hives.len(),
        "not_on_target": update
            .hives
            .iter()
            .filter(|(_, entry)| !entry.on_target())
            .map(|(hive, entry)| {
                (
                    hive.clone(),
                    json!({
                        "core": entry.core.state.as_str(),
                        "config": entry.config.state.as_str(),
                    }),
                )
            })
            .collect::<serde_json::Map<String, Value>>(),
    })
}

// ---------------------------------------------------------------------------------------------
// The mesh
// ---------------------------------------------------------------------------------------------

/// What the machinery asks of the mesh, behind a trait so the runs and the background task run
/// against a fake in the tests.
#[async_trait]
pub(crate) trait UpdateMesh: Send + Sync {
    /// One hive's `/versions` (`get_versions`' `hive`), through the motherbee's orchestrator.
    async fn versions(&self, hive: &str, timeout: Duration) -> Result<Value, String>;
    /// One `SYSTEM_UPDATE` to the hive's orchestrator (policy rule (2)): its answer, or why none
    /// came.
    async fn update(&self, hive: &str, payload: Value, timeout: Duration) -> Result<Value, String>;
    /// Whether the hive's orchestrator is connected now, from shared memory, asking no one: None
    /// when it cannot be read.
    fn orchestrator_connected(&self, hive: &str) -> Option<bool>;
    /// The hives an update reaches now, the motherbee and each one the registry calls connected
    /// (`connected_hives_with`), and the registry's others, named (`installation_skipped_hives`).
    fn hives(&self) -> (Vec<String>, Vec<Value>);
    /// The motherbee's name.
    fn motherbee(&self) -> &str;
}

/// The mesh SY.admin runs on.
pub(crate) struct LiveMesh {
    client: Arc<RouterDispatcher>,
    admin_hive: String,
    state_dir: PathBuf,
    connected_nodes: ConnectedNodes,
}

impl LiveMesh {
    pub(crate) fn of(ctx: &AdminContext) -> Arc<dyn UpdateMesh> {
        Arc::new(LiveMesh {
            client: Arc::clone(&ctx.rpc),
            admin_hive: ctx.hive_id.clone(),
            state_dir: ctx.state_dir.clone(),
            connected_nodes: Arc::clone(&ctx.connected_nodes),
        })
    }
}

#[async_trait]
impl UpdateMesh for LiveMesh {
    async fn versions(&self, hive: &str, timeout: Duration) -> Result<Value, String> {
        let action = "get_versions";
        let request = super::AdminRequest {
            action: action.to_string(),
            payload: super::normalize_admin_payload(action, json!({}), Some(hive)),
            target: format!("SY.orchestrator@{}", self.admin_hive),
            unicast: None,
        };
        let answer = super::send_admin_request(
            &self.client,
            request,
            timeout,
            &super::admin_node_name(&self.admin_hive),
        )
        .await
        .map_err(|err| err.to_string())?;
        if !super::payload_is_ok(&answer) {
            return Err(super::payload_error_detail(&answer).unwrap_or_else(|| answer.to_string()));
        }
        answer
            .get("hive")
            .filter(|hive| hive.is_object())
            .cloned()
            .ok_or_else(|| "its /versions answer holds no hive".to_string())
    }

    async fn update(&self, hive: &str, payload: Value, timeout: Duration) -> Result<Value, String> {
        super::send_system_request(
            &self.client,
            &format!("SY.orchestrator@{hive}"),
            "SYSTEM_UPDATE",
            "SYSTEM_UPDATE_RESPONSE",
            payload,
            timeout,
        )
        .await
        .map_err(|err| err.to_string())
    }

    fn orchestrator_connected(&self, hive: &str) -> Option<bool> {
        let name = format!("SY.orchestrator@{hive}");
        (self.connected_nodes)(&self.admin_hive)
            .ok()
            .map(|nodes| nodes.contains(&name))
    }

    fn hives(&self) -> (Vec<String>, Vec<Value>) {
        let registry = super::hive_registry_for(&self.state_dir);
        (
            super::connected_hives_with(&self.admin_hive, &registry),
            skipped_hives(super::installation_skipped_hives(&registry, false, None)),
        )
    }

    fn motherbee(&self) -> &str {
        &self.admin_hive
    }
}

/// The registry's hives whose join did not finish, as stage 3c names them, with what brings each
/// into the update.
fn skipped_hives(mut skipped: Vec<Value>) -> Vec<Value> {
    for hive in &mut skipped {
        hive["next"] = json!(
            "finish its join (add_hive again) or remove it (remove_hive); once it is connected, \
             update_installation brings it to the target"
        );
    }
    skipped
}

// ---------------------------------------------------------------------------------------------
// The runs
// ---------------------------------------------------------------------------------------------

/// How one hive's core update ended in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreEnd {
    /// Its services restart: the background task follows it, then applies the config.
    InFlight,
    /// On the target: the config may go now.
    Settled,
    /// It refused or failed, and its orchestrator answers.
    Failed,
    /// It did not answer.
    NotAnswered,
}

/// What a hive's `next` says for an update that did not get it to the target, by category and
/// error code.
fn next_after(hive: &str, category: Category, target: &Target, code: Option<&str>) -> String {
    match (category, code) {
        (Category::Core, Some("CORE_NOT_SETTLED")) => format!(
            "what the settle leaves open clears once dist holds the release's binaries; GET \
             /hives/{hive}/versions shows it (core.last_update, core.awaiting_update), and {} \
             installs what is pending",
            core_update_step(hive, target)
        ),
        (Category::Core, Some("CORE_UPDATE_ROLLED_BACK")) => format!(
            "its health gate refused the new release: see its units on {hive} (journalctl), then \
             update_installation, or {}",
            core_update_step(hive, target)
        ),
        (Category::Core, _) => format!(
            "GET /hives/{hive}/versions shows its core; update_installation, or {}, finishes it",
            core_update_step(hive, target)
        ),
        (Category::Config, code) => match &target.config {
            Some(config) => {
                super::installation_next_step(hive, code, config.version, &config.sha256)
            }
            None => "get_installation shows the motherbee's file".to_string(),
        },
    }
}

/// An apply's outcome, as an [`Outcome`], with how long it took.
fn applied_outcome(
    applied: &InstallationApplied,
    by: RunKind,
    started_at_ms: u64,
) -> (Outcome, Option<String>) {
    let now = now_epoch_ms();
    let mut outcome = Outcome::new(OutcomeStatus::Ok, by, started_at_ms, now);
    outcome.attempts = applied.attempts;
    let code = match &applied.outcome {
        InstallationApplyOutcome::Ok(answer) => {
            outcome.restarted =
                answer
                    .get("restarted")
                    .and_then(Value::as_array)
                    .map(|restarted| {
                        restarted
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    });
            None
        }
        InstallationApplyOutcome::SyncPending(answer) => {
            outcome.status = OutcomeStatus::SyncPending;
            outcome.detail = Some(format!(
                "its copy had not arrived at the bound: it holds version {} ({})",
                answer
                    .get("local_manifest_version")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "none".to_string()),
                text_of(Some(answer), "local_manifest_hash").unwrap_or("no hash")
            ));
            None
        }
        InstallationApplyOutcome::Error {
            code,
            detail,
            answer,
        } => {
            outcome.status = OutcomeStatus::Error;
            // A core update its health gate rolled back answers status rollback with its errors.
            let rolled_back = text_of(Some(answer), "status") == Some("rollback");
            let code = if rolled_back {
                "CORE_UPDATE_ROLLED_BACK".to_string()
            } else {
                code.clone()
            };
            outcome.error_code = Some(code.clone());
            outcome.detail = Some(detail.clone());
            Some(code)
        }
        InstallationApplyOutcome::NotAnswered(detail) => {
            outcome.status = OutcomeStatus::NotAnswered;
            outcome.detail = Some(detail.clone());
            None
        }
    };
    (outcome, code)
}

/// One update, asked again while `sync_pending` or unreachable (stage 3c's loop), within the
/// bound.
async fn ask(
    mesh: &dyn UpdateMesh,
    hive: &str,
    payload: &Value,
    bounds: &Bounds,
) -> InstallationApplied {
    apply_installation_on_hive(
        || {
            let payload = payload.clone();
            async move { mesh.update(hive, payload, bounds.update_call).await }
        },
        bounds.first_wait,
        bounds.max_wait,
        bounds.apply,
    )
    .await
}

/// The hive's `/versions`, read until its orchestrator answers, within `wait`.
async fn read_until_it_answers(
    mesh: &dyn UpdateMesh,
    hive: &str,
    wait: Duration,
    bounds: &Bounds,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let read = mesh
            .versions(hive, bounds.read.max(Duration::from_secs(1)))
            .await;
        match read {
            Ok(snapshot) => return Ok(snapshot),
            Err(why) if tokio::time::Instant::now() + bounds.answer_poll > deadline => {
                return Err(why)
            }
            Err(_) => tokio::time::sleep(bounds.answer_poll).await,
        }
    }
}

/// A step that is not an act, as the state it leaves.
fn record_step(updates: &Updates, id: &str, hive: &str, category: Category, step: &Step) {
    let (state, reason, next) = match step {
        Step::OnTarget => (Standing::OnTarget, None, None),
        Step::Act => return,
        Step::InFlight(why) => (Standing::InFlight, Some(why.clone()), None),
        Step::AfterCore(why) => (Standing::Pending, Some(why.clone()), None),
        Step::Excluded { reason, next } => {
            (Standing::Excluded, Some(reason.clone()), Some(next.clone()))
        }
        Step::Unknown(why) => (Standing::Unknown, Some(why.clone()), None),
    };
    updates.record(id, hive, category, state, reason, None, next);
}

/// An exclusion an explicit run or the automatic step made, as an outcome too.
fn record_exclusion(
    updates: &Updates,
    id: &str,
    hive: &str,
    category: Category,
    step: &Step,
    by: RunKind,
) {
    if let Step::Excluded { reason, next } = step {
        let now = now_epoch_ms();
        let mut outcome = Outcome::new(OutcomeStatus::Excluded, by, now, now);
        outcome.detail = Some(reason.clone());
        updates.record(
            id,
            hive,
            category,
            Standing::Excluded,
            Some(reason.clone()),
            Some(outcome),
            Some(next.clone()),
        );
    }
}

/// One explicit run's pass on one hive, once no other task acts on it: [`run_claimed_hive`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_hive(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    kind: RunKind,
    include_rolled_back: &[String],
    first: Option<Value>,
    bounds: &Bounds,
) {
    let Some(_claim) = updates.claim_within(hive, bounds.read).await else {
        tracing::info!(
            update = %id,
            hive = %hive,
            "installation update: another task acts on this hive now; this run leaves it to it"
        );
        return;
    };
    run_claimed_hive(
        updates,
        mesh,
        id,
        hive,
        target,
        kind,
        include_rolled_back,
        first,
        bounds,
    )
    .await;
}

/// One run on one hive (an explicit run's pass, or the automatic step), whose mark the caller
/// holds: its `/versions` (`first`, else read until it answers), the decision, the core, then the
/// config.
#[allow(clippy::too_many_arguments)]
async fn run_claimed_hive(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    kind: RunKind,
    include_rolled_back: &[String],
    first: Option<Value>,
    bounds: &Bounds,
) {
    let started = now_epoch_ms();
    let is_motherbee = hive == mesh.motherbee();
    let snapshot = match first {
        Some(snapshot) => Ok(snapshot),
        None => {
            let wait = match kind {
                RunKind::Explicit => bounds.answer,
                RunKind::Automatic => Duration::ZERO,
            };
            read_until_it_answers(mesh, hive, wait, bounds).await
        }
    };
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(why) => {
            for category in [Category::Core, Category::Config] {
                let mut outcome =
                    Outcome::new(OutcomeStatus::NotAnswered, kind, started, now_epoch_ms());
                outcome.detail = Some(format!("its /versions did not answer: {why}"));
                updates.record(
                    id,
                    hive,
                    category,
                    Standing::Pending,
                    Some("it did not answer".to_string()),
                    Some(outcome),
                    Some(format!(
                        "once it answers, the automatic step finishes it (once); or \
                         update_installation. {}",
                        next_after(hive, category, target, None)
                    )),
                );
            }
            return;
        }
    };
    updates.set_role(id, hive, text_of(Some(&snapshot), "role"));
    let known = updates.read(UpdatesFile::known_cores);
    let decision = decide(
        HiveFacts {
            hive,
            is_motherbee,
            snapshot: Some(&snapshot),
        },
        target,
        kind,
        include_rolled_back,
        &known,
    );
    for (category, step) in [
        (Category::Core, &decision.core),
        (Category::Config, &decision.config),
    ] {
        match step {
            Step::Excluded { .. } => record_exclusion(updates, id, hive, category, step, kind),
            // Recorded below, with its outcome.
            Step::InFlight(_) if category == Category::Core => {}
            _ => record_step(updates, id, hive, category, step),
        }
    }

    let core_end = match decision.core {
        Step::Act => {
            Some(core_update(updates, mesh, id, hive, target, kind, &snapshot, bounds).await)
        }
        Step::InFlight(_) => {
            // Under way already, not by this run: followed from its next record.
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::InFlight,
                Some("its core update is under way".to_string()),
                Some(Outcome::new(
                    OutcomeStatus::Restarting,
                    kind,
                    started,
                    now_epoch_ms(),
                )),
                None,
            );
            updates.look_again(hive, Duration::ZERO);
            Some(CoreEnd::InFlight)
        }
        _ => None,
    };
    let config_goes = match (&decision.config, core_end) {
        (Step::Act, None | Some(CoreEnd::Settled | CoreEnd::Failed)) => true,
        (Step::AfterCore(_), Some(CoreEnd::Settled)) => true,
        // Its core failed and its orchestrator answers: one config version everywhere.
        (Step::AfterCore(_), Some(CoreEnd::Failed)) => takes_category_config(&snapshot),
        _ => false,
    };
    if config_goes {
        config_update(updates, mesh, id, hive, target, kind, bounds).await;
    } else if core_end == Some(CoreEnd::NotAnswered) {
        let now = now_epoch_ms();
        let mut outcome = Outcome::new(OutcomeStatus::NotAnswered, kind, started, now);
        outcome.detail = Some("its core update did not answer".to_string());
        updates.record(
            id,
            hive,
            Category::Config,
            Standing::Pending,
            Some("after its core".to_string()),
            Some(outcome),
            Some(next_after(hive, Category::Config, target, None)),
        );
    }
}

/// The core update of one hive: sent, asked again while `sync_pending` or unreachable, and, when its
/// services restart, left in flight for the background task to follow.
#[allow(clippy::too_many_arguments)]
async fn core_update(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    kind: RunKind,
    snapshot: &Value,
    bounds: &Bounds,
) -> CoreEnd {
    let Some(core_target) = &target.core else {
        return CoreEnd::Failed;
    };
    let started = now_epoch_ms();
    let previous = previous_update_of(snapshot);
    let mut sending = Outcome::new(OutcomeStatus::Restarting, kind, started, started);
    sending.previous_update = previous.clone();
    updates.change(|file| {
        if let Some(update) = file.open_mut(id) {
            let slot = update
                .hives
                .entry(hive.to_string())
                .or_default()
                .category_mut(Category::Core);
            slot.state = Standing::InFlight;
            slot.reason = Some("its core update is being sent".to_string());
            slot.outcome = None;
            slot.next = None;
        }
    });
    let payload = json!({
        "category": "core",
        "manifest_version": 0,
        "manifest_hash": core_target.manifest_hash,
    });
    let applied = ask(mesh, hive, &payload, bounds).await;
    let (mut outcome, code) = applied_outcome(&applied, kind, started);
    outcome.previous_update = previous;
    match &applied.outcome {
        InstallationApplyOutcome::Ok(answer) => {
            outcome.answered_ms = Some(outcome.elapsed_ms());
            outcome.restarted = None;
            if text_of(Some(answer), "phase") == Some("restarting") {
                outcome.status = OutcomeStatus::Restarting;
                updates.record(
                    id,
                    hive,
                    Category::Core,
                    Standing::InFlight,
                    Some("its services restart onto the new release".to_string()),
                    Some(outcome),
                    None,
                );
                updates.look_again(hive, bounds.flight_check);
                updates.wake();
                return CoreEnd::InFlight;
            }
            // Nothing to restart (FINDINGS A-190): it is on the target once its record says so.
            match mesh.versions(hive, bounds.read).await {
                Ok(after) => {
                    match watch_core(
                        &after,
                        &core_target.manifest_hash,
                        outcome.previous_update.as_deref(),
                    ) {
                        CoreWatch::Settled => {
                            outcome.status = OutcomeStatus::Settled;
                            outcome.at_ms = now_epoch_ms();
                            outcome.installed_ms = outcome.answered_ms;
                            outcome.settled_ms = Some(outcome.elapsed_ms());
                            updates.record(
                                id,
                                hive,
                                Category::Core,
                                Standing::OnTarget,
                                None,
                                Some(outcome),
                                None,
                            );
                            CoreEnd::Settled
                        }
                        _ => {
                            outcome.status = OutcomeStatus::Ok;
                            updates.record(
                                id,
                                hive,
                                Category::Core,
                                Standing::InFlight,
                                Some(
                                    "its core update answered; its settle is followed".to_string(),
                                ),
                                Some(outcome),
                                None,
                            );
                            updates.look_again(hive, Duration::ZERO);
                            updates.wake();
                            CoreEnd::InFlight
                        }
                    }
                }
                Err(_) => {
                    outcome.status = OutcomeStatus::Ok;
                    updates.record(
                        id,
                        hive,
                        Category::Core,
                        Standing::InFlight,
                        Some("its core update answered; its settle is followed".to_string()),
                        Some(outcome),
                        None,
                    );
                    updates.look_again(hive, bounds.flight_check);
                    updates.wake();
                    CoreEnd::InFlight
                }
            }
        }
        InstallationApplyOutcome::NotAnswered(_) => {
            let next = next_after(hive, Category::Core, target, None);
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::Pending,
                Some("it did not answer its core update".to_string()),
                Some(outcome),
                Some(next),
            );
            CoreEnd::NotAnswered
        }
        InstallationApplyOutcome::SyncPending(_) => {
            outcome.detail = Some(
                "dist had not brought the motherbee's core manifest at the bound (GET /versions: \
                 core.manifest_hash)"
                    .to_string(),
            );
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::Pending,
                Some("its dist does not hold the release yet".to_string()),
                Some(outcome),
                Some(next_after(hive, Category::Core, target, None)),
            );
            CoreEnd::Failed
        }
        InstallationApplyOutcome::Error { .. } => {
            let next = next_after(hive, Category::Core, target, code.as_deref());
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::Pending,
                Some("its core update failed".to_string()),
                Some(outcome),
                Some(next),
            );
            CoreEnd::Failed
        }
    }
}

/// The config update of one hive: the version applied, asked again while its copy has not arrived.
async fn config_update(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    kind: RunKind,
    bounds: &Bounds,
) {
    let Some(config) = &target.config else {
        return;
    };
    let started = now_epoch_ms();
    updates.change(|file| {
        if let Some(update) = file.open_mut(id) {
            let slot = update
                .hives
                .entry(hive.to_string())
                .or_default()
                .category_mut(Category::Config);
            slot.state = Standing::InFlight;
            slot.reason = Some("its config update is being applied".to_string());
            slot.outcome = None;
            slot.next = None;
        }
    });
    let payload = json!({
        "category": "config",
        "manifest_version": config.version,
        "manifest_hash": config.sha256,
    });
    let applied = ask(mesh, hive, &payload, bounds).await;
    record_config_applied(updates, id, hive, target, &applied, kind, started);
}

/// One hive's config apply, recorded: on the target when it applied it; otherwise pending, with
/// what finishes it. `set_installation_ai`'s apply records here too.
pub(crate) fn record_config_applied(
    updates: &Updates,
    id: &str,
    hive: &str,
    target: &Target,
    applied: &InstallationApplied,
    kind: RunKind,
    started_at_ms: u64,
) {
    let (outcome, code) = applied_outcome(applied, kind, started_at_ms);
    match outcome.status {
        OutcomeStatus::Ok => {
            updates.record(
                id,
                hive,
                Category::Config,
                Standing::OnTarget,
                None,
                Some(outcome),
                None,
            );
        }
        status => {
            let reason = match status {
                OutcomeStatus::SyncPending => "its copy had not arrived at the bound",
                OutcomeStatus::NotAnswered => "it did not answer",
                _ => "its config update failed",
            };
            let next = next_after(hive, Category::Config, target, code.as_deref());
            updates.record(
                id,
                hive,
                Category::Config,
                Standing::Pending,
                Some(reason.to_string()),
                Some(outcome),
                Some(next),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The background task
// ---------------------------------------------------------------------------------------------

/// What the background task does with one hive of the open update now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    /// Its core is in flight: follow it to its settle, then its config.
    Follow,
    /// It did not answer an attempt of this update: once it answers, the automatic step.
    Automatic,
    /// Not on the target, nothing in flight: read it again, act on nothing.
    Recheck,
    Nothing,
}

pub(crate) fn look_at(entry: &HiveEntry) -> Look {
    if entry.on_target() {
        return Look::Nothing;
    }
    if entry.core.state == Standing::InFlight {
        return Look::Follow;
    }
    if entry.awaits_automatic_step() {
        return Look::Automatic;
    }
    Look::Recheck
}

/// The background task: while an update is open, every `flight_check` it looks at each hive whose
/// time came, each in a task of its own; the motherbee only, as SY.admin runs nowhere else.
pub(crate) async fn run_background(
    updates: Arc<Updates>,
    mesh: Arc<dyn UpdateMesh>,
    bounds: Bounds,
) {
    loop {
        let open = updates.read(|file| file.open.is_some());
        if open {
            tick(&updates, &mesh, &bounds).await;
        }
        let pause = if open {
            bounds.flight_check
        } else {
            bounds.retry_check
        };
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            _ = updates.wake.notified() => {}
        }
    }
}

/// One look at the open update: each hive due is followed, finished by the automatic step, or read
/// again, in a task of its own; then the update closes if every hive is on the target.
pub(crate) async fn tick(updates: &Arc<Updates>, mesh: &Arc<dyn UpdateMesh>, bounds: &Bounds) {
    let Some((id, target)) = updates.read(|file| {
        file.open
            .as_ref()
            .map(|update| (update.id.clone(), update.target.clone()))
    }) else {
        return;
    };
    let (hives, skipped) = mesh.hives();
    updates.change(|file| file.refresh_hives(&id, &hives, skipped));
    let now = now_epoch_ms();
    let looks: Vec<(String, Look)> = updates.read(|file| {
        file.open
            .as_ref()
            .map(|update| {
                update
                    .hives
                    .iter()
                    .map(|(hive, entry)| (hive.clone(), look_at(entry)))
                    .collect()
            })
            .unwrap_or_default()
    });
    for (hive, look) in looks {
        if look == Look::Nothing || updates.is_busy(&hive) || !updates.due_now(&hive, now) {
            continue;
        }
        let period = match look {
            Look::Follow => bounds.flight_check,
            _ => bounds.retry_check,
        };
        updates.look_again(&hive, period);
        let (updates, mesh, id, target, bounds) = (
            Arc::clone(updates),
            Arc::clone(mesh),
            id.clone(),
            target.clone(),
            *bounds,
        );
        tokio::spawn(async move {
            match look {
                Look::Follow => follow(&updates, mesh.as_ref(), &id, &hive, &target, &bounds).await,
                Look::Automatic => {
                    automatic_step(&updates, mesh.as_ref(), &id, &hive, &target, &bounds).await
                }
                Look::Recheck => {
                    recheck(&updates, mesh.as_ref(), &id, &hive, &target, &bounds).await
                }
                Look::Nothing => {}
            }
            updates.close_if_done();
        });
    }
    updates.close_if_done();
}

/// A hive in flight: its `/versions` read again; once settled, its config goes (the run that sent
/// the core goes on); past the settle bound, what stopped it.
pub(crate) async fn follow(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    bounds: &Bounds,
) {
    let Some(core_target) = &target.core else {
        return;
    };
    let Some(_claim) = updates.claim(hive) else {
        return;
    };
    let Some(mut outcome) = updates.read(|file| {
        file.open
            .as_ref()
            .filter(|update| update.id == id)
            .and_then(|update| update.hives.get(hive))
            .filter(|entry| entry.core.state == Standing::InFlight)
            .map(|entry| {
                entry.core.outcome.clone().unwrap_or_else(|| {
                    let now = now_epoch_ms();
                    Outcome::new(OutcomeStatus::Restarting, RunKind::Explicit, now, now)
                })
            })
    }) else {
        return;
    };
    let read = mesh.versions(hive, bounds.read).await;
    let now = now_epoch_ms();
    let past_bound = now.saturating_sub(outcome.started_at_ms) > bounds.settle.as_millis() as u64;
    let watch = read.as_ref().ok().map(|snapshot| {
        watch_core(
            snapshot,
            &core_target.manifest_hash,
            outcome.previous_update.as_deref(),
        )
    });
    outcome.at_ms = now;
    match watch {
        Some(CoreWatch::Settled) => {
            if outcome.installed_ms.is_none() {
                outcome.installed_ms = Some(outcome.elapsed_ms());
            }
            outcome.settled_ms = Some(outcome.elapsed_ms());
            outcome.status = OutcomeStatus::Settled;
            let by = outcome.by;
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::OnTarget,
                None,
                Some(outcome),
                None,
            );
            // The core is on the target: the config of the same run goes now.
            let Ok(snapshot) = read else {
                return;
            };
            let known = updates.read(UpdatesFile::known_cores);
            let decision = decide(
                HiveFacts {
                    hive,
                    is_motherbee: hive == mesh.motherbee(),
                    snapshot: Some(&snapshot),
                },
                target,
                by,
                &[],
                &known,
            );
            match &decision.config {
                Step::Act | Step::AfterCore(_) => {
                    config_update(updates, mesh, id, hive, target, by, bounds).await
                }
                Step::Excluded { .. } => {
                    record_exclusion(updates, id, hive, Category::Config, &decision.config, by)
                }
                step => record_step(updates, id, hive, Category::Config, step),
            }
        }
        Some(CoreWatch::Failed { code, detail }) => {
            outcome.status = OutcomeStatus::Error;
            outcome.error_code = Some(code.clone());
            outcome.detail = Some(detail);
            let next = next_after(hive, Category::Core, target, Some(&code));
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::Pending,
                Some("its core update ended off the target".to_string()),
                Some(outcome),
                Some(next),
            );
        }
        Some(CoreWatch::Waiting { installed, open }) => {
            let first_installed = installed && outcome.installed_ms.is_none();
            if first_installed {
                outcome.installed_ms = Some(outcome.elapsed_ms());
            }
            if past_bound {
                outcome.status = OutcomeStatus::Error;
                let (code, detail) = if installed {
                    (
                        "CORE_NOT_SETTLED",
                        format!(
                            "it runs the target, and its settle was not over within {} s: {}",
                            bounds.settle.as_secs(),
                            if open.is_empty() {
                                "the orchestrator it installed has not settled it".to_string()
                            } else {
                                open.join("; ")
                            }
                        ),
                    )
                } else {
                    (
                        "CORE_NOT_INSTALLED",
                        format!(
                            "it did not run the target within {} s of its core update",
                            bounds.settle.as_secs()
                        ),
                    )
                };
                outcome.error_code = Some(code.to_string());
                outcome.detail = Some(detail);
                let next = next_after(hive, Category::Core, target, Some(code));
                updates.record(
                    id,
                    hive,
                    Category::Core,
                    Standing::Pending,
                    Some("its core update was not over within the bound".to_string()),
                    Some(outcome),
                    Some(next),
                );
            } else if first_installed {
                tracing::info!(
                    update = %id,
                    hive = %hive,
                    category = "core",
                    outcome = "installed",
                    elapsed_ms = outcome.elapsed_ms(),
                    "installation update: outcome"
                );
                updates.change(|file| {
                    if let Some(slot) = file
                        .open_mut(id)
                        .and_then(|update| update.hives.get_mut(hive))
                        .map(|entry| &mut entry.core)
                    {
                        slot.outcome = Some(outcome);
                        slot.reason = Some(
                            "it runs the target; the orchestrator it installed settles it"
                                .to_string(),
                        );
                    }
                });
            }
        }
        None if past_bound => {
            outcome.status = OutcomeStatus::NotAnswered;
            outcome.detail = Some(format!(
                "its /versions did not answer within {} s of its core update: {}",
                bounds.settle.as_secs(),
                read.err().unwrap_or_default()
            ));
            let next = next_after(hive, Category::Core, target, None);
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::Pending,
                Some("it did not answer after its core update".to_string()),
                Some(outcome),
                Some(next),
            );
        }
        // Not readable while its orchestrator restarts: looked at again.
        None => {}
    }
}

/// The automatic step (§6): a hive that did not answer an attempt of this update and now does gets
/// the update finished, once per hive and target, by the same run as an explicit one's, never for
/// an excluded hive (the decision leaves those alone). A cheap read of shared memory first: only a
/// hive whose orchestrator is connected is asked.
pub(crate) async fn automatic_step(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    bounds: &Bounds,
) {
    if mesh.orchestrator_connected(hive) == Some(false) {
        return;
    }
    let Some(_claim) = updates.claim(hive) else {
        return;
    };
    let Ok(snapshot) = mesh.versions(hive, bounds.read).await else {
        return;
    };
    // Once per hive and target: the attempt is recorded before it acts.
    let started = now_epoch_ms();
    let first = updates.change(|file| {
        let Some(entry) = file
            .open_mut(id)
            .and_then(|update| update.hives.get_mut(hive))
        else {
            return false;
        };
        if !entry.awaits_automatic_step() {
            return false;
        }
        entry.automatic = Some(AutomaticAttempt {
            started_at_ms: started,
            finished_at_ms: None,
            result: None,
        });
        true
    });
    if !first {
        return;
    }
    tracing::info!(
        update = %id,
        hive = %hive,
        "installation update: automatic step, the hive answers again: the update is finished for it \
         (once)"
    );
    run_claimed_hive(
        updates,
        mesh,
        id,
        hive,
        target,
        RunKind::Automatic,
        &[],
        Some(snapshot),
        bounds,
    )
    .await;
    let finished = now_epoch_ms();
    let result = updates.change(|file| {
        let entry = file
            .open_mut(id)
            .and_then(|update| update.hives.get_mut(hive))?;
        let result = format!(
            "core {}, config {}",
            entry.core.state.as_str(),
            entry.config.state.as_str()
        );
        if let Some(attempt) = entry.automatic.as_mut() {
            attempt.finished_at_ms = Some(finished);
            attempt.result = Some(result.clone());
        }
        Some(result)
    });
    tracing::info!(
        update = %id,
        hive = %hive,
        result = %result.unwrap_or_else(|| "the update is no longer open".to_string()),
        elapsed_ms = finished.saturating_sub(started),
        "installation update: automatic step ended"
    );
}

/// A hive not on the target, nothing in flight: read again, its states set as a run would decide
/// them, nothing sent. An unreachable hive is read only when shared memory shows its orchestrator.
pub(crate) async fn recheck(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    bounds: &Bounds,
) {
    let unknown = updates.read(|file| {
        file.open
            .as_ref()
            .and_then(|update| update.hives.get(hive))
            .is_some_and(|entry| {
                entry.core.state == Standing::Unknown || entry.config.state == Standing::Unknown
            })
    });
    if !unknown && mesh.orchestrator_connected(hive) == Some(false) {
        return;
    }
    let Some(_claim) = updates.claim(hive) else {
        return;
    };
    let Ok(snapshot) = mesh.versions(hive, bounds.read).await else {
        return;
    };
    updates.set_role(id, hive, text_of(Some(&snapshot), "role"));
    let known = updates.read(UpdatesFile::known_cores);
    let decision = decide(
        HiveFacts {
            hive,
            is_motherbee: hive == mesh.motherbee(),
            snapshot: Some(&snapshot),
        },
        target,
        RunKind::Explicit,
        &[],
        &known,
    );
    for (category, step) in [
        (Category::Core, &decision.core),
        (Category::Config, &decision.config),
    ] {
        let current = updates.read(|file| {
            file.open
                .as_ref()
                .filter(|update| update.id == id)
                .and_then(|update| update.hives.get(hive))
                .map(|entry| entry.category(category).clone())
        });
        let Some(current) = current else {
            return;
        };
        match step {
            // Left as it is, with its last outcome: what a run sent, or nothing yet.
            Step::Act | Step::AfterCore(_) => {
                if current.state == Standing::Pending && current.outcome.is_some() {
                    continue;
                }
                updates.record(
                    id,
                    hive,
                    category,
                    Standing::Pending,
                    Some(match step {
                        Step::AfterCore(why) => why.clone(),
                        _ => "not on the target".to_string(),
                    }),
                    None,
                    Some(format!(
                        "update_installation brings it to the target, or {}",
                        match category {
                            Category::Core => core_update_step(hive, target),
                            Category::Config => config_update_step(hive, target),
                        }
                    )),
                );
            }
            Step::InFlight(_) if category == Category::Core => {
                let now = now_epoch_ms();
                updates.record(
                    id,
                    hive,
                    Category::Core,
                    Standing::InFlight,
                    Some("its core update is under way".to_string()),
                    Some(Outcome::new(
                        OutcomeStatus::Restarting,
                        RunKind::Explicit,
                        now,
                        now,
                    )),
                    None,
                );
                updates.look_again(hive, Duration::ZERO);
            }
            step => record_step(updates, id, hive, category, step),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The explicit run
// ---------------------------------------------------------------------------------------------

/// What an explicit run left.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RunEnd {
    /// No update was open, and every hive was on the target: nothing to do, nothing recorded.
    NothingToDo { target: Target, hives: Vec<String> },
    /// The update it opened or resumed, as it stands at its answer.
    Update {
        update: Box<UpdateRecord>,
        opening: Opening,
    },
}

/// `update_installation`'s run, after the motherbee's `/versions` gave the target: every hive
/// read, the update opened or resumed (or nothing to do), then every hive not on the target acted
/// on in parallel, each in a task that goes on past `within` (the background task follows what it
/// leaves in flight).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_explicit(
    updates: &Arc<Updates>,
    mesh: &Arc<dyn UpdateMesh>,
    origin: &str,
    reason: Option<&str>,
    include_rolled_back: &[String],
    motherbee: &Value,
    target: &Target,
    bounds: &Bounds,
    within: Duration,
) -> RunEnd {
    let (hives, skipped) = mesh.hives();
    let motherbee_name = mesh.motherbee().to_string();
    // Every hive's /versions, once, at once: the motherbee's is in hand.
    let reads = futures::future::join_all(hives.iter().map(|hive| {
        let mesh = Arc::clone(mesh);
        let hive = hive.clone();
        let motherbee_name = motherbee_name.clone();
        let motherbee = motherbee.clone();
        async move {
            if hive == motherbee_name {
                return Some(motherbee);
            }
            mesh.versions(&hive, bounds.read).await.ok()
        }
    }))
    .await;
    let known = updates.read(UpdatesFile::known_cores);
    let all_on_target = hives.iter().zip(&reads).all(|(hive, snapshot)| {
        let decision = decide(
            HiveFacts {
                hive,
                is_motherbee: *hive == motherbee_name,
                snapshot: snapshot.as_ref(),
            },
            target,
            RunKind::Explicit,
            include_rolled_back,
            &known,
        );
        decision.core == Step::OnTarget && decision.config == Step::OnTarget
    });
    let open = updates.read(|file| file.open.is_some());
    if all_on_target && !open {
        return RunEnd::NothingToDo {
            target: target.clone(),
            hives,
        };
    }
    let now = now_epoch_ms();
    let (id, opening) = updates.change(|file| {
        let opened = file.open_or_resume(target, origin, reason, now);
        file.refresh_hives(&opened.0, &hives, skipped);
        opened
    });
    match &opening {
        Opening::Opened => tracing::info!(
            update = %id,
            origin = %origin,
            target = %target.describe(),
            "installation update: opened"
        ),
        Opening::Resumed => tracing::info!(
            update = %id,
            origin = %origin,
            "installation update: resumed, the same target"
        ),
        Opening::Superseded(previous) => tracing::info!(
            update = %id,
            superseded = %previous,
            origin = %origin,
            target = %target.describe(),
            "installation update: opened, the open one superseded (another target)"
        ),
    }
    updates.wake();
    let tasks: Vec<tokio::task::JoinHandle<()>> = hives
        .iter()
        .cloned()
        .zip(reads)
        .map(|(hive, first)| {
            let (updates, mesh, id, target, include, bounds) = (
                Arc::clone(updates),
                Arc::clone(mesh),
                id.clone(),
                target.clone(),
                include_rolled_back.to_vec(),
                *bounds,
            );
            tokio::spawn(async move {
                run_hive(
                    &updates,
                    mesh.as_ref(),
                    &id,
                    &hive,
                    &target,
                    RunKind::Explicit,
                    &include,
                    first,
                    &bounds,
                )
                .await;
            })
        })
        .collect();
    let _ = tokio::time::timeout(within, futures::future::join_all(tasks)).await;
    updates.close_if_done();
    let update = updates.read(|file| {
        file.open
            .iter()
            .chain(file.closed.iter())
            .find(|update| update.id == id)
            .cloned()
    });
    match update {
        Some(update) => RunEnd::Update {
            update: Box::new(update),
            opening,
        },
        None => RunEnd::NothingToDo {
            target: target.clone(),
            hives,
        },
    }
}

// ---------------------------------------------------------------------------------------------
// The actions
// ---------------------------------------------------------------------------------------------

/// `update_installation`'s body, `{reason?, include_rolled_back?}`. A null field is an absent one:
/// the executor sends null for an optional field a plan leaves out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UpdateRequest {
    pub(crate) reason: Option<String>,
    pub(crate) include_rolled_back: Vec<String>,
}

pub(crate) fn parse_update_request(params: &Value) -> Result<UpdateRequest, String> {
    let fields = match params {
        Value::Null => return Ok(UpdateRequest::default()),
        Value::Object(fields) => fields,
        _ => return Err("the body must be an object: {reason?, include_rolled_back?}".to_string()),
    };
    if let Some(other) = fields
        .keys()
        .find(|key| !matches!(key.as_str(), "reason" | "include_rolled_back"))
    {
        return Err(format!(
            "update_installation takes reason and include_rolled_back (got {other})"
        ));
    }
    let reason = match fields.get("reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(reason)) => Some(reason.trim())
            .filter(|reason| !reason.is_empty())
            .map(cut),
        Some(_) => return Err("reason must be a string".to_string()),
    };
    let mut include_rolled_back = super::string_list(
        fields.get("include_rolled_back").cloned(),
        "include_rolled_back",
    )?;
    include_rolled_back.sort();
    include_rolled_back.dedup();
    Ok(UpdateRequest {
        reason,
        include_rolled_back,
    })
}

/// `get_installation_update`'s `history`: a boolean, or as a query string gives it.
pub(crate) fn parse_history_flag(params: &Value) -> Result<bool, String> {
    let fields = match params {
        Value::Null => return Ok(false),
        Value::Object(fields) => fields,
        _ => return Err("the body must be an object: {history?}".to_string()),
    };
    if let Some(other) = fields.keys().find(|key| key.as_str() != "history") {
        return Err(format!(
            "get_installation_update takes history (got {other})"
        ));
    }
    match fields.get("history") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(history)) => Ok(*history),
        Some(Value::String(history)) => match history.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" | "" => Ok(false),
            _ => Err("history must be true or false".to_string()),
        },
        Some(_) => Err("history must be true or false".to_string()),
    }
}

/// `update_installation` (§6, "As built (stage 4)"), the operator's word: the update to what the
/// motherbee holds now, opened or resumed, and one pass over the hives not on it, in parallel and
/// bounded. `ok` (200) when every hive is on the target, `partial` (202) while the update stays open;
/// a core restarting is a fine answer, which the background task follows to its settle.
pub(crate) async fn handle_update_installation(
    ctx: &AdminContext,
    client: &RouterDispatcher,
    params: &Value,
    origin: &str,
) -> (u16, String) {
    let action = "update_installation";
    let started = std::time::Instant::now();
    let request = match parse_update_request(params) {
        Ok(request) => request,
        Err(detail) => {
            return super::installation_refusal(action, "INVALID_REQUEST", detail, Value::Null)
        }
    };
    let mesh = LiveMesh::of(ctx);
    let (hives, _) = mesh.hives();
    if let Some(stranger) = request
        .include_rolled_back
        .iter()
        .find(|hive| !hives.contains(hive))
    {
        return super::installation_refusal(
            action,
            "INVALID_REQUEST",
            format!(
                "include_rolled_back names {stranger}, which is not a hive the update reaches (the \
                 motherbee and each hive the registry calls connected: {})",
                hives.join(", ")
            ),
            Value::Null,
        );
    }
    let motherbee = match super::installation_versions(ctx, client, "get_versions").await {
        Ok(answer) => answer.get("hive").cloned().unwrap_or(Value::Null),
        Err((code, detail)) => {
            return super::installation_refusal(
                action,
                &code,
                format!("the motherbee's /versions did not answer ({detail}): nothing was changed"),
                Value::Null,
            )
        }
    };
    let target = target_of(&motherbee);
    if target.core.is_none() {
        return super::installation_refusal(
            action,
            "INSTALLATION_TARGET_UNKNOWN",
            format!(
                "the motherbee's core cannot be named ({}): nothing was changed",
                target.core_note.as_deref().unwrap_or("no reason given")
            ),
            json!({ "target": target_view(&target) }),
        );
    }
    let within = super::admin_action_timeout(action).saturating_sub(started.elapsed());
    let end = run_explicit(
        &ctx.installation_update,
        &mesh,
        origin,
        request.reason.as_deref(),
        &request.include_rolled_back,
        &motherbee,
        &target,
        &Bounds::from_env(),
        within,
    )
    .await;
    run_answer(action, end, &request, started.elapsed())
}

/// An explicit run's answer.
fn run_answer(
    action: &str,
    end: RunEnd,
    request: &UpdateRequest,
    elapsed: Duration,
) -> (u16, String) {
    let elapsed_ms = elapsed.as_millis() as u64;
    match end {
        RunEnd::NothingToDo { target, hives } => super::installation_answer(
            action,
            200,
            "ok",
            json!({
                "nothing_to_do": true,
                "target": target_view(&target),
                "hives": hives,
                "message": "every hive the update reaches is on the motherbee's core and installation \
                            file: nothing to do",
                "elapsed_ms": elapsed_ms,
            }),
            None,
        ),
        RunEnd::Update { update, opening } => {
            let (opening, superseded) = match opening {
                Opening::Opened => ("opened", Value::Null),
                Opening::Resumed => ("resumed", Value::Null),
                Opening::Superseded(previous) => ("opened", json!(previous)),
            };
            let mut payload = json!({
                "update": record_view(&update),
                "opening": opening,
                "superseded": superseded,
                "include_rolled_back": request.include_rolled_back,
                "elapsed_ms": elapsed_ms,
            });
            if update.state == UpdateState::Done {
                payload["message"] = json!(format!(
                    "every hive is on the target ({}): update {} is done",
                    update.target.describe(),
                    update.id
                ));
                return super::installation_answer(action, 200, "ok", payload, None);
            }
            let pending = pending_of(&update);
            // A run with another target (a set_installation_ai) closed it while this one ran.
            if update.state == UpdateState::Superseded {
                payload["message"] = json!(format!(
                    "update {} was superseded by {} (another target) while it ran: GET \
                     /installation/update follows that one",
                    update.id,
                    update.superseded_by.as_deref().unwrap_or("another update")
                ));
                payload["pending"] = json!(pending);
                return super::installation_answer(action, 202, "partial", payload, None);
            }
            payload["message"] = json!(format!(
                "update {} is open: not every hive is on the target ({}) yet: {}. A core restarting \
                 is followed to its settle, and its config goes after it; a hive that did not answer \
                 gets the update finished once when it answers again; each hive's next says what \
                 moves the rest. GET /installation/update follows it.",
                update.id,
                update.target.describe(),
                pending.join(", ")
            ));
            payload["pending"] = json!(pending);
            super::installation_answer(action, 202, "partial", payload, None)
        }
    }
}

/// `get_installation_update`: a read of SY.admin's own state, so not in the audit log.
pub(crate) fn handle_get_installation_update(ctx: &AdminContext, params: &Value) -> (u16, String) {
    let action = "get_installation_update";
    match parse_history_flag(params) {
        Ok(history) => super::installation_answer(
            action,
            200,
            "ok",
            ctx.installation_update.view(history),
            None,
        ),
        Err(detail) => super::installation_refusal(action, "INVALID_REQUEST", detail, Value::Null),
    }
}

/// `set_installation_ai`'s update (§4, step 3): the target its write made, opened (a run with
/// another target), with the motherbee's core on it, the reference. Its explicit pass is the
/// stage 3c apply, each hive's outcome recorded with [`record_config_applied`]; the background
/// task reads the other hives' cores. Its id.
pub(crate) fn open_for_installation_change(ctx: &AdminContext, target: &Target) -> String {
    let updates = &ctx.installation_update;
    let (hives, skipped) = LiveMesh::of(ctx).hives();
    let now = now_epoch_ms();
    let (id, opening) = updates.change(|file| {
        let opened = file.open_or_resume(target, "set_installation_ai", None, now);
        file.refresh_hives(&opened.0, &hives, skipped);
        opened
    });
    match &opening {
        Opening::Superseded(previous) => tracing::info!(
            update = %id,
            superseded = %previous,
            target = %target.describe(),
            "installation update: opened by set_installation_ai, the open one superseded"
        ),
        _ => tracing::info!(
            update = %id,
            target = %target.describe(),
            "installation update: opened by set_installation_ai"
        ),
    }
    updates.record(
        &id,
        &ctx.hive_id,
        Category::Core,
        Standing::OnTarget,
        None,
        None,
        None,
    );
    updates.wake();
    id
}

/// `/versions` (`list_versions`, `get_versions`) as SY.admin answers it: with the installation's
/// update, its id, state, target and each hive's state.
pub(crate) fn with_update_summary(
    updates: &Updates,
    action: &str,
    response: Result<Value, super::AdminError>,
) -> Result<Value, super::AdminError> {
    if !matches!(action, "list_versions" | "get_versions") {
        return response;
    }
    response.map(|mut payload| {
        if payload.is_object() && super::payload_is_ok(&payload) {
            payload["installation_update"] = updates.summary();
        }
        payload
    })
}

#[cfg(test)]
#[path = "installation_update_tests.rs"]
mod tests;
