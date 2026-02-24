//! DEPRECATED: `jsr_client` is a legacy compatibility crate.
//!
//! New development must use `fluxbee_sdk` (`crates/fluxbee_sdk`) as the
//! canonical SDK for node communication, blob handling, and future tools.
//!
//! Migration status and plan:
//! - `docs/onworking/sdk_tasks.md`

pub mod client_config;
pub mod nats;
pub mod node_client;
pub mod protocol;
pub mod socket;
pub mod split;

pub use client_config::ClientConfig;
pub use node_client::{connect, connect_with_client_config, NodeConfig, NodeError};
pub use split::{NodeReceiver, NodeSender};
