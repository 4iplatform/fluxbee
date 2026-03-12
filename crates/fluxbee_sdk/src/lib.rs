pub mod blob;
pub mod client_config;
pub mod comm;
pub mod identity;
pub mod nats;
#[cfg(unix)]
pub mod node_client;
#[cfg(not(unix))]
#[path = "node_client_stub.rs"]
pub mod node_client;
pub mod payload;
pub mod prelude;
pub mod protocol;
pub mod socket;
pub mod split;

pub use client_config::ClientConfig;
pub use identity::{
    identity_shm_name_for_hive, identity_system_call, identity_system_call_ok, load_hive_id,
    provision_ilk, resolve_ilk_from_hive_config, resolve_ilk_from_hive_id,
    resolve_ilk_from_shm_name, IdentityError, IdentityShmError, IdentitySystemRequest,
    IdentitySystemResult, IlkProvisionRequest, IlkProvisionResult, MSG_IDENTITY_METRICS,
    MSG_ILK_ADD_CHANNEL, MSG_ILK_PROVISION, MSG_ILK_REGISTER, MSG_ILK_UPDATE, MSG_TNT_APPROVE,
    MSG_TNT_CREATE,
};
pub use node_client::{connect, connect_with_client_config, NodeConfig, NodeError};
pub use split::{NodeReceiver, NodeSender};
