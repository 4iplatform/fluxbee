//! Per-hive HMAC peer-auth for the SY.identity sync channel (TCP :9100). Unlike
//! the router WAN (which crosses the insecure boundary and uses mTLS, see
//! `mesh_tls`), identity sync is an internal channel, so it gets a lighter
//! symmetric mechanism: a per-hive shared key `K_hive` (the motherbee holds one
//! per replica; each replica holds its own), dedicated (not derived from the
//! mesh CA / SSH key) and distributed at `add_hive` over the authenticated SSH
//! channel.
//!
//! This module provides the key type and the HMAC proof primitives; the wire
//! choreography lives in sy_identity. The connection is authenticated with a
//! **mutual challenge-response handshake** at connect time (not per-frame):
//! each side proves it holds `K_hive` by HMAC'ing the *other* side's fresh
//! random nonce, bound to the hive_id and a direction context. A peer that does
//! not hold the key cannot answer a fresh challenge, so it cannot open a
//! connection under a hive_id it does not own. Fresh nonces make the exchange
//! replay-proof; the direction contexts prevent reflecting one side's proof as
//! the other's.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

type HmacSha256 = Hmac<Sha256>;

/// HMAC key length in bytes (256-bit).
pub const KEY_LEN: usize = 32;
/// Standard on-disk location for per-hive identity HMAC keys (one file per hive;
/// a replica holds its own, the motherbee holds every replica's).
pub const KEYS_DIR: &str = "/var/lib/fluxbee/identity/keys";

/// Whether `hive_id` is a safe token to use in a key filename — alphanumerics,
/// `_` and `-` only, 1..=64 chars. Rejects anything that could traverse out of
/// [`KEYS_DIR`] (`/`, `\`, `.`, `..`, absolute paths) when joined into a path.
/// Callers MUST validate untrusted hive_ids (e.g. a peer's claimed hive in the
/// handshake) before [`key_path`].
pub fn is_valid_hive_id(hive_id: &str) -> bool {
    !hive_id.is_empty()
        && hive_id.len() <= 64
        && hive_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Path to a hive's HMAC key file under [`KEYS_DIR`]. Validate `hive_id` with
/// [`is_valid_hive_id`] first when it comes from an untrusted source.
pub fn key_path(hive_id: &str) -> PathBuf {
    Path::new(KEYS_DIR).join(format!("{hive_id}.key"))
}
/// Direction contexts (domain separation): the value a side HMACs depends on
/// who it is, so a captured client proof can't be replayed as a server proof.
pub const CLIENT_CONTEXT: &str = "fluxbee-identity-handshake-client-v1";
pub const SERVER_CONTEXT: &str = "fluxbee-identity-handshake-server-v1";

#[derive(Debug)]
pub enum MeshHmacError {
    Io(std::io::Error),
    KeyFormat(String),
    MacMismatch,
}

impl std::fmt::Display for MeshHmacError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeshHmacError::Io(e) => write!(f, "io error: {e}"),
            MeshHmacError::KeyFormat(s) => write!(f, "key format: {s}"),
            MeshHmacError::MacMismatch => write!(f, "hmac verification failed"),
        }
    }
}

impl std::error::Error for MeshHmacError {}

impl From<std::io::Error> for MeshHmacError {
    fn from(e: std::io::Error) -> Self {
        MeshHmacError::Io(e)
    }
}

/// A per-hive 256-bit HMAC key.
#[derive(Clone)]
pub struct MeshHmacKey([u8; KEY_LEN]);

impl MeshHmacKey {
    /// Generate a fresh random key from the thread CSPRNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; KEY_LEN];
        rand::thread_rng().fill_bytes(&mut bytes);
        MeshHmacKey(bytes)
    }

    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        MeshHmacKey(bytes)
    }

    pub fn from_hex(s: &str) -> Result<Self, MeshHmacError> {
        let raw =
            hex::decode(s.trim()).map_err(|e| MeshHmacError::KeyFormat(format!("not hex: {e}")))?;
        let bytes: [u8; KEY_LEN] = raw
            .try_into()
            .map_err(|_| MeshHmacError::KeyFormat(format!("expected {KEY_LEN} bytes")))?;
        Ok(MeshHmacKey(bytes))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Load a key from `path` (hex, one line).
    pub fn load_from_file(path: &Path) -> Result<Self, MeshHmacError> {
        Self::from_hex(&fs::read_to_string(path)?)
    }

    /// Write the key to `path` with 0600 perms (owner-only), creating the parent
    /// dir 0700 if needed.
    pub fn write_to_file(&self, path: &Path) -> Result<(), MeshHmacError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(self.to_hex().as_bytes())?;
        f.write_all(b"\n")?;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        Ok(())
    }
}

/// A fresh 128-bit random nonce (hex), used as the per-handshake challenge.
pub fn random_nonce() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// Length-prefixed concatenation so distinct field splits never collide.
fn proof_input(context: &str, nonce: &str, hive: &str) -> Vec<u8> {
    let parts: [&[u8]; 3] = [context.as_bytes(), nonce.as_bytes(), hive.as_bytes()];
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.len() + 8).sum());
    for p in parts {
        out.extend_from_slice(&(p.len() as u64).to_le_bytes());
        out.extend_from_slice(p);
    }
    out
}

/// Compute the proof a side sends: `HMAC-SHA256(K, context || peer_nonce || hive)`,
/// hex-encoded. The signer proves it holds `K_hive` by answering the peer's
/// fresh `nonce`; `context` is CLIENT_CONTEXT or SERVER_CONTEXT.
pub fn prove(key: &MeshHmacKey, context: &str, nonce: &str, hive: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(&key.0).expect("HMAC accepts any key length");
    mac.update(&proof_input(context, nonce, hive));
    hex::encode(mac.finalize().into_bytes())
}

/// Constant-time verify a peer's proof. Returns Ok(()) iff `mac_hex` is the
/// expected `prove(key, context, nonce, hive)`.
pub fn verify_proof(
    key: &MeshHmacKey,
    context: &str,
    nonce: &str,
    hive: &str,
    mac_hex: &str,
) -> Result<(), MeshHmacError> {
    let want = hex::decode(mac_hex.trim()).map_err(|_| MeshHmacError::MacMismatch)?;
    let mut mac = HmacSha256::new_from_slice(&key.0).expect("HMAC accepts any key length");
    mac.update(&proof_input(context, nonce, hive));
    mac.verify_slice(&want)
        .map_err(|_| MeshHmacError::MacMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_hex_roundtrip() {
        let k = MeshHmacKey::generate();
        let k2 = MeshHmacKey::from_hex(&k.to_hex()).unwrap();
        assert_eq!(k.to_hex(), k2.to_hex());
        assert_eq!(k.to_hex().len(), KEY_LEN * 2);
    }

    #[test]
    fn proof_roundtrip_ok() {
        let k = MeshHmacKey::generate();
        let nonce = random_nonce();
        let mac = prove(&k, CLIENT_CONTEXT, &nonce, "worker1");
        assert!(verify_proof(&k, CLIENT_CONTEXT, &nonce, "worker1", &mac).is_ok());
    }

    #[test]
    fn wrong_key_rejected() {
        let k1 = MeshHmacKey::generate();
        let k2 = MeshHmacKey::generate();
        let nonce = random_nonce();
        let mac = prove(&k1, CLIENT_CONTEXT, &nonce, "worker1");
        assert!(matches!(
            verify_proof(&k2, CLIENT_CONTEXT, &nonce, "worker1", &mac),
            Err(MeshHmacError::MacMismatch)
        ));
    }

    #[test]
    fn reflected_context_rejected() {
        // A client proof must not verify as a server proof (reflection guard).
        let k = MeshHmacKey::generate();
        let nonce = random_nonce();
        let client_mac = prove(&k, CLIENT_CONTEXT, &nonce, "worker1");
        assert!(verify_proof(&k, SERVER_CONTEXT, &nonce, "worker1", &client_mac).is_err());
    }

    #[test]
    fn wrong_nonce_rejected() {
        let k = MeshHmacKey::generate();
        let mac = prove(&k, CLIENT_CONTEXT, &random_nonce(), "worker1");
        assert!(verify_proof(&k, CLIENT_CONTEXT, &random_nonce(), "worker1", &mac).is_err());
    }

    #[test]
    fn wrong_hive_rejected() {
        let k = MeshHmacKey::generate();
        let nonce = random_nonce();
        let mac = prove(&k, CLIENT_CONTEXT, &nonce, "worker1");
        assert!(verify_proof(&k, CLIENT_CONTEXT, &nonce, "motherbee", &mac).is_err());
    }

    #[test]
    fn hive_id_validation_blocks_traversal() {
        assert!(is_valid_hive_id("worker1"));
        assert!(is_valid_hive_id("mother-bee_2"));
        assert!(!is_valid_hive_id(""));
        assert!(!is_valid_hive_id("/etc/passwd"));
        assert!(!is_valid_hive_id("../../x"));
        assert!(!is_valid_hive_id("a.b"));
        assert!(!is_valid_hive_id("a/b"));
        assert!(!is_valid_hive_id(&"x".repeat(65)));
    }
}
