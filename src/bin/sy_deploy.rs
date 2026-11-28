//! SY.deploy, the CD node (docs/digital-executive-v1-stage-2.md §3, stage 3 §3 and §4,
//! docs/nodes/sy-deploy.md): it deploys solutions from the packages Archi publishes into groupings,
//! step by step through SY.admin, and records what it did. Motherbee only. It takes its orders from
//! `SY.admin@motherbee` alone, the eight `cd_*` actions SY.admin forwards; it keeps its state in
//! plain files under `/var/lib/fluxbee/state/sy-deploy`; it keeps no ED data, which lives in
//! identity. Its package format is the core library's (`json_router::solution_package`).

#[path = "sy_deploy/desired.rs"]
mod desired;
#[cfg(test)]
#[path = "sy_deploy/fixtures.rs"]
mod fixtures;
#[path = "sy_deploy/ids.rs"]
mod ids;
#[path = "sy_deploy/mesh.rs"]
mod mesh;
#[path = "sy_deploy/runner.rs"]
mod runner;
#[path = "sy_deploy/service.rs"]
mod service;
#[path = "sy_deploy/steps.rs"]
mod steps;
#[path = "sy_deploy/store.rs"]
mod store;
#[path = "sy_deploy/tree.rs"]
mod tree;
#[path = "sy_deploy/world.rs"]
mod world;

#[cfg(test)]
#[path = "sy_deploy/tests.rs"]
mod tests;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use fluxbee_sdk::protocol::SYSTEM_KIND;
use fluxbee_sdk::{
    try_handle_default_node_status, NodeConfig, OperationalRouteProfile, RouteMatch, RouteTarget,
    RouterDispatcher,
};
use json_router::router::system_policy::PRIMARY_HIVE_ID;

const NODE_BASE_NAME: &str = "SY.deploy";
const STATE_SUBDIR: &str = "sy-deploy";
const RPC_CH_INCOMING: &str = "incoming";

type DeployError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    blob: Option<BlobSection>,
}

#[derive(Debug, Deserialize)]
struct BlobSection {
    path: Option<String>,
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, DeployError> {
    let data = fs::read_to_string(config_dir.join("hive.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

fn is_mother_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee")
}

/// What reaches SY.deploy's channel: SY.admin's orders (`command`, `query`) and the system
/// messages (the status, which the SDK answers). An answer to one of its own admin calls goes to
/// the call that waits for it.
fn rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_INCOMING)
        .post_pending_rule(
            RouteMatch::any_msg_type("command"),
            RouteTarget::Command(RPC_CH_INCOMING),
        )
        .post_pending_rule(
            RouteMatch::any_msg_type("query"),
            RouteTarget::Command(RPC_CH_INCOMING),
        )
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_INCOMING),
        )
        .build()
}

#[tokio::main]
async fn main() -> Result<(), DeployError> {
    // Answer --help/--version before any runtime init: past this point it waits for the router.
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "sy_deploy — SY.deploy daemon (motherbee-only; NOT an interactive CLI).\n\
                     Takes the cd_* orders SY.admin forwards and runs each deployment's steps \
                     through SY.admin.\n\
                     Started by systemd (sy-deploy.service). Env: JSR_LOG_LEVEL.\n\
                     See docs/nodes/sy-deploy.md."
                );
                return Ok(());
            }
            "-V" | "--version" => {
                println!("sy_deploy {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => {}
        }
    }
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_deploy supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    fluxbee_sdk::logging::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let socket_dir = json_router::paths::router_socket_dir();
    let hive = load_hive(&config_dir)?;
    // The motherbee only, as SY.admin.
    if hive.hive_id == PRIMARY_HIVE_ID && !is_mother_role(hive.role.as_deref()) {
        return Err(format!(
            "invalid hive.yaml: hive_id='{PRIMARY_HIVE_ID}' is reserved for role=motherbee"
        )
        .into());
    }
    if !is_mother_role(hive.role.as_deref()) {
        tracing::warn!("SY.deploy runs only on the motherbee; role != motherbee");
        return Ok(());
    }
    if hive.hive_id != PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid hive.yaml: role=motherbee requires hive_id='{PRIMARY_HIVE_ID}' (got '{}')",
            hive.hive_id
        )
        .into());
    }
    json_router::instance_lock::hold_for_process(
        &json_router::paths::run_dir().join("sy-deploy.lock"),
        NODE_BASE_NAME,
    )?;

    let node_name = format!("{NODE_BASE_NAME}@{}", hive.hive_id);
    // Model D': the self ILK is deterministic from the name, the one SY.identity seeds from the
    // release's lists.
    let self_ilk_id = fluxbee_sdk::deterministic_system_ilk_id(&node_name);
    tracing::info!(node_name = %node_name, self_ilk_id = %self_ilk_id, "self system ILK computed deterministically");

    let store = store::Store::new(state_dir.join(STATE_SUBDIR));
    let node_config = NodeConfig {
        name: NODE_BASE_NAME.to_string(),
        router_socket: socket_dir,
        uuid_persistence_dir: state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let profile = rpc_profile().map_err(|err| format!("sy.deploy rpc profile invalid: {err}"))?;
    let dispatcher = Arc::new(
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?,
    );
    tracing::info!("connected to router");
    let mut incoming = dispatcher
        .take_command_receiver(RPC_CH_INCOMING)
        .await
        .map_err(|err| format!("sy.deploy incoming receiver: {err}"))?;

    let admin = Arc::new(mesh::MeshAdmin {
        rpc: Arc::clone(&dispatcher),
    });
    let world = Arc::new(mesh::MotherbeeWorld {
        hive_id: hive.hive_id.clone(),
        blob_root: mesh::blob_root(hive.blob.as_ref().and_then(|blob| blob.path.as_deref())),
    });
    let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();
    let deployer =
        service::Deployer::open(admin, world, store, service::Timings::production(), jobs_tx)?;
    tokio::spawn(Arc::clone(&deployer).run_jobs(jobs_rx));

    // The writes one at a time, in the order they come; each read in a task of its own, so none
    // waits behind another's status reads. Every order is answered before any step, well within
    // SY.admin's 10 s; the runs go on in the background.
    while let Some(msg) = incoming.recv().await {
        let sender = dispatcher.sender_snapshot();
        match try_handle_default_node_status(&sender, &msg).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(error = %err, "a status request could not be answered");
                continue;
            }
        }
        let kind = msg.meta.msg_type.clone();
        if kind != "command" && kind != "query" {
            continue;
        }
        if kind == "query" {
            let deployer = Arc::clone(&deployer);
            tokio::spawn(async move { serve_order(&deployer, &sender, &msg).await });
        } else {
            serve_order(&deployer, &sender, &msg).await;
        }
    }
    tracing::warn!("sy.deploy incoming channel closed; exiting");
    Ok(())
}

/// Answers one order: SY.admin's alone (the router stamps the sender, so no other node can claim
/// to be it); any other node's is refused.
async fn serve_order(
    deployer: &service::Deployer,
    sender: &fluxbee_sdk::NodeSender,
    msg: &fluxbee_sdk::protocol::Message,
) {
    let kind = msg.meta.msg_type.as_str();
    let action = msg.meta.action.clone().unwrap_or_default();
    let caller = msg.routing.src_l2_name.clone().unwrap_or_default();
    let answer = if caller == mesh::ADMIN_NODE {
        deployer.handle(kind, &action, msg.payload.clone()).await
    } else {
        tracing::warn!(caller = %caller, action = %action, "an order from another node than SY.admin is refused");
        mesh::not_from_admin(&caller)
    };
    tracing::info!(
        action = %action,
        status = %answer["status"].as_str().unwrap_or(""),
        error_code = %answer["error_code"].as_str().unwrap_or(""),
        "order answered"
    );
    mesh::answer(sender, msg, kind, &action, answer).await;
}
