use serde::Serialize;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IoControlPlaneMetricsSnapshot {
    pub config_set_ok: u64,
    pub config_set_error: u64,
    pub last_config_version: u64,
    pub current_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
}

#[derive(Debug, Default)]
struct IoControlPlaneMetricsInner {
    config_set_ok: u64,
    config_set_error: u64,
    last_config_version: u64,
    current_state: String,
    last_error_code: Option<String>,
}

#[derive(Debug, Default)]
pub struct IoControlPlaneMetrics {
    inner: Mutex<IoControlPlaneMetricsInner>,
}

impl IoControlPlaneMetrics {
    pub fn with_initial_state(current_state: impl Into<String>, last_config_version: u64) -> Self {
        Self {
            inner: Mutex::new(IoControlPlaneMetricsInner {
                config_set_ok: 0,
                config_set_error: 0,
                last_config_version,
                current_state: current_state.into(),
                last_error_code: None,
            }),
        }
    }

    pub fn record_config_set_ok(&self, current_state: impl Into<String>, config_version: u64) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.config_set_ok = guard.config_set_ok.saturating_add(1);
            guard.current_state = current_state.into();
            guard.last_config_version = config_version;
            guard.last_error_code = None;
        }
    }

    pub fn record_config_set_error(
        &self,
        current_state: impl Into<String>,
        config_version: u64,
        error_code: impl Into<String>,
    ) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.config_set_error = guard.config_set_error.saturating_add(1);
            guard.current_state = current_state.into();
            guard.last_config_version = config_version;
            guard.last_error_code = Some(error_code.into());
        }
    }

    pub fn snapshot(&self) -> IoControlPlaneMetricsSnapshot {
        match self.inner.lock() {
            Ok(guard) => IoControlPlaneMetricsSnapshot {
                config_set_ok: guard.config_set_ok,
                config_set_error: guard.config_set_error,
                last_config_version: guard.last_config_version,
                current_state: guard.current_state.clone(),
                last_error_code: guard.last_error_code.clone(),
            },
            Err(_) => IoControlPlaneMetricsSnapshot {
                config_set_ok: 0,
                config_set_error: 0,
                last_config_version: 0,
                current_state: "UNKNOWN".to_string(),
                last_error_code: Some("metrics_lock_poisoned".to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_record_ok_and_error_and_snapshot() {
        let metrics = IoControlPlaneMetrics::with_initial_state("UNCONFIGURED", 0);
        let first = metrics.snapshot();
        assert_eq!(first.config_set_ok, 0);
        assert_eq!(first.config_set_error, 0);
        assert_eq!(first.last_config_version, 0);
        assert_eq!(first.current_state, "UNCONFIGURED");

        metrics.record_config_set_ok("CONFIGURED", 7);
        let second = metrics.snapshot();
        assert_eq!(second.config_set_ok, 1);
        assert_eq!(second.config_set_error, 0);
        assert_eq!(second.last_config_version, 7);
        assert_eq!(second.current_state, "CONFIGURED");
        assert_eq!(second.last_error_code, None);

        metrics.record_config_set_error("FAILED_CONFIG", 8, "invalid_config");
        let third = metrics.snapshot();
        assert_eq!(third.config_set_ok, 1);
        assert_eq!(third.config_set_error, 1);
        assert_eq!(third.last_config_version, 8);
        assert_eq!(third.current_state, "FAILED_CONFIG");
        assert_eq!(third.last_error_code.as_deref(), Some("invalid_config"));
    }
}
