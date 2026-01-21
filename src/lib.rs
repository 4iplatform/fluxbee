#[cfg(not(target_os = "linux"))]
compile_error!("json-router supports only Linux targets.");

pub mod config;
pub mod node_client;
pub mod protocol;
pub mod router;
pub mod shm;
pub mod socket;
