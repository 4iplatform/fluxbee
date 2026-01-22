use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use jsr_client::protocol::Message;

#[derive(Debug, thiserror::Error)]
pub enum OpaError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct OpaResolver {
    policy_path: PathBuf,
    policy_mtime: Option<SystemTime>,
    policy_loaded: bool,
    policy_version: u64,
    opa_load_status: u8,
    logged_missing: bool,
}

impl OpaResolver {
    pub fn new(policy_path: PathBuf) -> Self {
        Self {
            policy_path,
            policy_mtime: None,
            policy_loaded: false,
            policy_version: 0,
            opa_load_status: OPA_STATUS_ERROR,
            logged_missing: false,
        }
    }

    pub fn resolve_target(&mut self, msg: &Message) -> Result<Option<String>, OpaError> {
        let _ = self.refresh_status()?;
        let target = msg.meta.target.as_deref();
        if target.is_none() {
            return Ok(None);
        }
        if !self.policy_loaded && !self.logged_missing {
            self.logged_missing = true;
            tracing::warn!(
                path = %self.policy_path.display(),
                "opa policy not loaded; using meta.target directly"
            );
        }
        Ok(target.map(|value| value.to_string()))
    }

    pub fn refresh_status(&mut self) -> Result<(u64, u8), OpaError> {
        let metadata = match std::fs::metadata(&self.policy_path) {
            Ok(metadata) => metadata,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return Err(err.into());
                }
                if self.policy_loaded || self.opa_load_status != OPA_STATUS_ERROR {
                    self.policy_loaded = false;
                    self.policy_mtime = None;
                    self.policy_version = 0;
                    self.opa_load_status = OPA_STATUS_ERROR;
                    tracing::warn!(
                        path = %self.policy_path.display(),
                        "opa policy missing; resolver disabled"
                    );
                }
                return Ok((self.policy_version, self.opa_load_status));
            }
        };
        let mtime = metadata.modified()?;
        if self.policy_loaded && self.policy_mtime == Some(mtime) {
            return Ok((self.policy_version, self.opa_load_status));
        }
        self.policy_loaded = true;
        self.policy_mtime = Some(mtime);
        self.policy_version = mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.opa_load_status = OPA_STATUS_OK;
        self.logged_missing = false;
        tracing::info!(
            path = %self.policy_path.display(),
            "opa policy loaded"
        );
        Ok((self.policy_version, self.opa_load_status))
    }

    pub fn status(&self) -> (u64, u8) {
        (self.policy_version, self.opa_load_status)
    }
}

const OPA_STATUS_OK: u8 = 0;
const OPA_STATUS_ERROR: u8 = 1;
