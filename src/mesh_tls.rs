//! Mesh mTLS for the router WAN (peer authentication across the insecure WAN
//! boundary). A motherbee-rooted **dedicated CA** issues per-hive leaf certs
//! (CN + SAN = hive_id); the WAN handshake is mutual TLS verifying the peer cert
//! is CA-signed, and the application binds the TLS identity (cert hive_id) to the
//! hive_id claimed in the WAN HELLO. The CA key is NOT derived from the SSH key
//! (isolation). See `docs/onworking COA/wan-mtls-peer-auth.md`.

use std::io::BufReader;
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};

const CA_COMMON_NAME: &str = "fluxbee-mesh-ca";

#[derive(Debug)]
pub enum MeshTlsError {
    Rcgen(rcgen::Error),
    Rustls(rustls::Error),
    Verifier(String),
    Io(std::io::Error),
    NoKey,
}

impl std::fmt::Display for MeshTlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeshTlsError::Rcgen(e) => write!(f, "rcgen: {e}"),
            MeshTlsError::Rustls(e) => write!(f, "rustls: {e}"),
            MeshTlsError::Verifier(e) => write!(f, "verifier: {e}"),
            MeshTlsError::Io(e) => write!(f, "io: {e}"),
            MeshTlsError::NoKey => write!(f, "no private key in PEM"),
        }
    }
}
impl std::error::Error for MeshTlsError {}
impl From<rcgen::Error> for MeshTlsError {
    fn from(e: rcgen::Error) -> Self {
        MeshTlsError::Rcgen(e)
    }
}
impl From<rustls::Error> for MeshTlsError {
    fn from(e: rustls::Error) -> Self {
        MeshTlsError::Rustls(e)
    }
}
impl From<std::io::Error> for MeshTlsError {
    fn from(e: std::io::Error) -> Self {
        MeshTlsError::Io(e)
    }
}

/// Install the `ring` crypto provider as the process default (idempotent). Call
/// once at startup before building any rustls config.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// PEM bundle of a leaf certificate + its private key.
pub struct LeafBundle {
    pub cert_pem: String,
    pub key_pem: String,
}

/// The mesh CA (motherbee only): the CA key + cert used to issue per-hive leaf
/// certs. Generated once, persisted to disk, reloaded on restart.
pub struct MeshCa {
    ca_key: KeyPair,
    ca_cert: rcgen::Certificate,
    ca_cert_pem: String,
    ca_key_pem: String,
}

impl MeshCa {
    /// Generate a fresh dedicated mesh CA (self-signed, long-lived).
    pub fn generate() -> Result<MeshCa, MeshTlsError> {
        let ca_key = KeyPair::generate()?;
        let mut params = CertificateParams::new(Vec::<String>::new())?;
        params
            .distinguished_name
            .push(DnType::CommonName, CA_COMMON_NAME);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let ca_cert = params.self_signed(&ca_key)?;
        let ca_cert_pem = ca_cert.pem();
        let ca_key_pem = ca_key.serialize_pem();
        Ok(MeshCa {
            ca_key,
            ca_cert,
            ca_cert_pem,
            ca_key_pem,
        })
    }

    /// Reload a CA from its persisted PEM (cert + key) so it can keep issuing
    /// leaves that chain to the same (already-distributed) CA cert.
    pub fn from_pem(ca_cert_pem: &str, ca_key_pem: &str) -> Result<MeshCa, MeshTlsError> {
        let ca_key = KeyPair::from_pem(ca_key_pem)?;
        let params = CertificateParams::from_ca_cert_pem(ca_cert_pem)?;
        let ca_cert = params.self_signed(&ca_key)?;
        Ok(MeshCa {
            ca_key,
            ca_cert,
            ca_cert_pem: ca_cert_pem.to_string(),
            ca_key_pem: ca_key_pem.to_string(),
        })
    }

    pub fn ca_cert_pem(&self) -> &str {
        &self.ca_cert_pem
    }
    pub fn ca_key_pem(&self) -> &str {
        &self.ca_key_pem
    }

    /// Issue a per-hive leaf cert (CN + SAN DNS = hive_id), signed by the CA. The
    /// SAN DNS name lets a TLS client verify the server by passing the hive_id as
    /// the server name; the CN is the fallback identity the server extracts to
    /// bind the client cert to its claimed hive_id.
    pub fn issue_leaf(&self, hive_id: &str) -> Result<LeafBundle, MeshTlsError> {
        let leaf_key = KeyPair::generate()?;
        let mut params = CertificateParams::new(vec![hive_id.to_string()])?;
        params.distinguished_name.push(DnType::CommonName, hive_id);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let leaf = params.signed_by(&leaf_key, &self.ca_cert, &self.ca_key)?;
        Ok(LeafBundle {
            cert_pem: leaf.pem(),
            key_pem: leaf_key.serialize_pem(),
        })
    }
}

/// A hive's loaded TLS material: its own leaf (to present) + the mesh CA roots
/// (to verify peers). Built from PEM on disk; produces rustls server/client
/// configs for the WAN transport.
pub struct MeshTlsMaterial {
    leaf_chain: Vec<CertificateDer<'static>>,
    leaf_key: PrivateKeyDer<'static>,
    ca_roots: Arc<RootCertStore>,
}

impl MeshTlsMaterial {
    pub fn from_pem(
        leaf_cert_pem: &str,
        leaf_key_pem: &str,
        ca_cert_pem: &str,
    ) -> Result<Self, MeshTlsError> {
        let leaf_chain = certs_from_pem(leaf_cert_pem)?;
        let leaf_key = key_from_pem(leaf_key_pem)?;
        let mut roots = RootCertStore::empty();
        for ca in certs_from_pem(ca_cert_pem)? {
            roots.add(ca)?;
        }
        Ok(Self {
            leaf_chain,
            leaf_key,
            ca_roots: Arc::new(roots),
        })
    }

    /// Server config: present our leaf, REQUIRE a CA-signed client cert. The
    /// caller still binds the verified client identity to the HELLO hive_id via
    /// `peer_hive_from_cert`.
    pub fn server_config(&self) -> Result<Arc<ServerConfig>, MeshTlsError> {
        let verifier = rustls::server::WebPkiClientVerifier::builder(self.ca_roots.clone())
            .build()
            .map_err(|e| MeshTlsError::Verifier(e.to_string()))?;
        let cfg = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(self.leaf_chain.clone(), self.leaf_key.clone_key())?;
        Ok(Arc::new(cfg))
    }

    /// Client config: present our leaf, verify the server cert against the CA.
    pub fn client_config(&self) -> Result<Arc<ClientConfig>, MeshTlsError> {
        let cfg = ClientConfig::builder()
            .with_root_certificates((*self.ca_roots).clone())
            .with_client_auth_cert(self.leaf_chain.clone(), self.leaf_key.clone_key())?;
        Ok(Arc::new(cfg))
    }
}

/// Extract the hive_id from a peer's leaf cert: prefer the SAN DNS name, fall
/// back to the CN. Used (server side) to verify the cryptographically
/// authenticated identity matches the hive_id claimed in the WAN HELLO, so a
/// valid mesh member cannot impersonate another hive in the protocol layer.
pub fn peer_hive_from_cert(cert: &CertificateDer<'_>) -> Option<String> {
    use x509_parser::prelude::*;
    let (_, parsed) = X509Certificate::from_der(cert.as_ref()).ok()?;
    if let Ok(Some(san)) = parsed.subject_alternative_name() {
        for name in &san.value.general_names {
            if let GeneralName::DNSName(dns) = name {
                return Some(dns.to_string());
            }
        }
    }
    for cn in parsed.subject().iter_common_name() {
        if let Ok(s) = cn.as_str() {
            return Some(s.to_string());
        }
    }
    None
}

fn certs_from_pem(pem: &str) -> Result<Vec<CertificateDer<'static>>, MeshTlsError> {
    let mut reader = BufReader::new(pem.as_bytes());
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(MeshTlsError::Io)
}

fn key_from_pem(pem: &str) -> Result<PrivateKeyDer<'static>, MeshTlsError> {
    let mut reader = BufReader::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)?.ok_or(MeshTlsError::NoKey)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_issues_leaf_with_hive_identity_and_builds_configs() {
        install_crypto_provider();
        let ca = MeshCa::generate().unwrap();
        let leaf = ca.issue_leaf("worker1").unwrap();

        let mat = MeshTlsMaterial::from_pem(&leaf.cert_pem, &leaf.key_pem, ca.ca_cert_pem()).unwrap();
        assert!(mat.server_config().is_ok());
        assert!(mat.client_config().is_ok());

        let certs = certs_from_pem(&leaf.cert_pem).unwrap();
        assert_eq!(peer_hive_from_cert(&certs[0]).as_deref(), Some("worker1"));
    }

    #[test]
    fn reloaded_ca_keeps_issuing_chainable_leaves() {
        install_crypto_provider();
        let ca = MeshCa::generate().unwrap();
        // Reload from persisted PEM, then issue — must still bind to the hive and
        // load against the ORIGINAL ca cert (same key => same chain).
        let reloaded = MeshCa::from_pem(ca.ca_cert_pem(), ca.ca_key_pem()).unwrap();
        let leaf = reloaded.issue_leaf("egress1").unwrap();
        let mat = MeshTlsMaterial::from_pem(&leaf.cert_pem, &leaf.key_pem, ca.ca_cert_pem()).unwrap();
        assert!(mat.client_config().is_ok());
        let certs = certs_from_pem(&leaf.cert_pem).unwrap();
        assert_eq!(peer_hive_from_cert(&certs[0]).as_deref(), Some("egress1"));
    }

    #[test]
    fn distinct_hives_get_distinct_identities() {
        install_crypto_provider();
        let ca = MeshCa::generate().unwrap();
        let a = certs_from_pem(&ca.issue_leaf("motherbee").unwrap().cert_pem).unwrap();
        let b = certs_from_pem(&ca.issue_leaf("worker7").unwrap().cert_pem).unwrap();
        assert_eq!(peer_hive_from_cert(&a[0]).as_deref(), Some("motherbee"));
        assert_eq!(peer_hive_from_cert(&b[0]).as_deref(), Some("worker7"));
        assert_ne!(a[0], b[0]);
    }
}
