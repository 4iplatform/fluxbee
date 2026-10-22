//! The checks a certificate chain and its private key pass before SY.edge serves them or SY.admin
//! stores them (PB-2; docs/acme-edge-certificates-v1.md §4). One function for both sides, so the
//! certificate SY.admin installs is judged by the same rules the edge reloads it with.

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

const BEGIN_CERTIFICATE_MARKER: &[u8] = b"-----BEGIN CERTIFICATE-----";

/// What the PEM parse revealed about the certificate chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainDiagnosis {
    /// `-----BEGIN CERTIFICATE-----` markers present in the PEM.
    pub markers: usize,
    /// Certificates the parser actually produced.
    pub parsed: usize,
}

impl ChainDiagnosis {
    /// The PEM held more certificates than the parser returned: some were dropped in silence.
    ///
    /// The real-world cause is a leaf's `END` glued to the next `BEGIN` on one line (CRLF
    /// mangling). openssl reads ZERO certs from such a file; `rustls_pemfile::certs` returns
    /// `Ok` with only the leaf and NO error, because its end-of-section test is satisfied by the
    /// glued line and the intermediate's body is then consumed outside any section. The edge
    /// would serve a leaf-only chain and strict clients fail.
    pub fn truncated(&self) -> bool {
        self.markers != self.parsed
    }

    /// A chain with no intermediates.
    pub fn leaf_only(&self) -> bool {
        self.parsed == 1
    }

    /// One line an operator can act on, or `None` when the chain is healthy.
    pub fn complaint(&self) -> Option<String> {
        if self.truncated() {
            Some(format!(
                "TLS cert PEM is TRUNCATED: {} '-----BEGIN CERTIFICATE-----' markers but only {} \
                 certificate(s) parsed. Almost always an END/BEGIN glued on one line — openssl \
                 reads ZERO certs from such a file while rustls silently keeps just the leaf. \
                 Normalize the PEM (CRLF->LF, one marker per line) and re-put it.",
                self.markers, self.parsed
            ))
        } else if self.leaf_only() {
            Some(
                "TLS chain is a LEAF WITH NO INTERMEDIATES. Browsers may still succeed via AIA \
                 fetch, but strict clients (Go, Python ssl, Meta/Slack webhooks) WILL fail. \
                 Serve leaf + intermediates."
                    .to_string(),
            )
        } else {
            None
        }
    }
}

/// What the leaf says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafInfo {
    /// SHA-256 of the leaf's DER, lowercase hex. The one fingerprint reported anywhere.
    pub fingerprint: String,
    /// Unix seconds.
    pub not_before: i64,
    /// Unix seconds.
    pub not_after: i64,
    /// The DNS names of the SAN extension, lowercase, without a trailing dot.
    pub names: Vec<String>,
    /// The issuer's distinguished name, as text.
    pub issuer: String,
    /// The serial number's raw bytes (for the ARI identifier).
    pub serial: Vec<u8>,
    /// The authority key identifier's key id, when present (for the ARI identifier).
    pub authority_key_id: Option<Vec<u8>>,
    /// Certificates in the chain, the leaf included.
    pub chain_len: usize,
}

impl LeafInfo {
    /// The leaf's whole lifetime in seconds (never negative).
    pub fn lifetime_secs(&self) -> i64 {
        (self.not_after - self.not_before).max(0)
    }
}

/// Why a chain and key are refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterialError {
    NoCertificate,
    Truncated { markers: usize, parsed: usize },
    BadCertificate(String),
    NoKey,
    BadKey(String),
    KeyMismatch,
    Expired { not_after: i64 },
    NotYetValid { not_before: i64 },
}

impl std::fmt::Display for MaterialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCertificate => write!(f, "the PEM holds no certificate"),
            Self::Truncated { markers, parsed } => write!(
                f,
                "the chain is truncated: {markers} BEGIN CERTIFICATE markers, {parsed} parsed"
            ),
            Self::BadCertificate(err) => write!(f, "the leaf does not parse: {err}"),
            Self::NoKey => write!(f, "the PEM holds no private key"),
            Self::BadKey(err) => write!(f, "the private key is not usable: {err}"),
            Self::KeyMismatch => write!(f, "the private key does not match the leaf"),
            Self::Expired { not_after } => write!(f, "the leaf expired at {not_after}"),
            Self::NotYetValid { not_before } => {
                write!(f, "the leaf is valid only from {not_before}")
            }
        }
    }
}

impl std::error::Error for MaterialError {}

/// A chain and key that passed [`inspect`], ready to build a rustls configuration from.
pub struct CheckedMaterial {
    pub certified_key: CertifiedKey,
    pub chain: Vec<CertificateDer<'static>>,
    pub leaf: LeafInfo,
    pub diagnosis: ChainDiagnosis,
}

/// Count the BEGIN CERTIFICATE markers of a PEM.
pub fn count_certificate_markers(pem: &[u8]) -> usize {
    pem.windows(BEGIN_CERTIFICATE_MARKER.len())
        .filter(|window| *window == BEGIN_CERTIFICATE_MARKER)
        .count()
}

/// Parse a PEM chain, counting what the parser kept against the markers present.
pub fn parse_chain(
    cert_pem: &[u8],
) -> Result<(Vec<CertificateDer<'static>>, ChainDiagnosis), MaterialError> {
    let mut reader = cert_pem;
    let chain: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<_, _>>()
        .map_err(|err| MaterialError::BadCertificate(err.to_string()))?;
    let diagnosis = ChainDiagnosis {
        markers: count_certificate_markers(cert_pem),
        parsed: chain.len(),
    };
    Ok((chain, diagnosis))
}

/// Read what a DER leaf says about itself.
pub fn leaf_info(leaf_der: &[u8], chain_len: usize) -> Result<LeafInfo, MaterialError> {
    let (_, cert) = X509Certificate::from_der(leaf_der)
        .map_err(|err| MaterialError::BadCertificate(err.to_string()))?;
    let mut names = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            if let GeneralName::DNSName(dns) = name {
                names.push(normalize_name(dns));
            }
        }
    }
    let authority_key_id =
        cert.tbs_certificate
            .extensions()
            .iter()
            .find_map(|ext| match ext.parsed_extension() {
                x509_parser::extensions::ParsedExtension::AuthorityKeyIdentifier(aki) => {
                    aki.key_identifier.as_ref().map(|id| id.0.to_vec())
                }
                _ => None,
            });
    Ok(LeafInfo {
        fingerprint: hex::encode(Sha256::digest(leaf_der)),
        not_before: cert.validity().not_before.timestamp(),
        not_after: cert.validity().not_after.timestamp(),
        names,
        issuer: cert.issuer().to_string(),
        serial: cert.raw_serial().to_vec(),
        authority_key_id,
        chain_len,
    })
}

/// The checks of docs/acme-edge-certificates-v1.md §4: the chain parses and is not truncated, the
/// key matches the leaf, and the leaf is valid at `now` (Unix seconds). A leaf-only chain is not
/// refused here (a hand-seeded lab certificate may have none); SY.admin requires intermediates
/// for what it issues.
pub fn inspect(
    cert_pem: &[u8],
    key_pem: &[u8],
    now: i64,
) -> Result<CheckedMaterial, MaterialError> {
    let (chain, diagnosis) = parse_chain(cert_pem)?;
    if chain.is_empty() {
        return Err(MaterialError::NoCertificate);
    }
    if diagnosis.truncated() {
        return Err(MaterialError::Truncated {
            markers: diagnosis.markers,
            parsed: diagnosis.parsed,
        });
    }
    let leaf = leaf_info(chain[0].as_ref(), chain.len())?;
    if now >= leaf.not_after {
        return Err(MaterialError::Expired {
            not_after: leaf.not_after,
        });
    }
    if now < leaf.not_before {
        return Err(MaterialError::NotYetValid {
            not_before: leaf.not_before,
        });
    }
    let mut key_reader = key_pem;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|err| MaterialError::BadKey(err.to_string()))?
        .ok_or(MaterialError::NoKey)?;
    // rustls 0.23 needs a process crypto provider (mirrors src/mesh_tls.rs). Idempotent.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = rustls::crypto::ring::default_provider();
    let signing_key = provider
        .key_provider
        .load_private_key(key)
        .map_err(|err| MaterialError::BadKey(err.to_string()))?;
    let certified_key = CertifiedKey::new(chain.clone(), signing_key);
    match certified_key.keys_match() {
        Ok(()) => {}
        Err(rustls::Error::InconsistentKeys(rustls::InconsistentKeys::KeyMismatch)) => {
            return Err(MaterialError::KeyMismatch)
        }
        Err(err) => return Err(MaterialError::BadKey(err.to_string())),
    }
    Ok(CheckedMaterial {
        certified_key,
        chain,
        leaf,
        diagnosis,
    })
}

/// A host name as names are compared here: lowercase, without a trailing dot.
pub fn normalize_name(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Whether one of `names` covers `host`. A wildcard covers exactly one leftmost label
/// (`*.example.com` covers `a.example.com`, not `example.com` nor `a.b.example.com`).
pub fn name_covered(names: &[String], host: &str) -> bool {
    let host = normalize_name(host);
    if host.is_empty() {
        return false;
    }
    names.iter().any(|name| {
        let name = normalize_name(name);
        if let Some(suffix) = name.strip_prefix("*.") {
            match host.split_once('.') {
                Some((label, rest)) => !label.is_empty() && rest == suffix,
                None => false,
            }
        } else {
            name == host
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateParams, DistinguishedName, KeyPair};

    fn self_signed(names: &[&str]) -> (String, String) {
        let key = KeyPair::generate().expect("key");
        let mut params =
            CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
                .expect("params");
        params.distinguished_name = DistinguishedName::new();
        let cert = params.self_signed(&key).expect("cert");
        (cert.pem(), key.serialize_pem())
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    #[test]
    fn a_matching_chain_and_key_pass_and_report_the_leaf() {
        let (cert, key) = self_signed(&["Edge.Example.com."]);
        let checked = inspect(cert.as_bytes(), key.as_bytes(), now()).expect("passes");
        assert_eq!(checked.leaf.names, vec!["edge.example.com".to_string()]);
        assert_eq!(checked.leaf.chain_len, 1);
        assert_eq!(checked.leaf.fingerprint.len(), 64);
        assert!(checked.diagnosis.leaf_only());
        assert!(checked.leaf.lifetime_secs() > 0);
    }

    #[test]
    fn a_key_from_another_certificate_is_refused() {
        let (cert, _) = self_signed(&["edge.example.com"]);
        let (_, other_key) = self_signed(&["edge.example.com"]);
        assert_eq!(
            inspect(cert.as_bytes(), other_key.as_bytes(), now()).err(),
            Some(MaterialError::KeyMismatch)
        );
    }

    #[test]
    fn a_glued_chain_is_refused_as_truncated() {
        let (leaf, key) = self_signed(&["edge.example.com"]);
        let (other, _) = self_signed(&["ca.example.com"]);
        let glued = format!(
            "{}{}",
            leaf.trim_end().replace(
                "-----END CERTIFICATE-----",
                "-----END CERTIFICATE----------BEGIN CERTIFICATE-----"
            ),
            other
                .trim_start()
                .trim_start_matches("-----BEGIN CERTIFICATE-----")
        );
        match inspect(glued.as_bytes(), key.as_bytes(), now()) {
            Err(MaterialError::Truncated { markers, parsed }) => assert!(markers > parsed),
            other => panic!("expected Truncated, got {:?}", other.map(|c| c.leaf)),
        }
    }

    #[test]
    fn a_leaf_outside_its_validity_is_refused() {
        // rcgen's default validity runs from 1975 to 4096: give this one real bounds.
        let key = KeyPair::generate().expect("key");
        let mut params =
            CertificateParams::new(vec!["edge.example.com".to_string()]).expect("params");
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2030, 1, 1);
        let cert = params.self_signed(&key).expect("cert").pem();
        let key = key.serialize_pem();
        const JAN_2019: i64 = 1_546_300_800;
        const JAN_2025: i64 = 1_735_689_600;
        const JAN_2031: i64 = 1_924_992_000;
        assert!(inspect(cert.as_bytes(), key.as_bytes(), JAN_2025).is_ok());
        assert!(matches!(
            inspect(cert.as_bytes(), key.as_bytes(), JAN_2031),
            Err(MaterialError::Expired { .. })
        ));
        assert!(matches!(
            inspect(cert.as_bytes(), key.as_bytes(), JAN_2019),
            Err(MaterialError::NotYetValid { .. })
        ));
    }

    #[test]
    fn missing_material_is_named() {
        let (cert, _) = self_signed(&["edge.example.com"]);
        assert_eq!(
            inspect(b"", b"", now()).err(),
            Some(MaterialError::NoCertificate)
        );
        assert_eq!(
            inspect(cert.as_bytes(), b"", now()).err(),
            Some(MaterialError::NoKey)
        );
    }

    #[test]
    fn a_wildcard_covers_one_leftmost_label() {
        let names = vec!["*.example.com".to_string(), "Exact.Example.org".to_string()];
        assert!(name_covered(&names, "hive-1.example.com"));
        assert!(name_covered(&names, "HIVE-1.EXAMPLE.COM."));
        assert!(!name_covered(&names, "example.com"));
        assert!(!name_covered(&names, "a.b.example.com"));
        assert!(name_covered(&names, "exact.example.org"));
        assert!(!name_covered(&names, "other.example.org"));
        assert!(!name_covered(&names, ""));
    }
}
