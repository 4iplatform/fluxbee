use std::path::PathBuf;
use std::time::SystemTime;

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
    logged_missing: bool,
}

impl OpaResolver {
    pub fn new(policy_path: PathBuf) -> Self {
        Self {
            policy_path,
            policy_mtime: None,
            policy_loaded: false,
            logged_missing: false,
        }
    }

    pub fn resolve_target(&mut self, msg: &Message) -> Result<Option<String>, OpaError> {
        self.refresh_policy()?;
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

    fn refresh_policy(&mut self) -> Result<(), OpaError> {
        let metadata = match std::fs::metadata(&self.policy_path) {
            Ok(metadata) => metadata,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return Err(err.into());
                }
                if self.policy_loaded {
                    self.policy_loaded = false;
                    self.policy_mtime = None;
                    tracing::warn!(
                        path = %self.policy_path.display(),
                        "opa policy missing; resolver disabled"
                    );
                }
                return Ok(());
            }
        };
        let mtime = metadata.modified()?;
        if self.policy_loaded && self.policy_mtime == Some(mtime) {
            return Ok(());
        }
        self.policy_loaded = true;
        self.policy_mtime = Some(mtime);
        self.logged_missing = false;
        tracing::info!(
            path = %self.policy_path.display(),
            "opa policy loaded"
        );
        Ok(())
    }
}
