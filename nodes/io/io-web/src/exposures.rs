//! io.web's exposures (docs/io-web-v2-stage-2-1.md §2.3, §3, §5.5): the set SY.admin pushes, the
//! keys of its live enablements, the one-time links io.web mints and redeems, and its sessions.
//!
//! - **The set** comes whole with `IO_WEB_EXPOSURES`, from SY.admin alone (`main.rs` checks the
//!   origin). A lower generation than the one held is refused; an equal one is acknowledged without
//!   reading anything; a higher one replaces the set in memory at once, so whatever it no longer
//!   holds (a disable, an unpublish, the oldest enablement of a renewal) fails at the next request.
//!   Then the set is written to `exposures.json`, beside io.web's `config.json`, mode 0600, and the
//!   keys io.web does not hold yet are read.
//! - **The keys.** Each live enablement's secret `K` is read from the vault as its owner, within
//!   3 s, and taken only as SY.admin wrote it: a `web_session_key` owned by io.web, at the
//!   enablement's `key_version` (§3.1). A key that cannot be read or does not match is unavailable:
//!   its links and sessions fail closed, and io.web reads it again every 30 s.
//! - **At boot** io.web loads its file, then the keys, before its port opens (`main.rs`).
//! - **Links and sessions** (`access`): io.web mints a link for an exposure's current enablement
//!   when SY.admin asks, redeems it once, on its host, within 120 s and only if minted since io.web's
//!   boot, and answers with a session cookie that it checks on every request (§3.2, §3.3).

use std::collections::HashMap;
use std::future::Future;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::protocol::{
    IoWebAccessLinkRequest, IoWebAccessLinkResponse, IoWebEnablement, IoWebEnablementKey,
    IoWebExposure, IoWebExposuresPayload, IoWebExposuresResponse, IoWebKeyState,
    IO_WEB_ERROR_ENABLEMENT_UNKNOWN, IO_WEB_ERROR_EXPOSURE_UNKNOWN, IO_WEB_ERROR_KEY_UNAVAILABLE,
    IO_WEB_ERROR_STALE_GENERATION, WEB_ENABLEMENT_ID_PREFIX, WEB_EXPOSURE_ID_PREFIX,
};
use fluxbee_sdk::vault::{web_exposure_vault_key, WEB_SESSION_KEY_RESOURCE_TYPE};
use fluxbee_sdk::{RouterDispatcher, VaultCallerOwned, VaultClient, VaultValueResponse};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::task::JoinSet;

use crate::access::{
    fresh_token, link_id, secret_from_hex, session_set_cookie, sid_prefix, EnablementKeys,
    LinkCode, SessionToken, LINK_LIFETIME_S, SESSION_IDLE_S, SESSION_SLIDE_S,
};

/// The bound of one key read (§5.5).
pub const KEY_READ_TIMEOUT: Duration = Duration::from_secs(3);
/// How often a key that is unavailable is read again (§5.5).
pub const KEY_RETRY: Duration = Duration::from_secs(30);
/// io.web's copy of the set, beside its `config.json` (§2.3).
pub const EXPOSURES_FILE: &str = "exposures.json";
/// The path a link opens, reserved on every host (§3.2, §5.5).
pub const ENTER_PATH: &str = "/.fluxbee/enter";
const FILE_SCHEMA_VERSION: u32 = 1;
/// The state of an enablement's key between its push and its read.
const NOT_READ_YET: &str = "not read yet";

/// The wall clock in Unix seconds, the unit of every time of the exposures.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// io.web's boot as links count it: the first whole second after `now`, the moment io.web starts.
/// A link that an earlier process minted carries an `iat` no later than the second this one started
/// in, so `iat ≥ boot` refuses every one of them (§3.2, §3.4); io.web mints none before it.
pub fn boot_second(now: u64) -> u64 {
    now.saturating_add(1)
}

/// A key as `VAULT_GET` answers it, or why there is none. Never carries the key in its error.
pub type KeyRead = Pin<Box<dyn Future<Output = Result<VaultValueResponse, String>> + Send>>;

/// Where io.web reads its keys: SY.vault, as io.web (`VaultKeys`); a fixed map in the tests.
pub trait KeySource: Send + Sync {
    /// The ILK io.web reads as, which owns its keys; `None` when io.web has none.
    fn owner(&self) -> Option<&str>;
    /// The current version of `vault_key`.
    fn read(&self, vault_key: &str) -> KeyRead;
}

/// SY.vault, asked as io.web with its own ILK, as io.slack reads its key (§3.1).
pub struct VaultKeys {
    client: Option<VaultClient>,
    ilk: Option<String>,
}

impl VaultKeys {
    /// `ilk` is io.web's own (`FLUXBEE_NODE_ILK_ID`): without one io.web can read no key, and
    /// every enablement is unavailable.
    pub fn new(dispatcher: Arc<RouterDispatcher>, node_name: &str, ilk: Option<String>) -> Self {
        let client = ilk.as_ref().map(|ilk| {
            VaultClient::for_primary(dispatcher, VaultCallerOwned::new(ilk.clone(), node_name))
        });
        Self { client, ilk }
    }
}

impl KeySource for VaultKeys {
    fn owner(&self) -> Option<&str> {
        self.ilk.as_deref()
    }

    fn read(&self, vault_key: &str) -> KeyRead {
        let Some(client) = self.client.clone() else {
            return Box::pin(std::future::ready(Err(
                "io.web has no ILK (FLUXBEE_NODE_ILK_ID), so it reads no key".to_string(),
            )));
        };
        let vault_key = vault_key.to_string();
        Box::pin(async move {
            client
                .get(&vault_key, KEY_READ_TIMEOUT)
                .await
                .map_err(|err| err.to_string())
        })
    }
}

/// The set, as io.web holds it, with what it does with it.
pub struct Exposures {
    file: PathBuf,
    keys: Arc<dyn KeySource>,
    /// `boot_second` of this process: links minted before it are refused, and io.web mints none
    /// before it (§3.2).
    boot: u64,
    held: RwLock<Held>,
    /// One change at a time: a push, the boot's load, a retry's choice of the keys to read and its
    /// file write. A retry's reads do not hold it (`retry`). Links and requests never wait on it.
    changes: tokio::sync::Mutex<()>,
    /// The links redeemed, `jti → iat + 120`, pruned at each insert: bounded by the redemptions of
    /// the last 120 s (§3.2).
    redeemed: Mutex<HashMap<String, u64>>,
    /// The file misses the set held: its last write failed. Written again at the next push or
    /// retry.
    unsaved: AtomicBool,
    links_issued: AtomicU64,
    links_redeemed: AtomicU64,
    links_refused: AtomicU64,
    sessions_minted: AtomicU64,
}

/// The set held: its generation and its exposures, each with its keys.
#[derive(Default)]
struct Held {
    generation: u64,
    exposures: Vec<HeldExposure>,
}

struct HeldExposure {
    /// As SY.admin pushed it: what the file keeps.
    wire: IoWebExposure,
    /// The host its requests come with, lowercase as the edge forwards it.
    host: String,
    /// Its uuid in 32 hex, `x` in links and cookies; `None` for an id that is not `exp:<uuid>`,
    /// whose enablements are then unavailable.
    x: Option<String>,
    /// Its enablements, newest last, as `wire` lists them.
    enablements: Vec<HeldEnablement>,
}

struct HeldEnablement {
    wire: IoWebEnablement,
    /// Its uuid in 32 hex, `e` in links and cookies; `None` for an id that is not `enb:<uuid>`.
    e: Option<String>,
    key: KeyState,
}

#[derive(Clone)]
enum KeyState {
    Ready(EnablementKeys),
    /// Why io.web holds no key: never the key.
    Unavailable(String),
}

impl KeyState {
    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    /// The same state, for the logs: ready both, or unavailable for the same reason.
    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ready(_), Self::Ready(_)) => true,
            (Self::Unavailable(one), Self::Unavailable(two)) => one == two,
            _ => false,
        }
    }
}

/// A live enablement whose key io.web does not hold, to read.
struct KeyWant {
    exposure_id: String,
    host: String,
    enablement: IoWebEnablement,
    /// Its vault key, or why it has none.
    vault_key: Result<String, String>,
}

/// Which key outcomes the log takes: each one (a push, the boot), or only what changed (a retry,
/// every 30 s).
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyLog {
    Every,
    Changes,
}

/// io.web's file: the set without secrets (§2.3).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExposuresFile {
    schema_version: u32,
    generation: u64,
    exposures: Vec<IoWebExposure>,
}

/// What the edge said of a request, for the logs (§3.2): its `X-Request-Id` and
/// `X-Forwarded-For`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestTrace<'a> {
    pub request_id: &'a str,
    pub forwarded_for: &'a str,
}

/// A link redeemed: the new session's cookie, and what the logs say of it.
struct Entered {
    set_cookie: String,
    sid: String,
    link_id: String,
    exposure_id: String,
    enablement_id: String,
    expires_at: u64,
    max_age: u64,
}

/// Why a link was refused, with what was known of it by then.
#[derive(Default)]
struct LinkRefusal {
    reason: &'static str,
    link_id: Option<String>,
    /// `iat + 120`, once the code parses (§3.2).
    expires_at: Option<u64>,
    exposure_id: Option<String>,
    enablement_id: Option<String>,
}

impl LinkRefusal {
    fn because(mut self, reason: &'static str) -> Self {
        self.reason = reason;
        self
    }
}

/// Why a session cookie does not serve (§3.3), with what was known of it by then, for the log.
#[derive(Debug, Default)]
pub struct SessionRefusal {
    pub reason: &'static str,
    pub exposure_id: Option<String>,
    pub enablement_id: Option<String>,
    /// The cookie is kept: its enablement is live, but io.web does not hold its key yet (a
    /// restart before the vault answered, §3.4), and it serves again once the retry reads it. Any
    /// other refusal is final, and the answer clears the cookie.
    pub kept: bool,
}

impl Held {
    fn by_host(&self, host: &str) -> Option<&HeldExposure> {
        self.exposures.iter().find(|exposure| exposure.host == host)
    }

    /// The live enablements, with their exposure: `now` is before their end.
    fn live(&self, now: u64) -> impl Iterator<Item = (&HeldExposure, &HeldEnablement)> {
        self.exposures.iter().flat_map(move |exposure| {
            exposure
                .enablements
                .iter()
                .filter(move |enablement| now < enablement.wire.until)
                .map(move |enablement| (exposure, enablement))
        })
    }

    fn missing_keys(&self, now: u64) -> Vec<KeyWant> {
        self.live(now)
            .filter(|(_, enablement)| !enablement.key.is_ready())
            .map(|(exposure, enablement)| {
                let vault_key = match (&exposure.x, &enablement.e) {
                    (None, _) => Err(format!(
                        "the exposure id is not {WEB_EXPOSURE_ID_PREFIX}<uuid>"
                    )),
                    (_, None) => Err(format!(
                        "the enablement id is not {WEB_ENABLEMENT_ID_PREFIX}<uuid>"
                    )),
                    _ => web_exposure_vault_key(&exposure.wire.exposure_id, enablement.wire.slot)
                        .ok_or_else(|| {
                            format!("the exposure id is not {WEB_EXPOSURE_ID_PREFIX}<uuid>")
                        }),
                };
                KeyWant {
                    exposure_id: exposure.wire.exposure_id.clone(),
                    host: exposure.host.clone(),
                    enablement: enablement.wire.clone(),
                    vault_key,
                }
            })
            .collect()
    }

    /// Sets the key `want` asked for, if its enablement is still held at the same slot and
    /// version, and io.web does not hold its key yet; the key it replaced. A key held stays: a
    /// retry's read, which holds no lock (`Exposures::retry`), may end after a push read it.
    fn set_key(&mut self, want: &KeyWant, key: KeyState) -> Option<KeyState> {
        let enablement = self
            .exposures
            .iter_mut()
            .filter(|exposure| {
                exposure.wire.exposure_id == want.exposure_id && exposure.host == want.host
            })
            .flat_map(|exposure| exposure.enablements.iter_mut())
            .find(|held| same_enablement(&held.wire, &want.enablement))?;
        if enablement.key.is_ready() {
            return None;
        }
        Some(std::mem::replace(&mut enablement.key, key))
    }
}

impl HeldExposure {
    /// `wire` as io.web holds it, with the keys `previous` holds for the enablements that stay at
    /// the same slot and version; the others wait for their read.
    fn new(wire: IoWebExposure, previous: Option<&HeldExposure>) -> Self {
        let enablements = wire
            .enablements
            .iter()
            .map(|enablement| {
                let kept = previous
                    .into_iter()
                    .flat_map(|previous| previous.enablements.iter())
                    .find(|held| same_enablement(&held.wire, enablement))
                    .map(|held| &held.key)
                    .filter(|key| key.is_ready());
                HeldEnablement {
                    wire: enablement.clone(),
                    e: uuid_hex(&enablement.enablement_id, WEB_ENABLEMENT_ID_PREFIX),
                    key: kept
                        .cloned()
                        .unwrap_or_else(|| KeyState::Unavailable(NOT_READ_YET.to_string())),
                }
            })
            .collect();
        Self {
            host: wire.host.to_ascii_lowercase(),
            x: uuid_hex(&wire.exposure_id, WEB_EXPOSURE_ID_PREFIX),
            enablements,
            wire,
        }
    }

    /// The same exposure, `wire` pushed again: the same id on the same host.
    fn is(&self, wire: &IoWebExposure) -> bool {
        self.wire.exposure_id == wire.exposure_id && self.host == wire.host.to_ascii_lowercase()
    }
}

/// The same enablement, whose key is the same: its id, its slot and its version.
fn same_enablement(one: &IoWebEnablement, two: &IoWebEnablement) -> bool {
    one.enablement_id == two.enablement_id
        && one.slot == two.slot
        && one.key_version == two.key_version
}

/// The uuid of an id `<prefix><uuid>` in 32 hex, as links and cookies carry it (§3.2): only for
/// the hyphenated lowercase form SY.admin mints, the one `web_exposure_vault_key` takes.
fn uuid_hex(id: &str, prefix: &str) -> Option<String> {
    let text = id.strip_prefix(prefix)?;
    let uuid = uuid::Uuid::parse_str(text).ok()?;
    (uuid.hyphenated().to_string() == text).then(|| uuid.simple().to_string())
}

/// The enablement's keys from the vault's answer, once it is the key SY.admin wrote for it
/// (§3.1): a `web_session_key` owned by io.web and assigned to no other node, at the enablement's
/// version, whose value is `{secret: <64 hex>}`. Otherwise why not, never the value.
fn take_key(
    answer: &VaultValueResponse,
    enablement: &IoWebEnablement,
    owner: Option<&str>,
) -> Result<EnablementKeys, String> {
    let metadata = answer.metadata.as_ref();
    let resource_type = metadata.and_then(|metadata| metadata.resource_type.as_deref());
    if resource_type != Some(WEB_SESSION_KEY_RESOURCE_TYPE) {
        return Err(format!(
            "the key's resource type is {}, not {WEB_SESSION_KEY_RESOURCE_TYPE}",
            resource_type.unwrap_or("none")
        ));
    }
    match answer.version {
        Some(version) if version == enablement.key_version => {}
        Some(version) => {
            return Err(format!(
                "the key is at version {version}, not {}",
                enablement.key_version
            ))
        }
        None => return Err("the vault named no version of the key".to_string()),
    }
    // The owner as SY.vault reads it: `ilk`, else the legacy `owner_ilk`.
    let key_owner = metadata.and_then(|metadata| {
        [
            metadata.ilk.as_deref().unwrap_or_default(),
            metadata.owner_ilk.as_str(),
        ]
        .into_iter()
        .map(str::trim)
        .find(|ilk| !ilk.is_empty())
    });
    if owner.is_none() || key_owner != owner {
        return Err("the key is not io.web's own".to_string());
    }
    // Nobody else reads `K` (§3.1): a key that other nodes may read too is not the one SY.admin
    // wrote.
    if metadata.is_some_and(|metadata| !metadata.assigned_ilks.is_empty()) {
        return Err("the key is assigned to other nodes".to_string());
    }
    let secret = answer
        .value
        .as_ref()
        .and_then(|value| value.get("secret"))
        .and_then(Value::as_str)
        .and_then(secret_from_hex)
        .ok_or_else(|| "the key's value is not {secret: <64 hex>}".to_string())?;
    Ok(EnablementKeys::derive(&secret))
}

impl Exposures {
    /// No exposure yet. `file` is io.web's copy of the set, `keys` where it reads the keys, and
    /// `boot` its `boot_second`.
    pub fn new(file: PathBuf, keys: Arc<dyn KeySource>, boot: u64) -> Self {
        Self {
            file,
            keys,
            boot,
            held: RwLock::new(Held::default()),
            changes: tokio::sync::Mutex::new(()),
            redeemed: Mutex::new(HashMap::new()),
            unsaved: AtomicBool::new(false),
            links_issued: AtomicU64::new(0),
            links_redeemed: AtomicU64::new(0),
            links_refused: AtomicU64::new(0),
            sessions_minted: AtomicU64::new(0),
        }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    fn held(&self) -> RwLockReadGuard<'_, Held> {
        self.held.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn held_mut(&self) -> RwLockWriteGuard<'_, Held> {
        self.held.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// At boot, before the port opens: the set of its file, then the keys of its live
    /// enablements. A file that cannot be read leaves io.web with no exposure, which fails closed,
    /// until SY.admin's next push.
    pub async fn load_at_boot(&self, now: u64) {
        let _change = self.changes.lock().await;
        let path = self.file.display().to_string();
        match read_file(&self.file) {
            Ok(Some(set)) => {
                let (generation, count) = (set.generation, set.exposures.len());
                if generation > self.held().generation {
                    self.install(set);
                    tracing::info!(
                        path = %path,
                        generation,
                        exposures = count,
                        "IO.web loaded its exposures from its file"
                    );
                }
            }
            Ok(None) => tracing::info!(
                path = %path,
                "IO.web has no exposures file yet: it holds none until SY.admin pushes its set"
            ),
            Err(err) => tracing::error!(
                path = %path,
                error = %err,
                "IO.web cannot read its exposures file: it holds none until SY.admin pushes its set"
            ),
        }
        self.read_missing_keys(now, KeyLog::Every).await;
    }

    /// `IO_WEB_EXPOSURES` from SY.admin (§5.5): the set replaces the one held, if newer.
    pub async fn apply_push(&self, set: IoWebExposuresPayload, now: u64) -> IoWebExposuresResponse {
        let _change = self.changes.lock().await;
        let held = self.held().generation;
        if set.generation < held {
            tracing::warn!(
                generation = set.generation,
                held,
                "IO.web refused a set older than the one it holds"
            );
            return IoWebExposuresResponse::Error {
                error_code: IO_WEB_ERROR_STALE_GENERATION.to_string(),
                error_detail: format!(
                    "io.web holds generation {held}, newer than {}",
                    set.generation
                ),
            };
        }
        if set.generation == held {
            // The set it holds: nothing is read. A file whose last write failed is written again.
            self.save_if_unsaved();
            return self.answer(false, now);
        }
        let generation = set.generation;
        let changed = self.install(set);
        self.save();
        self.read_missing_keys(now, KeyLog::Every).await;
        let view = self.view(now);
        tracing::info!(
            generation,
            changed,
            exposures = %view["exposures"],
            enabled = %view["enabled"],
            keys_unavailable = %view["keys_unavailable"],
            "IO.web applied its exposures"
        );
        self.answer(changed, now)
    }

    /// Every 30 s: a file whose last write failed is written again, and the keys still unavailable
    /// are read again. The keys to read are chosen in the changes' turn, and read out of it, so a
    /// push, a disable above all, never waits up to 3 s behind the retry's reads (§3.4); each
    /// outcome is then taken only by an enablement still held at the same slot and version
    /// (`Held::set_key`).
    pub async fn retry(&self, now: u64) {
        let wanted = {
            let _change = self.changes.lock().await;
            self.save_if_unsaved();
            self.held().missing_keys(now)
        };
        self.read_keys(&wanted, KeyLog::Changes).await;
    }

    /// The set held becomes `set`, at once: what it no longer holds is gone before any key is
    /// read. The keys of the enablements that stay at the same slot and version are kept. Whether
    /// the exposures changed.
    fn install(&self, set: IoWebExposuresPayload) -> bool {
        let mut held = self.held_mut();
        let changed = !held
            .exposures
            .iter()
            .map(|exposure| &exposure.wire)
            .eq(set.exposures.iter());
        for gone in held
            .exposures
            .iter()
            .filter(|old| !set.exposures.iter().any(|wire| old.is(wire)))
        {
            tracing::info!(
                exposure_id = %gone.wire.exposure_id,
                host = %gone.host,
                "IO.web dropped an exposure: its links and sessions fail from now on"
            );
        }
        let exposures = set
            .exposures
            .into_iter()
            .map(|wire| {
                let previous = held.exposures.iter().find(|old| old.is(&wire));
                if previous.map(|old| &old.wire) != Some(&wire) {
                    tracing::info!(
                        exposure_id = %wire.exposure_id,
                        host = %wire.host,
                        enablements = ?wire
                            .enablements
                            .iter()
                            .map(|enablement| (&enablement.enablement_id, enablement.until))
                            .collect::<Vec<_>>(),
                        "IO.web holds an exposure"
                    );
                }
                HeldExposure::new(wire, previous)
            })
            .collect();
        *held = Held {
            generation: set.generation,
            exposures,
        };
        changed
    }

    /// Reads the key of every live enablement whose key io.web does not hold (`read_keys`).
    async fn read_missing_keys(&self, now: u64, log: KeyLog) {
        let wanted = self.held().missing_keys(now);
        self.read_keys(&wanted, log).await;
    }

    /// Reads, at once and each within 3 s, the keys `wanted`, and takes each one that matches its
    /// enablement (`take_key`).
    async fn read_keys(&self, wanted: &[KeyWant], log: KeyLog) {
        if wanted.is_empty() {
            return;
        }
        let mut outcomes: Vec<Option<KeyState>> = vec![None; wanted.len()];
        let mut reads = JoinSet::new();
        for (index, want) in wanted.iter().enumerate() {
            match &want.vault_key {
                Ok(vault_key) => {
                    let read = self.keys.read(vault_key);
                    reads.spawn(async move {
                        (index, tokio::time::timeout(KEY_READ_TIMEOUT, read).await)
                    });
                }
                Err(detail) => outcomes[index] = Some(KeyState::Unavailable(detail.clone())),
            }
        }
        while let Some(joined) = reads.join_next().await {
            // A read that panicked leaves its key as it was, read again at the next retry.
            let Ok((index, read)) = joined else {
                continue;
            };
            outcomes[index] = Some(match read {
                Ok(Ok(answer)) => {
                    match take_key(&answer, &wanted[index].enablement, self.keys.owner()) {
                        Ok(keys) => KeyState::Ready(keys),
                        Err(detail) => KeyState::Unavailable(detail),
                    }
                }
                Ok(Err(detail)) => KeyState::Unavailable(detail),
                Err(_) => KeyState::Unavailable(format!(
                    "the vault did not answer within {} s",
                    KEY_READ_TIMEOUT.as_secs()
                )),
            });
        }
        let mut held = self.held_mut();
        for (want, outcome) in wanted.iter().zip(outcomes) {
            let Some(key) = outcome else {
                continue;
            };
            let Some(before) = held.set_key(want, key.clone()) else {
                continue;
            };
            if log == KeyLog::Every || !before.same_as(&key) {
                log_key(want, &key);
            }
        }
    }

    /// The answer to a push: the generation held, and each live enablement with its key's state.
    fn answer(&self, changed: bool, now: u64) -> IoWebExposuresResponse {
        let held = self.held();
        IoWebExposuresResponse::Ok {
            generation: held.generation,
            changed,
            enablements: held
                .live(now)
                .map(|(_, enablement)| match &enablement.key {
                    KeyState::Ready(_) => IoWebEnablementKey {
                        enablement_id: enablement.wire.enablement_id.clone(),
                        key: IoWebKeyState::Ready,
                        detail: None,
                    },
                    KeyState::Unavailable(detail) => IoWebEnablementKey {
                        enablement_id: enablement.wire.enablement_id.clone(),
                        key: IoWebKeyState::Unavailable,
                        detail: Some(detail.clone()),
                    },
                })
                .collect(),
        }
    }

    /// Writes the set held to the file. When that fails, the old file goes too, so that a restart
    /// never brings back what this set took away: io.web would then start with no exposure until
    /// SY.admin's next push.
    fn save(&self) {
        let file = {
            let held = self.held();
            ExposuresFile {
                schema_version: FILE_SCHEMA_VERSION,
                generation: held.generation,
                exposures: held
                    .exposures
                    .iter()
                    .map(|exposure| exposure.wire.clone())
                    .collect(),
            }
        };
        match write_file(&self.file, &file) {
            Ok(()) => self.unsaved.store(false, Ordering::Relaxed),
            Err(err) => {
                self.unsaved.store(true, Ordering::Relaxed);
                let old_file_gone = match std::fs::remove_file(&self.file) {
                    Ok(()) => true,
                    Err(err) => err.kind() == ErrorKind::NotFound,
                };
                tracing::error!(
                    path = %self.file.display(),
                    error = %err,
                    old_file_gone,
                    "IO.web could not write its exposures file: it serves the set it holds and \
                     writes the file again at the next push or retry"
                );
            }
        }
    }

    fn save_if_unsaved(&self) {
        if self.unsaved.load(Ordering::Relaxed) {
            self.save();
        }
    }

    /// `IO_WEB_ACCESS_LINK` (§3.2, §5.5): a one-time link to the exposure's current enablement,
    /// which must be live, with its key held, from io.web's boot on.
    pub fn issue_link(
        &self,
        request: &IoWebAccessLinkRequest,
        now: u64,
    ) -> IoWebAccessLinkResponse {
        // `host` is the exposure's, empty when io.web holds no such exposure.
        let refused = |host: &str, error_code: &str, error_detail: String| {
            tracing::warn!(
                exposure_id = %request.exposure_id,
                host = %host,
                enablement_id = %request.enablement_id,
                error_code,
                detail = %error_detail,
                "IO.web issued no link"
            );
            IoWebAccessLinkResponse::Error {
                error_code: error_code.to_string(),
                error_detail,
            }
        };
        let held = self.held();
        let Some(exposure) = held
            .exposures
            .iter()
            .find(|exposure| exposure.wire.exposure_id == request.exposure_id)
        else {
            return refused(
                "",
                IO_WEB_ERROR_EXPOSURE_UNKNOWN,
                format!(
                    "io.web's set (generation {}) holds no exposure {}",
                    held.generation, request.exposure_id
                ),
            );
        };
        let Some(current) = exposure.enablements.last().filter(|current| {
            current.wire.enablement_id == request.enablement_id && now < current.wire.until
        }) else {
            return refused(
                &exposure.host,
                IO_WEB_ERROR_ENABLEMENT_UNKNOWN,
                format!(
                    "{} is not the live current enablement of {} in io.web's set (generation {}; \
                     asked at generation {})",
                    request.enablement_id, request.exposure_id, held.generation, request.generation
                ),
            );
        };
        let (Some(x), Some(e), KeyState::Ready(keys)) = (&exposure.x, &current.e, &current.key)
        else {
            let why = match &current.key {
                KeyState::Unavailable(detail) => detail.as_str(),
                KeyState::Ready(_) => "its id is not a uuid",
            };
            return refused(
                &exposure.host,
                IO_WEB_ERROR_KEY_UNAVAILABLE,
                format!("io.web holds no key for {}: {why}", request.enablement_id),
            );
        };
        // io.web mints from its boot on (§3.2). In the second it started, `iat = now` would be
        // before its boot, so the link would never redeem; `iat = boot` would let a successor
        // started within that same second, whose boot is the same, redeem a code spent here.
        if now < self.boot {
            return refused(
                &exposure.host,
                IO_WEB_ERROR_KEY_UNAVAILABLE,
                format!(
                    "io.web started within the last second and mints links from {} on",
                    self.boot
                ),
            );
        }
        let iat = now;
        let jti = fresh_token();
        let code = LinkCode::mint(keys, &exposure.host, x, e, &jti, iat);
        let expires_at = iat.saturating_add(LINK_LIFETIME_S);
        let link_id = link_id(&jti);
        self.links_issued.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            link_id = %link_id,
            exposure_id = %exposure.wire.exposure_id,
            host = %exposure.host,
            enablement_id = %current.wire.enablement_id,
            expires_at,
            "IO.web link issued"
        );
        IoWebAccessLinkResponse::Ok {
            url: format!("https://{}{ENTER_PATH}?code={code}", exposure.host),
            expires_at,
            link_id,
        }
    }

    /// Whether `host` is an exposure's.
    pub fn knows_host(&self, host: &str) -> bool {
        self.held().by_host(host).is_some()
    }

    /// The id of the exposure of `host`, for the logs.
    pub fn exposure_of(&self, host: &str) -> Option<String> {
        self.held()
            .by_host(host)
            .map(|exposure| exposure.wire.exposure_id.clone())
    }

    /// A `GET /.fluxbee/enter` on `host` (§3.2): the `Set-Cookie` of a new session once the code
    /// passes every check, or `None`, and the caller answers the fixed 401 page. The reason is
    /// logged, never the code.
    pub fn redeem(
        &self,
        host: &str,
        code: Option<&str>,
        now: u64,
        trace: RequestTrace<'_>,
    ) -> Option<String> {
        match self.check_link(host, code, now) {
            Ok(entered) => {
                self.links_redeemed.fetch_add(1, Ordering::Relaxed);
                self.sessions_minted.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    link_id = %entered.link_id,
                    exposure_id = %entered.exposure_id,
                    host = %host,
                    enablement_id = %entered.enablement_id,
                    expires_at = entered.expires_at,
                    request_id = %trace.request_id,
                    forwarded_for = %trace.forwarded_for,
                    "IO.web link redeemed"
                );
                tracing::info!(
                    sid = %sid_prefix(&entered.sid),
                    exposure_id = %entered.exposure_id,
                    host = %host,
                    enablement_id = %entered.enablement_id,
                    max_age = entered.max_age,
                    "IO.web session minted"
                );
                Some(entered.set_cookie)
            }
            Err(refusal) => {
                self.links_refused.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    reason = refusal.reason,
                    link_id = refusal.link_id.as_deref().unwrap_or_default(),
                    expires_at = refusal.expires_at,
                    exposure_id = refusal.exposure_id.as_deref().unwrap_or_default(),
                    host = %host,
                    enablement_id = refusal.enablement_id.as_deref().unwrap_or_default(),
                    request_id = %trace.request_id,
                    forwarded_for = %trace.forwarded_for,
                    "IO.web link refused"
                );
                None
            }
        }
    }

    /// §3.2's checks, in their order, then a new session.
    fn check_link(&self, host: &str, code: Option<&str>, now: u64) -> Result<Entered, LinkRefusal> {
        let mut refusal = LinkRefusal::default();
        let held = self.held();
        // (1) The host is an exposure's.
        let Some(exposure) = held.by_host(host) else {
            return Err(refusal.because("the host is no exposure's"));
        };
        refusal.exposure_id = Some(exposure.wire.exposure_id.clone());
        // (2) The code parses strictly.
        let Some(code) = code.and_then(LinkCode::parse) else {
            return Err(refusal.because("no code, or a malformed one"));
        };
        refusal.link_id = Some(link_id(code.jti));
        refusal.expires_at = Some(code.iat.saturating_add(LINK_LIFETIME_S));
        // (3) It is this exposure's.
        if exposure.x.as_deref() != Some(code.x) {
            return Err(refusal.because("the code is another exposure's"));
        }
        // (4) Its enablement is a live one of the exposure, whose key io.web holds.
        let Some(enablement) = exposure
            .enablements
            .iter()
            .find(|enablement| enablement.e.as_deref() == Some(code.e))
        else {
            return Err(refusal.because("its enablement is not one io.web holds"));
        };
        refusal.enablement_id = Some(enablement.wire.enablement_id.clone());
        if now >= enablement.wire.until {
            return Err(refusal.because("its enablement has ended"));
        }
        let KeyState::Ready(keys) = &enablement.key else {
            return Err(refusal.because("io.web holds no key for its enablement"));
        };
        // (5) The tag verifies, in constant time.
        if !code.verifies(keys, host) {
            return Err(refusal.because("its tag does not verify"));
        }
        // (6) Minted since io.web's boot, and at most 120 s ago.
        let expires_at = code.iat.saturating_add(LINK_LIFETIME_S);
        if code.iat < self.boot {
            return Err(refusal.because("it was minted before io.web's boot"));
        }
        if now > expires_at {
            return Err(refusal.because("it expired"));
        }
        // (7) Once.
        {
            let mut redeemed = self.redeemed.lock().unwrap_or_else(PoisonError::into_inner);
            redeemed.retain(|_, until| *until >= now);
            if redeemed.contains_key(code.jti) {
                return Err(refusal.because("it was redeemed already"));
            }
            redeemed.insert(code.jti.to_string(), expires_at);
        }
        let sid = fresh_token();
        let value = SessionToken::mint(keys, host, code.x, code.e, &sid, now, now);
        let max_age = SESSION_IDLE_S.min(enablement.wire.until - now);
        Ok(Entered {
            set_cookie: session_set_cookie(&value, max_age),
            sid,
            link_id: link_id(code.jti),
            exposure_id: exposure.wire.exposure_id.clone(),
            enablement_id: enablement.wire.enablement_id.clone(),
            expires_at,
            max_age,
        })
    }

    /// A session cookie's value on `host` (§3.3): `Ok` while its tag verifies over the host, its
    /// enablement is live with its key held and it was active within 30 min; with the cookie again,
    /// `active = now`, once its last activity is 60 s old. Otherwise why not, and whether the
    /// cookie is kept: only while its enablement is live and its key not read yet (§3.4).
    pub fn session(
        &self,
        host: &str,
        value: &str,
        now: u64,
    ) -> Result<Option<String>, SessionRefusal> {
        let held = self.held();
        let Some(exposure) = held.by_host(host) else {
            return Err(SessionRefusal {
                reason: "the host is no exposure's",
                ..SessionRefusal::default()
            });
        };
        // Built only for a refusal: a request that serves allocates nothing for it.
        let refused = |reason: &'static str, enablement: Option<&HeldEnablement>| SessionRefusal {
            reason,
            exposure_id: Some(exposure.wire.exposure_id.clone()),
            enablement_id: enablement.map(|enablement| enablement.wire.enablement_id.clone()),
            kept: false,
        };
        let Some(token) = SessionToken::parse(value) else {
            return Err(refused("a malformed session cookie", None));
        };
        if exposure.x.as_deref() != Some(token.x) {
            return Err(refused("another exposure's session", None));
        }
        let Some(enablement) = exposure
            .enablements
            .iter()
            .find(|enablement| enablement.e.as_deref() == Some(token.e))
        else {
            return Err(refused("its enablement is not one io.web holds", None));
        };
        if now >= enablement.wire.until {
            return Err(refused("its enablement has ended", Some(enablement)));
        }
        let KeyState::Ready(keys) = &enablement.key else {
            // Its key may yet be read (every 30 s): the cookie is kept, to serve then, as a session
            // survives a restart of io.web whose first read of the key failed (§3.4).
            return Err(SessionRefusal {
                kept: true,
                ..refused(
                    "io.web holds no key for its enablement yet",
                    Some(enablement),
                )
            });
        };
        if !token.verifies(keys, host) {
            return Err(refused("its tag does not verify", Some(enablement)));
        }
        let idle = now.saturating_sub(token.active);
        if idle > SESSION_IDLE_S {
            return Err(refused("idle for more than 30 min", Some(enablement)));
        }
        if idle < SESSION_SLIDE_S {
            return Ok(None);
        }
        let value = SessionToken::mint(keys, host, token.x, token.e, token.sid, token.issued, now);
        let max_age = SESSION_IDLE_S.min(enablement.wire.until - now);
        Ok(Some(session_set_cookie(&value, max_age)))
    }

    /// NODE_STATUS_GET's `extensions.web.exposures` (§5.5).
    pub fn view(&self, now: u64) -> Value {
        let held = self.held();
        let enabled = held
            .exposures
            .iter()
            .filter(|exposure| {
                exposure
                    .enablements
                    .iter()
                    .any(|enablement| now < enablement.wire.until)
            })
            .count();
        let keys_unavailable = held
            .live(now)
            .filter(|(_, enablement)| !enablement.key.is_ready())
            .count();
        json!({
            "generation": held.generation,
            "exposures": held.exposures.len(),
            "enabled": enabled,
            "keys_unavailable": keys_unavailable,
            "links_issued": self.links_issued.load(Ordering::Relaxed),
            "links_redeemed": self.links_redeemed.load(Ordering::Relaxed),
            "links_refused": self.links_refused.load(Ordering::Relaxed),
            "sessions_minted": self.sessions_minted.load(Ordering::Relaxed),
        })
    }
}

fn log_key(want: &KeyWant, key: &KeyState) {
    match key {
        KeyState::Ready(_) => tracing::info!(
            exposure_id = %want.exposure_id,
            host = %want.host,
            enablement_id = %want.enablement.enablement_id,
            slot = want.enablement.slot.as_str(),
            key_version = want.enablement.key_version,
            "IO.web key ready"
        ),
        KeyState::Unavailable(detail) => tracing::warn!(
            exposure_id = %want.exposure_id,
            host = %want.host,
            enablement_id = %want.enablement.enablement_id,
            slot = want.enablement.slot.as_str(),
            key_version = want.enablement.key_version,
            detail = %detail,
            "IO.web key unavailable: its links and sessions fail closed, and it is read again every 30 s"
        ),
    }
}

/// io.web's file, if there is one.
fn read_file(path: &Path) -> Result<Option<IoWebExposuresPayload>, String> {
    let text = match std::fs::read(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };
    let file: ExposuresFile = serde_json::from_slice(&text).map_err(|err| err.to_string())?;
    if file.schema_version != FILE_SCHEMA_VERSION {
        return Err(format!(
            "schema_version {} is not {FILE_SCHEMA_VERSION}",
            file.schema_version
        ));
    }
    Ok(Some(IoWebExposuresPayload {
        generation: file.generation,
        exposures: file.exposures,
        // The file keeps them from stage 2.2's R2 on (io-web-v2-stage-2-2 §4.5).
        cloud_origins: Vec::new(),
    }))
}

/// Writes `file` whole, mode 0600, through a temporary file renamed over the old one.
fn write_file(path: &Path, file: &ExposuresFile) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "the file has no directory"))?;
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    let temporary = dir.join(format!(".{EXPOSURES_FILE}.tmp.{}", std::process::id()));
    // A leftover of a write that stopped midway.
    let _ = std::fs::remove_file(&temporary);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut out| {
            out.write_all(&text)?;
            out.sync_all()
        })
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
pub(crate) mod testing {
    //! A set, its keys and its file, for the tests of io.web.

    use super::*;
    use fluxbee_sdk::protocol::{WebExposureKeySlot, WebExposureUpstream};
    use fluxbee_sdk::VaultMetadata;
    use std::sync::atomic::AtomicUsize;

    pub const IO_WEB_ILK: &str = "ilk:7d3f0c1e-9a2b-4c5d-8e6f-0a1b2c3d4e5f";
    pub const SHOP: &str = "shop.hive-k3m9x7q2.fluxbee.ai";
    pub const ERP: &str = "erp.hive-k3m9x7q2.fluxbee.ai";
    pub const SHOP_ID: &str = "exp:6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
    pub const ERP_ID: &str = "exp:8b9c0d1e-2f3a-4b4c-9d5e-6f7a8b9c0d1e";
    pub const SHOP_ENB: &str = "enb:0a1b2c3d-4e5f-4a6b-8c7d-8e9fa0b1c2d3";
    pub const SHOP_RENEWED: &str = "enb:1b2c3d4e-5f6a-4b7c-9d8e-9fa0b1c2d3e4";
    pub const ERP_ENB: &str = "enb:2c3d4e5f-6a7b-4c8d-ae9f-a0b1c2d3e4f5";

    /// Keys as SY.vault would answer them, from a map; it counts its reads.
    pub struct MapKeys {
        owner: Option<String>,
        answers: Mutex<HashMap<String, Result<VaultValueResponse, String>>>,
        pub reads: AtomicUsize,
        /// Open, each read answers at once; closed, it waits until the gate opens, as a vault that
        /// is slow to answer.
        gate: tokio::sync::watch::Sender<bool>,
    }

    impl MapKeys {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                owner: Some(IO_WEB_ILK.to_string()),
                answers: Mutex::new(HashMap::new()),
                reads: AtomicUsize::new(0),
                gate: tokio::sync::watch::Sender::new(true),
            })
        }

        /// From now on each read waits, with the answer it found, until `open`.
        pub fn close(&self) {
            self.gate.send_replace(false);
        }

        pub fn open(&self) {
            self.gate.send_replace(true);
        }

        /// SY.admin's put: `secret` in the exposure's `slot`, at `version`, owned by io.web.
        pub fn put(&self, exposure_id: &str, slot: WebExposureKeySlot, version: i64, secret: u8) {
            let key = web_exposure_vault_key(exposure_id, slot).expect("a minted id");
            let answer = vault_answer(&key, version, &[secret; 32]);
            self.answer(&key, Ok(answer));
        }

        pub fn answer(&self, vault_key: &str, answer: Result<VaultValueResponse, String>) {
            self.answers
                .lock()
                .unwrap()
                .insert(vault_key.to_string(), answer);
        }

        pub fn read_count(&self) -> usize {
            self.reads.load(Ordering::Relaxed)
        }
    }

    impl KeySource for MapKeys {
        fn owner(&self) -> Option<&str> {
            self.owner.as_deref()
        }

        fn read(&self, vault_key: &str) -> KeyRead {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let answer = self
                .answers
                .lock()
                .unwrap()
                .get(vault_key)
                .cloned()
                .unwrap_or_else(|| {
                    Err(format!(
                        "vault returned error code=KEY_NOT_FOUND message={vault_key}"
                    ))
                });
            let mut gate = self.gate.subscribe();
            Box::pin(async move {
                let _ = gate.wait_for(|open| *open).await;
                answer
            })
        }
    }

    /// What `VAULT_GET` answers for a key SY.admin put: `{secret: <64 hex>}`, a
    /// `web_session_key` in the root tenant, owned by io.web.
    pub fn vault_answer(vault_key: &str, version: i64, secret: &[u8; 32]) -> VaultValueResponse {
        VaultValueResponse {
            status: "ok".to_string(),
            key: vault_key.to_string(),
            value: Some(json!({ "secret": crate::access::hex(secret) })),
            metadata: Some(VaultMetadata {
                tenant_id: "tnt:00000000-0000-0000-0000-000000000001".to_string(),
                owner_ilk: String::new(),
                resource_type: Some(WEB_SESSION_KEY_RESOURCE_TYPE.to_string()),
                ilk: Some(IO_WEB_ILK.to_string()),
                assigned_ilks: Vec::new(),
                description: None,
                created_by: Some("SY.admin@motherbee".to_string()),
                created_at: None,
                updated_at: None,
                tags: Vec::new(),
            }),
            version: Some(version),
            error_code: None,
            message: None,
        }
    }

    pub fn enablement(
        id: &str,
        until: u64,
        slot: WebExposureKeySlot,
        version: i64,
    ) -> IoWebEnablement {
        IoWebEnablement {
            enablement_id: id.to_string(),
            until,
            slot,
            key_version: version,
        }
    }

    pub fn exposure(id: &str, host: &str, enablements: Vec<IoWebEnablement>) -> IoWebExposure {
        IoWebExposure {
            exposure_id: id.to_string(),
            host: host.to_string(),
            upstream: WebExposureUpstream::Hive {
                hive: "worker1".to_string(),
                port: 8080,
            },
            enablements,
        }
    }

    pub fn set(generation: u64, exposures: Vec<IoWebExposure>) -> IoWebExposuresPayload {
        IoWebExposuresPayload {
            generation,
            exposures,
            cloud_origins: Vec::new(),
        }
    }

    /// A fresh directory of its own, and the file io.web would keep in it.
    pub fn file(label: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "io-web-exposures-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the node's dir");
        dir.join(EXPOSURES_FILE)
    }

    /// io.web booted at `boot`, holding `set` with the keys `keys` has.
    pub async fn holding(
        label: &str,
        keys: Arc<MapKeys>,
        boot: u64,
        set: IoWebExposuresPayload,
    ) -> Arc<Exposures> {
        let exposures = Arc::new(Exposures::new(file(label), keys, boot));
        let now = boot;
        let answer = exposures.apply_push(set, now).await;
        assert!(
            matches!(answer, IoWebExposuresResponse::Ok { .. }),
            "{answer:?}"
        );
        exposures
    }

    /// A fresh link's code for the current enablement of `exposure_id`, at `now`.
    pub fn link(exposures: &Exposures, exposure_id: &str, enablement_id: &str, now: u64) -> String {
        let request = IoWebAccessLinkRequest {
            exposure_id: exposure_id.to_string(),
            enablement_id: enablement_id.to_string(),
            generation: 0,
        };
        match exposures.issue_link(&request, now) {
            IoWebAccessLinkResponse::Ok { url, .. } => url
                .split_once("?code=")
                .map(|(_, code)| code.to_string())
                .expect("a code in the link"),
            other => panic!("no link: {other:?}"),
        }
    }

    pub fn remove(exposures: &Exposures) {
        if let Some(dir) = exposures.file().parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use fluxbee_sdk::protocol::{
        Destination, Message, Meta, Routing, WebExposureKeySlot, SYSTEM_KIND,
    };
    use fluxbee_sdk::{
        OperationalRouteProfile, RouterDispatcherTestHarness, MSG_VAULT_GET, MSG_VAULT_GET_RESPONSE,
    };
    use std::os::unix::fs::PermissionsExt;

    const BOOT: u64 = 1_763_121_000;
    const UNTIL: u64 = BOOT + 8 * 3600;

    fn shop(enablements: Vec<IoWebEnablement>) -> IoWebExposure {
        exposure(SHOP_ID, SHOP, enablements)
    }

    fn enabled_shop() -> IoWebExposure {
        shop(vec![enablement(SHOP_ENB, UNTIL, WebExposureKeySlot::A, 4)])
    }

    fn keys_of(answer: &IoWebExposuresResponse) -> Vec<(String, IoWebKeyState, Option<String>)> {
        match answer {
            IoWebExposuresResponse::Ok { enablements, .. } => enablements
                .iter()
                .map(|key| (key.enablement_id.clone(), key.key, key.detail.clone()))
                .collect(),
            other => panic!("not taken: {other:?}"),
        }
    }

    /// The cookie's value in a `Set-Cookie`.
    fn cookie_value(set_cookie: &str) -> String {
        set_cookie
            .split(';')
            .next()
            .and_then(|pair| pair.strip_prefix("__Host-fluxbee_session="))
            .expect("the session cookie")
            .to_string()
    }

    /// A lower generation keeps the set held and answers STALE_GENERATION; the same generation
    /// again is acknowledged without reading any key.
    #[tokio::test]
    async fn a_stale_generation_is_refused() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, WebExposureKeySlot::A, 4, 1);
        let exposures = holding("stale", keys.clone(), BOOT, set(10, vec![enabled_shop()])).await;
        assert_eq!(keys.read_count(), 1);

        let older = exposures.apply_push(set(9, Vec::new()), BOOT + 1).await;
        let IoWebExposuresResponse::Error {
            error_code,
            error_detail,
        } = &older
        else {
            panic!("an older set is refused: {older:?}");
        };
        assert_eq!(error_code, IO_WEB_ERROR_STALE_GENERATION);
        assert!(error_detail.contains("10"), "{error_detail}");
        assert_eq!(exposures.view(BOOT + 1)["generation"], 10);
        assert_eq!(exposures.view(BOOT + 1)["enabled"], 1, "the set held stays");

        let again = exposures
            .apply_push(set(10, vec![enabled_shop()]), BOOT + 2)
            .await;
        assert_eq!(
            again,
            IoWebExposuresResponse::Ok {
                generation: 10,
                changed: false,
                enablements: vec![IoWebEnablementKey {
                    enablement_id: SHOP_ENB.to_string(),
                    key: IoWebKeyState::Ready,
                    detail: None,
                }],
            }
        );
        assert_eq!(keys.read_count(), 1, "an equal generation reads nothing");
        remove(&exposures);
    }

    /// Through the vault as io.web reads it (its own ILK, its own name, SY.vault on the
    /// motherbee): a key at another version than the enablement's is unavailable, and so the
    /// enablement gives no link (§3.1).
    #[tokio::test]
    async fn a_key_whose_version_differs_is_unavailable() {
        let profile = OperationalRouteProfile::builder()
            .command_channel("exposures")
            .build()
            .expect("profile");
        let (dispatcher, mut vault) = RouterDispatcherTestHarness::new("IO.web@motherbee", profile);
        let keys = Arc::new(VaultKeys::new(
            dispatcher,
            "IO.web@motherbee",
            Some(IO_WEB_ILK.to_string()),
        ));
        let exposures = Arc::new(Exposures::new(file("version"), keys, BOOT));
        let pushed = tokio::spawn({
            let exposures = Arc::clone(&exposures);
            async move {
                exposures
                    .apply_push(set(2, vec![enabled_shop()]), BOOT)
                    .await
            }
        });

        let asked = vault
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("io.web asks the vault for the key");
        let vault_key = web_exposure_vault_key(SHOP_ID, WebExposureKeySlot::A).unwrap();
        assert_eq!(asked.meta.msg.as_deref(), Some(MSG_VAULT_GET));
        assert!(
            matches!(&asked.routing.dst, Destination::Unicast(to) if to == "SY.vault@motherbee"),
            "{:?}",
            asked.routing.dst
        );
        assert_eq!(asked.meta.src_ilk.as_deref(), Some(IO_WEB_ILK));
        assert_eq!(
            asked.routing.src_l2_name.as_deref(),
            Some("IO.web@motherbee")
        );
        assert_eq!(asked.payload, json!({ "key": vault_key }));
        vault
            .inject(Message {
                routing: Routing {
                    src: "vault-uuid".to_string(),
                    src_l2_name: Some("SY.vault@motherbee".to_string()),
                    dst: Destination::Unicast(asked.routing.src.clone()),
                    ttl: 16,
                    trace_id: asked.routing.trace_id.clone(),
                },
                meta: Meta {
                    msg_type: SYSTEM_KIND.to_string(),
                    msg: Some(MSG_VAULT_GET_RESPONSE.to_string()),
                    ..Meta::default()
                },
                payload: serde_json::to_value(vault_answer(&vault_key, 3, &[1; 32])).unwrap(),
            })
            .await
            .unwrap();

        let answer = pushed.await.unwrap();
        assert_eq!(
            keys_of(&answer),
            vec![(
                SHOP_ENB.to_string(),
                IoWebKeyState::Unavailable,
                Some("the key is at version 3, not 4".to_string())
            )]
        );
        assert_eq!(exposures.view(BOOT)["keys_unavailable"], 1);
        let request = IoWebAccessLinkRequest {
            exposure_id: SHOP_ID.to_string(),
            enablement_id: SHOP_ENB.to_string(),
            generation: 2,
        };
        assert!(matches!(
            exposures.issue_link(&request, BOOT + 1),
            IoWebAccessLinkResponse::Error { error_code, .. } if error_code == IO_WEB_ERROR_KEY_UNAVAILABLE
        ));
        remove(&exposures);
    }

    /// The key is taken only as SY.admin wrote it (§3.1): its resource type, its owner and no
    /// other node assigned to it, its version and its value's shape; the answer names why, never
    /// the value.
    #[test]
    fn a_key_is_taken_only_as_sy_admin_wrote_it() {
        let vault_key = web_exposure_vault_key(SHOP_ID, WebExposureKeySlot::A).unwrap();
        let wanted = enablement(SHOP_ENB, UNTIL, WebExposureKeySlot::A, 4);
        let good = vault_answer(&vault_key, 4, &[9; 32]);
        assert_eq!(
            take_key(&good, &wanted, Some(IO_WEB_ILK)).unwrap(),
            EnablementKeys::derive(&[9; 32])
        );

        let mut other_type = good.clone();
        other_type.metadata.as_mut().unwrap().resource_type = Some("slack".to_string());
        let mut pooled = good.clone();
        pooled.metadata.as_mut().unwrap().ilk = None;
        let mut shared = good.clone();
        shared.metadata.as_mut().unwrap().assigned_ilks =
            vec!["ilk:00000000-0000-4000-8000-000000000000".to_string()];
        let mut not_hex = good.clone();
        not_hex.value = Some(json!({"secret": "x".repeat(64)}));
        let mut no_version = good.clone();
        no_version.version = None;
        for (answer, owner, why) in [
            (&other_type, Some(IO_WEB_ILK), "resource type is slack"),
            (&pooled, Some(IO_WEB_ILK), "not io.web's own"),
            (
                &good,
                Some("ilk:00000000-0000-4000-8000-000000000000"),
                "not io.web's own",
            ),
            (&good, None, "not io.web's own"),
            (&shared, Some(IO_WEB_ILK), "assigned to other nodes"),
            (&not_hex, Some(IO_WEB_ILK), "{secret: <64 hex>}"),
            (&no_version, Some(IO_WEB_ILK), "no version"),
        ] {
            let err = take_key(answer, &wanted, owner).expect_err(why);
            assert!(err.contains(why), "{err}");
            assert!(!err.contains(&crate::access::hex(&[9; 32])), "{err}");
        }
        // The legacy owner field names the owner too, as SY.vault reads it.
        let mut legacy = good.clone();
        let metadata = legacy.metadata.as_mut().unwrap();
        metadata.ilk = None;
        metadata.owner_ilk = IO_WEB_ILK.to_string();
        assert!(take_key(&legacy, &wanted, Some(IO_WEB_ILK)).is_ok());
    }

    /// What io.web holds comes back from its file at boot, mode 0600, with its keys read again,
    /// so a session minted before a restart still serves after it (§3.4).
    #[tokio::test]
    async fn the_set_reloads_from_its_file_at_boot() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, WebExposureKeySlot::A, 4, 1);
        let before = holding("reload", keys.clone(), BOOT, set(7, vec![enabled_shop()])).await;
        let path = before.file().to_path_buf();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["schema_version"], 1);
        assert_eq!(written["generation"], 7);
        assert_eq!(
            written["exposures"],
            serde_json::to_value(vec![enabled_shop()]).unwrap()
        );
        let code = link(&before, SHOP_ID, SHOP_ENB, BOOT + 10);
        let set_cookie = before
            .redeem(SHOP, Some(&code), BOOT + 11, RequestTrace::default())
            .expect("redeemed");

        let after = Exposures::new(path.clone(), keys.clone(), BOOT + 100);
        assert_eq!(after.view(BOOT + 100)["generation"], 0);
        after.load_at_boot(BOOT + 100).await;
        let view = after.view(BOOT + 100);
        assert_eq!(
            (&view["generation"], &view["exposures"], &view["enabled"]),
            (&json!(7), &json!(1), &json!(1))
        );
        assert_eq!(view["keys_unavailable"], 0);
        assert_eq!(keys.read_count(), 2, "the key is read again at boot");
        // 90 s after its last activity: valid, and carried again.
        let slid = after
            .session(SHOP, &cookie_value(&set_cookie), BOOT + 101)
            .expect("the session survives the restart")
            .expect("slid");
        assert!(
            slid.contains(&format!(".{}.{}.", BOOT + 11, BOOT + 101)),
            "{slid}"
        );

        // A file that cannot be read leaves io.web with no exposure, which fails closed.
        std::fs::write(&path, b"not json").unwrap();
        let unreadable = Exposures::new(path, keys, BOOT + 200);
        unreadable.load_at_boot(BOOT + 200).await;
        assert_eq!(unreadable.view(BOOT + 200)["exposures"], 0);
        assert!(!unreadable.knows_host(SHOP));
        remove(&before);
    }

    /// A key that cannot be read is unavailable, then read again at the next retry, and serves
    /// once it reads.
    #[tokio::test]
    async fn an_unavailable_key_is_read_again_at_the_retry() {
        let keys = MapKeys::new();
        let exposures = holding("retry", keys.clone(), BOOT, set(3, vec![enabled_shop()])).await;
        let request = IoWebAccessLinkRequest {
            exposure_id: SHOP_ID.to_string(),
            enablement_id: SHOP_ENB.to_string(),
            generation: 3,
        };
        assert!(matches!(
            exposures.issue_link(&request, BOOT + 1),
            IoWebAccessLinkResponse::Error { error_code, .. } if error_code == IO_WEB_ERROR_KEY_UNAVAILABLE
        ));
        assert_eq!(exposures.view(BOOT + 1)["keys_unavailable"], 1);

        keys.put(SHOP_ID, WebExposureKeySlot::A, 4, 1);
        exposures.retry(BOOT + 30).await;
        assert_eq!(exposures.view(BOOT + 30)["keys_unavailable"], 0);
        assert!(matches!(
            exposures.issue_link(&request, BOOT + 31),
            IoWebAccessLinkResponse::Ok { .. }
        ));
        // A key held is never read again.
        exposures.retry(BOOT + 60).await;
        assert_eq!(keys.read_count(), 2);
        remove(&exposures);
    }

    /// A retry reads its keys out of the changes' turn: a disable is applied while the retry still
    /// waits on the vault (§3.4), and the key that read brings back afterwards goes to no
    /// enablement the disable dropped. A key held stays, whatever a read that ends later says.
    #[tokio::test]
    async fn a_push_never_waits_for_a_retry_s_reads() {
        let keys = MapKeys::new();
        // No key in the vault yet: the enablement's is unavailable, and the retry reads it again.
        let exposures = holding(
            "unblocked",
            keys.clone(),
            BOOT,
            set(1, vec![enabled_shop()]),
        )
        .await;
        keys.put(SHOP_ID, WebExposureKeySlot::A, 4, 1);
        keys.close();
        let retry = tokio::spawn({
            let exposures = Arc::clone(&exposures);
            async move { exposures.retry(BOOT + 30).await }
        });
        while keys.read_count() < 2 {
            tokio::task::yield_now().await;
        }
        let disabled = tokio::time::timeout(
            Duration::from_secs(1),
            exposures.apply_push(set(2, vec![shop(Vec::new())]), BOOT + 31),
        )
        .await
        .expect("the disable does not wait for the retry's read");
        assert!(
            matches!(disabled, IoWebExposuresResponse::Ok { generation: 2, .. }),
            "{disabled:?}"
        );
        keys.open();
        retry.await.unwrap();
        assert_eq!(exposures.view(BOOT + 32)["enabled"], 0);
        let request = IoWebAccessLinkRequest {
            exposure_id: SHOP_ID.to_string(),
            enablement_id: SHOP_ENB.to_string(),
            generation: 2,
        };
        assert!(matches!(
            exposures.issue_link(&request, BOOT + 32),
            IoWebAccessLinkResponse::Error { error_code, .. } if error_code == IO_WEB_ERROR_ENABLEMENT_UNKNOWN
        ));
        remove(&exposures);

        // A push read the key while a retry's read of it was still out: the retry's failure,
        // ending later, takes nothing back.
        let keys = MapKeys::new();
        keys.put(SHOP_ID, WebExposureKeySlot::A, 4, 1);
        let ready = holding("stays", keys, BOOT, set(1, vec![enabled_shop()])).await;
        let late = KeyWant {
            exposure_id: SHOP_ID.to_string(),
            host: SHOP.to_string(),
            enablement: enablement(SHOP_ENB, UNTIL, WebExposureKeySlot::A, 4),
            vault_key: Ok(web_exposure_vault_key(SHOP_ID, WebExposureKeySlot::A).unwrap()),
        };
        let failed = KeyState::Unavailable("the vault did not answer within 3 s".to_string());
        assert!(ready.held_mut().set_key(&late, failed).is_none());
        assert_eq!(ready.view(BOOT + 1)["keys_unavailable"], 0);
        remove(&ready);
    }

    /// A link is minted only for the current enablement, live, with its key: otherwise the code
    /// names why (§5.5).
    #[tokio::test]
    async fn a_link_is_minted_only_for_the_live_current_enablement() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, WebExposureKeySlot::A, 4, 1);
        keys.put(SHOP_ID, WebExposureKeySlot::B, 1, 2);
        let renewed = shop(vec![
            enablement(SHOP_ENB, BOOT + 600, WebExposureKeySlot::A, 4),
            enablement(SHOP_RENEWED, UNTIL, WebExposureKeySlot::B, 1),
        ]);
        let exposures = holding("mint", keys, BOOT, set(5, vec![renewed])).await;
        let ask = |exposure_id: &str, enablement_id: &str, now: u64| {
            let request = IoWebAccessLinkRequest {
                exposure_id: exposure_id.to_string(),
                enablement_id: enablement_id.to_string(),
                generation: 5,
            };
            match exposures.issue_link(&request, now) {
                IoWebAccessLinkResponse::Ok {
                    url,
                    expires_at,
                    link_id,
                } => Ok((url, expires_at, link_id)),
                IoWebAccessLinkResponse::Error { error_code, .. } => Err(error_code),
            }
        };
        let (url, expires_at, link_id) = ask(SHOP_ID, SHOP_RENEWED, BOOT + 5).expect("a link");
        assert!(
            url.starts_with(&format!("https://{SHOP}/.fluxbee/enter?code=L1.")),
            "{url}"
        );
        assert_eq!(expires_at, BOOT + 5 + 120);
        assert_eq!(link_id.len(), 16);
        let code = url.split_once("?code=").unwrap().1;
        assert!(!code.contains(&link_id), "the link id is not in the code");
        assert_eq!(
            ask(ERP_ID, ERP_ENB, BOOT + 5).unwrap_err(),
            IO_WEB_ERROR_EXPOSURE_UNKNOWN
        );
        // The renewed one is live still, but no longer the current one.
        assert_eq!(
            ask(SHOP_ID, SHOP_ENB, BOOT + 5).unwrap_err(),
            IO_WEB_ERROR_ENABLEMENT_UNKNOWN
        );
        assert_eq!(
            ask(SHOP_ID, SHOP_RENEWED, UNTIL).unwrap_err(),
            IO_WEB_ERROR_ENABLEMENT_UNKNOWN,
            "ended"
        );
        assert_eq!(exposures.view(BOOT + 5)["links_issued"], 1);
        remove(&exposures);
    }

    /// A set io.web cannot write takes the old file away, so that a restart cannot bring back
    /// what the set dropped; io.web writes it again at the next retry.
    #[tokio::test]
    async fn a_set_it_cannot_write_takes_the_old_file_away() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, WebExposureKeySlot::A, 4, 1);
        let exposures = holding("unwritable", keys, BOOT, set(1, vec![enabled_shop()])).await;
        let path = exposures.file().to_path_buf();
        assert!(path.exists());
        // The temporary file's name, taken by a directory, fails the next write.
        let blocker = path
            .parent()
            .unwrap()
            .join(format!(".{EXPOSURES_FILE}.tmp.{}", std::process::id()));
        std::fs::create_dir(&blocker).unwrap();

        let disabled = exposures
            .apply_push(set(2, vec![shop(Vec::new())]), BOOT + 1)
            .await;
        assert!(matches!(
            disabled,
            IoWebExposuresResponse::Ok { generation: 2, .. }
        ));
        assert!(
            !path.exists(),
            "the old file, which held the enablement, is gone"
        );
        assert!(!exposures.knows_host("nowhere") && exposures.knows_host(SHOP));
        assert_eq!(exposures.view(BOOT + 1)["enabled"], 0);

        std::fs::remove_dir(&blocker).unwrap();
        exposures.retry(BOOT + 30).await;
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["generation"], 2);
        assert_eq!(
            written["exposures"],
            serde_json::to_value(vec![shop(Vec::new())]).unwrap()
        );
        remove(&exposures);
    }
}
