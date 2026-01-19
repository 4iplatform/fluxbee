use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tokio::time;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use json_router::config_shm::{ConfigShmWriter, LocalRoute};
use json_router::protocol::system::{MSG_ANNOUNCE, MSG_QUERY};
use json_router::socket::connection::{read_frame, write_frame};

const DEFAULT_CONFIG_DIR: &str = "/etc/json-router";
const DEFAULT_STATE_DIR: &str = "/var/lib/json-router";
const DEFAULT_SOCKET_DIR: &str = "/var/run/json-router";
const DEFAULT_BROADCAST_INTERVAL_SECS: u64 = 60;
const HEARTBEAT_INTERVAL_SECS: u64 = 5;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IslandConfig {
    island: IslandSection,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IslandSection {
    id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SyConfigFile {
    version: u64,
    updated_at: String,
    routes: Vec<RouteConfig>,
    vpns: Vec<VpnConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RouteConfig {
    prefix: String,
    match_kind: String,
    action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_hop_island: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vpn_id: Option<u32>,
    metric: Option<u16>,
    priority: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct VpnConfig {
    #[serde(default)]
    vpn_id: u32,
    vpn_name: String,
    remote_island: String,
    endpoints: Vec<String>,
    #[serde(default)]
    created_at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AnnouncePayload {
    uuid: String,
    name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ConfigAnnouncePayload {
    island: String,
    version: u64,
    timestamp: String,
    routes: Vec<RouteConfig>,
    vpns: Vec<VpnConfig>,
}

const MAX_ROUTES: usize = 256;
const MAX_VPNS: usize = 64;
const PREFIX_MAX_LEN: usize = 64;

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(args.log_level))
        .init();

    ensure_dir(&args.config_dir)?;
    ensure_dir(&args.state_dir)?;
    ensure_dir(&args.socket_dir)?;

    let island = load_island(&args.config_dir)?;
    let mut file = load_or_create_routes(&args.config_dir)?;
    for route in &mut file.routes {
        if let Err((code, message)) = normalize_route(route) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid route in config: {code} - {message}"),
            ));
        }
    }
    let now = now_epoch_ms();
    for vpn in &mut file.vpns {
        if vpn.created_at == 0 {
            vpn.created_at = now;
        }
    }

    let identity = load_or_create_identity(&args.state_dir)?;
    let node_uuid = Uuid::parse_str(&identity.uuid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid identity uuid"))?;
    let shm_name = format!("/jsr-config-{}", island.island.id);
    let config_shm = ConfigShmWriter::open_or_create(&shm_name, &island.island.id)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let config_shm = Arc::new(Mutex::new(config_shm));

    let local_routes = routes_to_shm(&file.routes);
    {
        let mut shm = config_shm.lock().await;
        shm.write_local_routes(&local_routes)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    }

    let socket_path = args.node_socket.unwrap_or_else(|| {
        args.socket_dir
            .join("nodes")
            .join("SY.config.routes.sock")
    });
    ensure_parent_dir(&socket_path)?;
    let _ = fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;

    info!(target: "sy_config_routes", socket = %socket_path.display(), "sy-config-routes listening");

    let node_name = format!("SY.config.routes.{}", island.island.id);
    let state = Arc::new(Mutex::new(ConfigState {
        config_dir: args.config_dir.clone(),
        island_id: island.island.id.clone(),
        routes: file.routes.clone(),
        vpns: file.vpns.clone(),
        version: file.version,
        updated_at: file.updated_at.clone(),
    }));

    loop {
        let (stream, _) = listener.accept().await?;
        let node_uuid = node_uuid;
        let node_name = node_name.clone();
        let state_handle = Arc::clone(&state);
        let config_shm = Arc::clone(&config_shm);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(
                stream,
                node_uuid,
                node_name,
                state_handle,
                config_shm,
            )
            .await
            {
                warn!(target: "sy_config_routes", "connection error: {}", err);
            }
        });
    }
}

async fn handle_connection(
    stream: UnixStream,
    uuid: Uuid,
    name: String,
    state: Arc<Mutex<ConfigState>>,
    config_shm: Arc<Mutex<ConfigShmWriter>>,
) -> io::Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let Some(frame) = read_frame(&mut reader).await? else {
        return Ok(());
    };

    let msg: serde_json::Value = serde_json::from_slice(&frame)?;
    let msg_kind = msg
        .get("meta")
        .and_then(|meta| meta.get("type"))
        .and_then(|kind| kind.as_str());
    let msg_type = msg
        .get("meta")
        .and_then(|meta| meta.get("msg"))
        .and_then(|msg| msg.as_str());
    if msg_kind == Some("system") && msg_type == Some(MSG_QUERY) {
        send_announce(&mut writer, &uuid, &name).await?;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let send_tx = tx.clone();
    let node_uuid = uuid;
    let state_sender = Arc::clone(&state);
    let announce_tx = send_tx.clone();
    tokio::spawn(async move {
        if let Some(snapshot) = snapshot_state(&state_sender).await {
            let _ = send_config_announce(
                &announce_tx,
                &node_uuid,
                &snapshot.island_id,
                snapshot.version,
                &snapshot.routes,
                &snapshot.vpns,
            );
        }
        let mut interval = time::interval(Duration::from_secs(DEFAULT_BROADCAST_INTERVAL_SECS));
        loop {
            interval.tick().await;
            if let Some(snapshot) = snapshot_state(&state_sender).await {
                let _ = send_config_announce(
                    &announce_tx,
                    &node_uuid,
                    &snapshot.island_id,
                    snapshot.version,
                    &snapshot.routes,
                    &snapshot.vpns,
                );
            }
        }
    });

    tokio::spawn(async move {
        while let Some(payload) = rx.recv().await {
            if write_frame(&mut writer, &payload).await.is_err() {
                break;
            }
        }
    });

    let heartbeat_shm = Arc::clone(&config_shm);
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        loop {
            interval.tick().await;
            let mut shm = heartbeat_shm.lock().await;
            shm.update_heartbeat();
        }
    });

    loop {
        match read_frame(&mut reader).await? {
            Some(frame) => {
                if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(&frame) {
                    let msg_kind = msg
                        .get("meta")
                        .and_then(|meta| meta.get("type"))
                        .and_then(|kind| kind.as_str());
                    let msg_type = msg
                        .get("meta")
                        .and_then(|meta| meta.get("msg"))
                        .and_then(|msg| msg.as_str());
                    if msg_kind == Some("system") && msg_type == Some("CONFIG_ANNOUNCE") {
                        if let Some(payload) = msg.get("payload") {
                            if let Ok(payload) = serde_json::from_value::<ConfigAnnouncePayload>(
                                payload.clone(),
                            ) {
                                let island = {
                                    let state = state.lock().await;
                                    state.island_id.clone()
                                };
                                if payload.island != island {
                                    let routes = routes_to_shm(&payload.routes);
                                    let mut shm = config_shm.lock().await;
                                    let _ = shm.update_global_routes(
                                        &payload.island,
                                        payload.version,
                                        &routes,
                                    );
                                }
                            }
                        }
                    }
                    if msg_kind == Some("admin") {
                        if let Some(action) = msg
                            .get("meta")
                            .and_then(|meta| meta.get("action"))
                            .and_then(|action| action.as_str())
                        {
                            handle_admin_action(
                                action,
                                &msg,
                                &state,
                                &config_shm,
                                &send_tx,
                                &node_uuid,
                            )
                            .await?;
                        }
                    }
                }
            }
            None => break,
        }
    }

    Ok(())
}

async fn send_announce<W>(writer: &mut W, uuid: &Uuid, name: &str) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let msg = serde_json::json!({
        "routing": null,
        "meta": { "type": "system", "msg": MSG_ANNOUNCE },
        "payload": AnnouncePayload {
            uuid: uuid.to_string(),
            name: name.to_string(),
        },
    });
    let data = serde_json::to_vec(&msg)?;
    write_frame(writer, &data).await
}

fn send_config_announce(
    sender: &mpsc::UnboundedSender<Vec<u8>>,
    node_uuid: &Uuid,
    island: &str,
    version: u64,
    routes: &[RouteConfig],
    vpns: &[VpnConfig],
) -> Result<(), ()> {
    let routes: Vec<RouteConfig> = routes
        .iter()
        .filter(|route| !is_uuid_route(&route.prefix))
        .cloned()
        .collect();
    let msg = serde_json::json!({
        "routing": {
            "src": node_uuid.to_string(),
            "dst": "broadcast",
            "ttl": 16,
            "trace_id": Uuid::new_v4().to_string(),
        },
        "meta": {
            "type": "system",
            "msg": "CONFIG_ANNOUNCE",
            "target": "SY.config.*",
        },
        "payload": ConfigAnnouncePayload {
            island: island.to_string(),
            version,
            timestamp: chrono::Utc::now().to_rfc3339(),
            routes,
            vpns: vpns.to_vec(),
        },
    });
    let payload = serde_json::to_vec(&msg).map_err(|_| ())?;
    sender.send(payload).map_err(|_| ())
}

fn routes_to_shm(routes: &[RouteConfig]) -> Vec<LocalRoute> {
    routes
        .iter()
        .map(|route| LocalRoute {
            prefix: route.prefix.clone(),
            match_kind: match route.match_kind.to_uppercase().as_str() {
                "PREFIX" => 1,
                "GLOB" => 2,
                _ => 0,
            },
            action: match route.action.to_uppercase().as_str() {
                "DROP" => 1,
                "VPN" => 2,
                _ => 0,
            },
            metric: route.metric.unwrap_or(1),
            priority: route.priority.unwrap_or(100),
        })
        .collect()
}

fn load_island(config_dir: &Path) -> io::Result<IslandConfig> {
    let island_path = config_dir.join("island.yaml");
    let data = fs::read_to_string(&island_path)?;
    serde_yaml::from_str(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn load_or_create_routes(config_dir: &Path) -> io::Result<SyConfigFile> {
    let path = config_dir.join("sy-config-routes.yaml");
    if path.exists() {
        let data = fs::read_to_string(&path)?;
        let parsed: SyConfigFile =
            serde_yaml::from_str(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        return Ok(parsed);
    }

    let file = SyConfigFile {
        version: 1,
        updated_at: chrono::Utc::now().to_rfc3339(),
        routes: Vec::new(),
        vpns: Vec::new(),
    };
    let serialized = serde_yaml::to_string(&file)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&path, serialized)?;
    Ok(file)
}

fn is_uuid_route(prefix: &str) -> bool {
    Uuid::parse_str(prefix).is_ok()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IdentityFile {
    uuid: String,
    created_at: String,
}

fn load_or_create_identity(state_dir: &Path) -> io::Result<IdentityFile> {
    let identity_dir = state_dir.join("SY.config.routes");
    let identity_path = identity_dir.join("identity.yaml");
    if identity_path.exists() {
        let data = fs::read_to_string(&identity_path)?;
        let identity = serde_yaml::from_str(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        return Ok(identity);
    }

    fs::create_dir_all(&identity_dir)?;
    let identity = IdentityFile {
        uuid: Uuid::new_v4().to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let serialized =
        serde_yaml::to_string(&identity).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&identity_path, serialized)?;
    Ok(identity)
}

fn ensure_dir(path: &Path) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    fs::create_dir_all(path)?;
    Ok(())
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    Ok(())
}

struct Args {
    config_dir: PathBuf,
    state_dir: PathBuf,
    socket_dir: PathBuf,
    node_socket: Option<PathBuf>,
    log_level: String,
}

#[derive(Clone, Debug)]
struct ConfigState {
    config_dir: PathBuf,
    island_id: String,
    routes: Vec<RouteConfig>,
    vpns: Vec<VpnConfig>,
    version: u64,
    updated_at: String,
}

#[derive(Clone, Debug)]
struct ConfigSnapshot {
    island_id: String,
    routes: Vec<RouteConfig>,
    vpns: Vec<VpnConfig>,
    version: u64,
}

async fn snapshot_state(state: &Arc<Mutex<ConfigState>>) -> Option<ConfigSnapshot> {
    let state = state.lock().await;
    Some(ConfigSnapshot {
        island_id: state.island_id.clone(),
        routes: state.routes.clone(),
        vpns: state.vpns.clone(),
        version: state.version,
    })
}

async fn handle_admin_action(
    action: &str,
    msg: &serde_json::Value,
    state: &Arc<Mutex<ConfigState>>,
    config_shm: &Arc<Mutex<ConfigShmWriter>>,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
    node_uuid: &Uuid,
) -> io::Result<()> {
    let src = msg
        .get("routing")
        .and_then(|routing| routing.get("src"))
        .and_then(|src| src.as_str())
        .unwrap_or("");
    let trace_id = msg
        .get("routing")
        .and_then(|routing| routing.get("trace_id"))
        .and_then(|trace_id| trace_id.as_str())
        .unwrap_or("");

    match action {
        "list_routes" => {
            let snapshot = snapshot_state(state).await.unwrap();
            let response = serde_json::json!({
                "routing": { "src": node_uuid.to_string(), "dst": src, "ttl": 16, "trace_id": trace_id },
                "meta": { "type": "admin", "action": "list_routes", "status": "ok" },
                "payload": { "config_version": snapshot.version, "routes": snapshot.routes },
            });
            let payload = serde_json::to_vec(&response)?;
            sender.send(payload).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "send"))?;
        }
        "add_route" => {
            let payload = msg.get("payload").cloned().unwrap_or_default();
            let mut route: RouteConfig = serde_json::from_value(payload)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid route"))?;
            if let Err((code, message)) = normalize_route(&mut route) {
                send_admin_error(sender, node_uuid, src, trace_id, action, code, message)?;
                return Ok(());
            }
            let mut state_guard = state.lock().await;
            if state_guard
                .routes
                .iter()
                .any(|r| r.prefix == route.prefix)
            {
                send_admin_error(sender, node_uuid, src, trace_id, action, "DUPLICATE_PREFIX", "Route with prefix already exists")?;
                return Ok(());
            }
            if state_guard.routes.len() >= MAX_ROUTES {
                send_admin_error(sender, node_uuid, src, trace_id, action, "MAX_ROUTES_EXCEEDED", "Maximum routes reached")?;
                return Ok(());
            }
            if route.action.eq_ignore_ascii_case("VPN") {
                let vpn_id = route.vpn_id.unwrap_or(0);
                if vpn_id == 0 || !state_guard.vpns.iter().any(|vpn| vpn.vpn_id == vpn_id) {
                    send_admin_error(sender, node_uuid, src, trace_id, action, "VPN_NOT_FOUND", "VPN not found")?;
                    return Ok(());
                }
            }
            state_guard.routes.push(route.clone());
            bump_version(&mut state_guard);
            if persist_state(&state_guard).is_err() {
                send_admin_error(sender, node_uuid, src, trace_id, action, "PERSISTENCE_ERROR", "Failed to persist config")?;
                return Ok(());
            }
            let local_routes = routes_to_shm(&state_guard.routes);
            let mut shm = config_shm.lock().await;
            if shm.write_local_routes(&local_routes).is_err() {
                send_admin_error(sender, node_uuid, src, trace_id, action, "PERSISTENCE_ERROR", "SHM update failed")?;
                return Ok(());
            }
            send_config_announce(
                sender,
                node_uuid,
                &state_guard.island_id,
                state_guard.version,
                &state_guard.routes,
                &state_guard.vpns,
            )
            .ok();
            let response = serde_json::json!({
                "routing": { "src": node_uuid.to_string(), "dst": src, "ttl": 16, "trace_id": trace_id },
                "meta": { "type": "admin", "action": "add_route", "status": "ok" },
                "payload": { "config_version": state_guard.version, "route": route },
            });
            let data = serde_json::to_vec(&response)?;
            sender.send(data).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "send"))?;
        }
        "delete_route" => {
            let payload = msg.get("payload").cloned().unwrap_or_default();
            let prefix = payload
                .get("prefix")
                .and_then(|value| value.as_str())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing prefix"))?;
            let mut state_guard = state.lock().await;
            let before = state_guard.routes.len();
            state_guard.routes.retain(|route| route.prefix != prefix);
            if state_guard.routes.len() == before {
                send_admin_error(sender, node_uuid, src, trace_id, action, "PREFIX_NOT_FOUND", "Route not found")?;
                return Ok(());
            }
            bump_version(&mut state_guard);
            if persist_state(&state_guard).is_err() {
                send_admin_error(sender, node_uuid, src, trace_id, action, "PERSISTENCE_ERROR", "Failed to persist config")?;
                return Ok(());
            }
            let local_routes = routes_to_shm(&state_guard.routes);
            let mut shm = config_shm.lock().await;
            if shm.write_local_routes(&local_routes).is_err() {
                send_admin_error(sender, node_uuid, src, trace_id, action, "PERSISTENCE_ERROR", "SHM update failed")?;
                return Ok(());
            }
            send_config_announce(
                sender,
                node_uuid,
                &state_guard.island_id,
                state_guard.version,
                &state_guard.routes,
                &state_guard.vpns,
            )
            .ok();
            let response = serde_json::json!({
                "routing": { "src": node_uuid.to_string(), "dst": src, "ttl": 16, "trace_id": trace_id },
                "meta": { "type": "admin", "action": "delete_route", "status": "ok" },
                "payload": { "config_version": state_guard.version },
            });
            let data = serde_json::to_vec(&response)?;
            sender.send(data).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "send"))?;
        }
        "list_vpns" => {
            let snapshot = snapshot_state(state).await.unwrap();
            let response = serde_json::json!({
                "routing": { "src": node_uuid.to_string(), "dst": src, "ttl": 16, "trace_id": trace_id },
                "meta": { "type": "admin", "action": "list_vpns", "status": "ok" },
                "payload": { "vpns": snapshot.vpns },
            });
            let payload = serde_json::to_vec(&response)?;
            sender.send(payload).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "send"))?;
        }
        "add_vpn" => {
            let payload = msg.get("payload").cloned().unwrap_or_default();
            let mut vpn: VpnConfig = serde_json::from_value(payload)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid vpn"))?;
            let mut state_guard = state.lock().await;
            if state_guard.vpns.len() >= MAX_VPNS {
                send_admin_error(sender, node_uuid, src, trace_id, action, "MAX_VPNS_EXCEEDED", "Maximum VPNs reached")?;
                return Ok(());
            }
            if vpn.vpn_name.trim().is_empty() || vpn.remote_island.trim().is_empty() {
                send_admin_error(sender, node_uuid, src, trace_id, action, "INVALID_ACTION", "VPN name and remote_island are required")?;
                return Ok(());
            }
            if state_guard.vpns.iter().any(|v| v.vpn_name == vpn.vpn_name) {
                send_admin_error(sender, node_uuid, src, trace_id, action, "DUPLICATE_VPN_NAME", "VPN name already exists")?;
                return Ok(());
            }
            if vpn.endpoints.is_empty() || vpn.endpoints.len() > 2 {
                send_admin_error(sender, node_uuid, src, trace_id, action, "INVALID_ACTION", "VPN endpoints must include 1-2 entries")?;
                return Ok(());
            }
            if vpn.vpn_id == 0 {
                vpn.vpn_id = next_vpn_id(&state_guard.vpns);
            } else if state_guard.vpns.iter().any(|existing| existing.vpn_id == vpn.vpn_id) {
                send_admin_error(sender, node_uuid, src, trace_id, action, "INVALID_ACTION", "VPN ID already in use")?;
                return Ok(());
            }
            if vpn.created_at == 0 {
                vpn.created_at = now_epoch_ms();
            }
            state_guard.vpns.push(vpn.clone());
            bump_version(&mut state_guard);
            if persist_state(&state_guard).is_err() {
                send_admin_error(sender, node_uuid, src, trace_id, action, "PERSISTENCE_ERROR", "Failed to persist config")?;
                return Ok(());
            }
            send_config_announce(
                sender,
                node_uuid,
                &state_guard.island_id,
                state_guard.version,
                &state_guard.routes,
                &state_guard.vpns,
            )
            .ok();
            let response = serde_json::json!({
                "routing": { "src": node_uuid.to_string(), "dst": src, "ttl": 16, "trace_id": trace_id },
                "meta": { "type": "admin", "action": "add_vpn", "status": "ok" },
                "payload": { "config_version": state_guard.version, "vpn": vpn },
            });
            let data = serde_json::to_vec(&response)?;
            sender.send(data).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "send"))?;
        }
        "delete_vpn" => {
            let payload = msg.get("payload").cloned().unwrap_or_default();
            let vpn_id = payload
                .get("vpn_id")
                .and_then(|value| value.as_u64())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing vpn_id"))? as u32;
            let mut state_guard = state.lock().await;
            let before = state_guard.vpns.len();
            state_guard.vpns.retain(|vpn| vpn.vpn_id != vpn_id);
            if state_guard.vpns.len() == before {
                send_admin_error(sender, node_uuid, src, trace_id, action, "VPN_NOT_FOUND", "VPN not found")?;
                return Ok(());
            }
            bump_version(&mut state_guard);
            if persist_state(&state_guard).is_err() {
                send_admin_error(sender, node_uuid, src, trace_id, action, "PERSISTENCE_ERROR", "Failed to persist config")?;
                return Ok(());
            }
            send_config_announce(
                sender,
                node_uuid,
                &state_guard.island_id,
                state_guard.version,
                &state_guard.routes,
                &state_guard.vpns,
            )
            .ok();
            let response = serde_json::json!({
                "routing": { "src": node_uuid.to_string(), "dst": src, "ttl": 16, "trace_id": trace_id },
                "meta": { "type": "admin", "action": "delete_vpn", "status": "ok" },
                "payload": { "config_version": state_guard.version },
            });
            let data = serde_json::to_vec(&response)?;
            sender.send(data).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "send"))?;
        }
        _ => {}
    }

    Ok(())
}

fn send_admin_error(
    sender: &mpsc::UnboundedSender<Vec<u8>>,
    node_uuid: &Uuid,
    dst: &str,
    trace_id: &str,
    action: &str,
    code: &str,
    message: &str,
) -> io::Result<()> {
    let response = serde_json::json!({
        "routing": { "src": node_uuid.to_string(), "dst": dst, "ttl": 16, "trace_id": trace_id },
        "meta": { "type": "admin", "action": action, "status": "error" },
        "payload": { "error": code, "message": message },
    });
    let data = serde_json::to_vec(&response)?;
    sender.send(data).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "send"))
}

fn bump_version(state: &mut ConfigState) {
    state.version = state.version.saturating_add(1);
    state.updated_at = chrono::Utc::now().to_rfc3339();
}

fn persist_state(state: &ConfigState) -> io::Result<()> {
    let file = SyConfigFile {
        version: state.version,
        updated_at: state.updated_at.clone(),
        routes: state.routes.clone(),
        vpns: state.vpns.clone(),
    };
    let data = serde_yaml::to_string(&file)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(state.config_dir.join("sy-config-routes.yaml"), data)?;
    Ok(())
}

fn normalize_route(route: &mut RouteConfig) -> Result<(), (&'static str, &'static str)> {
    if route.prefix.trim().is_empty() || route.prefix.len() > PREFIX_MAX_LEN {
        return Err(("INVALID_PREFIX", "Prefix is empty or too long"));
    }
    let match_kind = route.match_kind.to_uppercase();
    match match_kind.as_str() {
        "EXACT" | "PREFIX" | "GLOB" => {}
        _ => {
            return Err(("INVALID_MATCH_KIND", "match_kind must be EXACT, PREFIX, or GLOB"));
        }
    }
    let action = route.action.to_uppercase();
    match action.as_str() {
        "FORWARD" | "DROP" | "VPN" => {}
        _ => {
            return Err(("INVALID_ACTION", "action must be FORWARD, DROP, or VPN"));
        }
    }
    route.match_kind = match_kind;
    route.action = action;
    Ok(())
}

fn next_vpn_id(vpns: &[VpnConfig]) -> u32 {
    let mut max_id = 0;
    for vpn in vpns {
        max_id = max_id.max(vpn.vpn_id);
    }
    max_id.saturating_add(1)
}

fn now_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Args {
    fn parse() -> Self {
        let mut config_dir = env::var("JSR_CONFIG_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_DIR));
        let mut state_dir = env::var("JSR_STATE_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR));
        let mut socket_dir = env::var("JSR_SOCKET_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET_DIR));
        let mut node_socket = env::var("JSR_NODE_SOCKET").ok().map(PathBuf::from);
        let mut log_level = env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config-dir" => {
                    if let Some(value) = args.next() {
                        config_dir = PathBuf::from(value);
                    }
                }
                "--state-dir" => {
                    if let Some(value) = args.next() {
                        state_dir = PathBuf::from(value);
                    }
                }
                "--socket-dir" => {
                    if let Some(value) = args.next() {
                        socket_dir = PathBuf::from(value);
                    }
                }
                "--node-socket" => {
                    node_socket = args.next().map(PathBuf::from);
                }
                "--log-level" => {
                    log_level = args.next().unwrap_or_else(|| log_level.clone());
                }
                _ => {}
            }
        }

        Self {
            config_dir,
            state_dir,
            socket_dir,
            node_socket,
            log_level,
        }
    }
}
