#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
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
    fn remember(&self, _channel: &str, _external_id: &str, _src_ilk: &str) {}
}

#[async_trait]
pub trait IdentityProvisioner: Send + Sync {
    async fn provision(
        &self,
        input: &ResolveOrCreateInput,
    ) -> Result<Option<String>, IdentityError>;
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
    async fn provision(
        &self,
        _input: &ResolveOrCreateInput,
    ) -> Result<Option<String>, IdentityError> {
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
    hive_id: String,
    cache_ttl: Duration,
    cache: Mutex<HashMap<(String, String), CacheEntry>>,
}

#[derive(Debug)]
struct CacheEntry {
    src_ilk: String,
    expires_at: Instant,
}

impl ShmIdentityResolver {
    pub fn new(island_id: &str) -> Self {
        Self::new_with_cache_ttl(island_id, Duration::from_secs(10))
    }

    pub fn new_with_cache_ttl(hive_id: &str, cache_ttl: Duration) -> Self {
        Self {
            channel_type_max_len: 16,
            external_id_max_len: 128,
            hive_id: hive_id.trim().to_string(),
            cache_ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cache_get(&self, channel: &str, external_id: &str) -> Option<String> {
        let key = (channel.to_string(), external_id.to_string());
        let mut guard = self.cache.lock().ok()?;
        let Some(entry) = guard.get(&key) else {
            return None;
        };
        if Instant::now() >= entry.expires_at {
            guard.remove(&key);
            return None;
        }
        Some(entry.src_ilk.clone())
    }

    fn cache_put(&self, channel: &str, external_id: &str, src_ilk: &str) {
        if src_ilk.trim().is_empty() {
            return;
        }
        if let Ok(mut guard) = self.cache.lock() {
            guard.insert(
                (channel.to_string(), external_id.to_string()),
                CacheEntry {
                    src_ilk: src_ilk.to_string(),
                    expires_at: Instant::now() + self.cache_ttl,
                },
            );
        }
    }
}

impl IdentityResolver for ShmIdentityResolver {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn lookup(&self, channel: &str, external_id: &str) -> Result<Option<String>, IdentityError> {
        if channel.len() > self.channel_type_max_len || external_id.len() > self.external_id_max_len
        {
            return Ok(None);
        }
        if self.hive_id.is_empty() {
            return Ok(None);
        }
        if let Some(cached) = self.cache_get(channel, external_id) {
            return Ok(Some(cached));
        }

        #[cfg(not(unix))]
        {
            let _ = (channel, external_id);
            return Ok(None);
        }

        #[cfg(unix)]
        {
            match fluxbee_sdk::resolve_ilk_from_hive_id(&self.hive_id, channel, external_id) {
                Ok(Some(src_ilk)) => {
                    self.cache_put(channel, external_id, &src_ilk);
                    Ok(Some(src_ilk))
                }
                Ok(None) => Ok(None),
                Err(err) => Err(IdentityError::Other(err.to_string())),
            }
        }
    }

    fn remember(&self, channel: &str, external_id: &str, src_ilk: &str) {
        self.cache_put(channel, external_id, src_ilk);
    }
}
