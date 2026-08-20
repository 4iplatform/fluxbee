use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::protocol::{
    ConfigChangedPayload, Destination, Message, Meta, Routing, MSG_CONFIG_CHANGED, MSG_CONFIG_GET,
    is_system_kind, MSG_CONFIG_SET, SYSTEM_KIND,
};
use fluxbee_sdk::{
    build_node_config_response_message, parse_node_config_request, try_handle_default_node_status,
    NodeConfig, NodeConfigControlRequest, NodeConfigSetPayload, NodeSender,
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher, ADMIN_KIND,
    NODE_CONFIG_APPLY_MODE_REPLACE, NODE_CONFIG_CONTROL_TARGET,
};
use json_router::shm::{
    copy_bytes_with_len, now_epoch_ms, ConfigRegionWriter, StaticRouteEntry, TapEntry,
    VpnAssignment, ACTION_DROP, ACTION_FORWARD, FLAG_ACTIVE, FLAG_FROZEN, MATCH_EXACT, MATCH_GLOB,
    MATCH_PREFIX,
};

const RPC_CH_INCOMING: &str = "incoming";

fn build_config_routes_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_INCOMING)
        .post_pending_rule(
            RouteMatch::any_msg_type(ADMIN_KIND),
            RouteTarget::Command(RPC_CH_INCOMING),
        )
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_INCOMING),
        )
        .build()
}

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct SyConfigFile {
    version: u64,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    routes: Vec<RouteConfig>,
    #[serde(default)]
    vpns: Vec<VpnConfig>,
    /// ROUTER-TAP-4: router-level tap entries persisted alongside routes/vpns.
    #[serde(default)]
    taps: Vec<TapConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct RouteConfig {
    prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    match_kind: Option<String>,
    action: String,
    #[serde(default)]
    next_hop_hive: Option<String>,
    #[serde(default)]
    metric: Option<u32>,
    #[serde(default)]
    priority: Option<u16>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct VpnConfig {
    pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    match_kind: Option<String>,
    vpn_id: u32,
    #[serde(default)]
    priority: Option<u16>,
}

/// ROUTER-TAP-4: persisted shape of a tap entry inside the config JSON.
/// Mirrors `shm::TapEntry` semantics: exact L2 name match on `(match_src,
/// match_dst)`, fire-and-forget copy to `target`. `enabled` defaults to
/// true so adding a tap is immediately active unless the operator opts out.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct TapConfig {
    match_src: String,
    match_dst: String,
    target: String,
    #[serde(default = "default_tap_mode")]
    mode: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_tap_mode() -> String {
    "best_effort".to_string()
}

fn default_true() -> bool {
    true
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_config_routes supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let socket_dir = json_router::paths::router_socket_dir();

    ensure_dir(&state_dir)?;

    let hive = load_hive(&config_dir)?;
    let hive_id = hive.hive_id.clone();
    // Model D': self-ILK is deterministic from L2 name (no SHM wait).
    // Same hash function identity uses to seed SHM, so the value matches
    // what other nodes expect. Eliminates boot-order dependency on identity.
    let self_node_name = format!("SY.config.routes@{hive_id}");
    let _self_ilk_id = fluxbee_sdk::deterministic_system_ilk_id(&self_node_name);
    tracing::info!(self_ilk_id = %_self_ilk_id, "self system ILK computed deterministically");
    let router_socket = match std::env::var("JSR_ROUTER_NAME") {
        Ok(router_name) => {
            let router_l2_name = ensure_l2_name(&router_name, &hive.hive_id);
            match load_router_uuid(&state_dir, &router_l2_name) {
                Ok(router_uuid) => socket_dir.join(format!("{}.sock", router_uuid.simple())),
                Err(_) => socket_dir.clone(),
            }
        }
        Err(_) => socket_dir.clone(),
    };
    let mut sy_config = load_config(&config_dir)?;
    let node_name = format!("SY.config.routes@{}", hive.hive_id);

    let shm_name = format!("/jsr-config-{}", hive.hive_id);
    let node_uuid = load_or_create_uuid(&state_dir.join("nodes"), "SY.config.routes")?;
    let mut writer = ConfigRegionWriter::open_or_create(&shm_name, node_uuid, &hive.hive_id)?;

    apply_config(&mut writer, &hive_id, &sy_config)?;

    let node_config = NodeConfig {
        name: "SY.config.routes".to_string(),
        router_socket: router_socket.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let profile = build_config_routes_rpc_profile()
        .map_err(|err| format!("sy.config-routes rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    tracing::info!("connected to router");

    let mut incoming_rx = dispatcher
        .take_command_receiver(RPC_CH_INCOMING)
        .await
        .map_err(|err| format!("sy.config-routes incoming receiver: {err}"))?;

    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                writer.update_heartbeat();
            }
            msg = incoming_rx.recv() => {
                let Some(msg) = msg else {
                    tracing::warn!("sy.config-routes incoming channel closed; exiting main loop");
                    return Ok(());
                };
                let sender = dispatcher.sender_snapshot();
                if try_handle_default_node_status(&sender, &msg).await? {
                    continue;
                }
                if msg.meta.msg_type == ADMIN_KIND {
                    if let Err(err) = handle_admin_action(
                        &sender,
                        &msg,
                        &config_dir,
                        &hive_id,
                        &mut sy_config,
                        &mut writer,
                    )
                    .await {
                        tracing::warn!("admin action error: {err}");
                    }
                    continue;
                }
                if is_system_kind(&msg.meta.msg_type) && msg.meta.msg.as_deref() == Some(MSG_CONFIG_GET) {
                    if let Err(err) = handle_node_config_get(
                        &sender,
                        &msg,
                        &node_name,
                        &hive_id,
                        &sy_config,
                    ).await {
                        tracing::warn!("node config get error: {err}");
                    }
                    continue;
                }
                if is_system_kind(&msg.meta.msg_type) && msg.meta.msg.as_deref() == Some(MSG_CONFIG_SET) {
                    if let Err(err) = handle_node_config_set(
                        &sender,
                        &msg,
                        &config_dir,
                        &node_name,
                        &hive_id,
                        &mut sy_config,
                        &mut writer,
                    ).await {
                        tracing::warn!("node config set error: {err}");
                    }
                    continue;
                }
                if !is_system_kind(&msg.meta.msg_type) || msg.meta.msg.as_deref() != Some(MSG_CONFIG_CHANGED) {
                    continue;
                }
                let payload_value = msg.payload.clone();
                let payload: ConfigChangedPayload = match serde_json::from_value(payload_value) {
                    Ok(payload) => payload,
                    Err(err) => {
                        tracing::warn!("invalid config changed payload: {err}");
                        let _ = send_config_response(
                            &sender,
                            &msg,
                            "unknown",
                            0,
                            "error",
                            Some("INVALID_PAYLOAD".to_string()),
                            Some(err.to_string()),
                            &hive_id,
                        ).await;
                        continue;
                    }
                };
                tracing::info!(
                    subsystem = %payload.subsystem,
                    payload_version = payload.version,
                    current_version = sy_config.version,
                    "config changed received"
                );
                if payload.version != 0 && payload.version <= sy_config.version {
                    tracing::info!(
                        payload_version = payload.version,
                        current_version = sy_config.version,
                        "config version not newer; skipping"
                    );
                    continue;
                }
                let mut next_config = sy_config.clone();
                match payload.subsystem.as_str() {
                    "routes" => {
                        let routes = match parse_routes(&payload.config) {
                            Ok(routes) => routes,
                            Err(err) => {
                                tracing::warn!("invalid routes payload: {err}");
                                let _ = send_config_response(
                                    &sender,
                                    &msg,
                                    "routes",
                                    payload.version,
                                    "error",
                                    Some("INVALID_CONFIG".to_string()),
                                    Some(err.to_string()),
                                    &hive_id,
                                ).await;
                                continue;
                            }
                        };
                        if let Some(routes) = routes {
                            next_config.routes = routes;
                        }
                    }
                    "vpn" | "vpns" => {
                        let vpns = match parse_vpns(&payload.config) {
                            Ok(vpns) => vpns,
                            Err(err) => {
                                tracing::warn!("invalid vpns payload: {err}");
                                let _ = send_config_response(
                                    &sender,
                                    &msg,
                                    "vpn",
                                    payload.version,
                                    "error",
                                    Some("INVALID_CONFIG".to_string()),
                                    Some(err.to_string()),
                                    &hive_id,
                                ).await;
                                continue;
                            }
                        };
                        if let Some(vpns) = vpns {
                            next_config.vpns = vpns;
                        }
                    }
                    "tap" | "taps" => {
                        let taps = match parse_taps(&payload.config) {
                            Ok(taps) => taps,
                            Err(err) => {
                                tracing::warn!("invalid taps payload: {err}");
                                let _ = send_config_response(
                                    &sender,
                                    &msg,
                                    "tap",
                                    payload.version,
                                    "error",
                                    Some("INVALID_CONFIG".to_string()),
                                    Some(err.to_string()),
                                    &hive_id,
                                )
                                .await;
                                continue;
                            }
                        };
                        if let Some(taps) = taps {
                            next_config.taps = taps;
                        }
                    }
                    _ => {
                        continue;
                    }
                }
                if next_config.routes == sy_config.routes
                    && next_config.vpns == sy_config.vpns
                    && next_config.taps == sy_config.taps
                {
                    tracing::info!(
                        subsystem = %payload.subsystem,
                        version = payload.version,
                        "config unchanged; skipping apply"
                    );
                    continue;
                }
                if payload.version == 0 {
                    next_config.version = sy_config.version.saturating_add(1);
                } else {
                    next_config.version = payload.version;
                }
                next_config.updated_at = now_epoch_ms().to_string();
                if let Err(err) = apply_config(&mut writer, &hive_id, &next_config) {
                    tracing::warn!("apply config failed: {err}");
                    let _ = send_config_response(
                        &sender,
                        &msg,
                        payload.subsystem.as_str(),
                        next_config.version,
                        "error",
                        Some("APPLY_FAILED".to_string()),
                        Some(err.to_string()),
                        &hive_id,
                    ).await;
                    continue;
                }
                if let Err(err) = write_config(&config_dir, &next_config) {
                    tracing::warn!("persist config failed: {err}");
                    let _ = send_config_response(
                        &sender,
                        &msg,
                        payload.subsystem.as_str(),
                        next_config.version,
                        "error",
                        Some("PERSIST_FAILED".to_string()),
                        Some(err.to_string()),
                        &hive_id,
                    ).await;
                    continue;
                }
                sy_config = next_config;
                tracing::info!(
                    subsystem = %payload.subsystem,
                    payload_version = payload.version,
                    applied_version = sy_config.version,
                    "config changed applied"
                );
                let _ = send_config_response(
                    &sender,
                    &msg,
                    payload.subsystem.as_str(),
                    sy_config.version,
                    "ok",
                    None,
                    None,
                    &hive_id,
                ).await;
            }
        }
    }
}

async fn handle_node_config_get(
    sender: &NodeSender,
    msg: &Message,
    node_name: &str,
    hive_id: &str,
    sy_config: &SyConfigFile,
) -> Result<(), Box<dyn std::error::Error>> {
    let requested_node_name = match parse_node_config_request(msg) {
        Ok(NodeConfigControlRequest::Get(req)) => req.node_name,
        Ok(NodeConfigControlRequest::Set(_)) => {
            send_node_config_control_response(
                sender,
                msg,
                serde_json::json!({
                    "ok": false,
                    "node_name": node_name,
                    "state": "error",
                    "error": {
                        "code": "INVALID_CONFIG_GET",
                        "detail": "received CONFIG_SET while handling CONFIG_GET"
                    }
                }),
            )
            .await?;
            return Ok(());
        }
        Err(err) => {
            send_node_config_control_response(
                sender,
                msg,
                serde_json::json!({
                    "ok": false,
                    "node_name": node_name,
                    "state": "error",
                    "error": {
                        "code": "INVALID_CONFIG_GET",
                        "detail": err.to_string()
                    }
                }),
            )
            .await?;
            return Ok(());
        }
    };

    send_node_config_control_response(
        sender,
        msg,
        build_node_config_get_payload(node_name, hive_id, sy_config, &requested_node_name),
    )
    .await
}

async fn handle_node_config_set(
    sender: &NodeSender,
    msg: &Message,
    config_dir: &Path,
    node_name: &str,
    hive_id: &str,
    sy_config: &mut SyConfigFile,
    writer: &mut ConfigRegionWriter,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match parse_node_config_request(msg) {
        Ok(NodeConfigControlRequest::Set(req)) => req,
        Ok(NodeConfigControlRequest::Get(_)) => {
            send_node_config_control_response(
                sender,
                msg,
                serde_json::json!({
                    "ok": false,
                    "node_name": node_name,
                    "state": "error",
                    "error": {
                        "code": "INVALID_CONFIG_SET",
                        "detail": "received CONFIG_GET while handling CONFIG_SET"
                    }
                }),
            )
            .await?;
            return Ok(());
        }
        Err(err) => {
            send_node_config_control_response(
                sender,
                msg,
                serde_json::json!({
                    "ok": false,
                    "node_name": node_name,
                    "state": "error",
                    "error": {
                        "code": "INVALID_CONFIG_SET",
                        "detail": err.to_string()
                    }
                }),
            )
            .await?;
            return Ok(());
        }
    };

    let (next_config, changed_fields) = match apply_node_config_set_request(sy_config, &request) {
        Ok(result) => result,
        Err(err) => {
            send_node_config_control_response(
                sender,
                msg,
                serde_json::json!({
                    "ok": false,
                    "node_name": node_name,
                    "state": "error",
                    "schema_version": request.schema_version,
                    "config_version": sy_config.version,
                    "error": {
                        "code": "INVALID_CONFIG_SET",
                        "detail": err.to_string()
                    }
                }),
            )
            .await?;
            return Ok(());
        }
    };

    if next_config != *sy_config {
        apply_config(writer, hive_id, &next_config)?;
        write_config(config_dir, &next_config)?;
        *sy_config = next_config;
    }

    send_node_config_control_response(
        sender,
        msg,
        serde_json::json!({
            "ok": true,
            "node_name": node_name,
            "state": "configured",
            "schema_version": request.schema_version,
            "config_version": sy_config.version,
            "effective_config": {
                "routes": sy_config.routes,
                "vpns": sy_config.vpns,
                "taps": sy_config.taps,
                "updated_at": sy_config.updated_at,
                "hive": hive_id,
            },
            "applied_fields": changed_fields,
        }),
    )
    .await
}

async fn send_node_config_control_response(
    sender: &NodeSender,
    request: &Message,
    payload: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let reply = build_node_config_response_message(request, sender.uuid(), payload);
    sender.send(reply).await?;
    Ok(())
}

async fn send_config_response(
    sender: &NodeSender,
    request: &Message,
    subsystem: &str,
    version: u64,
    status: &str,
    error_code: Option<String>,
    error_detail: Option<String>,
    hive: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut payload = serde_json::json!({
        "subsystem": subsystem,
        "version": version,
        "status": status,
        "hive": hive,
    });
    if let Some(code) = error_code {
        payload["error_code"] = serde_json::Value::String(code);
    }
    if let Some(detail) = error_detail {
        payload["error_detail"] = serde_json::Value::String(detail);
    }
    let reply = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(request.routing.src.clone()),
            ttl: 16,
            trace_id: request.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some("CONFIG_RESPONSE".to_string()),
            src_ilk: None,
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload,
    };
    sender.send(reply).await?;
    Ok(())
}

fn build_node_config_get_payload(
    node_name: &str,
    hive_id: &str,
    sy_config: &SyConfigFile,
    requested_node_name: &str,
) -> serde_json::Value {
    let state =
        if sy_config.routes.is_empty() && sy_config.vpns.is_empty() && sy_config.taps.is_empty() {
            "empty"
        } else {
            "configured"
        };
    serde_json::json!({
        "ok": true,
        "node_name": node_name,
        "state": state,
        "schema_version": 1,
        "config_version": sy_config.version,
        "effective_config": {
            "routes": sy_config.routes,
            "vpns": sy_config.vpns,
            "taps": sy_config.taps,
            "updated_at": sy_config.updated_at,
            "hive": hive_id,
        },
        "contract": {
            "supports": [MSG_CONFIG_GET, MSG_CONFIG_SET],
            "target": NODE_CONFIG_CONTROL_TARGET,
            "schema_version": 1,
            "apply_modes": [NODE_CONFIG_APPLY_MODE_REPLACE],
            "config_schema": "sy_config_routes_v2",
            "requested_node_name": requested_node_name,
        }
    })
}

async fn handle_admin_action(
    sender: &NodeSender,
    msg: &Message,
    config_dir: &Path,
    hive_id: &str,
    sy_config: &mut SyConfigFile,
    writer: &mut ConfigRegionWriter,
) -> Result<(), Box<dyn std::error::Error>> {
    let action = msg.meta.action.as_deref().unwrap_or("");
    let reply_payload = match action {
        "list_routes" => admin_success_payload(
            action,
            serde_json::json!({
                "config_version": sy_config.version,
                "routes": sy_config.routes.clone(),
            }),
        ),
        "list_vpns" => admin_success_payload(
            action,
            serde_json::json!({
                "config_version": sy_config.version,
                "vpns": sy_config.vpns.clone(),
            }),
        ),
        "add_route" => {
            let route: RouteConfig = serde_json::from_value(msg.payload.clone())?;
            if sy_config.routes.iter().any(|r| r.prefix == route.prefix) {
                admin_error_payload(
                    action,
                    "DUPLICATE_PREFIX",
                    format!("Route with prefix '{}' already exists", route.prefix),
                )
            } else {
                sy_config.routes.push(route.clone());
                sy_config.version = sy_config.version.saturating_add(1);
                sy_config.updated_at = now_epoch_ms().to_string();
                apply_config(writer, hive_id, sy_config)?;
                write_config(config_dir, sy_config)?;
                admin_success_payload(
                    action,
                    serde_json::json!({
                        "config_version": sy_config.version,
                        "route": route,
                    }),
                )
            }
        }
        "delete_route" => {
            let payload: serde_json::Value = msg.payload.clone();
            let prefix = payload.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
            let before = sy_config.routes.len();
            sy_config.routes.retain(|r| r.prefix != prefix);
            if sy_config.routes.len() == before {
                admin_error_payload(
                    action,
                    "NOT_FOUND",
                    format!("Route with prefix '{}' not found", prefix),
                )
            } else {
                sy_config.version = sy_config.version.saturating_add(1);
                sy_config.updated_at = now_epoch_ms().to_string();
                apply_config(writer, hive_id, sy_config)?;
                write_config(config_dir, sy_config)?;
                admin_success_payload(
                    action,
                    serde_json::json!({
                        "config_version": sy_config.version,
                    }),
                )
            }
        }
        "add_vpn" => {
            let vpn: VpnConfig = serde_json::from_value(msg.payload.clone())?;
            if sy_config.vpns.iter().any(|v| v.pattern == vpn.pattern) {
                admin_error_payload(
                    action,
                    "DUPLICATE_PATTERN",
                    format!("VPN with pattern '{}' already exists", vpn.pattern),
                )
            } else {
                sy_config.vpns.push(vpn.clone());
                sy_config.version = sy_config.version.saturating_add(1);
                sy_config.updated_at = now_epoch_ms().to_string();
                apply_config(writer, hive_id, sy_config)?;
                write_config(config_dir, sy_config)?;
                admin_success_payload(
                    action,
                    serde_json::json!({
                        "config_version": sy_config.version,
                        "vpn": vpn,
                    }),
                )
            }
        }
        "delete_vpn" => {
            let payload: serde_json::Value = msg.payload.clone();
            let pattern = payload
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let before = sy_config.vpns.len();
            sy_config.vpns.retain(|v| v.pattern != pattern);
            if sy_config.vpns.len() == before {
                admin_error_payload(
                    action,
                    "NOT_FOUND",
                    format!("VPN with pattern '{}' not found", pattern),
                )
            } else {
                sy_config.version = sy_config.version.saturating_add(1);
                sy_config.updated_at = now_epoch_ms().to_string();
                apply_config(writer, hive_id, sy_config)?;
                write_config(config_dir, sy_config)?;
                admin_success_payload(
                    action,
                    serde_json::json!({
                        "config_version": sy_config.version,
                    }),
                )
            }
        }
        "list_taps" => admin_success_payload(
            action,
            serde_json::json!({
                "config_version": sy_config.version,
                "taps": sy_config.taps.clone(),
            }),
        ),
        "add_tap" => {
            let tap: TapConfig = serde_json::from_value(msg.payload.clone())?;
            // Natural key for taps is the (match_src, match_dst, target) triple.
            // Adding a duplicate triple is rejected so the operator notices.
            let duplicate = sy_config.taps.iter().any(|t| {
                t.match_src == tap.match_src
                    && t.match_dst == tap.match_dst
                    && t.target == tap.target
            });
            if duplicate {
                admin_error_payload(
                    action,
                    "DUPLICATE_TAP",
                    format!(
                        "Tap (match_src={}, match_dst={}, target={}) already exists",
                        tap.match_src, tap.match_dst, tap.target
                    ),
                )
            } else {
                sy_config.taps.push(tap.clone());
                sy_config.version = sy_config.version.saturating_add(1);
                sy_config.updated_at = now_epoch_ms().to_string();
                apply_config(writer, hive_id, sy_config)?;
                write_config(config_dir, sy_config)?;
                admin_success_payload(
                    action,
                    serde_json::json!({
                        "config_version": sy_config.version,
                        "tap": tap,
                    }),
                )
            }
        }
        "delete_tap" => {
            let payload: serde_json::Value = msg.payload.clone();
            let match_src = payload
                .get("match_src")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let match_dst = payload
                .get("match_dst")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let target = payload.get("target").and_then(|v| v.as_str()).unwrap_or("");
            let before = sy_config.taps.len();
            sy_config.taps.retain(|t| {
                !(t.match_src == match_src && t.match_dst == match_dst && t.target == target)
            });
            if sy_config.taps.len() == before {
                admin_error_payload(
                    action,
                    "NOT_FOUND",
                    format!(
                        "Tap (match_src={}, match_dst={}, target={}) not found",
                        match_src, match_dst, target
                    ),
                )
            } else {
                sy_config.version = sy_config.version.saturating_add(1);
                sy_config.updated_at = now_epoch_ms().to_string();
                apply_config(writer, hive_id, sy_config)?;
                write_config(config_dir, sy_config)?;
                admin_success_payload(
                    action,
                    serde_json::json!({
                        "config_version": sy_config.version,
                    }),
                )
            }
        }
        _ => admin_error_payload(
            action,
            "UNKNOWN_ACTION",
            format!("Unsupported admin action '{}'", action),
        ),
    };

    let reply = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(msg.routing.src.clone()),
            ttl: 16,
            trace_id: msg.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: "admin".to_string(),
            msg: None,
            src_ilk: None,
            scope: None,
            target: None,
            action: Some(action.to_string()),
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload: reply_payload,
    };
    sender.send(reply).await?;
    Ok(())
}

fn admin_success_payload(action: &str, payload: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "action": action,
        "payload": payload,
    })
}

fn admin_error_payload(action: &str, code: &str, detail: String) -> serde_json::Value {
    serde_json::json!({
        "status": "error",
        "action": action,
        "payload": serde_json::Value::Null,
        "error_code": code,
        "error_detail": detail,
    })
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, Box<dyn std::error::Error>> {
    let data = fs::read_to_string(config_dir.join("hive.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, hive_id)
    }
}

fn load_router_uuid(
    state_dir: &Path,
    router_l2_name: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let identity_path = state_dir.join(router_l2_name).join("identity.yaml");
    let data = fs::read_to_string(identity_path)?;
    let value: serde_yaml::Value = serde_yaml::from_str(&data)?;
    let uuid = value
        .get("layer1")
        .and_then(|v| v.get("uuid"))
        .and_then(|v| v.as_str())
        .ok_or("missing layer1.uuid")?;
    Ok(Uuid::parse_str(uuid)?)
}

fn load_config(config_dir: &Path) -> Result<SyConfigFile, Box<dyn std::error::Error>> {
    let path = config_dir.join("sy-config-routes.yaml");
    if !path.exists() {
        let empty = SyConfigFile {
            version: 1,
            updated_at: "".to_string(),
            routes: Vec::new(),
            vpns: Vec::new(),
            taps: Vec::new(),
        };
        let data = serde_yaml::to_string(&empty)?;
        fs::write(&path, data)?;
        return Ok(empty);
    }
    let data = fs::read_to_string(&path)?;
    let mut cfg: SyConfigFile = serde_yaml::from_str(&data)?;
    if cfg.updated_at.trim().is_empty() {
        cfg.updated_at = now_epoch_ms().to_string();
        let data = serde_yaml::to_string(&cfg)?;
        fs::write(&path, data)?;
    }
    Ok(cfg)
}

fn write_config(
    config_dir: &Path,
    config: &SyConfigFile,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_dir.join("sy-config-routes.yaml");
    let data = serde_yaml::to_string(config)?;
    fs::write(&path, data)?;
    Ok(())
}

/// The frozen, system-owned `SY.` origin-authority markers, one per authorized
/// protected-SYSTEM origin pattern. Published into the route table for
/// OBSERVABILITY only: they carry FLAG_ACTIVE (so config readers surface them) and
/// FLAG_FROZEN (so the router FIB skips them — they never affect forwarding).
///
/// Enforcement is the router's `system_policy::authorize_system` gate (the single source of
/// truth); these entries are the visible mirror of that rule set and must be kept
/// in sync with it by hand. `SY.orchestrator@*` is any-hive (cross-hive forwards);
/// the rest are this hive's control plane.
fn frozen_system_authority_routes(hive_id: &str) -> Vec<StaticRouteEntry> {
    let patterns = [
        ("SY.orchestrator@*".to_string(), MATCH_GLOB),
        (format!("SY.admin@{hive_id}"), MATCH_EXACT),
        (format!("SY.wf-rules@{hive_id}"), MATCH_EXACT),
        (format!("WF.orch.diag@{hive_id}"), MATCH_EXACT),
    ];
    patterns
        .into_iter()
        .map(|(pattern, kind)| {
            let mut entry = empty_static_route();
            entry.prefix_len = copy_bytes_with_len(&mut entry.prefix, &pattern) as u16;
            entry.match_kind = kind;
            // Benign action; never consulted because the router FIB skips FLAG_FROZEN.
            entry.action = ACTION_FORWARD;
            entry.priority = 0;
            entry.flags = FLAG_ACTIVE | FLAG_FROZEN;
            entry.installed_at = now_epoch_ms();
            entry
        })
        .collect()
}

fn apply_config(
    writer: &mut ConfigRegionWriter,
    hive_id: &str,
    sy_config: &SyConfigFile,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut routes = build_routes(&sy_config.routes)?;
    // write_static_routes REPLACES the whole table on every apply, so the frozen
    // markers must be re-appended here each time (boot + every admin mutation).
    routes.extend(frozen_system_authority_routes(hive_id));
    let vpns = build_vpns(&sy_config.vpns)?;
    let taps = build_taps(&sy_config.taps)?;
    writer.write_static_routes(&routes, false)?;
    writer.write_vpn_assignments(&vpns, false)?;
    // ROUTER-TAP-4: bump config_version on the taps write so the router
    // (which watches config_version) re-reads the snapshot and picks up
    // the new tap set in a single observable seqlock generation.
    writer.write_taps(&taps, true)?;
    tracing::info!(
        routes = routes.len(),
        vpns = vpns.len(),
        taps = taps.len(),
        "config region written"
    );
    for vpn in &sy_config.vpns {
        tracing::info!(
            pattern = %vpn.pattern,
            match_kind = %vpn.match_kind.as_deref().unwrap_or("PREFIX"),
            vpn_id = vpn.vpn_id,
            "vpn rule loaded"
        );
    }
    for tap in &sy_config.taps {
        tracing::info!(
            match_src = %tap.match_src,
            match_dst = %tap.match_dst,
            target = %tap.target,
            mode = %tap.mode,
            enabled = tap.enabled,
            "tap rule loaded"
        );
    }
    Ok(())
}

fn build_routes(
    routes: &[RouteConfig],
) -> Result<Vec<StaticRouteEntry>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for route in routes {
        let mut entry = empty_static_route();
        entry.prefix_len = copy_bytes_with_len(&mut entry.prefix, &route.prefix) as u16;
        entry.match_kind = match_kind(route.match_kind.as_deref())?;
        entry.action = action_kind(&route.action)?;
        if let Some(next) = &route.next_hop_hive {
            entry.next_hop_hive_len = copy_bytes_with_len(&mut entry.next_hop_hive, next) as u8;
        }
        entry.metric = route.metric.unwrap_or(0);
        entry.priority = route.priority.unwrap_or(100);
        entry.flags = FLAG_ACTIVE;
        entry.installed_at = now_epoch_ms();
        out.push(entry);
    }
    Ok(out)
}

fn build_vpns(vpns: &[VpnConfig]) -> Result<Vec<VpnAssignment>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for vpn in vpns {
        let mut entry = empty_vpn_assignment();
        entry.pattern_len = copy_bytes_with_len(&mut entry.pattern, &vpn.pattern) as u16;
        entry.match_kind = match_kind(vpn.match_kind.as_deref())?;
        entry.vpn_id = vpn.vpn_id;
        entry.priority = vpn.priority.unwrap_or(100);
        entry.flags = FLAG_ACTIVE;
        out.push(entry);
    }
    Ok(out)
}

fn parse_routes(
    value: &serde_json::Value,
) -> Result<Option<Vec<RouteConfig>>, Box<dyn std::error::Error>> {
    if value.is_null() {
        return Ok(None);
    }
    if value.is_array() {
        let routes: Vec<RouteConfig> = serde_json::from_value(value.clone())?;
        return Ok(Some(routes));
    }
    if let Some(routes_value) = value.get("routes") {
        let routes: Vec<RouteConfig> = serde_json::from_value(routes_value.clone())?;
        return Ok(Some(routes));
    }
    Ok(None)
}

fn parse_vpns(
    value: &serde_json::Value,
) -> Result<Option<Vec<VpnConfig>>, Box<dyn std::error::Error>> {
    if value.is_null() {
        return Ok(None);
    }
    if value.is_array() {
        let vpns: Vec<VpnConfig> = serde_json::from_value(value.clone())?;
        return Ok(Some(vpns));
    }
    if let Some(vpns_value) = value.get("vpns") {
        let vpns: Vec<VpnConfig> = serde_json::from_value(vpns_value.clone())?;
        return Ok(Some(vpns));
    }
    Ok(None)
}

fn parse_taps(
    value: &serde_json::Value,
) -> Result<Option<Vec<TapConfig>>, Box<dyn std::error::Error>> {
    if value.is_null() {
        return Ok(None);
    }
    if value.is_array() {
        let taps: Vec<TapConfig> = serde_json::from_value(value.clone())?;
        return Ok(Some(taps));
    }
    if let Some(taps_value) = value.get("taps") {
        let taps: Vec<TapConfig> = serde_json::from_value(taps_value.clone())?;
        return Ok(Some(taps));
    }
    Ok(None)
}

fn build_taps(taps: &[TapConfig]) -> Result<Vec<TapEntry>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for tap in taps {
        let mut entry = empty_tap_entry();
        entry.match_src_len = copy_bytes_with_len(&mut entry.match_src, &tap.match_src) as u16;
        entry.match_dst_len = copy_bytes_with_len(&mut entry.match_dst, &tap.match_dst) as u16;
        entry.target_len = copy_bytes_with_len(&mut entry.target, &tap.target) as u16;
        entry.mode = tap_mode_kind(&tap.mode)?;
        entry.enabled = if tap.enabled { 1 } else { 0 };
        entry.flags = FLAG_ACTIVE;
        entry.installed_at = now_epoch_ms();
        out.push(entry);
    }
    Ok(out)
}

fn tap_mode_kind(mode: &str) -> Result<u8, Box<dyn std::error::Error>> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "best_effort" | "" => Ok(0),
        other => Err(format!("invalid tap mode '{other}' (expected 'best_effort')").into()),
    }
}

fn apply_node_config_set_request(
    current: &SyConfigFile,
    request: &NodeConfigSetPayload,
) -> Result<(SyConfigFile, Vec<&'static str>), Box<dyn std::error::Error>> {
    if request.apply_mode != NODE_CONFIG_APPLY_MODE_REPLACE {
        return Err(format!("unsupported apply_mode '{}'", request.apply_mode).into());
    }
    let config = request
        .config
        .as_object()
        .ok_or("config must be a JSON object")?;

    let mut next = current.clone();
    let mut changed_fields = Vec::new();

    if config.get("routes").is_some() {
        let routes = parse_routes(&request.config)?.ok_or("routes must be present")?;
        if routes != current.routes {
            next.routes = routes;
            changed_fields.push("routes");
        }
    }
    if config.get("vpns").is_some() {
        let vpns = parse_vpns(&request.config)?.ok_or("vpns must be present")?;
        if vpns != current.vpns {
            next.vpns = vpns;
            changed_fields.push("vpns");
        }
    }
    if config.get("taps").is_some() {
        let taps = parse_taps(&request.config)?.ok_or("taps must be present")?;
        if taps != current.taps {
            next.taps = taps;
            changed_fields.push("taps");
        }
    }
    if config.get("routes").is_none()
        && config.get("vpns").is_none()
        && config.get("taps").is_none()
    {
        return Err("config must include 'routes', 'vpns', or 'taps'".into());
    }

    if !changed_fields.is_empty() {
        next.version = if request.config_version == 0 {
            current.version.saturating_add(1)
        } else {
            request.config_version
        };
        next.updated_at = now_epoch_ms().to_string();
    }

    Ok((next, changed_fields))
}

fn match_kind(value: Option<&str>) -> Result<u8, Box<dyn std::error::Error>> {
    match value.unwrap_or("PREFIX") {
        "EXACT" => Ok(MATCH_EXACT),
        "PREFIX" => Ok(MATCH_PREFIX),
        "GLOB" => Ok(MATCH_GLOB),
        _ => Err("invalid match_kind".into()),
    }
}

fn action_kind(value: &str) -> Result<u8, Box<dyn std::error::Error>> {
    match value {
        "FORWARD" => Ok(ACTION_FORWARD),
        "DROP" => Ok(ACTION_DROP),
        _ => Err(format!("invalid action '{value}'").into()),
    }
}

fn ensure_dir(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(path)
}

fn load_or_create_uuid(dir: &Path, base_name: &str) -> Result<Uuid, Box<dyn std::error::Error>> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.uuid", base_name));
    if path.exists() {
        let data = fs::read_to_string(&path)?;
        return Ok(Uuid::parse_str(data.trim())?);
    }
    let uuid = Uuid::new_v4();
    fs::write(&path, uuid.to_string())?;
    Ok(uuid)
}

fn empty_static_route() -> StaticRouteEntry {
    StaticRouteEntry {
        prefix: [0u8; 256],
        prefix_len: 0,
        match_kind: 0,
        action: 0,
        next_hop_hive: [0u8; 32],
        next_hop_hive_len: 0,
        _pad: [0u8; 3],
        metric: 0,
        priority: 0,
        flags: 0,
        installed_at: 0,
        _reserved: [0u8; 8],
    }
}

fn empty_vpn_assignment() -> VpnAssignment {
    VpnAssignment {
        pattern: [0u8; 256],
        pattern_len: 0,
        match_kind: 0,
        _pad0: 0,
        vpn_id: 0,
        priority: 0,
        flags: 0,
        _reserved: [0u8; 20],
    }
}

fn empty_tap_entry() -> TapEntry {
    TapEntry {
        match_src: [0u8; 256],
        match_src_len: 0,
        _pad0: [0u8; 6],
        match_dst: [0u8; 256],
        match_dst_len: 0,
        _pad1: [0u8; 6],
        target: [0u8; 256],
        target_len: 0,
        _pad2: [0u8; 6],
        mode: 0,
        enabled: 0,
        flags: 0,
        installed_at: 0,
        _reserved: [0u8; 32],
    }
}

// CONFIG_CHANGED lo emite SY.admin (motherbee).

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_config() -> SyConfigFile {
        SyConfigFile {
            version: 3,
            updated_at: "100".to_string(),
            routes: vec![RouteConfig {
                prefix: "support/".to_string(),
                match_kind: Some("PREFIX".to_string()),
                action: "FORWARD".to_string(),
                next_hop_hive: Some("worker-1".to_string()),
                metric: Some(10),
                priority: Some(100),
            }],
            vpns: vec![VpnConfig {
                pattern: "tenant/*".to_string(),
                match_kind: Some("GLOB".to_string()),
                vpn_id: 7,
                priority: Some(100),
            }],
            taps: vec![TapConfig {
                match_src: "IO.api.sales@motherbee".to_string(),
                match_dst: "AI.sales@motherbee".to_string(),
                target: "IO.slack.sales@motherbee".to_string(),
                mode: "best_effort".to_string(),
                enabled: true,
            }],
        }
    }

    #[test]
    fn build_node_config_get_payload_exposes_contract() {
        let payload = build_node_config_get_payload(
            "SY.config.routes@motherbee",
            "motherbee",
            &sample_config(),
            "SY.config.routes@motherbee",
        );
        assert_eq!(payload["ok"], json!(true));
        assert_eq!(payload["node_name"], json!("SY.config.routes@motherbee"));
        assert_eq!(
            payload["contract"]["supports"],
            json!([MSG_CONFIG_GET, MSG_CONFIG_SET])
        );
        assert_eq!(
            payload["effective_config"]["routes"][0]["prefix"],
            json!("support/")
        );
        assert_eq!(
            payload["effective_config"]["taps"][0]["target"],
            json!("IO.slack.sales@motherbee")
        );
    }

    #[test]
    fn apply_node_config_set_request_replaces_selected_sections() {
        let current = sample_config();
        let request = NodeConfigSetPayload {
            node_name: "SY.config.routes@motherbee".to_string(),
            schema_version: 1,
            config_version: 8,
            apply_mode: NODE_CONFIG_APPLY_MODE_REPLACE.to_string(),
            config: json!({
                "routes": [{
                    "prefix": "billing/",
                    "match_kind": "PREFIX",
                    "action": "FORWARD",
                    "next_hop_hive": "worker-2",
                    "metric": 5,
                    "priority": 90
                }]
            }),
            ..Default::default()
        };

        let (next, changed) = apply_node_config_set_request(&current, &request).unwrap();
        assert_eq!(changed, vec!["routes"]);
        assert_eq!(next.version, 8);
        assert_eq!(next.routes[0].prefix, "billing/");
        assert_eq!(next.vpns, current.vpns);
        assert_eq!(next.taps, current.taps);
    }

    #[test]
    fn apply_node_config_set_request_replaces_taps_section() {
        let current = sample_config();
        let request = NodeConfigSetPayload {
            node_name: "SY.config.routes@motherbee".to_string(),
            schema_version: 1,
            config_version: 9,
            apply_mode: NODE_CONFIG_APPLY_MODE_REPLACE.to_string(),
            config: json!({
                "taps": [{
                    "match_src": "IO.api.sales@motherbee",
                    "match_dst": "AI.sales@motherbee",
                    "target": "WF.sales@motherbee",
                    "mode": "best_effort",
                    "enabled": true
                }]
            }),
            ..Default::default()
        };

        let (next, changed) = apply_node_config_set_request(&current, &request).unwrap();
        assert_eq!(changed, vec!["taps"]);
        assert_eq!(next.version, 9);
        assert_eq!(next.taps[0].target, "WF.sales@motherbee");
        assert_eq!(next.routes, current.routes);
        assert_eq!(next.vpns, current.vpns);
    }

    #[test]
    fn apply_node_config_set_request_rejects_non_object_config() {
        let current = sample_config();
        let request = NodeConfigSetPayload {
            node_name: "SY.config.routes@motherbee".to_string(),
            schema_version: 1,
            config_version: 8,
            apply_mode: NODE_CONFIG_APPLY_MODE_REPLACE.to_string(),
            config: json!("bad"),
            ..Default::default()
        };
        let err = apply_node_config_set_request(&current, &request).unwrap_err();
        assert!(err.to_string().contains("JSON object"));
    }

    #[test]
    fn admin_error_payload_uses_canonical_fields() {
        let payload = admin_error_payload("delete_route", "NOT_FOUND", "missing route".to_string());
        assert_eq!(payload["status"], json!("error"));
        assert_eq!(payload["action"], json!("delete_route"));
        assert_eq!(payload["payload"], serde_json::Value::Null);
        assert_eq!(payload["error_code"], json!("NOT_FOUND"));
        assert_eq!(payload["error_detail"], json!("missing route"));
        assert!(payload.get("error").is_none());
        assert!(payload.get("message").is_none());
    }

    #[test]
    fn admin_success_payload_uses_canonical_envelope() {
        let payload = admin_success_payload(
            "list_routes",
            json!({
                "config_version": 4,
                "routes": [{ "prefix": "alpha/**" }],
            }),
        );
        assert_eq!(payload["status"], json!("ok"));
        assert_eq!(payload["action"], json!("list_routes"));
        assert_eq!(
            payload["payload"],
            json!({
                "config_version": 4,
                "routes": [{ "prefix": "alpha/**" }],
            })
        );
        assert!(payload.get("config_version").is_none());
        assert!(payload.get("routes").is_none());
    }
}
