use std::collections::HashMap;
use std::convert::Infallible;
use std::env;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hyper::body::to_bytes;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use json_router::protocol::system::{MSG_ANNOUNCE, MSG_QUERY};
use json_router::socket::connection::{read_frame, write_frame};

const DEFAULT_CONFIG_DIR: &str = "/etc/json-router";
const DEFAULT_STATE_DIR: &str = "/var/lib/json-router";
const DEFAULT_SOCKET_DIR: &str = "/var/run/json-router";
const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AdminConfig {
    islands: Vec<IslandConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IslandConfig {
    id: String,
    #[serde(default)]
    orchestrator: Option<String>,
    #[serde(default)]
    config_routes: Option<String>,
    #[serde(default)]
    opa_rules: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IdentityFile {
    uuid: String,
    created_at: String,
}

struct Args {
    config_dir: PathBuf,
    state_dir: PathBuf,
    socket_dir: PathBuf,
    node_socket: Option<PathBuf>,
    log_level: String,
}

#[derive(Clone)]
struct AppState {
    router: Arc<RouterState>,
    islands: HashMap<String, IslandConfig>,
}

struct RouterState {
    sender: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    pending: Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
    node_uuid: Uuid,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(args.log_level))
        .init();

    ensure_dir(&args.config_dir)?;
    ensure_dir(&args.state_dir)?;
    ensure_dir(&args.socket_dir)?;

    let admin_config = load_admin_config(&args.config_dir)?;
    let islands = admin_config
        .islands
        .into_iter()
        .map(|island| (island.id.clone(), island))
        .collect::<HashMap<_, _>>();

    let identity = load_or_create_identity(&args.state_dir)?;
    let node_uuid = Uuid::parse_str(&identity.uuid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid identity uuid"))?;

    let node_socket = args.node_socket.unwrap_or_else(|| {
        args.socket_dir
            .join("nodes")
            .join("SY.admin.sock")
    });

    let router_state = Arc::new(RouterState {
        sender: Mutex::new(None),
        pending: Mutex::new(HashMap::new()),
        node_uuid,
    });

    let router_state_handle = Arc::clone(&router_state);
    tokio::spawn(async move {
        if let Err(err) = router_listener(node_socket, router_state_handle).await {
            error!(target: "sy_admin", "router listener error: {}", err);
        }
    });

    let listen = resolve_listen_addr();
    let addr: SocketAddr = listen.parse().map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "invalid listen address")
    })?;

    let state = AppState {
        router: router_state,
        islands,
    };

    let make_svc = make_service_fn(move |_| {
        let state = state.clone();
        async move { Ok::<_, Infallible>(service_fn(move |req| handle(req, state.clone()))) }
    });

    info!(target: "sy_admin", listen = %addr, "sy-admin listening");
    Server::bind(&addr).serve(make_svc).await?;
    Ok(())
}

async fn router_listener(path: PathBuf, state: Arc<RouterState>) -> io::Result<()> {
    ensure_parent_dir(&path)?;
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;

    info!(target: "sy_admin", socket = %path.display(), "SY.admin socket listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(err) = handle_router_connection(stream, state).await {
                warn!(target: "sy_admin", "router connection error: {}", err);
            }
        });
    }
}

async fn handle_router_connection(stream: UnixStream, state: Arc<RouterState>) -> io::Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    if let Some(frame) = read_frame(&mut reader).await? {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&frame) {
            let msg_kind = value
                .get("meta")
                .and_then(|meta| meta.get("type"))
                .and_then(|kind| kind.as_str());
            let msg_type = value
                .get("meta")
                .and_then(|meta| meta.get("msg"))
                .and_then(|msg| msg.as_str());
            if msg_kind == Some("system") && msg_type == Some(MSG_QUERY) {
                send_announce(&mut writer, &state.node_uuid).await?;
            }
        }
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut sender = state.sender.lock().await;
        *sender = Some(tx);
    }

    let mut write_task = tokio::spawn(async move {
        while let Some(payload) = rx.recv().await {
            if write_frame(&mut writer, &payload).await.is_err() {
                break;
            }
        }
    });

    loop {
        match read_frame(&mut reader).await? {
            Some(frame) => {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&frame) {
                    if let Some(trace_id) = value
                        .get("routing")
                        .and_then(|routing| routing.get("trace_id"))
                        .and_then(|trace| trace.as_str())
                    {
                        if let Some(tx) = state.pending.lock().await.remove(trace_id) {
                            let _ = tx.send(value);
                        }
                    }
                }
            }
            None => break,
        }
    }

    {
        let mut sender = state.sender.lock().await;
        *sender = None;
    }
    write_task.abort();
    Ok(())
}

async fn send_announce<W>(writer: &mut W, uuid: &Uuid) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let msg = serde_json::json!({
        "routing": null,
        "meta": { "type": "system", "msg": MSG_ANNOUNCE },
        "payload": { "uuid": uuid.to_string(), "name": "SY.admin" },
    });
    let data = serde_json::to_vec(&msg)?;
    write_frame(writer, &data).await
}

async fn handle(req: Request<Body>, state: AppState) -> Result<Response<Body>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let segments: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();

    if method == Method::GET && segments.as_slice() == ["islands"] {
        let islands = state
            .islands
            .values()
            .map(|island| serde_json::json!({ "id": island.id }))
            .collect::<Vec<_>>();
        return Ok(json_response(StatusCode::OK, serde_json::json!({ "islands": islands })));
    }

    if segments.len() >= 2 && segments[0] == "islands" {
        let island_id = segments[1];
        let island = match state.islands.get(island_id) {
            Some(island) => island.clone(),
            None => {
                return Ok(error_response(
                    StatusCode::NOT_FOUND,
                    "island not found",
                ))
            }
        };

        if segments.len() >= 3 && segments[2] == "routes" {
            return Ok(handle_routes(req, state.router, island, segments).await);
        }
        if segments.len() >= 3 && segments[2] == "vpns" {
            return Ok(handle_vpns(req, state.router, island, segments).await);
        }
    }

    Ok(error_response(StatusCode::NOT_FOUND, "not found"))
}

async fn handle_routes(
    req: Request<Body>,
    router: Arc<RouterState>,
    island: IslandConfig,
    segments: Vec<&str>,
) -> Response<Body> {
    let target = match island.config_routes {
        Some(value) => value,
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "config_routes not configured for island",
            )
        }
    };

    match *req.method() {
        Method::GET if segments.len() == 3 => {
            match send_admin(&router, &target, "list_routes", serde_json::Value::Null).await {
                Ok(value) => map_admin_response(value),
                Err(err) => err.to_response(),
            }
        }
        Method::POST if segments.len() == 3 => {
            match parse_body(req).await {
                Ok(payload) => match send_admin(&router, &target, "add_route", payload).await {
                    Ok(value) => map_admin_response(value),
                    Err(err) => err.to_response(),
                },
                Err(err) => err,
            }
        }
        Method::DELETE if segments.len() == 4 => {
            let prefix = match urlencoding::decode(segments[3]) {
                Ok(value) => value.into_owned(),
                Err(_) => {
                    return error_response(StatusCode::BAD_REQUEST, "invalid prefix encoding")
                }
            };
            let payload = serde_json::json!({ "prefix": prefix });
            match send_admin(&router, &target, "delete_route", payload).await {
                Ok(value) => map_admin_response(value),
                Err(err) => err.to_response(),
            }
        }
        _ => error_response(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn handle_vpns(
    req: Request<Body>,
    router: Arc<RouterState>,
    island: IslandConfig,
    segments: Vec<&str>,
) -> Response<Body> {
    let target = match island.config_routes {
        Some(value) => value,
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "config_routes not configured for island",
            )
        }
    };

    match *req.method() {
        Method::GET if segments.len() == 3 => {
            match send_admin(&router, &target, "list_vpns", serde_json::Value::Null).await {
                Ok(value) => map_admin_response(value),
                Err(err) => err.to_response(),
            }
        }
        Method::POST if segments.len() == 3 => {
            match parse_body(req).await {
                Ok(payload) => match send_admin(&router, &target, "add_vpn", payload).await {
                    Ok(value) => map_admin_response(value),
                    Err(err) => err.to_response(),
                },
                Err(err) => err,
            }
        }
        Method::DELETE if segments.len() == 4 => {
            let vpn_id = match segments[3].parse::<u32>() {
                Ok(value) => value,
                Err(_) => return error_response(StatusCode::BAD_REQUEST, "invalid vpn id"),
            };
            let payload = serde_json::json!({ "vpn_id": vpn_id });
            match send_admin(&router, &target, "delete_vpn", payload).await {
                Ok(value) => map_admin_response(value),
                Err(err) => err.to_response(),
            }
        }
        _ => error_response(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn parse_body(req: Request<Body>) -> Result<serde_json::Value, Response<Body>> {
    let body = to_bytes(req.into_body())
        .await
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "invalid body"))?;
    if body.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&body)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "invalid json"))
}

fn map_admin_response(value: serde_json::Value) -> Response<Body> {
    let status = value
        .get("meta")
        .and_then(|meta| meta.get("status"))
        .and_then(|value| value.as_str());
    match status {
        Some("error") => json_response(StatusCode::BAD_REQUEST, value),
        _ => json_response(StatusCode::OK, value),
    }
}

async fn send_admin(
    router: &RouterState,
    target: &str,
    action: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, AdminError> {
    let sender = {
        let sender = router.sender.lock().await;
        sender.clone()
    };
    let sender = sender.ok_or(AdminError::Unavailable)?;
    let trace_id = Uuid::new_v4().to_string();

    let msg = serde_json::json!({
        "routing": {
            "src": router.node_uuid.to_string(),
            "dst": null,
            "ttl": 16,
            "trace_id": trace_id,
        },
        "meta": {
            "type": "admin",
            "action": action,
            "target": target,
        },
        "payload": payload,
    });
    let data = serde_json::to_vec(&msg).map_err(|_| AdminError::InvalidPayload)?;

    let (tx, rx) = oneshot::channel();
    {
        let mut pending = router.pending.lock().await;
        pending.insert(trace_id.clone(), tx);
    }

    sender.send(data).map_err(|_| AdminError::Unavailable)?;

    let timeout = time::timeout(Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS), rx);
    match timeout.await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(AdminError::Unavailable),
        Err(_) => {
            let mut pending = router.pending.lock().await;
            pending.remove(&trace_id);
            Err(AdminError::Timeout)
        }
    }
}

#[derive(Debug)]
enum AdminError {
    Unavailable,
    Timeout,
    InvalidPayload,
}

impl AdminError {
    fn to_response(&self) -> Response<Body> {
        match self {
            AdminError::Unavailable => error_response(StatusCode::SERVICE_UNAVAILABLE, "router not connected"),
            AdminError::Timeout => error_response(StatusCode::GATEWAY_TIMEOUT, "request timed out"),
            AdminError::InvalidPayload => error_response(StatusCode::BAD_REQUEST, "invalid payload"),
        }
    }
}

fn json_response(status: StatusCode, value: serde_json::Value) -> Response<Body> {
    let body = serde_json::to_vec(&value).unwrap_or_default();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::from("{\"error\":\"response\"}")))
}

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    json_response(status, serde_json::json!({ "error": message }))
}

fn resolve_listen_addr() -> String {
    env::var("JSR_ADMIN_LISTEN")
        .or_else(|_| env::var("SY_ADMIN_LISTEN"))
        .unwrap_or_else(|_| DEFAULT_LISTEN.to_string())
}

fn load_admin_config(config_dir: &Path) -> io::Result<AdminConfig> {
    let path = config_dir.join("sy-admin.yaml");
    let data = fs::read_to_string(&path)?;
    serde_yaml::from_str(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn load_or_create_identity(state_dir: &Path) -> io::Result<IdentityFile> {
    let identity_dir = state_dir.join("SY.admin");
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
