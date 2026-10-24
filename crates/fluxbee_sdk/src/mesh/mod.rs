//! The mesh between hives (feature `mesh-tls`): its own TLS and the motherbee's hive registry, one
//! copy for every node that uses them (docs/io-web-v2.md §11.2).
//!
//! - **TLS:** a dedicated CA on the motherbee issues each hive a leaf at add_hive (CN + SAN DNS =
//!   its hive id). The router's WAN runs mutual TLS over it, and so does io.web's port, which only
//!   the edge on the ingress dials.
//! - **The hive registry:** the hives the motherbee joined, as SY.orchestrator records them. The
//!   router admits WAN peers by it, SY.admin gives a mesh caller its hive's role by it, and io.web
//!   admits only an ingress by it.

mod registry;
mod tls;

pub use registry::{is_valid_hive_id, HiveEntry, HiveRegistry, HiveRole, HIVE_REGISTRY_DIR};
pub use tls::{
    install_crypto_provider, peer_hive_from_cert, LeafBundle, MeshCa, MeshTlsError,
    MeshTlsMaterial, MESH_TLS_DIR,
};

/// io.web's port on the motherbee, for the edge on the ingress only, over mesh mTLS. Never 8443,
/// the edge's old port in the handbook; the router's WAN holds 9000 and identity sync 9100.
pub const IO_WEB_PORT: u16 = 9200;

/// The name the edge verifies io.web's certificate under: io.web presents the motherbee's leaf,
/// whose SAN is the motherbee's hive id.
pub const IO_WEB_SERVER_NAME: &str = crate::protocol::PRIMARY_HIVE_ID;

/// A registry in a temporary directory, with one entry per `(hive, info.yaml)`.
#[cfg(test)]
fn test_registry(entries: &[(&str, &str)]) -> HiveRegistry {
    let root = std::env::temp_dir().join(format!("fluxbee-mesh-registry-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create registry");
    for (hive, info) in entries {
        std::fs::create_dir(root.join(hive)).expect("create registry entry");
        std::fs::write(root.join(hive).join("info.yaml"), info).expect("write info.yaml");
    }
    HiveRegistry::new(root)
}
