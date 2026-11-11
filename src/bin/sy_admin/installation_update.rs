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
//!   superseded; one with the same target resumes it. An update whose only hives off the target
//!   are the operator's core rollbacks is `held` (A-5's one allowed divergence).
//! - **The reads** of a hive's `/versions` go through the motherbee's orchestrator, which forwards
//!   a spoke's read within 45 s, each read apart since 0.1.127 (FINDINGS A-199): SY.admin waits
//!   past that bound, and keeps at most one read of a hive outstanding
//!   ([`Updates::read_versions`]). While a hive's core update restarts it, a read waits 10 s, and
//!   one its restart lost is asked again at once (FINDINGS A-250).
//! - **The background task** ([`run_background`]), while an update is open: it follows a hive in
//!   flight to its settle and then sends its config, finishes the update once for a hive that did
//!   not answer and now does (the automatic step, §6), never for an excluded one, and reads again
//!   the hives off the target, at a cadence by where each stands. It acts on nothing while the
//!   motherbee holds another target than the open update's (`stale_target`).
//!
//! Every outcome is one log line, `installation update: outcome`, with the update's id, the hive,
//! the category, the outcome and its duration (`ops.py timeline` reads it).

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// What a task holding a hive does while it reads its `/versions`: an explicit run waits for it.
const READING: &str = "reading its /versions";

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
    /// Every hive is on the target but those the operator rolled back (`core_rollback`, A-5's one
    /// allowed divergence), each with its config on the target. Still the open update: the hive's
    /// per-hive update closes it.
    Held,
    /// Every hive reached the target.
    Done,
    /// A run with another target closed it.
    Superseded,
}

impl UpdateState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            UpdateState::Open => "open",
            UpdateState::Held => "held",
            UpdateState::Done => "done",
            UpdateState::Superseded => "superseded",
        }
    }
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

    /// What an outcome says of the category on the target: a core settled, a config applied.
    fn on_target_status(self) -> OutcomeStatus {
        match self {
            Category::Core => OutcomeStatus::Settled,
            Category::Config => OutcomeStatus::Ok,
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

/// Why a hive's category is left as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Exclusion {
    /// The operator rolled its core back (`core_rollback`, A-5).
    RolledBack,
    /// Its core is a newer release than the motherbee's.
    Ahead,
    /// Its copy of the installation file is edited by hand.
    HandEdited,
    /// Its orchestrator takes no category config, and its core is left as it is.
    OldOrchestrator,
    /// The target names nothing to apply (the motherbee's file is not a recorded version).
    NoTarget,
}

/// What an attempt (or an exclusion) left. Never "not applied" (P-13): a hive that did not answer
/// may hold the target already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutcomeStatus {
    Ok,
    /// An update sent, its answer not in yet.
    Sent,
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
            OutcomeStatus::Sent => "sent",
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
/// background task finishing what that run started), the automatic step (and the background task
/// finishing what it started), or the background task on its own, finishing an update it found
/// under way (a core it did not send, a config apply SY.admin's restart cut). Each also reads a
/// hive: an outcome of a read that found it on the target is marked `seen` ([`seen_on_target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunKind {
    Explicit,
    Automatic,
    Background,
}

impl RunKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RunKind::Explicit => "explicit",
            RunKind::Automatic => "automatic",
            RunKind::Background => "background",
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
    /// A core update SY.admin sent: the hive's last core update before it
    /// (`core.last_update.started_at_ms`, `none` without one), so the watch tells this update's
    /// record from an older one. None for an update it found under way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) previous_update: Option<String>,
    /// A core update that did not settle: the hive's record of it then
    /// ([`core_record_signature`]). The hive is in flight again only once that record changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) record_seen: Option<String>,
    /// A read found the category on the target, its last outcome saying otherwise: no attempt of
    /// `by` moved it there ([`seen_on_target`], FINDINGS A-204). `ops.py` prints it as "on target".
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) seen: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
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
            record_seen: None,
            seen: false,
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
    /// Why it is excluded, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exclusion: Option<Exclusion>,
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
            exclusion: None,
            outcome: None,
            next: None,
        }
    }
}

/// The automatic step's one attempt on a hive, in this update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AutomaticAttempt {
    pub(crate) started_at_ms: u64,
    /// When what it started ended: none while a core it sent is still followed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at_ms: Option<u64>,
    /// Where it left the hive, in a few words.
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

    /// Off the target only by the operator's core rollback: its core excluded as rolled back, its
    /// config on the target.
    fn held(&self) -> bool {
        self.core.state == Standing::Excluded
            && self.core.exclusion == Some(Exclusion::RolledBack)
            && self.config.state == Standing::OnTarget
    }

    fn in_flight(&self) -> bool {
        self.core.state == Standing::InFlight || self.config.state == Standing::InFlight
    }

    /// Its core update restarts it: from SY.admin's `restarting` outcome for it (the update's
    /// answer, or a core found under way) to its `settled`, or to the follow's verdict at the
    /// settle bound (FINDINGS A-250). A core update that did not answer in time is followed too,
    /// but not as a restart: the hive may be slow.
    fn in_restart(&self) -> bool {
        self.core.state == Standing::InFlight
            && self
                .core
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.status == OutcomeStatus::Restarting)
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

    fn summary(&self) -> String {
        format!(
            "core {}, config {}",
            self.core.state.as_str(),
            self.config.state.as_str()
        )
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

/// The motherbee holds another target than the open update's (its release or its file changed):
/// nothing acts on the update until the next explicit run supersedes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StaleTarget {
    /// What the motherbee holds now.
    pub(crate) target: Target,
    pub(crate) since_ms: u64,
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
    /// When the last explicit run on it started, on the motherbee's clock: what a reader counts the
    /// outcomes' times from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_run_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stale_target: Option<StaleTarget>,
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

    /// The hives off the target only by the operator's core rollback, when nothing else is off it.
    pub(crate) fn held_hives(&self) -> Option<Vec<String>> {
        let mut held = Vec::new();
        for (hive, entry) in &self.hives {
            if entry.on_target() {
                continue;
            }
            if !entry.held() {
                return None;
            }
            held.push(hive.clone());
        }
        (!held.is_empty()).then_some(held)
    }

    /// What the update's state is by its hives: done, held, or open; never done or held while the
    /// motherbee holds another target.
    fn state_by_hives(&self) -> UpdateState {
        if self.stale_target.is_some() {
            UpdateState::Open
        } else if self.all_on_target() {
            UpdateState::Done
        } else if self.held_hives().is_some() {
            UpdateState::Held
        } else {
            UpdateState::Open
        }
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
    /// unknown. The run's start is recorded either way.
    pub(crate) fn open_or_resume(
        &mut self,
        target: &Target,
        origin: &str,
        reason: Option<&str>,
        now_ms: u64,
    ) -> (String, Opening) {
        if let Some(open) = self.open.as_mut() {
            if open.target.same_as(target) {
                open.last_run_at_ms = Some(now_ms);
                open.stale_target = None;
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
            last_run_at_ms: Some(now_ms),
            stale_target: None,
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

    /// The open update's state set by its hives: closed as done once every hive is on the target,
    /// `held` while only the operator's rollbacks are off it, `open` otherwise. The new state when
    /// it changed.
    pub(crate) fn settle_state(&mut self, now_ms: u64) -> Option<(String, UpdateState)> {
        let open = self.open.as_mut()?;
        let state = open.state_by_hives();
        if state == open.state {
            return None;
        }
        let id = open.id.clone();
        if state == UpdateState::Done {
            let mut update = self.open.take()?;
            update.state = UpdateState::Done;
            update.closed_at_ms = Some(now_ms);
            self.close(update);
        } else {
            open.state = state;
        }
        Some((id, state))
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

/// A field that is a time in ms, as the orchestrator's records write it (a string or a number).
fn stamp_of(value: Option<&Value>) -> Option<String> {
    let value = value.filter(|value| !value.is_null())?;
    Some(
        value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string()),
    )
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
    /// Over, with something open: a settle pending or with errors, components left out (with no
    /// settle to judge them), binaries awaiting their update.
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
    match settled {
        // The settle judges what the update left out: the orchestrator it installed dropped a
        // node its release no longer names, or keeps it pending; `left_out` is never rewritten.
        Some(settled) => {
            if settled.get("settled").and_then(Value::as_bool) == Some(false) {
                items.push(format!(
                    "settle pending={} errors={}",
                    names(settled.get("pending")).join(","),
                    names(settled.get("errors")).join("; ")
                ));
            }
        }
        None => {
            let left_out = names(last.and_then(|last| last.get("left_out")));
            if !left_out.is_empty() {
                items.push(format!("left_out={}", left_out.join(",")));
            }
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

/// The hive's last core update's record, by when it started and when it settled: what tells a
/// record that changed from one that did not.
pub(crate) fn core_record_signature(core: &Value) -> String {
    let last = core.get("last_update").filter(|last| last.is_object());
    format!(
        "{}|{}",
        stamp_of(last.and_then(|last| last.get("started_at_ms"))).unwrap_or_else(|| "none".into()),
        stamp_of(
            last.and_then(|last| last.get("settled"))
                .and_then(|settled| settled.get("at_ms"))
        )
        .unwrap_or_else(|| "none".into())
    )
}

/// The hive's last core update, by when it started (`none` without one).
fn last_update_stamp(core: &Value) -> String {
    stamp_of(
        core.get("last_update")
            .and_then(|last| last.get("started_at_ms")),
    )
    .unwrap_or_else(|| "none".to_string())
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

/// The motherbee's own core is rolled back (a mesh `core_rollback` on it): its installed hash and
/// its dist's version disagree, so it names no target.
pub(crate) fn motherbee_rolled_back(motherbee: &Value) -> bool {
    motherbee.get("core").is_some_and(core_rolled_back)
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
        kind: Exclusion,
        reason: String,
        next: String,
    },
    /// Not known: why.
    Unknown(String),
}

impl Step {
    fn label(&self) -> &'static str {
        match self {
            Step::OnTarget => "on_target",
            Step::Act => "act",
            Step::InFlight(_) => "in_flight",
            Step::AfterCore(_) => "after_core",
            Step::Excluded { .. } => "excluded",
            Step::Unknown(_) => "unknown",
        }
    }
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
/// - The automatic step, and the background task acting on its own, leave alone, both categories, a
///   hive the operator rolled back, one ahead of the motherbee and one whose copy is edited by
///   hand.
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

    if kind != RunKind::Explicit {
        let who = match kind {
            RunKind::Automatic => "the automatic step",
            _ => "the background task",
        };
        let left_alone = if rolled_back {
            Some((
                Exclusion::RolledBack,
                format!(
                    "the operator rolled its core back (core_rollback, the one allowed \
                     divergence): {who} leaves it alone"
                ),
                rolled_back_next(hive, target),
            ))
        } else if ahead {
            Some((
                Exclusion::Ahead,
                format!(
                    "its core ({}) is newer than the motherbee's ({}): {who} leaves it alone",
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
                Exclusion::HandEdited,
                format!(
                    "its copy of the installation file is edited by hand: {who} leaves it alone"
                ),
                format!(
                    "restore its copy to the motherbee's bytes (GET /versions shows it), then \
                     update_installation, or {}",
                    config_update_step(hive, target)
                ),
            ))
        } else {
            None
        };
        if let Some((kind, reason, next)) = left_alone {
            let step = Step::Excluded { kind, reason, next };
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
        return Step::Excluded {
            kind: Exclusion::NoTarget,
            reason: target
                .core_note
                .clone()
                .unwrap_or_else(|| "the motherbee's core is not known".to_string()),
            next: "install a release on the motherbee, then update_installation".to_string(),
        };
    };
    if text_of(Some(core), "status") != Some("ok") {
        return Step::Unknown(format!(
            "its core manifest cannot be read: {}",
            text_of(Some(core), "message").unwrap_or("its /versions has no core section")
        ));
    }
    let left_alone = rolled_back
        && (kind != RunKind::Explicit
            || !include_rolled_back.iter().any(|named| named == facts.hive));
    let rolled_back_step = || Step::Excluded {
        kind: Exclusion::RolledBack,
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
            kind: Exclusion::NoTarget,
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
            Step::Excluded { reason, next, .. } => Step::Excluded {
                kind: Exclusion::OldOrchestrator,
                reason: format!("{why}, and its core is left as it is: {reason}"),
                next: next.clone(),
            },
            Step::Unknown(why) => Step::Unknown(why.clone()),
            Step::OnTarget | Step::AfterCore(_) => Step::Act,
        };
    }
    if edited {
        return Step::Excluded {
            kind: Exclusion::HandEdited,
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
    /// Still under way. `installed`: the hive runs the target; `open`: what its settle leaves open;
    /// `seen`: its record shows the update (a record newer than the one before it, or the target
    /// installed).
    Waiting {
        installed: bool,
        open: Vec<String>,
        seen: bool,
    },
    /// On the target, settled, nothing open.
    Settled,
    /// The update ended without the target installed.
    Failed { code: String, detail: String },
}

/// The watch: `previous` is the hive's last core update before this one (`none` without one), so
/// an older record is never read as this one's; None for an update found under way, whose record
/// counts as this one's.
pub(crate) fn watch_core(snapshot: &Value, target_hash: &str, previous: Option<&str>) -> CoreWatch {
    let core = snapshot.get("core").unwrap_or(&Value::Null);
    if installed_core_hash(core).as_deref() == Some(normalize_hash(target_hash).as_str()) {
        return match core_settle(core) {
            Settle::Settled => CoreWatch::Settled,
            Settle::NotOver => CoreWatch::Waiting {
                installed: true,
                open: Vec::new(),
                seen: true,
            },
            Settle::Open(open) => CoreWatch::Waiting {
                installed: true,
                open,
                seen: true,
            },
        };
    }
    let last = core.get("last_update").filter(|last| last.is_object());
    let this_one = previous.is_none_or(|previous| last_update_stamp(core) != previous);
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
        seen: this_one && last.is_some(),
    }
}

// ---------------------------------------------------------------------------------------------
// The state, on disk
// ---------------------------------------------------------------------------------------------

/// The bounds and cadences of the machinery (environment overrides in the style of `JSR_ADMIN_*`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bounds {
    /// How long an explicit run waits for a hive's orchestrator to answer before its core update,
    /// counted from the run's start (`JSR_ADMIN_INSTALLATION_ANSWER_TIMEOUT_SECS`, 120): the
    /// motherbee's router may not have the spokes back right after an install, and one read may
    /// take a whole `read`.
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
    /// Its shared-memory check of a hive that did not answer, or not read yet
    /// (`JSR_ADMIN_INSTALLATION_RETRY_CHECK_SECS`, 30): read once its orchestrator shows.
    pub(crate) retry_check: Duration,
    /// Its read of a hive left as it is, or whose last attempt failed
    /// (`JSR_ADMIN_INSTALLATION_SLOW_CHECK_SECS`, 600).
    pub(crate) slow_check: Duration,
    /// One read of a hive's `/versions` (`JSR_ADMIN_INSTALLATION_READ_TIMEOUT_SECS`, 50): past the
    /// motherbee's orchestrator's own bound for forwarding it (45 s,
    /// `JSR_ORCH_SYSTEM_FORWARD_TIMEOUT_SECS`), so a read SY.admin gives up is no longer queued
    /// there. Outside a restart: it protects a hive that is slow.
    pub(crate) read: Duration,
    /// One read of a hive's `/versions` while its core update restarts it, from SY.admin's
    /// `restarting` outcome for it to its `settled`
    /// (`JSR_ADMIN_INSTALLATION_RESTART_READ_TIMEOUT_SECS`, 10, FINDINGS A-250): its orchestrator
    /// restarts itself for the new binary, and a read that reaches it in the few seconds it is off
    /// its router, or just before it stops, is never answered. One that times out leaves no hold,
    /// and the follow asks again at once ([`Updates::read_versions`]).
    pub(crate) restart_read: Duration,
}

impl Bounds {
    pub(crate) fn from_env() -> Self {
        let secs = |name: &str, default: u64| {
            Duration::from_secs(env_timeout_secs(name).unwrap_or(default))
        };
        Bounds {
            answer: secs("JSR_ADMIN_INSTALLATION_ANSWER_TIMEOUT_SECS", 120),
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
            slow_check: secs("JSR_ADMIN_INSTALLATION_SLOW_CHECK_SECS", 600)
                .max(Duration::from_secs(1)),
            read: secs("JSR_ADMIN_INSTALLATION_READ_TIMEOUT_SECS", 50),
            // A read lost in a restart is asked again at once: never a bound of nothing.
            restart_read: secs("JSR_ADMIN_INSTALLATION_RESTART_READ_TIMEOUT_SECS", 10)
                .max(Duration::from_secs(1)),
        }
    }

    /// The longest an explicit run's own pass takes on one hive: the wait for its answer and the
    /// read under way at its end, a core update asked again up to the apply bound with an update's
    /// last answer, the read after a core update that restarted nothing, and a config update.
    pub(crate) fn explicit_pass(&self) -> Duration {
        self.answer + 2 * self.read + 2 * (self.apply + self.update_call)
    }

    /// The longest an explicit run's pass waits for a task holding its hive: a read, then the send a
    /// task of a closed update may still have under way (an update asked again up to the apply
    /// bound, with its last answer), with a margin. Such a task sends nothing more after it.
    pub(crate) fn holder_wait(&self) -> Duration {
        self.read + self.apply + self.update_call + Duration::from_secs(5)
    }
}

/// Who reads a hive's `/versions`: an explicit run, or the background task, one hive at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reader {
    Run,
    /// The background task, which waits for its turn here.
    Background,
    /// The background task with its turn already taken: [`recheck`] takes it before it claims the
    /// hive, so a read waiting for its turn holds no hive.
    InTurn,
}

/// A task acting on a hive: what it does, for which update, since when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Holder {
    pub(crate) what: String,
    pub(crate) update: String,
    pub(crate) since_ms: u64,
}

/// The explicit run under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Running {
    pub(crate) origin: String,
    pub(crate) since_ms: u64,
    pub(crate) update: Option<String>,
}

/// The installation's update: its state on disk, and what is being acted on now.
pub(crate) struct Updates {
    path: PathBuf,
    file: Mutex<UpdatesFile>,
    /// The hives a task acts on now: another run, or the background task, leaves them.
    busy: Mutex<HashMap<String, Holder>>,
    /// When the background task looks at each hive next, and when it last did (ms), in memory: a
    /// restart looks at once.
    due: Mutex<HashMap<String, u64>>,
    looked: Mutex<HashMap<String, u64>>,
    /// The update the background task looked at last: its first look at another forgets `due`.
    ticked: Mutex<Option<String>>,
    /// One read of a hive's `/versions` at a time, and until when a read SY.admin gave up may
    /// still be queued at the motherbee's orchestrator (outside a restart, FINDINGS A-250).
    reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    quiet_until: Mutex<HashMap<String, u64>>,
    /// The background task reads one hive at a time.
    background_reads: tokio::sync::Semaphore,
    /// One explicit run at a time.
    running: Mutex<Option<Running>>,
    /// The open update's target checked once after SY.admin's start; the registry's failure said.
    resume_checked: AtomicBool,
    registry_said: AtomicBool,
    /// Wakes the background task: an update opened, something to look at now.
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

impl Claim<'_> {
    /// What the holder does now.
    fn doing(&self, what: &str) {
        if let Some(holder) = lock(&self.updates.busy).get_mut(&self.hive) {
            holder.what = what.to_string();
        }
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        lock(&self.updates.busy).remove(&self.hive);
    }
}

/// The explicit run's mark, released when it answers.
pub(crate) struct RunGuard<'a> {
    updates: &'a Updates,
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        *lock(&self.updates.running) = None;
    }
}

impl Updates {
    /// The state SY.admin's start finds, the open update resumed and every hive looked at at once:
    /// a core or a config found in flight is followed (a core to its settle, or past the bound with
    /// no sign of it, `not_answered`; a config apply sent again).
    pub(crate) fn load(state_dir: &Path) -> Arc<Updates> {
        let path = updates_path(state_dir);
        let (file, note) = load_file(&path, now_epoch_ms());
        if let Some(note) = note {
            tracing::warn!(note = %note, "installation update: its state starts empty");
        }
        if let Some(open) = file.open.as_ref() {
            tracing::info!(
                update = %open.id,
                state = open.state.as_str(),
                target = %open.target.describe(),
                hives = open.hives.len(),
                "installation update: an open update resumed at SY.admin's start; its target is \
                 checked against the motherbee's, and its hives are looked at again"
            );
        }
        Arc::new(Updates {
            path,
            file: Mutex::new(file),
            busy: Mutex::new(HashMap::new()),
            due: Mutex::new(HashMap::new()),
            looked: Mutex::new(HashMap::new()),
            ticked: Mutex::new(None),
            reads: Mutex::new(HashMap::new()),
            quiet_until: Mutex::new(HashMap::new()),
            background_reads: tokio::sync::Semaphore::new(1),
            running: Mutex::new(None),
            resume_checked: AtomicBool::new(false),
            registry_said: AtomicBool::new(false),
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

    fn claim(&self, hive: &str, what: &str, update: &str) -> Option<Claim<'_>> {
        let mut busy = lock(&self.busy);
        if busy.contains_key(hive) {
            return None;
        }
        busy.insert(
            hive.to_string(),
            Holder {
                what: what.to_string(),
                update: update.to_string(),
                since_ms: now_epoch_ms(),
            },
        );
        Some(Claim {
            updates: self,
            hive: hive.to_string(),
        })
    }

    pub(crate) fn holder(&self, hive: &str) -> Option<Holder> {
        lock(&self.busy).get(hive).cloned()
    }

    /// The task of the update that acts on `hive` now, a read aside: one that may send it a core
    /// update or a config. An egress switch is not started under it (docs/edge-egress-nat-spec.md
    /// §15.1).
    pub(crate) fn acting_on(&self, hive: &str) -> Option<Holder> {
        self.holder(hive).filter(|holder| holder.what != READING)
    }

    fn is_busy(&self, hive: &str) -> bool {
        lock(&self.busy).contains_key(hive)
    }

    /// One explicit run at a time: the run's mark, or the one under way.
    pub(crate) fn start_run(&self, origin: &str) -> Result<RunGuard<'_>, Running> {
        let mut running = lock(&self.running);
        if let Some(under_way) = running.as_ref() {
            return Err(under_way.clone());
        }
        *running = Some(Running {
            origin: origin.to_string(),
            since_ms: now_epoch_ms(),
            update: None,
        });
        Ok(RunGuard { updates: self })
    }

    /// The update the explicit run under way opened or resumed, which a refused run names.
    fn note_run_update(&self, id: &str) {
        if let Some(running) = lock(&self.running).as_mut() {
            running.update = Some(id.to_string());
        }
    }

    /// When the background task looks at `hive` next (0: at once).
    fn due_at(&self, hive: &str) -> u64 {
        lock(&self.due).get(hive).copied().unwrap_or(0)
    }

    fn look_again(&self, hive: &str, after: Duration) {
        let now = now_epoch_ms();
        lock(&self.due).insert(
            hive.to_string(),
            now.saturating_add(after.as_millis() as u64),
        );
        lock(&self.looked).insert(hive.to_string(), now);
    }

    /// An immediate look at `hive`: due now, and the background task woken. Never blocks.
    pub(crate) fn look_now(&self, hive: &str) {
        lock(&self.due).insert(hive.to_string(), 0);
        self.wake();
    }

    /// An immediate look at every hive of the open update the background task has not looked at
    /// for `min_age` (a reader of the update or of `/versions` sees it fresh, and polling does not
    /// read a hive more often than that). Never blocks.
    pub(crate) fn look_soon_all(&self, min_age: Duration) {
        let hives: Vec<String> = self.read(|file| {
            file.open
                .as_ref()
                .map(|update| {
                    update
                        .hives
                        .iter()
                        .filter(|(_, entry)| !entry.on_target())
                        .map(|(hive, _)| hive.clone())
                        .collect()
                })
                .unwrap_or_default()
        });
        if hives.is_empty() {
            return;
        }
        let now = now_epoch_ms();
        let min_age = min_age.as_millis() as u64;
        let mut any = false;
        {
            let looked = lock(&self.looked);
            let mut due = lock(&self.due);
            for hive in hives {
                let stale = looked
                    .get(&hive)
                    .is_none_or(|at| now.saturating_sub(*at) >= min_age);
                if stale && due.get(&hive).is_some_and(|at| *at > now) {
                    due.insert(hive, 0);
                    any = true;
                }
            }
        }
        if any {
            self.wake();
        }
    }

    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    fn read_lock(&self, hive: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            lock(&self.reads)
                .entry(hive.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// Whether the open update's record has `hive`'s core update restarting it
    /// ([`HiveEntry::in_restart`], FINDINGS A-250).
    fn in_restart(&self, hive: &str) -> bool {
        self.read(|file| {
            file.open
                .as_ref()
                .and_then(|update| update.hives.get(hive))
                .is_some_and(HiveEntry::in_restart)
        })
    }

    /// One read of `hive`'s `/versions`, at most one outstanding for the hive, everywhere. The
    /// background task reads one hive at a time. The motherbee's orchestrator forwards a spoke's
    /// read within its own bound, each read apart since 0.1.127 (FINDINGS A-199), and runs one
    /// SY.admin gave up to that bound:
    /// - **outside a restart,** `bounds.read` waits past that bound, and a read that timed out
    ///   anyway may still be queued there: the hive is not read again for another `bounds.read`
    ///   (the hold), which protects a hive that is slow;
    /// - **while the hive's core update restarts it** ([`HiveEntry::in_restart`], FINDINGS A-250),
    ///   a read waits `bounds.restart_read`, and one that times out leaves no hold, nor waits for
    ///   one: its orchestrator restarts itself for the new binary, and a read that reaches it in
    ///   the few seconds it is off its router, or just before it stops, is never answered. The
    ///   follow asks again at once; the read SY.admin gave up runs on at the motherbee's
    ///   orchestrator to its 45 s in one of the 4 places it keeps for each target hive, and the
    ///   dispatcher drops its late answer (an `admin` reply without `ADMIN_COMMAND`: stale, or
    ///   past the stale table's 30 s unmatched). A read of the hive still under way, SY.admin's
    ///   own, is waited for all the same (this lock).
    ///
    /// A read that is answered ends any hold: the hive answers.
    pub(crate) async fn read_versions(
        &self,
        mesh: &dyn UpdateMesh,
        hive: &str,
        reader: Reader,
        bounds: &Bounds,
    ) -> Result<Value, String> {
        let _background = match reader {
            Reader::Background => Some(
                self.background_reads
                    .acquire()
                    .await
                    .map_err(|err| err.to_string())?,
            ),
            Reader::Run | Reader::InTurn => None,
        };
        let hive_lock = self.read_lock(hive);
        let _one = hive_lock.lock().await;
        let restart = self.in_restart(hive);
        let now = now_epoch_ms();
        let quiet_until = lock(&self.quiet_until).get(hive).copied();
        if let Some(until) = quiet_until.filter(|until| !restart && *until > now) {
            return Err(format!(
                "a read of its /versions SY.admin gave up may still be queued at the motherbee's \
                 orchestrator: not read again for {} s",
                (until - now) / 1000 + 1
            ));
        }
        let bound = if restart {
            bounds.restart_read
        } else {
            bounds.read
        };
        let read = mesh.versions(hive, bound).await;
        match &read {
            Ok(_) => {
                lock(&self.quiet_until).remove(hive);
            }
            Err(why) if !restart && super::infer_transport_error_code(why) == "TIMEOUT" => {
                lock(&self.quiet_until).insert(
                    hive.to_string(),
                    now_epoch_ms().saturating_add(bounds.read.as_millis() as u64),
                );
            }
            Err(_) => {}
        }
        read
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
        self.record_with(id, hive, category, state, None, reason, outcome, next)
    }

    #[allow(clippy::too_many_arguments)]
    fn record_with(
        &self,
        id: &str,
        hive: &str,
        category: Category,
        state: Standing,
        exclusion: Option<Exclusion>,
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
            slot.exclusion = exclusion.filter(|_| state == Standing::Excluded);
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

    fn entry(&self, id: &str, hive: &str) -> Option<HiveEntry> {
        self.read(|file| {
            file.open
                .as_ref()
                .filter(|update| update.id == id)
                .and_then(|update| update.hives.get(hive))
                .cloned()
        })
    }

    /// The open update's state set by its hives (done, held or open), and said when it changed.
    pub(crate) fn settle_state(&self) -> Option<UpdateState> {
        let changed = self.change(|file| file.settle_state(now_epoch_ms()));
        let (id, state) = changed?;
        match state {
            UpdateState::Done => {
                tracing::info!(update = %id, "installation update: done, every hive on the target")
            }
            UpdateState::Held => {
                let held = self
                    .read(|file| file.open.as_ref().and_then(UpdateRecord::held_hives))
                    .unwrap_or_default();
                tracing::warn!(
                    update = %id,
                    held = ?held,
                    "installation update: held, every hive on the target but those the operator \
                     rolled back (core_rollback, the one allowed divergence)"
                )
            }
            UpdateState::Open | UpdateState::Superseded => {
                tracing::info!(update = %id, state = state.as_str(), "installation update: open again")
            }
        }
        Some(state)
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
                "stale_target": update.stale_target.is_some(),
                "held": update.held_hives().filter(|_| update.state == UpdateState::Held),
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
            fields.remove("record_seen");
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
        "last_run_at_ms": update.last_run_at_ms,
        "origin": update.origin,
        "reason": update.reason,
        "superseded_by": update.superseded_by,
        "target": target_view(&update.target),
        "stale_target": update.stale_target.as_ref().map(|stale| json!({
            "target": target_view(&stale.target),
            "since_ms": stale.since_ms,
            "note": "the motherbee holds another target than this update's: nothing acts on it; \
                     update_installation supersedes it",
        })),
        "held": update.held_hives().filter(|_| update.state == UpdateState::Held).unwrap_or_default(),
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
    /// The hives an update reaches now, the motherbee and each one the registry calls connected,
    /// and the registry's others, named; an error when the registry cannot be read (never "the
    /// motherbee alone").
    fn hives(&self) -> Result<(Vec<String>, Vec<Value>), String>;
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

/// The motherbee and each hive the registry calls connected (`connected_hives_with`'s rule), and
/// the registry's others; the registry's read error, never a list without the spokes.
fn registry_hives(
    admin_hive: &str,
    registry: &fluxbee_sdk::mesh::HiveRegistry,
) -> Result<(Vec<String>, Vec<Value>), String> {
    // No registry directory at all: no hive was ever added (the orchestrator creates it with the
    // first), so the motherbee alone. Any other error is a registry that cannot be read.
    let entries = match registry.entries() {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            return Err(format!(
                "the hive registry ({}) cannot be read: {err}",
                registry.root().display()
            ))
        }
    };
    let mut hives = vec![admin_hive.to_string()];
    for entry in &entries {
        if entry.field("status") != Some("connected") {
            continue;
        }
        if let Some(hive) = entry
            .field("hive_id")
            .map(str::trim)
            .filter(|hive| !hive.is_empty())
        {
            if !hives.iter().any(|known| known == hive) {
                hives.push(hive.to_string());
            }
        }
    }
    hives.sort();
    Ok((
        hives,
        skipped_hives(super::installation_skipped_hives(registry, false, None)),
    ))
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

    fn hives(&self) -> Result<(Vec<String>, Vec<Value>), String> {
        registry_hives(&self.admin_hive, &super::hive_registry_for(&self.state_dir))
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
    /// Its services restart, or its answer did not come in time: the background task follows it,
    /// then applies the config.
    InFlight,
    /// On the target: the config may go now.
    Settled,
    /// It refused or failed, and its orchestrator answers.
    Failed,
    /// It never reached the hive.
    NotAnswered,
    /// Its update is no longer open (superseded, done): nothing was sent.
    Closed,
}

/// Whether the update `id` is still the open one, before a task sends `category` to `hive`: a task
/// of an update closed meanwhile (superseded, done) sends nothing more, and says so. A send already
/// under way when the update closes finishes, its outcome not recorded.
fn still_open(updates: &Updates, id: &str, hive: &str, category: Category) -> bool {
    let open = updates.read(|file| file.open.as_ref().is_some_and(|update| update.id == id));
    if !open {
        tracing::info!(
            update = %id,
            hive = %hive,
            category = category.as_str(),
            "installation update: no longer open (superseded or done): nothing more is sent for it"
        );
    }
    open
}

/// What a hive's `next` says for an update that did not get it to the target, by category and
/// error code.
fn next_after(hive: &str, category: Category, target: &Target, code: Option<&str>) -> String {
    match (category, code) {
        (Category::Core, Some("CORE_NOT_SETTLED" | "CORE_NOT_INSTALLED")) => format!(
            "it stays pending until its record changes: run update_installation again, which sends \
             its core update again (it installs what is pending), or {}; GET /hives/{hive}/versions \
             shows its record (core.last_update, core.awaiting_update)",
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

/// What a hive that did not answer gets as its `next`.
fn next_unanswered(hive: &str, category: Category, target: &Target) -> String {
    format!(
        "once it answers again, the automatic step finishes the update for it, once, unless it \
         leaves the hive alone (rolled back by the operator, ahead of the motherbee, or a copy \
         edited by hand); or update_installation. {}",
        next_after(hive, category, target, None)
    )
}

/// An apply's outcome, as an [`Outcome`] from `started_at_ms` to `at_ms`, with its error code.
fn applied_outcome(
    applied: &InstallationApplied,
    by: RunKind,
    started_at_ms: u64,
    at_ms: u64,
) -> (Outcome, Option<String>) {
    let mut outcome = Outcome::new(OutcomeStatus::Ok, by, started_at_ms, at_ms);
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

/// The hive's `/versions`, read until its orchestrator answers, up to `deadline` (a read under way
/// then ends on its own bound).
async fn read_until_it_answers(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    hive: &str,
    deadline: tokio::time::Instant,
    bounds: &Bounds,
) -> Result<Value, String> {
    loop {
        let read = updates.read_versions(mesh, hive, Reader::Run, bounds).await;
        match read {
            Ok(snapshot) => return Ok(snapshot),
            Err(why) if tokio::time::Instant::now() + bounds.answer_poll > deadline => {
                return Err(why)
            }
            Err(_) => tokio::time::sleep(bounds.answer_poll).await,
        }
    }
}

/// The outcome of a read, by `by` from `read_at` on, that found `category` on the target while its
/// last outcome here says otherwise (excluded, an error, no answer): moved there outside the
/// update (its per-hive update, say), or by an attempt whose answer never came. The category's own
/// status on the target, marked `seen`, its detail naming the outcome it replaces (FINDINGS
/// A-204). None when its last outcome says on the target, or there is none: it stands.
fn seen_on_target(
    updates: &Updates,
    id: &str,
    hive: &str,
    category: Category,
    by: RunKind,
    read_at: u64,
) -> Option<Outcome> {
    let last = updates
        .entry(id, hive)?
        .category(category)
        .outcome
        .clone()
        .filter(|last| last.status != category.on_target_status())?;
    let mut seen = Outcome::new(category.on_target_status(), by, read_at, now_epoch_ms());
    seen.seen = true;
    seen.detail = Some(format!(
        "a read of its /versions saw it on the target, after its last outcome here said {} ({})",
        last.status.as_str(),
        last.by.as_str()
    ));
    Some(seen)
}

/// A step that is not an act, as the state it leaves, decided by `by` on a read from `read_at` on:
/// a category on the target its last outcome says is not gets the read's outcome
/// ([`seen_on_target`]).
fn record_step(
    updates: &Updates,
    id: &str,
    hive: &str,
    category: Category,
    step: &Step,
    by: RunKind,
    read_at: u64,
) {
    let (state, exclusion, reason, next) = match step {
        Step::OnTarget => {
            let seen = seen_on_target(updates, id, hive, category, by, read_at);
            updates.record(id, hive, category, Standing::OnTarget, None, seen, None);
            return;
        }
        Step::Act => return,
        Step::InFlight(why) => (Standing::InFlight, None, Some(why.clone()), None),
        Step::AfterCore(why) => (Standing::Pending, None, Some(why.clone()), None),
        Step::Excluded { kind, reason, next } => (
            Standing::Excluded,
            Some(*kind),
            Some(reason.clone()),
            Some(next.clone()),
        ),
        Step::Unknown(why) => (Standing::Unknown, None, Some(why.clone()), None),
    };
    updates.record_with(id, hive, category, state, exclusion, reason, None, next);
}

/// An exclusion a run made, as an outcome too.
fn record_exclusion(
    updates: &Updates,
    id: &str,
    hive: &str,
    category: Category,
    step: &Step,
    by: RunKind,
) {
    if let Step::Excluded { kind, reason, next } = step {
        let now = now_epoch_ms();
        let mut outcome = Outcome::new(OutcomeStatus::Excluded, by, now, now);
        outcome.detail = Some(reason.clone());
        updates.record_with(
            id,
            hive,
            category,
            Standing::Excluded,
            Some(*kind),
            Some(reason.clone()),
            Some(outcome),
            Some(next.clone()),
        );
    }
}

/// The hive's core outcome says its update did not settle, and its record is still the one it
/// saw then: nothing new to follow.
fn core_unchanged_since_not_settled(entry: &HiveEntry, snapshot: &Value) -> bool {
    let core = snapshot.get("core").unwrap_or(&Value::Null);
    entry.core.outcome.as_ref().is_some_and(|outcome| {
        matches!(
            outcome.error_code.as_deref(),
            Some("CORE_NOT_SETTLED" | "CORE_NOT_INSTALLED")
        ) && outcome.record_seen.as_deref() == Some(core_record_signature(core).as_str())
    })
}

/// Which holders of a hive an explicit run waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitFor {
    /// A task that only reads it, or one of a closed update, which sends nothing more; one that
    /// acts on it for this update keeps it (`update_installation`'s pass).
    ReadersAndClosed,
    /// Any task (`set_installation_ai`'s apply: the operator's change reaches every hive in one go
    /// whenever it can).
    Anyone,
}

/// An explicit run's mark on a hive (`update_installation`'s pass, `set_installation_ai`'s apply),
/// as `what`: a holder it waits for (`wait_for`) is waited for, up to `wait_until`; another keeps
/// the hive, and so does one still there then: None. A holder that ended meanwhile is no holder:
/// the mark is taken.
async fn claim_for_run<'a>(
    updates: &'a Updates,
    hive: &str,
    what: &str,
    id: &str,
    wait_for: WaitFor,
    wait_until: tokio::time::Instant,
) -> Option<Claim<'a>> {
    loop {
        if let Some(claim) = updates.claim(hive, what, id) {
            return Some(claim);
        }
        let Some(holder) = updates.holder(hive) else {
            continue;
        };
        let waited_for = match wait_for {
            WaitFor::Anyone => true,
            WaitFor::ReadersAndClosed => holder.what == READING || holder.update != id,
        };
        if !waited_for || tokio::time::Instant::now() >= wait_until {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// One explicit run's pass on one hive, once no other task acts on it: [`run_claimed_hive`]. A
/// holder that only reads the hive, or one of a closed update (superseded, done), which sends
/// nothing more, is waited for ([`Bounds::holder_wait`]); one that acts on it for this update is
/// not, and the hive is recorded as left to it, never left unrecorded.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_hive(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    include_rolled_back: &[String],
    first: Option<Value>,
    answer_deadline: tokio::time::Instant,
    bounds: &Bounds,
) {
    let wait_until = tokio::time::Instant::now() + bounds.holder_wait();
    let Some(_claim) = claim_for_run(
        updates,
        hive,
        "the explicit run",
        id,
        WaitFor::ReadersAndClosed,
        wait_until,
    )
    .await
    else {
        left_to_holder(updates, id, hive);
        return;
    };
    run_claimed_hive(
        updates,
        mesh,
        id,
        hive,
        target,
        RunKind::Explicit,
        include_rolled_back,
        first,
        answer_deadline,
        bounds,
    )
    .await;
}

/// A hive another task acts on: the run leaves it to that task and says so in the record. What it
/// says.
fn left_to_holder(updates: &Updates, id: &str, hive: &str) -> String {
    let holder = updates.holder(hive);
    let since = holder.as_ref().map_or(0, |holder| {
        now_epoch_ms().saturating_sub(holder.since_ms) / 1000
    });
    let reason = match &holder {
        Some(holder) if holder.update == id => format!(
            "this run left it to {} of this update, under way for {since} s",
            holder.what
        ),
        Some(holder) => format!(
            "this run left it to {} of the superseded update {}, under way for {since} s",
            holder.what, holder.update
        ),
        None => "this run left it to another task, which has just ended".to_string(),
    };
    tracing::info!(update = %id, hive = %hive, reason = %reason, "installation update: a hive left to another task");
    updates.change(|file| {
        let Some(entry) = file
            .open_mut(id)
            .and_then(|update| update.hives.get_mut(hive))
        else {
            return;
        };
        for slot in [&mut entry.core, &mut entry.config] {
            if slot.state == Standing::OnTarget {
                continue;
            }
            slot.reason = Some(cut(&reason));
            slot.next = Some(
                "GET /installation/update follows it; run update_installation again once that \
                 task ends"
                    .to_string(),
            );
        }
    });
    updates.look_now(hive);
    reason
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
    answer_deadline: tokio::time::Instant,
    bounds: &Bounds,
) {
    let started = now_epoch_ms();
    let is_motherbee = hive == mesh.motherbee();
    let snapshot = match first {
        Some(snapshot) => Ok(snapshot),
        None => read_until_it_answers(updates, mesh, hive, answer_deadline, bounds).await,
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
                    Some(next_unanswered(hive, category, target)),
                );
            }
            return;
        }
    };
    updates.set_role(id, hive, text_of(Some(&snapshot), "role"));
    let known = updates.read(UpdatesFile::known_cores);
    let mut decision = decide(
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
    // A core that did not settle, its record unchanged since: an explicit run sends its core
    // update again (it installs what is pending, and writes a new record).
    if kind == RunKind::Explicit && matches!(decision.core, Step::InFlight(_)) {
        if let Some(entry) = updates.entry(id, hive) {
            if core_unchanged_since_not_settled(&entry, &snapshot) {
                decision.core = Step::Act;
            }
        }
    }
    if kind == RunKind::Automatic {
        let left_alone = match &decision.core {
            Step::Excluded { reason, .. } => reason.as_str(),
            _ => "",
        };
        tracing::info!(
            update = %id,
            hive = %hive,
            core = decision.core.label(),
            config = decision.config.label(),
            left_alone = %left_alone,
            "installation update: automatic step decided"
        );
    }
    for (category, step) in [
        (Category::Core, &decision.core),
        (Category::Config, &decision.config),
    ] {
        match step {
            Step::Excluded { .. } => record_exclusion(updates, id, hive, category, step, kind),
            // Recorded below, with its outcome.
            Step::InFlight(_) if category == Category::Core => {}
            _ => record_step(updates, id, hive, category, step, kind, started),
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
                    RunKind::Background,
                    started,
                    now_epoch_ms(),
                )),
                None,
            );
            updates.look_now(hive);
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
        outcome.detail = Some("its core update did not reach it".to_string());
        updates.record(
            id,
            hive,
            Category::Config,
            Standing::Pending,
            Some("after its core".to_string()),
            Some(outcome),
            Some(next_unanswered(hive, Category::Config, target)),
        );
    }
}

/// The core update of one hive: sent, asked again while `sync_pending` or unreachable, and, when its
/// services restart or its answer does not come in time, left in flight for the background task to
/// follow through `/versions`. Not sent once its update is closed ([`still_open`]).
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
    if !still_open(updates, id, hive, Category::Core) {
        return CoreEnd::Closed;
    }
    let started = now_epoch_ms();
    // What the hive's record said before: after a restart of SY.admin, the follow tells this
    // update's record from it.
    let previous = Some(last_update_stamp(
        snapshot.get("core").unwrap_or(&Value::Null),
    ));
    let mut sent = Outcome::new(OutcomeStatus::Sent, kind, started, started);
    sent.previous_update = previous.clone();
    updates.record(
        id,
        hive,
        Category::Core,
        Standing::InFlight,
        Some("its core update is being sent".to_string()),
        Some(sent),
        None,
    );
    let payload = json!({
        "category": "core",
        "manifest_version": 0,
        "manifest_hash": core_target.manifest_hash,
    });
    let applied = ask(mesh, hive, &payload, bounds).await;
    let (mut outcome, code) = applied_outcome(&applied, kind, started, now_epoch_ms());
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
            let after = updates.read_versions(mesh, hive, Reader::Run, bounds).await;
            let settled = after.as_ref().ok().map(|after| {
                watch_core(
                    after,
                    &core_target.manifest_hash,
                    outcome.previous_update.as_deref(),
                )
            }) == Some(CoreWatch::Settled);
            if settled {
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
                return CoreEnd::Settled;
            }
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
        // No answer within an update's 60 s: the hive may still be applying it. Its /versions is
        // followed until the settle bound; only a hive whose record never shows the update is
        // `not_answered`.
        InstallationApplyOutcome::NotAnswered(detail)
            if super::infer_transport_error_code(detail) == "TIMEOUT" =>
        {
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::InFlight,
                Some(
                    "its core update did not answer in time: its /versions is followed until the \
                     settle bound, for a sign it took it"
                        .to_string(),
                ),
                Some(outcome),
                None,
            );
            updates.look_again(hive, bounds.flight_check);
            updates.wake();
            CoreEnd::InFlight
        }
        InstallationApplyOutcome::NotAnswered(_) => {
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::Pending,
                Some("its core update did not reach it".to_string()),
                Some(outcome),
                Some(next_unanswered(hive, Category::Core, target)),
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
/// Not sent once its update is closed ([`still_open`]).
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
    if !still_open(updates, id, hive, Category::Config) {
        return;
    }
    let started = now_epoch_ms();
    updates.record(
        id,
        hive,
        Category::Config,
        Standing::InFlight,
        Some("its config update is being applied".to_string()),
        Some(Outcome::new(OutcomeStatus::Sent, kind, started, started)),
        None,
    );
    let payload = json!({
        "category": "config",
        "manifest_version": config.version,
        "manifest_hash": config.sha256,
    });
    let applied = ask(mesh, hive, &payload, bounds).await;
    record_config_applied(
        updates,
        id,
        hive,
        target,
        &applied,
        kind,
        started,
        now_epoch_ms(),
    );
}

/// One hive's config apply, from `started_at_ms` to `at_ms`, recorded: on the target when it
/// applied it; otherwise pending, with what finishes it. `set_installation_ai`'s apply records here
/// too ([`apply_change_on_hive`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_config_applied(
    updates: &Updates,
    id: &str,
    hive: &str,
    target: &Target,
    applied: &InstallationApplied,
    kind: RunKind,
    started_at_ms: u64,
    at_ms: u64,
) {
    let (outcome, code) = applied_outcome(applied, kind, started_at_ms, at_ms);
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
            let (reason, next) = match status {
                OutcomeStatus::SyncPending => (
                    "its copy had not arrived at the bound",
                    next_after(hive, Category::Config, target, None),
                ),
                OutcomeStatus::NotAnswered => (
                    "it did not answer",
                    next_unanswered(hive, Category::Config, target),
                ),
                _ => (
                    "its config update failed",
                    next_after(hive, Category::Config, target, code.as_deref()),
                ),
            };
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

/// What `set_installation_ai`'s apply did with one hive of the update it opened.
#[derive(Debug)]
pub(crate) enum ChangeApply {
    /// Applied, whatever it answered, and recorded: its apply and how long it took.
    Applied(InstallationApplied, Duration),
    /// Another task held the hive until the change's ceiling: the change left it to that task, as
    /// an explicit run leaves one, and recorded why (this text). Nothing was sent to it.
    LeftToTask(String),
}

/// One hive's apply of `set_installation_ai`'s version, the pass of the update the change opened:
/// the hive claimed ([`claim_for_run`]), so no background read of it is recorded while it applies
/// (FINDINGS A-205); the version applied (`apply`, given the time left before `deadline`, the
/// change's ceiling); and its outcome recorded as it ends, with its own start and end (A-204),
/// before the claim is released. A task holding the hive, reading it or acting on it, is waited
/// for, so the operator's change reaches every hive in one go whenever it can; but only while the
/// apply would still have its own time, one call (`call`, the update's answer) and never less than
/// 10 s, before the ceiling. One still there then keeps the hive, which is left to it, said in the
/// record and to the caller: given less, the hive's apply would read `not_answered` though it
/// applies it.
pub(crate) async fn apply_change_on_hive<F, Fut>(
    updates: &Updates,
    id: &str,
    hive: &str,
    target: &Target,
    deadline: tokio::time::Instant,
    call: Duration,
    apply: F,
) -> ChangeApply
where
    F: FnOnce(Duration) -> Fut,
    Fut: std::future::Future<Output = (InstallationApplied, Duration)>,
{
    let wait_until = deadline
        .checked_sub(call.max(Duration::from_secs(10)))
        .unwrap_or_else(tokio::time::Instant::now);
    let Some(_claim) = claim_for_run(
        updates,
        hive,
        "set_installation_ai's apply",
        id,
        WaitFor::Anyone,
        wait_until,
    )
    .await
    else {
        return ChangeApply::LeftToTask(left_to_holder(updates, id, hive));
    };
    let started = now_epoch_ms();
    let (applied, elapsed) =
        apply(deadline.saturating_duration_since(tokio::time::Instant::now())).await;
    let ended = started.saturating_add(elapsed.as_millis() as u64);
    record_config_applied(
        updates,
        id,
        hive,
        target,
        &applied,
        RunKind::Explicit,
        started,
        ended,
    );
    ChangeApply::Applied(applied, elapsed)
}

// ---------------------------------------------------------------------------------------------
// The background task
// ---------------------------------------------------------------------------------------------

/// What the background task does with one hive of the open update, and how often.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    /// A core or a config in flight: followed every few seconds.
    Follow,
    /// It did not answer an attempt of this update: once shared memory shows its orchestrator,
    /// the automatic step.
    Automatic,
    /// It did not answer, or was not read yet: the shared-memory check, and a read once it shows
    /// its orchestrator; nothing sent.
    AnswerCheck,
    /// Left as it is, or its last attempt failed: read again now and then; nothing sent.
    Slow,
    Nothing,
}

impl Look {
    fn period(self, bounds: &Bounds) -> Duration {
        match self {
            Look::Follow => bounds.flight_check,
            Look::Automatic | Look::AnswerCheck => bounds.retry_check,
            Look::Slow | Look::Nothing => bounds.slow_check,
        }
    }
}

pub(crate) fn look_at(entry: &HiveEntry) -> Look {
    if entry.on_target() {
        return Look::Nothing;
    }
    if entry.in_flight() {
        return Look::Follow;
    }
    if entry.awaits_automatic_step() {
        return Look::Automatic;
    }
    let unanswered = [&entry.core, &entry.config].into_iter().any(|slot| {
        slot.state == Standing::Unknown
            || (slot.state == Standing::Pending
                && slot
                    .outcome
                    .as_ref()
                    .is_some_and(|outcome| outcome.status == OutcomeStatus::NotAnswered))
    });
    if unanswered {
        Look::AnswerCheck
    } else {
        Look::Slow
    }
}

/// The background task: while an update is open, it looks at each hive when its time comes (in a
/// task of its own), and sleeps until the next one is due or something wakes it; the motherbee
/// only, as SY.admin runs nowhere else.
pub(crate) async fn run_background(
    updates: Arc<Updates>,
    mesh: Arc<dyn UpdateMesh>,
    bounds: Bounds,
) {
    loop {
        let open = updates.read(|file| file.open.is_some());
        let next_due = if open {
            tick(&updates, &mesh, &bounds).await
        } else {
            None
        };
        let pause = match next_due {
            Some(due) => Duration::from_millis(due.saturating_sub(now_epoch_ms()))
                .clamp(Duration::from_millis(200), bounds.slow_check),
            None => bounds.slow_check,
        };
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            _ = updates.wake.notified() => {}
        }
    }
}

/// One look at the open update: the hive list refreshed (never on a registry the task cannot read),
/// each hive due looked at in a task of its own, then the update's state settled. When the next
/// hive is due.
pub(crate) async fn tick(
    updates: &Arc<Updates>,
    mesh: &Arc<dyn UpdateMesh>,
    bounds: &Bounds,
) -> Option<u64> {
    let (id, target) = updates.read(|file| {
        file.open
            .as_ref()
            .map(|update| (update.id.clone(), update.target.clone()))
    })?;
    // A hive's cadence is the open update's: the first look at a new update (a run's, a change's)
    // forgets when the looks of the one it superseded had each hive due next, up to its 10 min.
    {
        let mut ticked = lock(&updates.ticked);
        if ticked.as_deref() != Some(id.as_str()) {
            lock(&updates.due).clear();
            *ticked = Some(id.clone());
        }
    }
    // SY.admin's start: the target is checked before anything acts on the update.
    if !updates.resume_checked.swap(true, Ordering::SeqCst) {
        target_is_current(updates, mesh.as_ref(), &id, &target, bounds).await;
    }
    let listed = match mesh.hives() {
        Ok((hives, skipped)) => {
            updates.registry_said.store(false, Ordering::SeqCst);
            updates.change(|file| file.refresh_hives(&id, &hives, skipped));
            true
        }
        Err(why) => {
            if !updates.registry_said.swap(true, Ordering::SeqCst) {
                tracing::warn!(
                    update = %id,
                    error = %why,
                    "installation update: the hive registry cannot be read: the update keeps its \
                     hives, and is not closed, until it can"
                );
            }
            false
        }
    };
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
    let mut next_due: Option<u64> = None;
    let mut sooner = |at: u64| next_due = Some(next_due.map_or(at, |due| due.min(at)));
    for (hive, look) in looks {
        if look == Look::Nothing {
            continue;
        }
        if updates.is_busy(&hive) {
            sooner(now.saturating_add(bounds.flight_check.as_millis() as u64));
            continue;
        }
        let due = updates.due_at(&hive);
        if due > now {
            sooner(due);
            continue;
        }
        let period = look.period(bounds);
        updates.look_again(&hive, period);
        sooner(now.saturating_add(period.as_millis() as u64));
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
                Look::AnswerCheck | Look::Slow => {
                    recheck(&updates, mesh.as_ref(), &id, &hive, &target, &bounds, look).await
                }
                Look::Nothing => {}
            }
            updates.settle_state();
        });
    }
    if listed {
        updates.settle_state();
    }
    next_due
}

/// Whether the open update `id`'s target is still what the motherbee holds. While it is not (its
/// release or its file changed, a downgrade included), the update is marked `stale_target`, said
/// once, and nothing acts on it: the next explicit run supersedes it. A motherbee that cannot be
/// read is no verdict: nothing acts this time.
pub(crate) async fn target_is_current(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    target: &Target,
    bounds: &Bounds,
) -> bool {
    let motherbee = mesh.motherbee().to_string();
    let now_target = match updates
        .read_versions(mesh, &motherbee, Reader::Background, bounds)
        .await
    {
        Ok(snapshot) => target_of(&snapshot),
        Err(why) => {
            tracing::info!(
                update = %id,
                error = %why,
                "installation update: the motherbee's /versions did not answer: nothing acts on \
                 the update this time"
            );
            return false;
        }
    };
    let current = now_target.same_as(target);
    let now = now_epoch_ms();
    let changed = updates.change(|file| {
        let update = file.open_mut(id)?;
        if current {
            return update.stale_target.take().map(|_| false);
        }
        let first = update.stale_target.is_none();
        update.stale_target = Some(StaleTarget {
            target: now_target.clone(),
            since_ms: update
                .stale_target
                .as_ref()
                .map_or(now, |stale| stale.since_ms),
        });
        if update.state == UpdateState::Held {
            update.state = UpdateState::Open;
        }
        first.then_some(true)
    });
    match changed {
        Some(true) => tracing::warn!(
            update = %id,
            target = %target.describe(),
            motherbee_holds = %now_target.describe(),
            "installation update: the motherbee holds another target than the open update's: \
             nothing acts on it until update_installation supersedes it"
        ),
        Some(false) => tracing::info!(
            update = %id,
            "installation update: the motherbee holds the open update's target again"
        ),
        None => {}
    }
    current
}

/// Where the automatic step left a hive, said in its attempt: finished once nothing it started is
/// in flight.
fn refresh_automatic_result(updates: &Updates, id: &str, hive: &str) {
    let now = now_epoch_ms();
    updates.change(|file| {
        let Some(entry) = file
            .open_mut(id)
            .and_then(|update| update.hives.get_mut(hive))
        else {
            return;
        };
        let summary = entry.summary();
        let in_flight = entry.in_flight();
        if let Some(attempt) = entry.automatic.as_mut() {
            attempt.result = Some(if in_flight {
                format!("{summary}: followed")
            } else {
                summary
            });
            attempt.finished_at_ms = (!in_flight).then_some(now);
        }
    });
}

/// A hive in flight: a core read again until it settles, then its config sent (by the run that
/// sent the core, or, for one found under way, by the background task); past the settle bound,
/// what stopped it. A config apply a previous process left in flight is sent again. A read the
/// hive's restart lost is asked again at once (FINDINGS A-250), by the next look, once this one
/// let the hive go: each look still holds the hive for one read.
pub(crate) async fn follow(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    bounds: &Bounds,
) {
    let Some(claim) = updates.claim(hive, READING, id) else {
        return;
    };
    let Some(entry) = updates.entry(id, hive) else {
        return;
    };
    let automatic = entry.automatic.is_some();
    let mut ask_again = false;
    if entry.core.state == Standing::InFlight {
        ask_again = follow_core(updates, mesh, &claim, id, hive, target, bounds, entry).await;
    } else if entry.config.state == Standing::InFlight {
        if !target_is_current(updates, mesh, id, target, bounds).await {
            return;
        }
        let by = entry
            .config
            .outcome
            .as_ref()
            .map_or(RunKind::Background, |outcome| outcome.by);
        tracing::info!(
            update = %id,
            hive = %hive,
            "installation update: a config apply under way without its answer (SY.admin restarted): \
             sent again"
        );
        claim.doing("applying its config");
        config_update(updates, mesh, id, hive, target, by, bounds).await;
    }
    if automatic {
        refresh_automatic_result(updates, id, hive);
    }
    // The background task passes over a hive another task holds: the look at once comes once
    // the hive is let go.
    drop(claim);
    if ask_again {
        updates.look_now(hive);
    }
}

/// One look at a core in flight. True when its read was lost to the hive's restart (no answer
/// within the restart's bound, the settle bound not past, FINDINGS A-250): it is asked again at
/// once.
#[allow(clippy::too_many_arguments)]
async fn follow_core(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    claim: &Claim<'_>,
    id: &str,
    hive: &str,
    target: &Target,
    bounds: &Bounds,
    entry: HiveEntry,
) -> bool {
    let Some(core_target) = &target.core else {
        return false;
    };
    let mut outcome = entry.core.outcome.clone().unwrap_or_else(|| {
        let now = now_epoch_ms();
        Outcome::new(OutcomeStatus::Restarting, RunKind::Background, now, now)
    });
    let read_at = now_epoch_ms();
    let read = updates
        .read_versions(mesh, hive, Reader::Background, bounds)
        .await;
    let now = now_epoch_ms();
    let past_bound = now.saturating_sub(outcome.started_at_ms) > bounds.settle.as_millis() as u64;
    let watch = read.as_ref().ok().map(|snapshot| {
        watch_core(
            snapshot,
            &core_target.manifest_hash,
            outcome.previous_update.as_deref(),
        )
    });
    let signature = read
        .as_ref()
        .ok()
        .map(|snapshot| core_record_signature(snapshot.get("core").unwrap_or(&Value::Null)));
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
            // The core is on the target: the config of the same run goes now, while the motherbee
            // still holds this target.
            let Ok(snapshot) = read else {
                return false;
            };
            if !target_is_current(updates, mesh, id, target, bounds).await {
                return false;
            }
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
                    claim.doing("applying its config");
                    config_update(updates, mesh, id, hive, target, by, bounds).await
                }
                Step::Excluded { .. } => {
                    record_exclusion(updates, id, hive, Category::Config, &decision.config, by)
                }
                step => record_step(updates, id, hive, Category::Config, step, by, read_at),
            }
        }
        Some(CoreWatch::Failed { code, detail }) => {
            outcome.status = OutcomeStatus::Error;
            outcome.error_code = Some(code.clone());
            outcome.detail = Some(detail);
            outcome.record_seen = signature;
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
        Some(CoreWatch::Waiting {
            installed,
            open,
            seen,
        }) => {
            let first_installed = installed && outcome.installed_ms.is_none();
            if first_installed {
                outcome.installed_ms = Some(outcome.elapsed_ms());
            }
            if past_bound && !seen {
                outcome.status = OutcomeStatus::NotAnswered;
                outcome.detail = Some(format!(
                    "its /versions never showed the core update within {} s",
                    bounds.settle.as_secs()
                ));
                updates.record(
                    id,
                    hive,
                    Category::Core,
                    Standing::Pending,
                    Some("its core update never showed".to_string()),
                    Some(outcome),
                    Some(next_unanswered(hive, Category::Core, target)),
                );
            } else if past_bound {
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
                outcome.record_seen = signature;
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
            updates.record(
                id,
                hive,
                Category::Core,
                Standing::Pending,
                Some("it did not answer after its core update".to_string()),
                Some(outcome),
                Some(next_unanswered(hive, Category::Core, target)),
            );
        }
        // Not readable while its orchestrator restarts: looked at again. A read its restart lost
        // (its orchestrator off its router for a few seconds, or stopping as the read reached
        // it) is asked again at once: the next read finds it back (FINDINGS A-250). Whether it
        // was a restart's read is the open update's record, as the read found it: only this look
        // writes the hive's record while it holds the hive, and a run that superseded the update
        // meanwhile left the hive out of a restart.
        None => {
            let why = read.err().unwrap_or_default();
            if super::infer_transport_error_code(&why) == "TIMEOUT" && updates.in_restart(hive) {
                tracing::info!(
                    update = %id,
                    hive = %hive,
                    bound_ms = bounds.restart_read.as_millis() as u64,
                    error = %why,
                    "installation update: a read of a hive in its restart got no answer within its \
                     bound: asked again at once"
                );
                return true;
            }
        }
    }
    false
}

/// The automatic step (§6): a hive that did not answer an attempt of this update and now does gets
/// the update finished, once per hive and target, by the same pass as an explicit run's, never for
/// an excluded hive (the decision leaves those alone), and never while the motherbee holds another
/// target. A cheap read of shared memory first: only a hive whose orchestrator is connected is
/// asked.
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
    if !target_is_current(updates, mesh, id, target, bounds).await {
        return;
    }
    let Some(claim) = updates.claim(hive, READING, id) else {
        return;
    };
    let Ok(snapshot) = updates
        .read_versions(mesh, hive, Reader::Background, bounds)
        .await
    else {
        return;
    };
    // A core update found under way (one SY.admin's restart or a TIMEOUT left without its answer,
    // or the operator's own): followed, and the attempt is not spent on it.
    let known = updates.read(UpdatesFile::known_cores);
    let found = decide(
        HiveFacts {
            hive,
            is_motherbee: hive == mesh.motherbee(),
            snapshot: Some(&snapshot),
        },
        target,
        RunKind::Automatic,
        &[],
        &known,
    );
    if matches!(found.core, Step::InFlight(_)) {
        let now = now_epoch_ms();
        tracing::info!(
            update = %id,
            hive = %hive,
            "installation update: the hive answers again with its core update under way: followed, \
             the automatic step kept for later"
        );
        updates.record(
            id,
            hive,
            Category::Core,
            Standing::InFlight,
            Some("its core update is under way".to_string()),
            Some(Outcome::new(
                OutcomeStatus::Restarting,
                RunKind::Background,
                now,
                now,
            )),
            None,
        );
        updates.look_now(hive);
        return;
    }
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
        "installation update: automatic step, the hive answers again: deciding what it finishes"
    );
    claim.doing("the automatic step");
    run_claimed_hive(
        updates,
        mesh,
        id,
        hive,
        target,
        RunKind::Automatic,
        &[],
        Some(snapshot),
        tokio::time::Instant::now(),
        bounds,
    )
    .await;
    refresh_automatic_result(updates, id, hive);
    let result = updates
        .entry(id, hive)
        .and_then(|entry| entry.automatic)
        .and_then(|attempt| attempt.result);
    tracing::info!(
        update = %id,
        hive = %hive,
        result = %result.unwrap_or_else(|| "the update is no longer open".to_string()),
        elapsed_ms = now_epoch_ms().saturating_sub(started),
        "installation update: automatic step ended its pass"
    );
}

/// A hive not on the target, nothing in flight: read again, its states set as a run would decide
/// them, nothing sent. A hive that did not answer, or was not read yet, is read only once shared
/// memory shows its orchestrator (read anyway when shared memory cannot be read). A core it finds
/// under way becomes in flight, for the follow; one that did not settle stays pending until its
/// record changes. A category whose last outcome came in after the read began keeps it; one the
/// read finds on the target, its last outcome saying otherwise, records that it saw it there
/// ([`record_step`]), as every read that decides does.
pub(crate) async fn recheck(
    updates: &Updates,
    mesh: &dyn UpdateMesh,
    id: &str,
    hive: &str,
    target: &Target,
    bounds: &Bounds,
    look: Look,
) {
    if look == Look::AnswerCheck && mesh.orchestrator_connected(hive) == Some(false) {
        return;
    }
    // Its turn among the background reads first, then the hive: a read waiting for its turn holds
    // no hive, so an explicit run's pass is never left to it, and waits only for the read under way.
    let Ok(_turn) = updates.background_reads.acquire().await else {
        return;
    };
    let Some(_claim) = updates.claim(hive, READING, id) else {
        return;
    };
    let read_at = now_epoch_ms();
    let Ok(snapshot) = updates
        .read_versions(mesh, hive, Reader::InTurn, bounds)
        .await
    else {
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
    let Some(entry) = updates.entry(id, hive) else {
        return;
    };
    for (category, step) in [
        (Category::Core, &decision.core),
        (Category::Config, &decision.config),
    ] {
        let current = entry.category(category);
        // An outcome that came in while the read was under way (a config apply that answered
        // meanwhile, FINDINGS A-205) is newer than what the read shows: it stands.
        if current
            .outcome
            .as_ref()
            .is_some_and(|outcome| outcome.at_ms > read_at)
        {
            tracing::info!(
                update = %id,
                hive = %hive,
                category = category.as_str(),
                "installation update: a read that began before the hive's last outcome, not \
                 recorded"
            );
            continue;
        }
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
                if core_unchanged_since_not_settled(&entry, &snapshot) {
                    continue;
                }
                let now = now_epoch_ms();
                updates.record(
                    id,
                    hive,
                    Category::Core,
                    Standing::InFlight,
                    Some("its core update is under way".to_string()),
                    Some(Outcome::new(
                        OutcomeStatus::Restarting,
                        RunKind::Background,
                        now,
                        now,
                    )),
                    None,
                );
                updates.look_now(hive);
            }
            // On the target, its last outcome saying otherwise: the read's own outcome
            // (FINDINGS A-204).
            step => record_step(
                updates,
                id,
                hive,
                category,
                step,
                RunKind::Background,
                read_at,
            ),
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
    /// The hive registry could not be read: nothing done.
    RegistryUnreadable(String),
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
    let (hives, skipped) = match mesh.hives() {
        Ok(listed) => listed,
        Err(why) => return RunEnd::RegistryUnreadable(why),
    };
    let answer_deadline = tokio::time::Instant::now() + bounds.answer;
    let motherbee_name = mesh.motherbee().to_string();
    // Every hive's /versions, once, at once: the motherbee's is in hand.
    let reads = futures::future::join_all(hives.iter().map(|hive| {
        let (updates, mesh) = (Arc::clone(updates), Arc::clone(mesh));
        let hive = hive.clone();
        let motherbee_name = motherbee_name.clone();
        let motherbee = motherbee.clone();
        async move {
            if hive == motherbee_name {
                return Some(motherbee);
            }
            updates
                .read_versions(mesh.as_ref(), &hive, Reader::Run, bounds)
                .await
                .ok()
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
    updates.note_run_update(&id);
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
                    &include,
                    first,
                    answer_deadline,
                    &bounds,
                )
                .await;
            })
        })
        .collect();
    let _ = tokio::time::timeout(within, futures::future::join_all(tasks)).await;
    updates.settle_state();
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

/// A second explicit run's refusal: the one under way, and since when. An egress action holds the
/// run's mark too (`<action> on <hive>`, docs/edge-egress-nat-spec.md §15), and is named as one.
pub(crate) fn run_under_way(action: &str, running: &Running) -> (u16, String) {
    let since_s = now_epoch_ms().saturating_sub(running.since_ms) / 1000;
    let egress = ["set_hive_egress on ", "clear_hive_egress on "]
        .iter()
        .any(|origin| running.origin.starts_with(origin));
    super::installation_refusal(
        action,
        "INSTALLATION_UPDATE_RUNNING",
        if egress {
            format!(
                "an egress action runs ({}), for {since_s} s, and holds the installation's run: one \
                 at a time. Run it again once that one answered",
                running.origin
            )
        } else {
            format!(
                "an installation update run is under way ({}, update {}), for {since_s} s: one at \
                 a time. Read get_installation_update, and run it again once that one answered",
                running.origin,
                running.update.as_deref().unwrap_or("not opened yet")
            )
        },
        json!({
            "running": {
                "origin": running.origin,
                "since_ms": running.since_ms,
                "update": running.update,
            }
        }),
    )
}

/// `update_installation` (§6, "As built (stage 4)"), the operator's word: the update to what the
/// motherbee holds now, opened or resumed, and one pass over the hives not on it, in parallel and
/// bounded. `ok` (200) when every hive is on the target, or held by the operator's rollbacks
/// (`held`); `partial` (202) while the update stays open. A core restarting is a fine answer,
/// which the background task follows to its settle. One explicit run at a time.
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
    let run = match ctx.installation_update.start_run(origin) {
        Ok(run) => run,
        Err(running) => return run_under_way(action, &running),
    };
    let mesh = LiveMesh::of(ctx);
    let hives = match mesh.hives() {
        Ok((hives, _)) => hives,
        Err(why) => return registry_unreadable(action, &why),
    };
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
    // A mesh core_rollback on the motherbee itself: its installed hash and its dist's version
    // disagree, so it names no target to bring the hives to.
    if motherbee_rolled_back(&motherbee) {
        return super::installation_refusal(
            action,
            "MOTHERBEE_ROLLED_BACK",
            "the motherbee's own core is rolled back (core.installed.source core_rollback): the \
             release it runs and its dist's disagree, so it names no target. Install a release on \
             the motherbee first (apt-get install fluxbee=<version>), then run the update; nothing \
             was changed"
                .to_string(),
            Value::Null,
        );
    }
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
    drop(run);
    run_answer(action, end, &request, started.elapsed())
}

/// A hive registry that cannot be read refuses an explicit run (F1), `update_installation` or
/// `set_installation_ai`, before it changes anything: it is no list, never the motherbee alone.
pub(crate) fn registry_unreadable(action: &str, why: &str) -> (u16, String) {
    super::installation_refusal(
        action,
        "HIVE_REGISTRY_UNREADABLE",
        format!(
            "{why}: the hives an update reaches cannot be named, and nothing was changed; retry \
             once it can be read"
        ),
        Value::Null,
    )
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
        RunEnd::RegistryUnreadable(why) => registry_unreadable(action, &why),
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
            match update.state {
                UpdateState::Done => {
                    payload["message"] = json!(format!(
                        "every hive is on the target ({}): update {} is done",
                        update.target.describe(),
                        update.id
                    ));
                    return super::installation_answer(action, 200, "ok", payload, None);
                }
                UpdateState::Held => {
                    let held = update.held_hives().unwrap_or_default();
                    payload["message"] = json!(format!(
                        "every hive is on the target ({}) but {}, which the operator rolled back \
                         (core_rollback, the one allowed divergence; their config is on the \
                         target): update {} is held. Each one's per-hive update closes it",
                        update.target.describe(),
                        held.join(", "),
                        update.id
                    ));
                    payload["held"] = json!(held);
                    return super::installation_answer(action, 200, "ok", payload, None);
                }
                UpdateState::Superseded => {
                    payload["message"] = json!(format!(
                        "update {} was superseded by {} (another target) while it ran: GET \
                         /installation/update follows that one",
                        update.id,
                        update.superseded_by.as_deref().unwrap_or("another update")
                    ));
                    payload["pending"] = json!(pending_of(&update));
                    return super::installation_answer(action, 202, "partial", payload, None);
                }
                UpdateState::Open => {}
            }
            let pending = pending_of(&update);
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

/// `get_installation_update`: a read of SY.admin's own state, so not in the audit log. The hives off
/// the target the background task has not looked at lately are looked at at once.
pub(crate) fn handle_get_installation_update(ctx: &AdminContext, params: &Value) -> (u16, String) {
    let action = "get_installation_update";
    match parse_history_flag(params) {
        Ok(history) => {
            ctx.installation_update
                .look_soon_all(Bounds::from_env().retry_check);
            super::installation_answer(
                action,
                200,
                "ok",
                ctx.installation_update.view(history),
                None,
            )
        }
        Err(detail) => super::installation_refusal(action, "INVALID_REQUEST", detail, Value::Null),
    }
}

/// `set_installation_ai`'s update (§4, step 3): the target its write made, opened (a run with
/// another target), with the motherbee's core on it, the reference. Its explicit pass is the
/// stage 3c apply, each hive applied and recorded by [`apply_change_on_hive`]; once that apply
/// ended, the caller wakes the background task, which reads the other hives' cores (before, it
/// would read the hives the apply is about to claim). Its id; why none was opened, when the hive
/// registry cannot be read or the motherbee's own core is rolled back.
pub(crate) fn open_for_installation_change(
    ctx: &AdminContext,
    motherbee: &Value,
    target: &Target,
) -> Result<String, String> {
    if motherbee_rolled_back(motherbee) {
        return Err(
            "the motherbee's own core is rolled back (core_rollback), so it names no core target: \
             install a release on the motherbee, then update_installation"
                .to_string(),
        );
    }
    let updates = &ctx.installation_update;
    let (hives, skipped) = LiveMesh::of(ctx).hives()?;
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
    updates.note_run_update(&id);
    updates.record(
        &id,
        &ctx.hive_id,
        Category::Core,
        Standing::OnTarget,
        None,
        None,
        None,
    );
    Ok(id)
}

/// `/versions` (`list_versions`, `get_versions`) as SY.admin answers it: with the installation's
/// update, its id, state, target and each hive's state; the hives off the target the background
/// task has not looked at lately are looked at at once.
pub(crate) fn with_update_summary(
    updates: &Updates,
    action: &str,
    response: Result<Value, super::AdminError>,
) -> Result<Value, super::AdminError> {
    if !matches!(action, "list_versions" | "get_versions") {
        return response;
    }
    updates.look_soon_all(Bounds::from_env().retry_check);
    response.map(|mut payload| {
        if payload.is_object() && super::payload_is_ok(&payload) {
            payload["installation_update"] = updates.summary();
        }
        payload
    })
}

/// The hives whose states move the open update: an immediate look at `hive` after SY.admin ran a
/// per-hive action on it (`update`, `core_rollback`) or a join or a removal, which never blocks.
pub(crate) fn look_after_action(updates: &Updates, hive: Option<&str>) {
    match hive.map(str::trim).filter(|hive| !hive.is_empty()) {
        Some(hive) => updates.look_now(hive),
        None => updates.wake(),
    }
}

#[cfg(test)]
#[path = "installation_update_tests.rs"]
mod tests;
