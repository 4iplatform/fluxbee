//! Mesh mTLS: peer authentication between hives across the insecure WAN boundary. A
//! motherbee-rooted **dedicated CA** issues per-hive leaf certs (CN + SAN = hive_id). Two uses:
//! - **the router's WAN:** mutual TLS verifying the peer cert is CA-signed, and the application
//!   binds the TLS identity (cert hive_id) to the hive_id claimed in the WAN HELLO
//!   (`server_config`, `client_config`);
//! - **io.web:** the edge verifies io.web's cert by name (`client_config_by_name`), and io.web
//!   admits only a hive the registry gives an admitted role (`server_config_for_roles`), at every
//!   handshake: neither side resumes a session.
//!
//! The CA key is NOT derived from the SSH key (isolation). See
//! `docs/history/onworking-coa/wan-mtls-peer-auth.md`.

use std::io::BufReader;
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig, RootCertStore, ServerConfig};

use super::registry::{HiveRegistry, HiveRole};

/// Where each hive keeps its material, `<dir>/<hive_id>/{ca.crt,cert.crt,cert.key}`, as add_hive
/// distributes it (`MeshTlsMaterial::load_from_dir`). On the motherbee, `ca/` holds the CA.
pub const MESH_TLS_DIR: &str = "/var/lib/fluxbee/tls";

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
    /// The CA's subject and key, as read back from its certificate: a leaf's
    /// issuer name is the CA certificate's subject, so the leaf chains to it.
    issuer: Issuer<'static, KeyPair>,
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
        let ca_cert_pem = params.self_signed(&ca_key)?.pem();
        // Issue through the reload path, so a fresh CA and a persisted one issue alike.
        Self::from_pem(&ca_cert_pem, &ca_key.serialize_pem())
    }

    /// Reload a CA from its persisted PEM (cert + key) so it can keep issuing
    /// leaves that chain to the same (already-distributed) CA cert.
    pub fn from_pem(ca_cert_pem: &str, ca_key_pem: &str) -> Result<MeshCa, MeshTlsError> {
        let ca_key = KeyPair::from_pem(ca_key_pem)?;
        let issuer = Issuer::from_ca_cert_pem(ca_cert_pem, ca_key)?;
        Ok(MeshCa {
            issuer,
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
        let leaf = params.signed_by(&leaf_key, &self.issuer)?;
        Ok(LeafBundle {
            cert_pem: leaf.pem(),
            key_pem: leaf_key.serialize_pem(),
        })
    }
}

/// A hive's loaded TLS material: its own leaf (to present) + the mesh CA roots
/// (to verify peers). Built from PEM on disk; produces rustls server/client
/// configs for the WAN transport and for io.web.
pub struct MeshTlsMaterial {
    leaf_chain: Vec<CertificateDer<'static>>,
    leaf_key: PrivateKeyDer<'static>,
    ca_roots: Arc<RootCertStore>,
}

impl MeshTlsMaterial {
    /// Load a hive's TLS material from a directory holding `ca.crt`, `cert.crt`,
    /// and `cert.key` (what `add_hive` distributes to `<MESH_TLS_DIR>/<hive>`).
    pub fn load_from_dir(dir: &std::path::Path) -> Result<Self, MeshTlsError> {
        let ca = std::fs::read_to_string(dir.join("ca.crt"))?;
        let cert = std::fs::read_to_string(dir.join("cert.crt"))?;
        let key = std::fs::read_to_string(dir.join("cert.key"))?;
        Self::from_pem(&cert, &key, &ca)
    }

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

    /// Server config for a mesh service only some hives may call (io.web admits the ingress):
    /// present our leaf, and REQUIRE a client cert that chains to the mesh CA and whose hive
    /// (`peer_hive_from_cert`) `registry` gives one of `roles`, read at each handshake with the
    /// strict reading (`HiveRegistry::role`). Any other client fails inside the handshake, so the
    /// service never sees its connection.
    ///
    /// The role check must run on every handshake, and rustls calls the client verifier only on a
    /// full one: a resumed session restores the client's certificate without it, so a hive the
    /// registry removed or demoted would stay admitted until a restart. The server therefore keeps
    /// no session and sends no TLS 1.3 ticket (nor a TLS 1.2 one, rustls' default), and every
    /// handshake is a full one, whatever the client offers.
    pub fn server_config_for_roles(
        &self,
        registry: HiveRegistry,
        roles: &[HiveRole],
    ) -> Result<Arc<ServerConfig>, MeshTlsError> {
        let ca = rustls::server::WebPkiClientVerifier::builder(self.ca_roots.clone())
            .build()
            .map_err(|e| MeshTlsError::Verifier(e.to_string()))?;
        let verifier = Arc::new(RoleClientVerifier {
            ca,
            registry,
            roles: roles.to_vec(),
        });
        let mut cfg = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(self.leaf_chain.clone(), self.leaf_key.clone_key())?;
        cfg.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        cfg.send_tls13_tickets = 0;
        Ok(Arc::new(cfg))
    }

    /// Client config: present our leaf, verify the server cert chains to the
    /// mesh CA — but DO NOT check the server name (the WAN client doesn't know
    /// the uplink's hive_id ahead of the handshake). Identity is bound at the
    /// application layer: after the handshake, the caller compares the server's
    /// cert hive_id (`peer_hive_from_cert`) to the hive_id it claims in the HELLO.
    pub fn client_config(&self) -> Result<Arc<ClientConfig>, MeshTlsError> {
        let provider = rustls::crypto::ring::default_provider();
        let verifier = Arc::new(CaOnlyServerVerifier {
            roots: self.ca_roots.clone(),
            supported: provider.signature_verification_algorithms,
        });
        let cfg = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(self.leaf_chain.clone(), self.leaf_key.clone_key())?;
        Ok(Arc::new(cfg))
    }

    /// Client config for a mesh service at a known hive (the edge dialling io.web): present our
    /// leaf, and verify the server cert with rustls' WebPki verifier over the mesh CA, its name
    /// included. The caller connects with the server's hive id as the `ServerName`
    /// (`IO_WEB_SERVER_NAME` for io.web), which a mesh leaf carries as its SAN; any other hive's
    /// leaf fails the handshake. It never resumes a session: the service checks its client's role
    /// only on a full handshake (`server_config_for_roles`).
    pub fn client_config_by_name(&self) -> Result<Arc<ClientConfig>, MeshTlsError> {
        let mut cfg = ClientConfig::builder()
            .with_root_certificates(self.ca_roots.clone())
            .with_client_auth_cert(self.leaf_chain.clone(), self.leaf_key.clone_key())?;
        cfg.resumption = rustls::client::Resumption::disabled();
        Ok(Arc::new(cfg))
    }
}

/// Client-side verifier that requires the server cert to chain to the mesh CA
/// but skips the server-name (SAN) check. "Dangerous" only in that it omits the
/// hostname check that a public-PKI TLS client would do — here the identity is
/// the cert's CA-signed hive_id, bound to the HELLO at the app layer, not a DNS
/// hostname. The chain + signature verification (the part that proves "issued by
/// our CA") is fully enforced via webpki.
#[derive(Debug)]
struct CaOnlyServerVerifier {
    roots: Arc<RootCertStore>,
    supported: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl rustls::client::danger::ServerCertVerifier for CaOnlyServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let cert = rustls::server::ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_cert_signed_by_trust_anchor(
            &cert,
            &self.roots,
            intermediates,
            now,
            self.supported.all,
        )?;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.supported)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.supported)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.supported.supported_schemes()
    }
}

/// Server-side verifier of `server_config_for_roles`: the mesh CA's webpki client verifier
/// first, then the client's hive must hold one of `roles` in the registry, read now. Its refusal
/// is `ApplicationVerificationFailure`, which rustls sends as the fatal alert `access_denied`: in
/// TLS a valid certificate that access control turned away.
#[derive(Debug)]
struct RoleClientVerifier {
    ca: Arc<dyn ClientCertVerifier>,
    registry: HiveRegistry,
    roles: Vec<HiveRole>,
}

impl ClientCertVerifier for RoleClientVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        self.ca.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: rustls::pki_types::UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        let verified = self.ca.verify_client_cert(end_entity, intermediates, now)?;
        let hive = peer_hive_from_cert(end_entity);
        let role = hive.as_deref().and_then(|hive| self.registry.role(hive));
        if role.is_some_and(|role| self.roles.contains(&role)) {
            return Ok(verified);
        }
        // The caller sees only rustls' error, so the hive and what the registry says are logged
        // here.
        tracing::warn!(
            hive = hive.as_deref().unwrap_or_default(),
            role = ?role,
            admitted = ?self.roles,
            "mesh client refused in the handshake: the hive registry gives its hive no admitted role"
        );
        Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::ApplicationVerificationFailure,
        ))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.ca.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.ca.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.ca.supported_verify_schemes()
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
    use crate::mesh::{test_registry, IO_WEB_SERVER_NAME};
    use rustls::{AlertDescription, CertificateError};

    /// A mesh CA and a worker1 leaf made by the 0.1.82 code (rcgen 0.13), as a motherbee and a
    /// spoke hold them before their update.
    const FIXTURE_CA_CRT: &str = include_str!("../../tests/fixtures/mesh-ca-0.1.82/ca.crt");
    const FIXTURE_CA_KEY: &str = include_str!("../../tests/fixtures/mesh-ca-0.1.82/ca.key");
    const FIXTURE_WORKER1_CRT: &str =
        include_str!("../../tests/fixtures/mesh-ca-0.1.82/worker1.crt");
    const FIXTURE_WORKER1_KEY: &str =
        include_str!("../../tests/fixtures/mesh-ca-0.1.82/worker1.key");

    /// `hive`'s material: a leaf `ca` issues, with `ca`'s certificate as the roots.
    fn material(ca: &MeshCa, hive: &str) -> MeshTlsMaterial {
        let leaf = ca.issue_leaf(hive).unwrap();
        MeshTlsMaterial::from_pem(&leaf.cert_pem, &leaf.key_pem, ca.ca_cert_pem()).unwrap()
    }

    /// The rustls error a TLS stream's I/O error carries.
    fn rustls_error(err: &std::io::Error) -> Option<&rustls::Error> {
        err.get_ref()?.downcast_ref::<rustls::Error>()
    }

    /// The hive of the first certificate a peer presented.
    fn peer_hive(certs: Option<&[CertificateDer<'_>]>) -> std::io::Result<String> {
        certs
            .and_then(|certs| certs.first())
            .and_then(peer_hive_from_cert)
            .ok_or_else(|| std::io::Error::other("the peer presented no hive"))
    }

    /// One handshake over `server` and `client`, the client naming the server `name`, then a byte
    /// from the server: the hive each side saw in the other's certificate, or the error it ended
    /// with. A refusal inside the handshake fails the server's accept, and the client's handshake
    /// or its read of that byte.
    async fn handshake(
        server: Arc<ServerConfig>,
        client: Arc<ClientConfig>,
        name: &'static str,
    ) -> (std::io::Result<String>, std::io::Result<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_rustls::{TlsAcceptor, TlsConnector};
        let (client_io, server_io) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            let mut tls = TlsAcceptor::from(server).accept(server_io).await?;
            let hive = peer_hive(tls.get_ref().1.peer_certificates())?;
            tls.write_all(b"y").await?;
            tls.flush().await?;
            Ok::<_, std::io::Error>(hive)
        });
        let client = tokio::spawn(async move {
            let name = rustls::pki_types::ServerName::try_from(name).unwrap();
            let mut tls = TlsConnector::from(client).connect(name, client_io).await?;
            let hive = peer_hive(tls.get_ref().1.peer_certificates())?;
            let mut buf = [0u8; 1];
            tls.read_exact(&mut buf).await?;
            Ok::<_, std::io::Error>(hive)
        });
        (server.await.unwrap(), client.await.unwrap())
    }

    /// One mutual-TLS handshake over the WAN configs, then a byte: (the hive the server saw, the
    /// hive the client saw).
    async fn mtls_round_trip(
        server: &MeshTlsMaterial,
        client: &MeshTlsMaterial,
    ) -> (String, String) {
        // CA-only verifier ignores the server name; any valid DNS name works.
        let (server_saw, client_saw) = handshake(
            server.server_config().unwrap(),
            client.client_config().unwrap(),
            "mesh",
        )
        .await;
        (
            server_saw.expect("server handshake"),
            client_saw.expect("client handshake"),
        )
    }

    /// The update's gate: the updated motherbee reloads the CA the 0.1.82 code made and issues its
    /// leaf with the new code, and a spoke still on 0.1.82 reconnects. The new leaf carries the
    /// extensions the old code wrote, since such a spoke parses it with x509-parser 0.16, and it
    /// verifies against the distributed `ca.crt` both ways: as a server (the client's CA-only
    /// verifier) and as a client (the server's webpki client verifier).
    #[tokio::test]
    async fn a_ca_made_by_the_previous_code_issues_leaves_its_spokes_accept() {
        install_crypto_provider();
        let ca = MeshCa::from_pem(FIXTURE_CA_CRT, FIXTURE_CA_KEY).expect("loads the 0.1.82 CA");
        assert_eq!(
            ca.ca_cert_pem(),
            FIXTURE_CA_CRT,
            "the distributed CA certificate is never reissued"
        );
        let leaf = ca.issue_leaf("motherbee").expect("issues a leaf");
        let leaf_der = certs_from_pem(&leaf.cert_pem).unwrap();
        let old_leaf_der = certs_from_pem(FIXTURE_WORKER1_CRT).unwrap();
        let ca_der = certs_from_pem(FIXTURE_CA_CRT).unwrap();
        {
            use x509_parser::prelude::{FromDer, X509Certificate};
            fn parse<'a>(der: &'a CertificateDer<'_>) -> X509Certificate<'a> {
                X509Certificate::from_der(der.as_ref()).unwrap().1
            }
            let (new_leaf, old_leaf) = (parse(&leaf_der[0]), parse(&old_leaf_der[0]));
            assert_eq!(
                new_leaf.issuer().as_raw(),
                parse(&ca_der[0]).subject().as_raw(),
                "the leaf names its issuer with the CA's subject, byte for byte"
            );

            // Each extension's OID and criticality, as a set.
            let extensions = |cert: &X509Certificate<'_>| {
                let mut set: Vec<(String, bool)> = cert
                    .extensions()
                    .iter()
                    .map(|ext| (ext.oid.to_id_string(), ext.critical))
                    .collect();
                set.sort();
                set
            };
            assert_eq!(
                extensions(&old_leaf),
                [
                    ("2.5.29.15".to_string(), true),  // key usage
                    ("2.5.29.17".to_string(), false), // subject alternative name
                    ("2.5.29.37".to_string(), false), // extended key usage
                ]
            );
            assert_eq!(extensions(&new_leaf), extensions(&old_leaf));
            // The same key usages, not the same bytes: rcgen 0.13 wrote nine bits, which leaves a
            // trailing zero byte that DER forbids, and rcgen 0.14 writes the minimal string.
            // x509-parser 0.16 reads both as the same flags.
            let key_usage =
                |cert: &X509Certificate<'_>| cert.key_usage().unwrap().unwrap().value.flags;
            assert_eq!(key_usage(&new_leaf), key_usage(&old_leaf));
            // The extended key usage, which does not name the hive either, is the same bytes.
            let extended_key_usage = |cert: &X509Certificate<'_>| {
                cert.extensions()
                    .iter()
                    .find(|ext| ext.oid.to_id_string() == "2.5.29.37")
                    .map(|ext| ext.value.to_vec())
            };
            assert_eq!(extended_key_usage(&new_leaf), extended_key_usage(&old_leaf));
        }

        let motherbee =
            MeshTlsMaterial::from_pem(&leaf.cert_pem, &leaf.key_pem, FIXTURE_CA_CRT).unwrap();
        let worker1 =
            MeshTlsMaterial::from_pem(FIXTURE_WORKER1_CRT, FIXTURE_WORKER1_KEY, FIXTURE_CA_CRT)
                .unwrap();
        assert_eq!(
            mtls_round_trip(&motherbee, &worker1).await,
            ("worker1".to_string(), "motherbee".to_string())
        );
        assert_eq!(
            mtls_round_trip(&worker1, &motherbee).await,
            ("motherbee".to_string(), "worker1".to_string())
        );
    }

    #[test]
    fn ca_issues_leaf_with_hive_identity_and_builds_configs() {
        install_crypto_provider();
        let ca = MeshCa::generate().unwrap();
        let leaf = ca.issue_leaf("worker1").unwrap();

        let mat =
            MeshTlsMaterial::from_pem(&leaf.cert_pem, &leaf.key_pem, ca.ca_cert_pem()).unwrap();
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
        let mat =
            MeshTlsMaterial::from_pem(&leaf.cert_pem, &leaf.key_pem, ca.ca_cert_pem()).unwrap();
        assert!(mat.client_config().is_ok());
        let certs = certs_from_pem(&leaf.cert_pem).unwrap();
        assert_eq!(peer_hive_from_cert(&certs[0]).as_deref(), Some("egress1"));
    }

    #[tokio::test]
    async fn mtls_handshake_round_trips_and_exposes_peer_identity() {
        install_crypto_provider();
        let ca = MeshCa::generate().unwrap();
        let (server_saw, client_saw) =
            mtls_round_trip(&material(&ca, "motherbee"), &material(&ca, "worker1")).await;

        // mutual auth succeeded; each side sees the OTHER's hive identity.
        assert_eq!(server_saw, "worker1", "server sees the client hive");
        assert_eq!(client_saw, "motherbee", "client sees the server hive");
    }

    #[tokio::test]
    async fn mtls_handshake_rejects_cert_from_a_foreign_ca() {
        install_crypto_provider();
        let mesh_ca = MeshCa::generate().unwrap();
        let attacker_ca = MeshCa::generate().unwrap(); // different CA
        let mb = mesh_ca.issue_leaf("motherbee").unwrap();
        // Attacker presents a cert signed by a CA the mesh does NOT trust.
        let forged = attacker_ca.issue_leaf("worker1").unwrap();
        let server_mat =
            MeshTlsMaterial::from_pem(&mb.cert_pem, &mb.key_pem, mesh_ca.ca_cert_pem()).unwrap();
        let client_mat =
            MeshTlsMaterial::from_pem(&forged.cert_pem, &forged.key_pem, mesh_ca.ca_cert_pem())
                .unwrap();
        let (server_saw, _) = handshake(
            server_mat.server_config().unwrap(),
            client_mat.client_config().unwrap(),
            "mesh",
        )
        .await;
        // The server must reject the attacker's foreign-CA client cert.
        assert!(
            server_saw.is_err(),
            "foreign-CA client cert must be rejected"
        );
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

    /// The edge dials io.web by name: the motherbee's leaf passes under `motherbee`, and another
    /// hive's leaf from the same CA fails the client's handshake on the name.
    #[tokio::test]
    async fn the_named_client_accepts_only_the_hive_it_names() {
        install_crypto_provider();
        let ca = MeshCa::generate().unwrap();
        let client = material(&ca, "ingress1").client_config_by_name().unwrap();

        let motherbee = material(&ca, "motherbee").server_config().unwrap();
        let (server_saw, client_saw) =
            handshake(motherbee, Arc::clone(&client), IO_WEB_SERVER_NAME).await;
        assert_eq!(
            server_saw.expect("the server admits the client"),
            "ingress1"
        );
        assert_eq!(client_saw.expect("the name matches"), "motherbee");

        let worker1 = material(&ca, "worker1").server_config().unwrap();
        let (_, client_saw) = handshake(worker1, client, IO_WEB_SERVER_NAME).await;
        let err = client_saw.expect_err("worker1's leaf does not carry the name motherbee");
        assert!(
            matches!(
                rustls_error(&err),
                Some(rustls::Error::InvalidCertificate(
                    CertificateError::NotValidForName
                        | CertificateError::NotValidForNameContext { .. }
                ))
            ),
            "{err}"
        );
    }

    /// io.web admits a connected ingress, by the registry read at each handshake. A worker, a hive
    /// not connected, a join not done and a hive the registry does not hold fail the server's
    /// handshake on the registry (`access_denied`), and a certificate from another CA on its chain.
    #[tokio::test]
    async fn the_role_verifier_admits_only_a_hive_with_an_admitted_role() {
        install_crypto_provider();
        let ca = MeshCa::generate().unwrap();
        let registry = test_registry(&[
            (
                "ingress1",
                "hive_id: ingress1\nstatus: connected\nrole: ingress\njoin:\n  phase: done\n",
            ),
            (
                "worker1",
                "hive_id: worker1\nstatus: connected\nrole: worker\njoin:\n  phase: done\n",
            ),
            (
                "ingress2",
                "hive_id: ingress2\nstatus: failed\nrole: ingress\n",
            ),
            (
                "ingress3",
                "hive_id: ingress3\nstatus: connected\nrole: ingress\njoin:\n  phase: harden_ssh\n",
            ),
        ]);
        let server = material(&ca, "motherbee")
            .server_config_for_roles(registry.clone(), &[HiveRole::Ingress])
            .unwrap();
        let dial = |hive: &str| material(&ca, hive).client_config_by_name().unwrap();

        let (server_saw, client_saw) =
            handshake(Arc::clone(&server), dial("ingress1"), IO_WEB_SERVER_NAME).await;
        assert_eq!(server_saw.expect("a connected ingress"), "ingress1");
        assert_eq!(client_saw.expect("served"), "motherbee");

        for hive in ["worker1", "ingress2", "ingress3", "ingress9"] {
            let (server_saw, client_saw) =
                handshake(Arc::clone(&server), dial(hive), IO_WEB_SERVER_NAME).await;
            let err = server_saw.expect_err(hive);
            assert_eq!(
                rustls_error(&err),
                Some(&rustls::Error::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure
                )),
                "{hive}: {err}"
            );
            let err = client_saw.expect_err(hive);
            assert_eq!(
                rustls_error(&err),
                Some(&rustls::Error::AlertReceived(
                    AlertDescription::AccessDenied
                )),
                "{hive}: {err}"
            );
        }

        // A leaf for ingress1 from another CA fails on its chain and never reaches the registry.
        // Which chain error depends on the other CA's subject: this one, made by the same code,
        // names our CA's subject, so the signature fails (BadSignature, not UnknownIssuer).
        let forged = MeshCa::generate().unwrap().issue_leaf("ingress1").unwrap();
        let forged =
            MeshTlsMaterial::from_pem(&forged.cert_pem, &forged.key_pem, ca.ca_cert_pem()).unwrap();
        let (server_saw, _) = handshake(
            Arc::clone(&server),
            forged.client_config_by_name().unwrap(),
            IO_WEB_SERVER_NAME,
        )
        .await;
        let err = server_saw.expect_err("another CA");
        assert!(
            matches!(
                rustls_error(&err),
                Some(rustls::Error::InvalidCertificate(reason))
                    if *reason != CertificateError::ApplicationVerificationFailure
            ),
            "{err}"
        );

        // Read live: once the registry stops calling ingress1 an ingress, its next handshake fails.
        std::fs::write(
            registry.root().join("ingress1").join("info.yaml"),
            "hive_id: ingress1\nstatus: connected\nrole: worker\njoin:\n  phase: done\n",
        )
        .unwrap();
        let (server_saw, _) = handshake(server, dial("ingress1"), IO_WEB_SERVER_NAME).await;
        let err = server_saw.expect_err("ingress1 is no longer an ingress");
        assert_eq!(
            rustls_error(&err),
            Some(&rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure
            )),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// One handshake of `client` with `server` under io.web's name, then a byte from the server,
    /// which brings the client any ticket the server sent: how the client's handshake went.
    async fn client_handshake_kind(
        server: Arc<ServerConfig>,
        client: Arc<ClientConfig>,
    ) -> rustls::HandshakeKind {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_rustls::{TlsAcceptor, TlsConnector};
        let (client_io, server_io) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            let mut tls = TlsAcceptor::from(server).accept(server_io).await?;
            tls.write_all(b"y").await?;
            tls.flush().await?;
            Ok::<_, std::io::Error>(tls)
        });
        let name = rustls::pki_types::ServerName::try_from(IO_WEB_SERVER_NAME).unwrap();
        let mut tls = TlsConnector::from(client)
            .connect(name, client_io)
            .await
            .expect("handshake");
        let mut buf = [0u8; 1];
        tls.read_exact(&mut buf).await.expect("a byte");
        let _server = server.await.unwrap().expect("accept");
        tls.get_ref().1.handshake_kind().expect("a handshake")
    }

    /// A demotion holds at the hive's very next handshake, on a client configuration kept from the
    /// first one, as the edge keeps its connector. A resumed session would restore the client's
    /// certificate without the role check, so io.web's server keeps no session and sends no
    /// ticket, even to a client that offers to resume, and the edge's client never offers.
    #[tokio::test]
    async fn a_demoted_hive_is_refused_at_its_next_handshake_on_the_same_client_config() {
        install_crypto_provider();
        let ca = MeshCa::generate().unwrap();
        let ingress = "hive_id: ingress1\nstatus: connected\nrole: ingress\njoin:\n  phase: done\n";
        let demoted = "hive_id: ingress1\nstatus: connected\nrole: worker\njoin:\n  phase: done\n";
        let registry = test_registry(&[("ingress1", ingress)]);
        let info = registry.root().join("ingress1").join("info.yaml");
        let server = material(&ca, "motherbee")
            .server_config_for_roles(registry.clone(), &[HiveRole::Ingress])
            .unwrap();
        let ingress1 = material(&ca, "ingress1");
        // A client that offers to resume, as rustls' default does.
        let resuming = Arc::new(
            ClientConfig::builder()
                .with_root_certificates(ingress1.ca_roots.clone())
                .with_client_auth_cert(ingress1.leaf_chain.clone(), ingress1.leaf_key.clone_key())
                .unwrap(),
        );
        for (client, label) in [
            (
                ingress1.client_config_by_name().unwrap(),
                "the edge's client",
            ),
            (resuming, "a client that offers to resume"),
        ] {
            std::fs::write(&info, ingress).unwrap();
            let (server_saw, client_saw) =
                handshake(Arc::clone(&server), Arc::clone(&client), IO_WEB_SERVER_NAME).await;
            assert_eq!(server_saw.expect(label), "ingress1", "{label}");
            assert_eq!(client_saw.expect(label), "motherbee", "{label}");

            std::fs::write(&info, demoted).unwrap();
            let (server_saw, client_saw) =
                handshake(Arc::clone(&server), client, IO_WEB_SERVER_NAME).await;
            let err = server_saw.expect_err(label);
            assert_eq!(
                rustls_error(&err),
                Some(&rustls::Error::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure
                )),
                "{label}: {err}"
            );
            let err = client_saw.expect_err(label);
            assert_eq!(
                rustls_error(&err),
                Some(&rustls::Error::AlertReceived(
                    AlertDescription::AccessDenied
                )),
                "{label}: {err}"
            );
        }

        // The edge's client never offers to resume, even to a server that sends tickets (the
        // WAN's); a client that does resumes there.
        let wan = material(&ca, "motherbee").server_config().unwrap();
        let named = ingress1.client_config_by_name().unwrap();
        for _ in 0..2 {
            assert_eq!(
                client_handshake_kind(Arc::clone(&wan), Arc::clone(&named)).await,
                rustls::HandshakeKind::Full
            );
        }
        let resuming = Arc::new(
            ClientConfig::builder()
                .with_root_certificates(ingress1.ca_roots.clone())
                .with_client_auth_cert(ingress1.leaf_chain.clone(), ingress1.leaf_key.clone_key())
                .unwrap(),
        );
        assert_eq!(
            client_handshake_kind(Arc::clone(&wan), Arc::clone(&resuming)).await,
            rustls::HandshakeKind::Full
        );
        assert_eq!(
            client_handshake_kind(wan, resuming).await,
            rustls::HandshakeKind::Resumed,
            "the WAN's server resumes, so the check above is not vacuous"
        );
        let _ = std::fs::remove_dir_all(registry.root());
    }
}
