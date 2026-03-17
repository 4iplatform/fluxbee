#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityLookupInput {
    pub channel: String,
    pub external_id: String,
    #[serde(default)]
    pub tenant_hint: Option<String>,
    #[serde(default)]
    pub attributes: serde_json::Value,
}

pub type ResolveOrCreateInput = IdentityLookupInput;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("miss")]
    Miss,
    #[error("timeout")]
    Timeout,
    #[error("unavailable")]
    Unavailable,
    #[error("other: {0}")]
    Other(String),
}

pub trait IdentityResolver: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn lookup(&self, channel: &str, external_id: &str) -> Result<Option<String>, IdentityError>;
}

#[async_trait]
pub trait IdentityProvisioner: Send + Sync {
    async fn provision(&self, input: &ResolveOrCreateInput) -> Result<Option<String>, IdentityError>;
}

#[derive(Default)]
pub struct DisabledIdentityProvisioner;

impl DisabledIdentityProvisioner {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl IdentityProvisioner for DisabledIdentityProvisioner {
    async fn provision(&self, _input: &ResolveOrCreateInput) -> Result<Option<String>, IdentityError> {
        Ok(None)
    }
}

#[derive(Default)]
pub struct DisabledIdentityResolver;

impl DisabledIdentityResolver {
    pub fn new() -> Self {
        Self
    }
}

impl IdentityResolver for DisabledIdentityResolver {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn lookup(&self, _channel: &str, _external_id: &str) -> Result<Option<String>, IdentityError> {
        Ok(None)
    }
}

#[derive(Default)]
pub struct MockIdentityResolver {
    map: Mutex<HashMap<(String, String), String>>,
}

impl MockIdentityResolver {
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdentityResolver for MockIdentityResolver {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn lookup(&self, channel: &str, external_id: &str) -> Result<Option<String>, IdentityError> {
        let mut map = self.map.lock().map_err(|_| IdentityError::Unavailable)?;
        let key = (channel.to_string(), external_id.to_string());
        let src_ilk = map
            .entry(key)
            .or_insert_with(|| format!("ilk:mock:{}", Uuid::new_v4()))
            .clone();
        Ok(Some(src_ilk))
    }
}

#[derive(Debug)]
pub struct ShmIdentityResolver {
    channel_type_max_len: usize,
    external_id_max_len: usize,
    path: PathBuf,
    min_reload_interval: Duration,
    cache: Mutex<ShmCache>,
}

#[derive(Debug)]
struct ShmCache {
    bytes: Vec<u8>,
    last_read: Option<Instant>,
}

impl ShmIdentityResolver {
    pub fn new(island_id: &str) -> Self {
        Self::new_with_path_and_reload(
            identity_shm_path_for_island(island_id),
            Duration::from_millis(250),
        )
    }

    pub fn new_with_path_and_reload(path: PathBuf, min_reload_interval: Duration) -> Self {
        Self {
            channel_type_max_len: 16,
            external_id_max_len: 128,
            path,
            min_reload_interval,
            cache: Mutex::new(ShmCache {
                bytes: Vec::new(),
                last_read: None,
            }),
        }
    }
}

impl IdentityResolver for ShmIdentityResolver {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn lookup(&self, channel: &str, external_id: &str) -> Result<Option<String>, IdentityError> {
        if channel.len() > self.channel_type_max_len || external_id.len() > self.external_id_max_len {
            return Ok(None);
        }

        #[cfg(not(unix))]
        {
            let _ = (channel, external_id);
            return Ok(None);
        }

        #[cfg(unix)]
        {
            let bytes = self.read_best_effort()?;
            resolve_ich_to_ilk(&bytes, channel, external_id)
        }
    }
}

impl ShmIdentityResolver {
    #[cfg(unix)]
    fn read_best_effort(&self) -> Result<Vec<u8>, IdentityError> {
        use std::fs;

        let mut cache = self.cache.lock().map_err(|_| IdentityError::Unavailable)?;
        let now = Instant::now();
        let should_reload = match cache.last_read {
            None => true,
            Some(t) => now.duration_since(t) >= self.min_reload_interval,
        };

        if should_reload {
            if let Ok(b) = fs::read(&self.path) {
                cache.bytes = b;
            } else {
                // Best-effort: keep last successful snapshot.
            }
            cache.last_read = Some(now);
        }

        Ok(cache.bytes.clone())
    }
}

fn identity_shm_path_for_island(island_id: &str) -> PathBuf {
    // Spec name: "/jsr-identity-<island_id>" (POSIX shm). Common FS view on Linux is "/dev/shm/jsr-identity-<island_id>".
    // We keep this resolver best-effort and portable by looking at the common path first.
    PathBuf::from(format!("/dev/shm/jsr-identity-{island_id}"))
}

#[cfg(unix)]
fn resolve_ich_to_ilk(bytes: &[u8], channel_type: &str, external_id: &str) -> Result<Option<String>, IdentityError> {
    const IDENTITY_HEADER_SIZE: usize = 128;
    const ENTRY_SIZE: usize = 256;
    const MAX_ILKS: usize = 8192;
    const MAX_ICHS: usize = 4096;
    const MAX_ICH_MAPPINGS: usize = 8192;

    let offset_ilks = IDENTITY_HEADER_SIZE;
    let offset_ichs = offset_ilks + (MAX_ILKS * ENTRY_SIZE);
    let offset_ich_mappings = offset_ichs + (MAX_ICHS * ENTRY_SIZE);

    if bytes.len() < offset_ich_mappings + (MAX_ICH_MAPPINGS * ENTRY_SIZE) {
        return Ok(None);
    }

    let ich = find_ich_in_mappings(
        &bytes[offset_ich_mappings..offset_ich_mappings + (MAX_ICH_MAPPINGS * ENTRY_SIZE)],
        channel_type,
        external_id,
    )?;

    let Some(ich) = ich else {
        return Ok(None);
    };

    find_ilk_for_ich(
        &bytes[offset_ichs..offset_ichs + (MAX_ICHS * ENTRY_SIZE)],
        &ich,
    )
}

#[cfg(unix)]
fn find_ich_in_mappings(mappings_region: &[u8], channel_type: &str, external_id: &str) -> Result<Option<String>, IdentityError> {
    const ENTRY_SIZE: usize = 256;
    const CHANNEL_TYPE_OFF: usize = 0;
    const CHANNEL_TYPE_LEN: usize = 16;
    const EXTERNAL_ID_OFF: usize = CHANNEL_TYPE_OFF + CHANNEL_TYPE_LEN;
    const EXTERNAL_ID_LEN: usize = 128;
    const ICH_OFF: usize = EXTERNAL_ID_OFF + EXTERNAL_ID_LEN;
    const ICH_LEN: usize = 48;
    const FLAGS_OFF: usize = ICH_OFF + ICH_LEN;

    for chunk in mappings_region.chunks_exact(ENTRY_SIZE) {
        let flags = u16::from_le_bytes([chunk[FLAGS_OFF], chunk[FLAGS_OFF + 1]]);
        if flags == 0 {
            continue;
        }
        let ct = read_cstr(&chunk[CHANNEL_TYPE_OFF..CHANNEL_TYPE_OFF + CHANNEL_TYPE_LEN]);
        if ct != channel_type {
            continue;
        }
        let eid = read_cstr(&chunk[EXTERNAL_ID_OFF..EXTERNAL_ID_OFF + EXTERNAL_ID_LEN]);
        if eid != external_id {
            continue;
        }
        let ich = read_cstr(&chunk[ICH_OFF..ICH_OFF + ICH_LEN]);
        if ich.is_empty() {
            continue;
        }
        return Ok(Some(ich));
    }

    Ok(None)
}

#[cfg(unix)]
fn find_ilk_for_ich(ichs_region: &[u8], ich: &str) -> Result<Option<String>, IdentityError> {
    const ENTRY_SIZE: usize = 256;
    const ICH_OFF: usize = 0;
    const ICH_LEN: usize = 48;
    const ILK_OFF: usize = 48 + 16 + 128; // ich + channel_type + external_id
    const ILK_LEN: usize = 48;
    const FLAGS_OFF: usize = ILK_OFF + ILK_LEN;

    for chunk in ichs_region.chunks_exact(ENTRY_SIZE) {
        let flags = u16::from_le_bytes([chunk[FLAGS_OFF], chunk[FLAGS_OFF + 1]]);
        if flags == 0 {
            continue;
        }
        let entry_ich = read_cstr(&chunk[ICH_OFF..ICH_OFF + ICH_LEN]);
        if entry_ich != ich {
            continue;
        }
        let ilk = read_cstr(&chunk[ILK_OFF..ILK_OFF + ILK_LEN]);
        if ilk.is_empty() {
            return Ok(None);
        }
        return Ok(Some(ilk));
    }

    Ok(None)
}

fn read_cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let slice = &bytes[..end];
    // Identity SHM strings are ASCII/UTF-8.
    String::from_utf8_lossy(slice).trim().to_string()
}
