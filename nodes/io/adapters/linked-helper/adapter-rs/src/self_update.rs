//! OS-agnostic self-update mechanics: resolve → download → verify (mandatory
//! SHA-256) → hand off to the platform seam for the binary swap + restart.
//!
//! Integrity is enforced with SHA-256 from the Cloud directive. Detached
//! signature verification (`target.sig`) is a wired seam only: no signing key
//! is configured yet, so a present signature is logged but not yet enforced
//! (the signing pipeline is a fast-follow). SHA-256 remains mandatory so a
//! tampered or truncated artifact is never swapped in.
//!
//! The OS-specific steps — swapping the running binary and restarting into it —
//! live behind the `platform` seam (`platform::current()`), with a Unix impl
//! and a Windows stub, so Windows plugs in without touching this module.

use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::cloud_client::AdapterUpdateTarget;

const UPDATE_HTTP_TIMEOUT_SECS: u64 = 120;

#[derive(Debug)]
pub enum UpdateError {
    Download(String),
    Integrity(String),
    Swap(String),
    /// Returned by the Windows stub until the swap/restart impl lands (phase 2).
    #[allow(dead_code)]
    Unsupported(String),
}

impl Display for UpdateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Download(m) => write!(f, "update download failed: {}", m),
            UpdateError::Integrity(m) => write!(f, "update integrity check failed: {}", m),
            UpdateError::Swap(m) => write!(f, "update binary swap failed: {}", m),
            UpdateError::Unsupported(m) => write!(f, "update not supported: {}", m),
        }
    }
}

impl std::error::Error for UpdateError {}

impl UpdateError {
    /// Short, stable code for persisting into runtime state (used in tests and
    /// available for diagnostics/logging).
    #[allow(dead_code)]
    pub fn code(&self) -> &'static str {
        match self {
            UpdateError::Download(_) => "update_download_failed",
            UpdateError::Integrity(_) => "update_integrity_failed",
            UpdateError::Swap(_) => "update_swap_failed",
            UpdateError::Unsupported(_) => "update_unsupported",
        }
    }
}

/// Lowercase hex SHA-256 of the given bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// Verifies a downloaded artifact against the Cloud directive: exact size and
/// mandatory SHA-256. A present detached signature is a wired seam (logged, not
/// yet enforced — see module docs).
pub fn verify_artifact(bytes: &[u8], target: &AdapterUpdateTarget) -> Result<(), UpdateError> {
    if bytes.len() as u64 != target.size {
        return Err(UpdateError::Integrity(format!(
            "size mismatch: expected {} bytes, got {}",
            target.size,
            bytes.len()
        )));
    }

    let actual = sha256_hex(bytes);
    if !actual.eq_ignore_ascii_case(target.sha256.trim()) {
        return Err(UpdateError::Integrity(format!(
            "sha256 mismatch: expected {}, got {}",
            target.sha256, actual
        )));
    }

    if target.sig.is_some() {
        eprintln!(
            "self-update: release {} carries a detached signature but signature verification is not enabled yet; relying on sha256",
            target.release_id
        );
    }

    Ok(())
}

/// Resolves the artifact download URL. Absolute URLs are used verbatim; a path
/// (e.g. `/api/adapters/<id>/artifacts/<release>`) is joined to the Cloud base.
pub fn resolve_download_url(cloud_base: &str, target_url: &str) -> String {
    let trimmed = target_url.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }
    let base = cloud_base.trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix('/') {
        format!("{}/{}", base, rest)
    } else {
        format!("{}/{}", base, trimmed)
    }
}

/// Downloads the artifact bytes with the adapter bearer credential.
pub fn download_artifact(url: &str, bearer: &str) -> Result<Vec<u8>, UpdateError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(UPDATE_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| UpdateError::Download(e.to_string()))?;

    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", bearer))
        .send()
        .map_err(|e| UpdateError::Download(e.to_string()))?;

    if !response.status().is_success() {
        return Err(UpdateError::Download(format!(
            "unexpected status {} downloading {}",
            response.status().as_u16(),
            url
        )));
    }

    let bytes = response
        .bytes()
        .map_err(|e| UpdateError::Download(e.to_string()))?;
    Ok(bytes.to_vec())
}

/// Stable sibling path used to retain the previous binary for rollback.
pub fn prev_binary_path(current_exe: &Path) -> PathBuf {
    let file_name = current_exe
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("adapter-rs");
    current_exe.with_file_name(format!("{}.prev", file_name))
}

/// Stable sibling temp path used to stage the downloaded binary before the swap.
pub fn temp_binary_path(current_exe: &Path) -> PathBuf {
    let file_name = current_exe
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("adapter-rs");
    current_exe.with_file_name(format!(".{}.new", file_name))
}

/// Restores the retained previous binary back into place (rollback). Plain
/// rename, OS-agnostic (the previous process image is gone by the time the
/// boot-gate calls this).
pub fn restore_prev(current_exe: &Path, prev: &Path) -> Result<(), UpdateError> {
    fs::rename(prev, current_exe).map_err(|e| UpdateError::Swap(format!("restore prev: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(sha256: &str, size: u64, sig: Option<&str>) -> AdapterUpdateTarget {
        AdapterUpdateTarget {
            release_id: "lh-adapter-0.2.0-linux-x64".to_string(),
            version: "0.2.0".to_string(),
            url: "/api/adapters/a/artifacts/lh-adapter-0.2.0-linux-x64".to_string(),
            sha256: sha256.to_string(),
            size,
            sig: sig.map(str::to_string),
        }
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // sha256("") = e3b0c442...
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verify_artifact_accepts_matching_bytes() {
        let bytes = b"hello world";
        let sha = sha256_hex(bytes);
        assert!(verify_artifact(bytes, &target(&sha, bytes.len() as u64, None)).is_ok());
    }

    #[test]
    fn verify_artifact_accepts_uppercase_and_sig_present() {
        let bytes = b"payload";
        let sha = sha256_hex(bytes).to_uppercase();
        // A present signature must not block when sha256 matches (seam only).
        assert!(verify_artifact(bytes, &target(&sha, bytes.len() as u64, Some("sig"))).is_ok());
    }

    #[test]
    fn verify_artifact_rejects_sha_mismatch() {
        let bytes = b"hello world";
        let err = verify_artifact(bytes, &target(&"0".repeat(64), bytes.len() as u64, None))
            .unwrap_err();
        assert_eq!(err.code(), "update_integrity_failed");
    }

    #[test]
    fn verify_artifact_rejects_size_mismatch() {
        let bytes = b"hello world";
        let sha = sha256_hex(bytes);
        let err = verify_artifact(bytes, &target(&sha, 999, None)).unwrap_err();
        assert_eq!(err.code(), "update_integrity_failed");
    }

    #[test]
    fn resolve_download_url_joins_relative_and_passes_absolute() {
        assert_eq!(
            resolve_download_url("https://cloud.example.com/", "/api/adapters/a/artifacts/r"),
            "https://cloud.example.com/api/adapters/a/artifacts/r"
        );
        assert_eq!(
            resolve_download_url("https://cloud.example.com", "api/adapters/a/artifacts/r"),
            "https://cloud.example.com/api/adapters/a/artifacts/r"
        );
        assert_eq!(
            resolve_download_url("https://cloud.example.com", "https://cdn.example.com/r"),
            "https://cdn.example.com/r"
        );
    }
}
