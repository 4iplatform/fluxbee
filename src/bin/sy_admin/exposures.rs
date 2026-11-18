//! io.web's exposures (docs/io-web-v2-stage-2-1.md §2, §5.1, §6): one host on the public edge,
//! `<label>.<main name>`, mapped to one fixed upstream, and opened for a window by an enablement
//! whose secret signs its links and sessions. Fluxbee Cloud alone asks for them, through IO.cloud's
//! relay.
//!
//! - **The registry** ([`Exposures`]): `state_dir/sy-admin/exposures.json`, written as
//!   `publications.json` is, and held in memory. Opening (publish, enable) writes the file before
//!   it opens anything; closing (disable, unpublish) acts first and writes after, a failed write
//!   retried every minute. Its `generation` never goes back, even when the file is lost (§2.3).
//! - **The pushes**: the edge's routes (`EDGE_WEB_ROUTES`, through [`super::WebRoutes`]) and
//!   io.web's set (`IO_WEB_EXPOSURES`), at each change and every 60 s.
//! - **The actions**: `publish_exposure`, `enable_exposure`, `disable_exposure`,
//!   `unpublish_exposure` and `issue_exposure_link`, IO.cloud@motherbee's alone
//!   ([`handle_exposure_write`]), and `list_exposures`, read by IO.cloud and Archi
//!   ([`handle_list_exposures`]).
//!
//! No log line holds a key, a link or a code; each one about an exposure names it and its host
//! (§5.10).

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::IpAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use fluxbee_sdk::mesh::{is_valid_hive_id, HiveRegistry, HiveRole, IO_WEB_PORT};
use fluxbee_sdk::protocol::{
    IoWebAccessLinkRequest, IoWebAccessLinkResponse, IoWebEnablement, IoWebEnablementKey,
    IoWebExposure, IoWebExposuresPayload, IoWebExposuresResponse, IoWebKeyState,
    WebExposureKeySlot, WebExposureUpstream, IO_WEB_ERROR_ENABLEMENT_UNKNOWN,
    IO_WEB_ERROR_EXPOSURE_UNKNOWN, MSG_IO_WEB_ACCESS_LINK, MSG_IO_WEB_ACCESS_LINK_RESPONSE,
    MSG_IO_WEB_EXPOSURES, MSG_IO_WEB_EXPOSURES_RESPONSE, WEB_ENABLEMENT_ID_PREFIX,
    WEB_EXPOSURE_ID_PREFIX,
};
use fluxbee_sdk::vault::{web_exposure_vault_key, WEB_SESSION_KEY_RESOURCE_TYPE};
use fluxbee_sdk::{
    RouterDispatcher, DEFAULT_ROOT_TENANT_ID, MSG_VAULT_PUT, MSG_VAULT_PUT_RESPONSE,
};

use super::{
    acme, hive_registry, never_reached, no_answer_detail, payload_error_code, payload_error_detail,
    payload_is_ok, registry_hive_entries, send_system_request_with_meta, unexpected_parameter,
    unix_rfc3339, AdminContext, AdminOrigin, InternalAdminDispatchResult, WebRoutesOutcome,
    IO_WEB_NODE,
};

/// The five actions that change an exposure or ask io.web for a link: Fluxbee Cloud's alone,
/// through IO.cloud@motherbee's relay (§5.1).
pub(crate) const EXPOSURE_CLOUD_ACTIONS: &[&str] = &[
    "publish_exposure",
    "enable_exposure",
    "disable_exposure",
    "unpublish_exposure",
    "issue_exposure_link",
];

/// The read of every exposure: IO.cloud's and Archi's over the mesh, the operator's over HTTP
/// (`GET /web/exposures`).
pub(crate) const LIST_EXPOSURES: &str = "list_exposures";

/// At most this many exposures (§2.1): each is a name on the one certificate the edge serves, and
/// Let's Encrypt allows 100.
pub(crate) const MAX_EXPOSURES: usize = 16;

/// An enablement's window, in seconds: one minute to 8 hours (§3, Q4).
pub(crate) const ENABLE_MIN_SECS: u64 = 60;
pub(crate) const ENABLE_MAX_SECS: u64 = 8 * 3_600;

/// Each call an exposure action makes (a vault put, a push, a link) waits this long at most
/// (§5.1): the action answers within the 30 s its caller waits, and an unanswered push comes back
/// `pending` for the 60 s loops to send again.
pub(crate) const EXPOSURE_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// io.web's set is pushed at each change and every 60 s (§2.3), so an io.web that was stopped or
/// restarted holds it within a minute; a write a closing change left undone is tried again then.
pub(crate) const IO_WEB_PUSH_INTERVAL: Duration = Duration::from_secs(60);
/// One push of the loop waits this long, as one of the edge's routes does.
pub(crate) const IO_WEB_LOOP_CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// Fluxbee's own ports, never an upstream's, whatever its kind (§2.2): where each is set.
pub(crate) const REFUSED_UPSTREAM_PORTS: &[(u16, &str)] = &[
    (22, "SSH"),
    (80, "the edge"),
    (443, "the edge"),
    // src/bin/sy_architect.rs: Archi's own listener.
    (3000, "Archi"),
    // SY.admin's HTTP API (`main`'s default listen).
    (8080, "SY.admin"),
    // src/bin/sy_orchestrator.rs: Syncthing's API.
    (8384, "Syncthing's API"),
    // src/config/mod.rs: the routers' WAN.
    (9000, "the WAN"),
    // src/bin/sy_identity.rs: identity's sync between hives.
    (9100, "identity sync"),
    (IO_WEB_PORT, "io.web"),
    (21027, "Syncthing discovery"),
    // src/bin/sy_orchestrator.rs: Syncthing's sync.
    (22000, "Syncthing"),
];

/// The registry's schema.
const REGISTRY_SCHEMA_VERSION: u32 = 1;

/// Who published an exposure: only Fluxbee Cloud does (§2.3).
const PUBLISHED_BY_CLOUD: &str = "cloud";

/// Where SY.admin keeps the registry.
pub(crate) fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sy-admin").join("exposures.json")
}

/// io.web's node name on this motherbee.
pub(crate) fn io_web_node(hive_id: &str) -> String {
    format!("{IO_WEB_NODE}@{hive_id}")
}

// ---------------------------------------------------------------------------------------------
// The registry (§2.3)
// ---------------------------------------------------------------------------------------------

/// `exposures.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RegistryFile {
    pub(crate) schema_version: u32,
    /// `max(previous + 1, now in ms)` at each write: it never goes back.
    pub(crate) generation: u64,
    #[serde(default)]
    pub(crate) exposures: Vec<ExposureRecord>,
}

/// One exposure, as the registry keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ExposureRecord {
    /// `exp:<uuid v4>`, minted at publish and never reused.
    pub(crate) exposure_id: String,
    pub(crate) label: String,
    /// `<label>.<main name>`.
    pub(crate) host: String,
    pub(crate) upstream: WebExposureUpstream,
    /// Unix seconds.
    pub(crate) published_at: i64,
    pub(crate) published_by: String,
    /// The live enablements, newest last: at most two, the current one and the one it renewed. An
    /// ended one is pruned at the next write.
    #[serde(default)]
    pub(crate) enablements: Vec<EnablementRecord>,
}

/// One enablement: its window, and the vault slot and version of its key. Never the key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EnablementRecord {
    /// `enb:<uuid v4>`.
    pub(crate) enablement_id: String,
    /// Unix seconds.
    pub(crate) from: u64,
    pub(crate) until: u64,
    pub(crate) slot: WebExposureKeySlot,
    pub(crate) key_version: i64,
}

impl ExposureRecord {
    /// The newest enablement still live at `now` (Unix seconds): the one an enable keeps and whose
    /// slot it leaves alone, and what a disable closes, with every other. None: none is live.
    pub(crate) fn current(&self, now: u64) -> Option<&EnablementRecord> {
        self.enablements
            .iter()
            .rev()
            .find(|enablement| enablement.until > now)
    }

    /// The enablement the exposure is open by at `now`, as io.web judges it (§3.4): the last one,
    /// while it is live, from which alone io.web mints new links. None: closed, the view says so
    /// and a link is `EXPOSURE_CLOSED`, though the enablement the last one renewed may run its
    /// sessions to its own end.
    pub(crate) fn open(&self, now: u64) -> Option<&EnablementRecord> {
        self.enablements
            .last()
            .filter(|enablement| enablement.until > now)
    }

    /// As io.web holds it (§5.5): no secret.
    fn for_io_web(&self) -> IoWebExposure {
        IoWebExposure {
            exposure_id: self.exposure_id.clone(),
            host: self.host.clone(),
            upstream: self.upstream.clone(),
            enablements: self
                .enablements
                .iter()
                .map(|enablement| IoWebEnablement {
                    enablement_id: enablement.enablement_id.clone(),
                    until: enablement.until,
                    slot: enablement.slot,
                    key_version: enablement.key_version,
                })
                .collect(),
        }
    }
}

/// The other key slot.
fn other_slot(slot: WebExposureKeySlot) -> WebExposureKeySlot {
    match slot {
        WebExposureKeySlot::A => WebExposureKeySlot::B,
        WebExposureKeySlot::B => WebExposureKeySlot::A,
    }
}

/// The generation after `previous`: one more, or now in ms when that is later, so a registry
/// written again after its file was lost still goes past every set io.web took (§2.3).
pub(crate) fn next_generation(previous: u64, now_ms: u64) -> u64 {
    previous.saturating_add(1).max(now_ms)
}

/// Unix milliseconds, from the clock SY.admin runs on (a test's own in the tests).
pub(crate) type ClockMs = Arc<dyn Fn() -> u64 + Send + Sync>;

/// What SY.admin holds of the registry.
struct Registry {
    file: RegistryFile,
    /// Why the file could not be read at boot: it is kept for the operator, the sets pushed are
    /// empty (they fail closed), and every exposure write answers `RECORD_FAILED`.
    unreadable: Option<String>,
    /// Why the last write of a closing change failed: memory holds a change the file does not
    /// yet, written again every minute.
    unpersisted: Option<String>,
}

/// The exposures, their file, the lock of their writes and what io.web last answered.
pub(crate) struct Exposures {
    path: PathBuf,
    clock: ClockMs,
    registry: RwLock<Registry>,
    /// One exposure write at a time (`exposure_lock`, §5.1): publish, enable, disable and
    /// unpublish. Another one is refused at once, `409 BUSY`, nothing read. The link and the list
    /// take no lock.
    write_lock: tokio::sync::Mutex<()>,
    io_web: Mutex<IoWebSync>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Exposures {
    /// The registry at `state_dir/sy-admin/exposures.json`. A missing file is an empty registry
    /// whose generation is now in ms, past every set pushed before; a file that cannot be read is
    /// logged as an ERROR, kept for the operator and held as an empty registry that refuses every
    /// write.
    pub(crate) fn load(state_dir: &Path, clock: ClockMs) -> Arc<Self> {
        let path = registry_path(state_dir);
        let now_ms = clock();
        let (file, unreadable) = match read_registry(&path) {
            Ok(Some(file)) => (file, None),
            Ok(None) => (empty_registry(now_ms), None),
            Err(reason) => {
                tracing::error!(
                    path = %path.display(),
                    reason = %reason,
                    "the exposures registry cannot be read: kept as it is for the operator; \
                     SY.admin pushes empty sets and refuses every exposure write until it is \
                     fixed or removed and SY.admin restarts"
                );
                (empty_registry(now_ms), Some(reason))
            }
        };
        if unreadable.is_none() {
            tracing::info!(
                path = %path.display(),
                exposures = file.exposures.len(),
                generation = file.generation,
                "exposures registry loaded"
            );
        }
        Arc::new(Self {
            path,
            clock,
            registry: RwLock::new(Registry {
                file,
                unreadable,
                unpersisted: None,
            }),
            write_lock: tokio::sync::Mutex::new(()),
            io_web: Mutex::new(IoWebSync::default()),
        })
    }

    pub(crate) fn now_ms(&self) -> u64 {
        (self.clock)()
    }

    pub(crate) fn now_secs(&self) -> u64 {
        self.now_ms() / 1_000
    }

    fn read<T>(&self, f: impl FnOnce(&Registry) -> T) -> T {
        let registry = self
            .registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&registry)
    }

    /// What memory holds now.
    pub(crate) fn file(&self) -> RegistryFile {
        self.read(|registry| registry.file.clone())
    }

    /// The published hosts, sorted: the edge's routes and the certificate's added names.
    pub(crate) fn hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self.read(|registry| {
            registry
                .file
                .exposures
                .iter()
                .map(|exposure| exposure.host.clone())
                .collect()
        });
        hosts.sort();
        hosts
    }

    /// Why the file could not be read at boot.
    pub(crate) fn unreadable(&self) -> Option<String> {
        self.read(|registry| registry.unreadable.clone())
    }

    /// Whether the file holds what memory holds.
    pub(crate) fn persisted(&self) -> bool {
        self.read(|registry| registry.unpersisted.is_none())
    }

    /// The set io.web gets (§5.5), with no secret.
    pub(crate) fn io_web_payload(&self) -> IoWebExposuresPayload {
        self.read(|registry| IoWebExposuresPayload {
            generation: registry.file.generation,
            exposures: registry
                .file
                .exposures
                .iter()
                .map(ExposureRecord::for_io_web)
                .collect(),
        })
    }

    /// The exposure lock, if no other exposure write holds it.
    pub(crate) fn try_lock(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        self.write_lock.try_lock().ok()
    }

    /// An opening change (§2.3): `change` on a copy, the ended enablements pruned and the
    /// generation moved on, then the file written; memory takes it only once the file holds it.
    /// The caller holds the exposure lock.
    fn open_change(&self, change: impl FnOnce(&mut RegistryFile)) -> Result<(), String> {
        let now_ms = self.now_ms();
        let mut next = self.file();
        change(&mut next);
        prune_ended(&mut next, now_ms / 1_000);
        next.generation = next_generation(next.generation, now_ms);
        write_registry(&self.path, &next)?;
        let mut registry = self
            .registry
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.file = next;
        registry.unpersisted = None;
        Ok(())
    }

    /// A closing change (§2.3): `change` on memory now, the ended enablements pruned and the
    /// generation moved on. The caller pushes, then writes ([`Self::persist`]). The caller holds
    /// the exposure lock.
    fn close_change(&self, change: impl FnOnce(&mut RegistryFile)) {
        let now_ms = self.now_ms();
        let mut registry = self
            .registry
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        change(&mut registry.file);
        prune_ended(&mut registry.file, now_ms / 1_000);
        registry.file.generation = next_generation(registry.file.generation, now_ms);
        registry.unpersisted = Some("not written yet".to_string());
    }

    /// Write again what a closing change left unwritten: true when the file holds what memory
    /// holds. The caller holds the exposure lock, so no change is under way.
    pub(crate) fn retry_persist(&self) -> bool {
        if self.persisted() {
            return true;
        }
        let written = self.persist().is_ok();
        if written {
            tracing::info!(
                path = %self.path.display(),
                "the exposures registry is written: the file holds the last change again"
            );
        }
        written
    }

    /// The loop (§2.3, §5.1): every minute, the write a closing change left undone (under the
    /// exposure lock: a change under way writes and pushes for itself), then io.web's whole set.
    pub(crate) async fn run_io_web(
        self: Arc<Self>,
        client: Arc<RouterDispatcher>,
        hive_id: String,
    ) {
        let mut ticker = tokio::time::interval(IO_WEB_PUSH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Some(_one_write) = self.try_lock() {
                self.retry_persist();
            }
            self.push_io_web(&client, &hive_id, IO_WEB_LOOP_CALL_TIMEOUT)
                .await;
        }
    }

    /// Write what memory holds. A failure is kept, logged, and written again every minute.
    fn persist(&self) -> Result<(), String> {
        let file = self.file();
        let result = write_registry(&self.path, &file);
        let mut registry = self
            .registry
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &result {
            Ok(()) if registry.file.generation == file.generation => registry.unpersisted = None,
            Ok(()) => {}
            Err(reason) => {
                tracing::error!(
                    path = %self.path.display(),
                    reason = %reason,
                    "the exposures registry could not be written: memory holds the change, and \
                     the write is tried again every minute"
                );
                registry.unpersisted = Some(reason.clone());
            }
        }
        result
    }
}

fn empty_registry(now_ms: u64) -> RegistryFile {
    RegistryFile {
        schema_version: REGISTRY_SCHEMA_VERSION,
        generation: now_ms,
        exposures: Vec::new(),
    }
}

/// Drop the enablements that ended by `now` (Unix seconds).
fn prune_ended(file: &mut RegistryFile, now: u64) {
    for exposure in &mut file.exposures {
        exposure
            .enablements
            .retain(|enablement| enablement.until > now);
    }
}

/// The file, `None` when there is none. Anything else that keeps it from being the registry is
/// the error.
fn read_registry(path: &Path) -> Result<Option<RegistryFile>, String> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("{} cannot be read: {err}", path.display())),
    };
    let file: RegistryFile = serde_json::from_slice(&raw)
        .map_err(|err| format!("{} does not parse: {err}", path.display()))?;
    if file.schema_version != REGISTRY_SCHEMA_VERSION {
        return Err(format!(
            "{} has schema {} (expected {REGISTRY_SCHEMA_VERSION})",
            path.display(),
            file.schema_version
        ));
    }
    for exposure in &file.exposures {
        if web_exposure_vault_key(&exposure.exposure_id, WebExposureKeySlot::A).is_none() {
            return Err(format!(
                "{} holds an exposure id that is not exp:<uuid>: {}",
                path.display(),
                exposure.exposure_id
            ));
        }
    }
    Ok(Some(file))
}

/// Written as `publications.json` is: a temporary file of mode 0640 in a directory of mode 0750,
/// synced, then renamed over the file.
fn write_registry(path: &Path, file: &RegistryFile) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "the exposures registry has no directory".to_string())?;
    fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o750))
        .map_err(|err| format!("chmod {}: {err}", parent.display()))?;
    let temp = parent.join(format!(
        ".exposures-{}-{}.tmp",
        std::process::id(),
        Uuid::new_v4().simple()
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
        fs::set_permissions(path, fs::Permissions::from_mode(0o640))
            .map_err(|err| format!("chmod {}: {err}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

// ---------------------------------------------------------------------------------------------
// What Cloud names (§2.1, §2.2)
// ---------------------------------------------------------------------------------------------

/// One DNS label: lowercase ASCII letters, digits and hyphens, 1 to 63 characters, starting and
/// ending with a letter or a digit, and never hyphens as its 3rd and 4th characters. Taken as
/// given: an uppercase label is refused, never folded.
pub(crate) fn check_label(label: &str) -> Result<(), String> {
    let edge_ok = |byte: Option<&u8>| {
        byte.is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    };
    let valid = (1..=63).contains(&label.len())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && edge_ok(label.as_bytes().first())
        && edge_ok(label.as_bytes().last());
    if valid {
        // A label shaped '??--' is a reserved LDH label (R-LDH, RFC 5890 §2.3.1; 'xn--' is one):
        // Let's Encrypt refuses it when the order is created, so one such host would fail every
        // order that carries it (§2.1, §5.2).
        if label.as_bytes().get(2..4) == Some(b"--".as_slice()) {
            return Err(format!(
                "label '{label}' has hyphens as its 3rd and 4th characters: a label shaped '??--' \
                 is a reserved LDH label (R-LDH, as 'xn--' is), which the CA refuses"
            ));
        }
        return Ok(());
    }
    let folded = label.to_ascii_lowercase();
    let hint = if folded != label && check_label(&folded).is_ok() {
        " (a label is taken as given, never folded: name it in lowercase)"
    } else {
        ""
    };
    Err(format!(
        "label '{label}' is not one DNS label: 1 to 63 lowercase letters, digits and hyphens, \
         starting and ending with a letter or a digit{hint}"
    ))
}

/// An upstream SY.admin takes (§2.2): Archi; a worker of the registry by its hive id; or a private
/// IP literal that is no hive of the registry. Never one of Fluxbee's own ports.
pub(crate) fn check_upstream(
    upstream: &WebExposureUpstream,
    admin_hive: &str,
    registry: &HiveRegistry,
) -> Result<(), String> {
    match upstream {
        WebExposureUpstream::Archi {} => Ok(()),
        WebExposureUpstream::Hive { hive, port } => {
            check_port(*port)?;
            if !is_valid_hive_id(hive) {
                return Err(format!("upstream.hive '{hive}' is not a hive id"));
            }
            if hive == admin_hive {
                return Err(format!(
                    "upstream.hive '{hive}' is the motherbee: only Archi is reached there \
                     (upstream {{\"kind\": \"archi\"}})"
                ));
            }
            match registry.role(hive) {
                Some(HiveRole::Worker) => Ok(()),
                Some(HiveRole::Ingress) => Err(format!(
                    "upstream.hive '{hive}' is an ingress: an upstream is a worker"
                )),
                Some(HiveRole::Egress) => Err(format!(
                    "upstream.hive '{hive}' is an egress: an upstream is a worker"
                )),
                None if registry.lists(hive) => Err(format!(
                    "upstream.hive '{hive}' is not a connected worker with its join done in the \
                     hive registry"
                )),
                None => Err(format!(
                    "upstream.hive '{hive}' is not a hive of the registry"
                )),
            }
        }
        WebExposureUpstream::Address { address, port } => {
            check_port(*port)?;
            check_private_address(address)?;
            if let Some(hive) = hive_at_address(registry, address) {
                return Err(format!(
                    "upstream.address {address} is the address of hive '{hive}': name the hive \
                     instead (upstream {{\"kind\": \"hive\", \"hive\": \"{hive}\", \"port\": \
                     {port}}})"
                ));
            }
            Ok(())
        }
    }
}

fn check_port(port: u16) -> Result<(), String> {
    if port == 0 {
        return Err("upstream.port 0 is no port".to_string());
    }
    if let Some((_, what)) = REFUSED_UPSTREAM_PORTS
        .iter()
        .find(|(refused, _)| *refused == port)
    {
        return Err(format!(
            "upstream.port {port} is {what}'s, one of Fluxbee's own ports: an exposure never leads \
             to a Fluxbee service"
        ));
    }
    Ok(())
}

/// An address, IPv4-mapped IPv6 judged as its IPv4.
fn canonical_ip(address: &IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(*v6)),
        v4 => *v4,
    }
}

/// 10/8, 172.16/12, 192.168/16 or fc00::/7, and never loopback, link-local, unspecified,
/// multicast or broadcast.
fn check_private_address(address: &IpAddr) -> Result<(), String> {
    let ip = canonical_ip(address);
    let refused = |what: &str| {
        Err(format!(
            "upstream.address {address} is {what}: an address upstream is a private one (10/8, \
             172.16/12, 192.168/16 or fc00::/7)"
        ))
    };
    if ip.is_loopback() {
        return refused("loopback");
    }
    if ip.is_unspecified() {
        return refused("unspecified");
    }
    if ip.is_multicast() {
        return refused("multicast");
    }
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_broadcast() {
                return refused("broadcast");
            }
            if v4.is_link_local() {
                return refused("link-local");
            }
            if v4.is_private() {
                Ok(())
            } else {
                refused("not private")
            }
        }
        IpAddr::V6(v6) => {
            let segment = v6.segments()[0];
            if segment & 0xffc0 == 0xfe80 {
                return refused("link-local");
            }
            if segment & 0xfe00 == 0xfc00 {
                Ok(())
            } else {
                refused("not private")
            }
        }
    }
}

/// The hive of the registry whose recorded `address` is `address`. An address the registry records
/// as a name is not compared: SY.admin resolves no name.
fn hive_at_address(registry: &HiveRegistry, address: &IpAddr) -> Option<String> {
    let wanted = canonical_ip(address);
    registry_hive_entries(registry)
        .ok()?
        .into_iter()
        .find(|entry| {
            entry
                .field("address")
                .map(str::trim)
                .and_then(|recorded| {
                    recorded.parse::<IpAddr>().ok().or_else(|| {
                        recorded
                            .parse::<std::net::SocketAddr>()
                            .ok()
                            .map(|socket| socket.ip())
                    })
                })
                .is_some_and(|recorded| canonical_ip(&recorded) == wanted)
        })
        .map(|entry| entry.name().to_string())
}

/// An exposure id as SY.admin mints it, `exp:<uuid>` hyphenated and lowercase.
fn check_exposure_id(exposure_id: &str) -> Result<(), String> {
    match web_exposure_vault_key(exposure_id, WebExposureKeySlot::A) {
        Some(_) => Ok(()),
        None => Err(format!(
            "exposure_id '{exposure_id}' is not an exposure id ({WEB_EXPOSURE_ID_PREFIX}<uuid>)"
        )),
    }
}

/// `publish_exposure`'s params.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishParams {
    label: String,
    upstream: WebExposureUpstream,
}

/// `enable_exposure`'s params.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnableParams {
    exposure_id: String,
    /// A whole number of seconds.
    duration_s: u64,
}

/// The params of `disable_exposure`, `unpublish_exposure` and `issue_exposure_link`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExposureParams {
    exposure_id: String,
}

/// The fields IO.cloud's relay may carry for each exposure action (`enforce_cloud_relay_content`):
/// its own, and never a tenant. `None`: not an exposure action.
pub(crate) fn relay_fields(action: &str) -> Option<&'static [&'static str]> {
    match action {
        "publish_exposure" => Some(&["label", "upstream"]),
        "enable_exposure" => Some(&["exposure_id", "duration_s"]),
        "disable_exposure" | "unpublish_exposure" | "issue_exposure_link" => Some(&["exposure_id"]),
        LIST_EXPOSURES => Some(&[]),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// io.web's answers (§5.5)
// ---------------------------------------------------------------------------------------------

/// How io.web answered an `IO_WEB_EXPOSURES`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IoWebAnswer {
    /// It holds the set: the generation it holds, whether the set changed, and each live
    /// enablement's key.
    Taken {
        generation: u64,
        changed: bool,
        keys: Vec<IoWebEnablementKey>,
    },
    /// It answered with an error code.
    Refused { code: String, detail: String },
    /// No answer in time, or another error on the way.
    NoAnswer(String),
    /// The router reached no io.web: UNREACHABLE or TTL_EXCEEDED.
    Absent(String),
}

impl IoWebAnswer {
    fn state(&self) -> &'static str {
        match self {
            IoWebAnswer::Taken { .. } => "taken",
            IoWebAnswer::Refused { .. } => "refused",
            IoWebAnswer::NoAnswer(_) => "no_answer",
            IoWebAnswer::Absent(_) => "absent",
        }
    }

    /// What a change is judged by (`log_change`'s rule): the answer's kind and detail, and, for a
    /// taken set, which keys io.web lacks. A new generation alone is no news.
    fn news_key(&self) -> String {
        match self {
            IoWebAnswer::Taken { keys, .. } => {
                let unavailable: Vec<&str> = keys
                    .iter()
                    .filter(|key| key.key == IoWebKeyState::Unavailable)
                    .map(|key| key.enablement_id.as_str())
                    .collect();
                format!("taken; unavailable: {}", unavailable.join(","))
            }
            IoWebAnswer::Refused { code, detail } => format!("refused: {code}: {detail}"),
            IoWebAnswer::NoAnswer(detail) => format!("no_answer: {detail}"),
            IoWebAnswer::Absent(detail) => format!("absent: {detail}"),
        }
    }

    fn status(&self) -> Value {
        match self {
            IoWebAnswer::Taken {
                generation,
                changed,
                keys,
            } => json!({
                "state": self.state(),
                "generation": generation,
                "changed": changed,
                "keys": keys,
            }),
            IoWebAnswer::Refused { code, detail } => json!({
                "state": self.state(),
                "error_code": code,
                "detail": detail,
            }),
            IoWebAnswer::NoAnswer(detail) | IoWebAnswer::Absent(detail) => json!({
                "state": self.state(),
                "detail": detail,
            }),
        }
    }
}

/// What SY.admin last pushed to io.web and how io.web answered.
#[derive(Debug, Default)]
struct IoWebSync {
    pushed_generation: Option<u64>,
    acknowledged_generation: Option<u64>,
    last: Option<IoWebAnswer>,
    /// Since when io.web answers this way (Unix seconds).
    since: Option<i64>,
}

impl Exposures {
    /// One `IO_WEB_EXPOSURES` of the whole set, waited for `timeout`, its answer kept. An answer to
    /// a push that a newer one overtook is not kept: the newer push's answer names what io.web
    /// holds.
    pub(crate) async fn push_io_web(
        &self,
        client: &RouterDispatcher,
        hive_id: &str,
        timeout: Duration,
    ) -> IoWebAnswer {
        let payload = self.io_web_payload();
        let generation = payload.generation;
        {
            let mut sync = locked(&self.io_web);
            sync.pushed_generation = Some(sync.pushed_generation.unwrap_or(0).max(generation));
        }
        let target = io_web_node(hive_id);
        let body = match serde_json::to_value(&payload) {
            Ok(body) => body,
            Err(err) => return IoWebAnswer::NoAnswer(format!("the set does not serialize: {err}")),
        };
        let sent = send_system_request_with_meta(
            client,
            &target,
            MSG_IO_WEB_EXPOSURES,
            MSG_IO_WEB_EXPOSURES_RESPONSE,
            body,
            16,
            None,
            None,
            Some(target.clone()),
            None,
            None,
            timeout,
        )
        .await;
        let answer = match sent {
            Ok(value) => match serde_json::from_value::<IoWebExposuresResponse>(value.clone()) {
                Ok(IoWebExposuresResponse::Ok {
                    generation,
                    changed,
                    enablements,
                }) => IoWebAnswer::Taken {
                    generation,
                    changed,
                    keys: enablements,
                },
                Ok(IoWebExposuresResponse::Error {
                    error_code,
                    error_detail,
                }) => IoWebAnswer::Refused {
                    code: error_code,
                    detail: error_detail,
                },
                Err(err) => IoWebAnswer::Refused {
                    code: payload_error_code(&value)
                        .unwrap_or_else(|| "INVALID_ANSWER".to_string()),
                    detail: payload_error_detail(&value)
                        .unwrap_or_else(|| format!("an answer SY.admin cannot read: {err}")),
                },
            },
            Err(err) if never_reached(&err) => IoWebAnswer::Absent(err.to_string()),
            Err(err) => IoWebAnswer::NoAnswer(no_answer_detail(&err, timeout)),
        };
        self.record_io_web(generation, &answer);
        answer
    }

    /// Keep io.web's answer to the push of `generation`, and log it when it changed.
    fn record_io_web(&self, generation: u64, answer: &IoWebAnswer) {
        let now = (self.now_ms() / 1_000) as i64;
        let previous = {
            let mut sync = locked(&self.io_web);
            if sync
                .pushed_generation
                .is_some_and(|newest| newest > generation)
            {
                return;
            }
            if let IoWebAnswer::Taken { generation, .. } = answer {
                sync.acknowledged_generation =
                    Some(sync.acknowledged_generation.unwrap_or(0).max(*generation));
            }
            let previous = sync.last.replace(answer.clone());
            let same = previous
                .as_ref()
                .is_some_and(|previous| previous.news_key() == answer.news_key());
            if same {
                return;
            }
            sync.since = Some(now);
            previous
        };
        self.log_io_web_change(generation, answer, previous.as_ref());
    }

    /// One line for io.web's new answer, and one per enablement whose key io.web lacks now or held
    /// before, named by its exposure and host. Never a key.
    fn log_io_web_change(
        &self,
        generation: u64,
        answer: &IoWebAnswer,
        previous: Option<&IoWebAnswer>,
    ) {
        let before = previous.map_or("the first push", IoWebAnswer::state);
        match answer {
            IoWebAnswer::Taken { changed, .. } => tracing::info!(
                generation,
                changed = *changed,
                before = %before,
                "io.web took the exposures"
            ),
            IoWebAnswer::Refused { code, detail } => tracing::warn!(
                generation,
                error_code = %code,
                detail = %detail,
                before = %before,
                "io.web refused the exposures; pushed again every minute"
            ),
            IoWebAnswer::NoAnswer(detail) => tracing::warn!(
                generation,
                detail = %detail,
                before = %before,
                "io.web did not answer the exposures; pushed again every minute"
            ),
            IoWebAnswer::Absent(detail)
                if self.read(|registry| registry.file.exposures.is_empty()) =>
            {
                tracing::info!(
                    generation,
                    detail = %detail,
                    before = %before,
                    "no io.web to push the (empty) exposures to"
                )
            }
            IoWebAnswer::Absent(detail) => tracing::warn!(
                generation,
                detail = %detail,
                before = %before,
                "no io.web to push the exposures to (run_node IO.web@motherbee, in the root tenant)"
            ),
        }
        let keys = |answer: Option<&IoWebAnswer>| -> BTreeMap<String, IoWebKeyState> {
            match answer {
                Some(IoWebAnswer::Taken { keys, .. }) => keys
                    .iter()
                    .map(|key| (key.enablement_id.clone(), key.key))
                    .collect(),
                _ => BTreeMap::new(),
            }
        };
        let then = keys(previous);
        let IoWebAnswer::Taken { keys: now, .. } = answer else {
            return;
        };
        let file = self.file();
        for key in now {
            if then.get(&key.enablement_id) == Some(&key.key) {
                continue;
            }
            let Some(exposure) = file.exposures.iter().find(|exposure| {
                exposure
                    .enablements
                    .iter()
                    .any(|enablement| enablement.enablement_id == key.enablement_id)
            }) else {
                continue;
            };
            match key.key {
                IoWebKeyState::Ready => tracing::info!(
                    exposure_id = %exposure.exposure_id,
                    host = %exposure.host,
                    enablement_id = %key.enablement_id,
                    "io.web holds the enablement's key"
                ),
                IoWebKeyState::Unavailable => tracing::warn!(
                    exposure_id = %exposure.exposure_id,
                    host = %exposure.host,
                    enablement_id = %key.enablement_id,
                    detail = %key.detail.as_deref().unwrap_or(""),
                    "io.web cannot use the enablement's key: its links and sessions fail closed"
                ),
            }
        }
    }

    /// Whether io.web's last answer took the set of `generation` or a later one (§6). A stopped,
    /// restarting or slow io.web has not: its last answer is no answer, or an older set's.
    pub(crate) fn io_web_acknowledged(&self, generation: u64) -> bool {
        let sync = locked(&self.io_web);
        matches!(
            &sync.last,
            Some(IoWebAnswer::Taken { generation: held, .. }) if *held >= generation
        )
    }

    /// io.web's part of `web_status` (§5.1): the generation pushed last, the one io.web holds,
    /// its last answer and since when it answers that way.
    pub(crate) fn io_web_status(&self) -> Value {
        let sync = locked(&self.io_web);
        json!({
            "pushed_generation": sync.pushed_generation,
            "acknowledged_generation": sync.acknowledged_generation,
            "last_answer": sync.last.as_ref().map(IoWebAnswer::status),
            "since": sync.since.map(unix_rfc3339),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// The answers (§6)
// ---------------------------------------------------------------------------------------------

/// io.web's state in a view or an answer (§6): `absent` while IO.web@motherbee has no identity
/// (never launched: what `enable_exposure` answers `IO_WEB_ABSENT` for); `acknowledged` once io.web
/// answered ok at the current generation; `pending` otherwise (stopped, after
/// `kill_node IO.web@motherbee`, restarting, or slow).
fn io_web_state(io_web_absent: bool, acknowledged: bool) -> &'static str {
    match (io_web_absent, acknowledged) {
        (true, _) => "absent",
        (false, true) => "acknowledged",
        (false, false) => "pending",
    }
}

/// What every view of one answer is judged against: the registry's generation, now (Unix
/// seconds), and whether io.web has an identity, read once per answer.
pub(crate) struct ViewFacts {
    generation: u64,
    now: u64,
    io_web_absent: bool,
}

impl ViewFacts {
    /// The facts of the registry at `generation`, now.
    pub(crate) fn of(ctx: &AdminContext, generation: u64) -> Self {
        Self {
            generation,
            now: ctx.exposures.now_secs(),
            io_web_absent: io_web_ilk(ctx).is_err(),
        }
    }
}

/// The exposure view, in every action's answer (§6).
pub(crate) fn exposure_view(
    ctx: &AdminContext,
    exposure: &ExposureRecord,
    facts: &ViewFacts,
) -> Value {
    // Enabled while io.web mints links (§3.4): by the last enablement, while it is live.
    let open = exposure.open(facts.now);
    let acknowledged = ctx.exposures.io_web_acknowledged(facts.generation);
    json!({
        "exposure_id": exposure.exposure_id,
        "label": exposure.label,
        "host": exposure.host,
        "url": format!("https://{}/", exposure.host),
        "upstream": exposure.upstream,
        "state": if open.is_some() { "enabled" } else { "closed" },
        "enabled_until": open.map(|open| unix_rfc3339(open.until as i64)),
        "published_at": unix_rfc3339(exposure.published_at),
        "certificate": ctx.acme.host_certificate(&exposure.host),
        "route": { "state": ctx.web.route_state(&exposure.host) },
        "io_web": { "state": io_web_state(facts.io_web_absent, acknowledged) },
    })
}

/// Every exposure's view, in the order they were published.
pub(crate) fn exposure_views(ctx: &AdminContext) -> Vec<Value> {
    let file = ctx.exposures.file();
    let facts = ViewFacts::of(ctx, file.generation);
    file.exposures
        .iter()
        .map(|exposure| exposure_view(ctx, exposure, &facts))
        .collect()
}

fn answer_ok(action: &str, payload: Value) -> InternalAdminDispatchResult {
    InternalAdminDispatchResult {
        http_status: 200,
        envelope: json!({
            "status": "ok",
            "action": action,
            "payload": payload,
            "error_code": Value::Null,
            "error_detail": Value::Null,
        }),
    }
}

fn answer_error(
    action: &str,
    http_status: u16,
    error_code: &str,
    detail: impl Into<String>,
) -> InternalAdminDispatchResult {
    let retryable = matches!(error_code, "BUSY" | "IO_WEB_UNAVAILABLE");
    InternalAdminDispatchResult {
        http_status,
        envelope: json!({
            "status": "error",
            "action": action,
            "payload": if retryable { json!({ "retryable": true }) } else { Value::Null },
            "error_code": error_code,
            "error_detail": detail.into(),
        }),
    }
}

fn invalid(action: &str, detail: impl Into<String>) -> InternalAdminDispatchResult {
    answer_error(action, 400, "INVALID_REQUEST", detail)
}

fn not_found(action: &str, exposure_id: &str) -> InternalAdminDispatchResult {
    answer_error(
        action,
        404,
        "EXPOSURE_NOT_FOUND",
        format!("no exposure {exposure_id} is published"),
    )
}

/// `409 BUSY`, `payload.retryable: true`: another exposure write holds the lock, and nothing was
/// read.
fn busy(action: &str) -> InternalAdminDispatchResult {
    answer_error(
        action,
        409,
        "BUSY",
        "another exposure change (publish, enable, disable or unpublish) is running, one at a \
         time: nothing was read or changed. Ask again once it has answered.",
    )
}

/// `500 RECORD_FAILED` while the registry on disk could not be read at boot: nothing opens or
/// closes until the operator fixes or removes the file and SY.admin restarts.
fn registry_unreadable(action: &str, path: &Path, reason: &str) -> InternalAdminDispatchResult {
    answer_error(
        action,
        500,
        "RECORD_FAILED",
        format!(
            "the exposures registry {} could not be read at SY.admin's start ({reason}): nothing \
             was changed. The operator fixes or removes the file, then restarts SY.admin.",
            path.display()
        ),
    )
}

fn parse<T: serde::de::DeserializeOwned>(
    action: &str,
    params: &Value,
) -> Result<T, InternalAdminDispatchResult> {
    let params = if params.is_null() {
        json!({})
    } else {
        params.clone()
    };
    serde_json::from_value(params).map_err(|err| invalid(action, format!("{action}: {err}")))
}

/// §5.1 (2): the five actions are IO.cloud@motherbee's alone, whatever the mesh's table says; the
/// HTTP API and the executor included.
fn cloud_alone(ctx: &AdminContext, action: &str, origin: AdminOrigin<'_>) -> Result<(), String> {
    let relay = format!("IO.cloud@{}", ctx.hive_id);
    match origin {
        AdminOrigin::Mesh(caller) if caller == relay => Ok(()),
        AdminOrigin::Mesh(caller) => Err(format!(
            "{action} is Fluxbee Cloud's alone, through {relay}: {} may not ask for it",
            if caller.is_empty() {
                "a caller with no name"
            } else {
                caller
            }
        )),
        AdminOrigin::Http => Err(format!(
            "{action} is Fluxbee Cloud's alone, through {relay}: the HTTP API may not ask for it"
        )),
        AdminOrigin::Executor => Err(format!(
            "{action} is Fluxbee Cloud's alone, through {relay}: the executor may not run it"
        )),
    }
}

// ---------------------------------------------------------------------------------------------
// The actions (§5.1)
// ---------------------------------------------------------------------------------------------

/// The five actions only IO.cloud's relay may ask for, each after its origin is checked.
pub(crate) async fn handle_exposure_write(
    ctx: &AdminContext,
    client: &RouterDispatcher,
    action: &str,
    params: &Value,
    origin: AdminOrigin<'_>,
) -> InternalAdminDispatchResult {
    if let Err(detail) = cloud_alone(ctx, action, origin) {
        tracing::warn!(
            action = %action,
            origin = %origin.audit_label(),
            "an exposure action refused: it is Fluxbee Cloud's alone"
        );
        return answer_error(action, 403, "UNAUTHORIZED", detail);
    }
    match action {
        "publish_exposure" => publish(ctx, client, params).await,
        "enable_exposure" => enable(ctx, client, params).await,
        "disable_exposure" => disable(ctx, client, params).await,
        "unpublish_exposure" => unpublish(ctx, client, params).await,
        "issue_exposure_link" => issue_link(ctx, client, params).await,
        _ => invalid(action, format!("{action} is not an exposure action")),
    }
}

/// `list_exposures`: every exposure's view (§6). A read: no lock, no call.
pub(crate) fn handle_list_exposures(
    ctx: &AdminContext,
    params: &Value,
) -> InternalAdminDispatchResult {
    if let Some(field) = unexpected_parameter(params) {
        return invalid(
            LIST_EXPOSURES,
            format!("{LIST_EXPOSURES} takes no parameters (got {field})"),
        );
    }
    let views = exposure_views(ctx);
    answer_ok(
        LIST_EXPOSURES,
        json!({
            "count": views.len(),
            "max": MAX_EXPOSURES,
            "exposures": views,
        }),
    )
}

/// The route's state in an answer, from the push that answer made: `acknowledged`, `no_edge` or
/// `pending`.
fn route_state_of(outcome: &WebRoutesOutcome) -> &'static str {
    match outcome {
        WebRoutesOutcome::Acknowledged => "acknowledged",
        WebRoutesOutcome::NoEdge(_) => "no_edge",
        WebRoutesOutcome::Refused(_) | WebRoutesOutcome::NoAnswer(_) => "pending",
    }
}

/// Whether the push an unpublish made was taken at its `generation` or a later one.
fn taken_at(answer: &IoWebAnswer, generation: u64) -> bool {
    matches!(answer, IoWebAnswer::Taken { generation: held, .. } if *held >= generation)
}

/// `publish_exposure {label, upstream}` (§5.1): the field checks, the lock, (1) the host under the
/// main name, (2) the label's rules, (3) the file, (4) the edge's routes and io.web's set pushed,
/// (5) the ACME client woken, (6) the answer, `{exposure, created}`.
async fn publish(
    ctx: &AdminContext,
    client: &RouterDispatcher,
    params: &Value,
) -> InternalAdminDispatchResult {
    let action = "publish_exposure";
    let request: PublishParams = match parse(action, params) {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    if let Err(detail) = check_label(&request.label) {
        return invalid(action, detail);
    }
    if let Err(detail) = check_upstream(&request.upstream, &ctx.hive_id, &hive_registry(ctx)) {
        return invalid(action, detail);
    }
    let Some(_one_write) = ctx.exposures.try_lock() else {
        return busy(action);
    };
    if let Some(reason) = ctx.exposures.unreadable() {
        return registry_unreadable(action, &registry_path(&ctx.state_dir), &reason);
    }
    // (1) The host, under the main name the certificate carries.
    let main = match acme::certified_name(ctx.public_base_url.as_deref()) {
        Ok(main) => main,
        Err(reason) => {
            return answer_error(
                action,
                409,
                "NO_MAIN_HOST",
                format!(
                    "the hive's admin.public_base_url gives no host to publish under: {reason}"
                ),
            )
        }
    };
    let host = format!("{}.{main}", request.label);
    if host.len() > 253 {
        return invalid(
            action,
            format!("the host {host} is longer than a DNS name (253 characters)"),
        );
    }
    // (2) One label, one exposure; one exposure of Archi; at most MAX_EXPOSURES.
    let file = ctx.exposures.file();
    let now = ctx.exposures.now_secs();
    if let Some(existing) = file
        .exposures
        .iter()
        .find(|exposure| exposure.label == request.label)
    {
        if existing.upstream == request.upstream {
            let facts = ViewFacts::of(ctx, file.generation);
            return answer_ok(
                action,
                json!({
                    "exposure": exposure_view(ctx, existing, &facts),
                    "created": false,
                }),
            );
        }
        return answer_error(
            action,
            409,
            "EXPOSURE_EXISTS",
            format!(
                "the label {} is published already ({}), with another upstream",
                request.label, existing.exposure_id
            ),
        );
    }
    if matches!(request.upstream, WebExposureUpstream::Archi {}) {
        if let Some(archi) = file
            .exposures
            .iter()
            .find(|exposure| matches!(exposure.upstream, WebExposureUpstream::Archi {}))
        {
            return answer_error(
                action,
                409,
                "EXPOSURE_EXISTS",
                format!(
                    "Archi is exposed already, as {} ({}): one exposure leads to Archi",
                    archi.label, archi.exposure_id
                ),
            );
        }
    }
    if file.exposures.len() >= MAX_EXPOSURES {
        return answer_error(
            action,
            409,
            "EXPOSURE_LIMIT",
            format!("{MAX_EXPOSURES} exposures are published, the most a hive holds"),
        );
    }
    // (3) The file first: nothing opens before it holds the exposure.
    let record = ExposureRecord {
        exposure_id: format!("{WEB_EXPOSURE_ID_PREFIX}{}", Uuid::new_v4()),
        label: request.label,
        host,
        upstream: request.upstream,
        published_at: now as i64,
        published_by: PUBLISHED_BY_CLOUD.to_string(),
        enablements: Vec::new(),
    };
    if let Err(reason) = ctx
        .exposures
        .open_change(|file| file.exposures.push(record.clone()))
    {
        return answer_error(
            action,
            500,
            "RECORD_FAILED",
            format!("the exposures registry could not be written: {reason}; nothing was published"),
        );
    }
    // (4) The edge's routes and io.web's set, together.
    let ((routes, _), _) = tokio::join!(
        ctx.web.push_once(client, EXPOSURE_CALL_TIMEOUT),
        ctx.exposures
            .push_io_web(client, &ctx.hive_id, EXPOSURE_CALL_TIMEOUT)
    );
    // (5) The certificate's names follow the published hosts.
    ctx.acme.names_changed();
    tracing::info!(
        exposure_id = %record.exposure_id,
        host = %record.host,
        upstream = %serde_json::to_string(&record.upstream).unwrap_or_default(),
        route = %route_state_of(&routes.outcome),
        "exposure published"
    );
    let facts = ViewFacts::of(ctx, ctx.exposures.file().generation);
    answer_ok(
        action,
        json!({
            "exposure": exposure_view(ctx, &record, &facts),
            "created": true,
        }),
    )
}

/// io.web's ILK, which the vault put names as the key's owner; `IO_WEB_ABSENT` without one.
fn io_web_ilk(ctx: &AdminContext) -> Result<String, String> {
    let node = io_web_node(&ctx.hive_id);
    match (ctx.node_ilk)(&ctx.config_dir, &node) {
        Ok(Some(ilk)) => Ok(ilk),
        Ok(None) => Err(format!(
            "{node} has no ILK in identity: run_node {node} in the root tenant first"
        )),
        Err(reason) => Err(format!(
            "{node}'s ILK cannot be read from identity ({reason}): run_node {node} in the root \
             tenant first"
        )),
    }
}

/// A new key, 32 bytes from the OS, as 64 hex. SY.admin keeps no copy past the vault's put.
fn mint_key() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// The vault put of a slot's key (§3.1): `{secret}`, resource type `web_session_key`, the root
/// tenant, owned by io.web's ILK. Returns the version the put wrote.
async fn put_key(
    ctx: &AdminContext,
    client: &RouterDispatcher,
    vault_key: &str,
    owner_ilk: &str,
    key: String,
) -> Result<i64, String> {
    let target = format!("SY.vault@{}", ctx.hive_id);
    let answer = send_system_request_with_meta(
        client,
        &target,
        MSG_VAULT_PUT,
        MSG_VAULT_PUT_RESPONSE,
        json!({
            "key": vault_key,
            "value": { "secret": key },
            "metadata": {
                "tenant_id": DEFAULT_ROOT_TENANT_ID,
                "resource_type": WEB_SESSION_KEY_RESOURCE_TYPE,
                "ilk": owner_ilk,
                "description": "io.web's key of an exposure's enablement",
            },
        }),
        16,
        Some(ctx.self_ilk_id.clone()),
        None,
        Some(target.clone()),
        Some("vault_put".to_string()),
        None,
        EXPOSURE_CALL_TIMEOUT,
    )
    .await
    .map_err(|err| {
        format!(
            "SY.vault did not answer: {}",
            no_answer_detail(&err, EXPOSURE_CALL_TIMEOUT)
        )
    })?;
    if !payload_is_ok(&answer) {
        return Err(format!(
            "SY.vault refused the put ({}): {}",
            payload_error_code(&answer).unwrap_or_else(|| "ERROR".to_string()),
            payload_error_detail(&answer).unwrap_or_default()
        ));
    }
    answer
        .get("version")
        .and_then(Value::as_i64)
        .ok_or_else(|| "SY.vault's answer named no version".to_string())
}

/// `enable_exposure {exposure_id, duration_s}` (§5.1): (1) the window, the lock, (2) io.web's ILK,
/// (3) a new key put in the free slot, (4) the file, (5) io.web's set pushed, (6) the answer,
/// `{exposure, renewed}`. With at most two enablements live, a retried enable (after a lost
/// answer) makes one more, and the one before the enablement it renews ends at once.
async fn enable(
    ctx: &AdminContext,
    client: &RouterDispatcher,
    params: &Value,
) -> InternalAdminDispatchResult {
    let action = "enable_exposure";
    let request: EnableParams = match parse(action, params) {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    if let Err(detail) = check_exposure_id(&request.exposure_id) {
        return invalid(action, detail);
    }
    // (1) The window.
    if !(ENABLE_MIN_SECS..=ENABLE_MAX_SECS).contains(&request.duration_s) {
        return invalid(
            action,
            format!(
                "duration_s {} is outside {ENABLE_MIN_SECS}..{ENABLE_MAX_SECS} seconds (one minute \
                 to 8 hours); renew before the end for more",
                request.duration_s
            ),
        );
    }
    let Some(_one_write) = ctx.exposures.try_lock() else {
        return busy(action);
    };
    if let Some(reason) = ctx.exposures.unreadable() {
        return registry_unreadable(action, &registry_path(&ctx.state_dir), &reason);
    }
    let now = ctx.exposures.now_secs();
    let Some(exposure) = ctx
        .exposures
        .file()
        .exposures
        .into_iter()
        .find(|exposure| exposure.exposure_id == request.exposure_id)
    else {
        return not_found(action, &request.exposure_id);
    };
    // (2) The key's owner: io.web's ILK, from identity.
    let owner = match io_web_ilk(ctx) {
        Ok(owner) => owner,
        Err(detail) => return answer_error(action, 409, "IO_WEB_ABSENT", detail),
    };
    // (3) A new key in the slot the current enablement does not use: a put that fails or goes
    // unanswered touches no live key but, at most, the one of the enablement it renewed.
    let kept = exposure.current(now).cloned();
    let slot = kept
        .as_ref()
        .map_or(WebExposureKeySlot::A, |kept| other_slot(kept.slot));
    let Some(vault_key) = web_exposure_vault_key(&exposure.exposure_id, slot) else {
        return invalid(
            action,
            format!("{} is not an exposure id", exposure.exposure_id),
        );
    };
    let key_version = match put_key(ctx, client, &vault_key, &owner, mint_key()).await {
        Ok(version) => version,
        Err(detail) => {
            tracing::warn!(
                exposure_id = %exposure.exposure_id,
                host = %exposure.host,
                slot = %slot.as_str(),
                detail = %detail,
                "an enable's key was not stored: nothing changed"
            );
            return answer_error(
                action,
                502,
                "VAULT_UNAVAILABLE",
                format!("{detail}: nothing changed, the exposure is as it was"),
            );
        }
    };
    // (4) The file, with the new enablement after the one it renews; an older one goes.
    let enablement = EnablementRecord {
        enablement_id: format!("{WEB_ENABLEMENT_ID_PREFIX}{}", Uuid::new_v4()),
        from: now,
        until: now + request.duration_s,
        slot,
        key_version,
    };
    let exposure_id = exposure.exposure_id.clone();
    let recorded = ctx.exposures.open_change(|file| {
        if let Some(record) = file
            .exposures
            .iter_mut()
            .find(|record| record.exposure_id == exposure_id)
        {
            record.enablements = kept.iter().cloned().chain([enablement.clone()]).collect();
        }
    });
    if let Err(reason) = recorded {
        return answer_error(
            action,
            500,
            "RECORD_FAILED",
            format!(
                "the exposures registry could not be written: {reason}; the exposure is as it was, \
                 and the key just stored is used by nothing"
            ),
        );
    }
    // (5) io.web answers once it holds the new key.
    ctx.exposures
        .push_io_web(client, &ctx.hive_id, EXPOSURE_CALL_TIMEOUT)
        .await;
    tracing::info!(
        exposure_id = %exposure.exposure_id,
        host = %exposure.host,
        enablement_id = %enablement.enablement_id,
        until = %unix_rfc3339(enablement.until as i64),
        slot = %slot.as_str(),
        key_version,
        renewed = kept.is_some(),
        "exposure enabled"
    );
    let file = ctx.exposures.file();
    let Some(record) = file
        .exposures
        .iter()
        .find(|record| record.exposure_id == exposure.exposure_id)
    else {
        return not_found(action, &exposure.exposure_id);
    };
    let facts = ViewFacts::of(ctx, file.generation);
    answer_ok(
        action,
        json!({
            "exposure": exposure_view(ctx, record, &facts),
            "renewed": kept.is_some(),
        }),
    )
}

/// `disable_exposure {exposure_id}` (§5.1): the enablements cleared in memory, io.web's set
/// pushed, then the file written; `{exposure, was_enabled, persisted}`.
async fn disable(
    ctx: &AdminContext,
    client: &RouterDispatcher,
    params: &Value,
) -> InternalAdminDispatchResult {
    let action = "disable_exposure";
    let request: ExposureParams = match parse(action, params) {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    if let Err(detail) = check_exposure_id(&request.exposure_id) {
        return invalid(action, detail);
    }
    let Some(_one_write) = ctx.exposures.try_lock() else {
        return busy(action);
    };
    if let Some(reason) = ctx.exposures.unreadable() {
        return registry_unreadable(action, &registry_path(&ctx.state_dir), &reason);
    }
    let now = ctx.exposures.now_secs();
    let Some(exposure) = ctx
        .exposures
        .file()
        .exposures
        .into_iter()
        .find(|exposure| exposure.exposure_id == request.exposure_id)
    else {
        return not_found(action, &request.exposure_id);
    };
    let was_enabled = exposure.current(now).is_some();
    if was_enabled {
        ctx.exposures.close_change(|file| {
            if let Some(record) = file
                .exposures
                .iter_mut()
                .find(|record| record.exposure_id == request.exposure_id)
            {
                record.enablements.clear();
            }
        });
        ctx.exposures
            .push_io_web(client, &ctx.hive_id, EXPOSURE_CALL_TIMEOUT)
            .await;
        let _ = ctx.exposures.persist();
        tracing::info!(
            exposure_id = %exposure.exposure_id,
            host = %exposure.host,
            "exposure disabled"
        );
    }
    let file = ctx.exposures.file();
    let Some(record) = file
        .exposures
        .iter()
        .find(|record| record.exposure_id == request.exposure_id)
    else {
        return not_found(action, &request.exposure_id);
    };
    let facts = ViewFacts::of(ctx, file.generation);
    answer_ok(
        action,
        json!({
            "exposure": exposure_view(ctx, record, &facts),
            "was_enabled": was_enabled,
            "persisted": ctx.exposures.persisted(),
        }),
    )
}

/// `unpublish_exposure {exposure_id}` (§5.1): the exposure removed in memory, the edge's routes
/// and io.web's set pushed, then the file written. Nothing is ordered: the name stays on the
/// certificate until its renewal.
async fn unpublish(
    ctx: &AdminContext,
    client: &RouterDispatcher,
    params: &Value,
) -> InternalAdminDispatchResult {
    let action = "unpublish_exposure";
    let request: ExposureParams = match parse(action, params) {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    if let Err(detail) = check_exposure_id(&request.exposure_id) {
        return invalid(action, detail);
    }
    let Some(_one_write) = ctx.exposures.try_lock() else {
        return busy(action);
    };
    if let Some(reason) = ctx.exposures.unreadable() {
        return registry_unreadable(action, &registry_path(&ctx.state_dir), &reason);
    }
    let Some(exposure) = ctx
        .exposures
        .file()
        .exposures
        .into_iter()
        .find(|exposure| exposure.exposure_id == request.exposure_id)
    else {
        return not_found(action, &request.exposure_id);
    };
    ctx.exposures.close_change(|file| {
        file.exposures
            .retain(|record| record.exposure_id != request.exposure_id)
    });
    let generation = ctx.exposures.file().generation;
    let ((routes, _), io_web) = tokio::join!(
        ctx.web.push_once(client, EXPOSURE_CALL_TIMEOUT),
        ctx.exposures
            .push_io_web(client, &ctx.hive_id, EXPOSURE_CALL_TIMEOUT)
    );
    let _ = ctx.exposures.persist();
    tracing::info!(
        exposure_id = %exposure.exposure_id,
        host = %exposure.host,
        route = %route_state_of(&routes.outcome),
        "exposure unpublished"
    );
    let io_web_absent = io_web_ilk(ctx).is_err();
    answer_ok(
        action,
        json!({
            "exposure_id": exposure.exposure_id,
            "host": exposure.host,
            "removed": true,
            "route": { "state": route_state_of(&routes.outcome) },
            "io_web": { "state": io_web_state(io_web_absent, taken_at(&io_web, generation)) },
            "persisted": ctx.exposures.persisted(),
        }),
    )
}

/// io.web's answer to one `IO_WEB_ACCESS_LINK`, or why there is none.
async fn ask_link(
    ctx: &AdminContext,
    client: &RouterDispatcher,
    request: &IoWebAccessLinkRequest,
) -> Result<IoWebAccessLinkResponse, String> {
    let target = io_web_node(&ctx.hive_id);
    let body = serde_json::to_value(request).map_err(|err| err.to_string())?;
    let answer = send_system_request_with_meta(
        client,
        &target,
        MSG_IO_WEB_ACCESS_LINK,
        MSG_IO_WEB_ACCESS_LINK_RESPONSE,
        body,
        16,
        None,
        None,
        Some(target.clone()),
        None,
        None,
        EXPOSURE_CALL_TIMEOUT,
    )
    .await
    .map_err(|err| {
        format!(
            "io.web did not answer: {}",
            no_answer_detail(&err, EXPOSURE_CALL_TIMEOUT)
        )
    })?;
    serde_json::from_value::<IoWebAccessLinkResponse>(answer)
        .map_err(|err| format!("io.web's answer cannot be read: {err}"))
}

/// io.web's set is older than SY.admin's: it does not know the exposure or the enablement yet.
fn set_older(answer: &IoWebAccessLinkResponse) -> bool {
    match answer {
        IoWebAccessLinkResponse::Error { error_code, .. } => [
            IO_WEB_ERROR_ENABLEMENT_UNKNOWN,
            IO_WEB_ERROR_EXPOSURE_UNKNOWN,
        ]
        .contains(&error_code.as_str()),
        IoWebAccessLinkResponse::Ok { .. } => false,
    }
}

/// `issue_exposure_link {exposure_id}` (§5.1, §3.2): the exposure must be open, its last
/// enablement live ([`ExposureRecord::open`]); io.web mints the link of that enablement. When
/// io.web's set is older, it gets the set and is asked
/// once more. The link is answered once, `expires_at` in RFC 3339, and logged by its `link_id`
/// alone. It takes no exposure lock, so it never answers `BUSY`.
async fn issue_link(
    ctx: &AdminContext,
    client: &RouterDispatcher,
    params: &Value,
) -> InternalAdminDispatchResult {
    let action = "issue_exposure_link";
    let request: ExposureParams = match parse(action, params) {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    if let Err(detail) = check_exposure_id(&request.exposure_id) {
        return invalid(action, detail);
    }
    // No exposure lock: a link reads the registry, and its one push on ENABLEMENT_UNKNOWN is the
    // set as it is, which io.web takes or holds already.
    let now = ctx.exposures.now_secs();
    let file = ctx.exposures.file();
    let Some(exposure) = file
        .exposures
        .iter()
        .find(|exposure| exposure.exposure_id == request.exposure_id)
    else {
        return not_found(action, &request.exposure_id);
    };
    // io.web mints links from the last enablement alone, while it is live (§3.4): after a renewal
    // that ended before the enablement it renewed, the exposure is closed to new links.
    let Some(current) = exposure.open(now) else {
        return answer_error(
            action,
            409,
            "EXPOSURE_CLOSED",
            format!("{} is closed: enable it first", exposure.exposure_id),
        );
    };
    let ask = IoWebAccessLinkRequest {
        exposure_id: exposure.exposure_id.clone(),
        enablement_id: current.enablement_id.clone(),
        generation: file.generation,
    };
    let mut answer = ask_link(ctx, client, &ask).await;
    if answer.as_ref().is_ok_and(set_older) {
        ctx.exposures
            .push_io_web(client, &ctx.hive_id, EXPOSURE_CALL_TIMEOUT)
            .await;
        answer = ask_link(ctx, client, &ask).await;
    }
    match answer {
        Ok(IoWebAccessLinkResponse::Ok {
            url,
            expires_at,
            link_id,
        }) => {
            tracing::info!(
                exposure_id = %exposure.exposure_id,
                host = %exposure.host,
                enablement_id = %current.enablement_id,
                link_id = %link_id,
                expires_at = %unix_rfc3339(expires_at as i64),
                "link issued"
            );
            answer_ok(
                action,
                json!({
                    "exposure_id": exposure.exposure_id,
                    "url": url,
                    "expires_at": unix_rfc3339(expires_at as i64),
                    "link_id": link_id,
                }),
            )
        }
        Ok(IoWebAccessLinkResponse::Error {
            error_code,
            error_detail,
        }) => {
            tracing::warn!(
                exposure_id = %exposure.exposure_id,
                host = %exposure.host,
                enablement_id = %current.enablement_id,
                error_code = %error_code,
                detail = %error_detail,
                "io.web issued no link"
            );
            answer_error(
                action,
                503,
                "IO_WEB_UNAVAILABLE",
                format!("io.web issued no link ({error_code}: {error_detail}); ask again"),
            )
        }
        Err(detail) => {
            tracing::warn!(
                exposure_id = %exposure.exposure_id,
                host = %exposure.host,
                enablement_id = %current.enablement_id,
                detail = %detail,
                "io.web issued no link"
            );
            answer_error(
                action,
                503,
                "IO_WEB_UNAVAILABLE",
                format!("{detail}; ask again"),
            )
        }
    }
}

#[cfg(test)]
#[path = "exposures_tests.rs"]
pub(crate) mod tests;
