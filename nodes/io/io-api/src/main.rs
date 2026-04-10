#![forbid(unsafe_code)]

use anyhow::Result;
use fluxbee_sdk::protocol::{Destination, Message as WireMessage, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    build_node_secret_record, connect, load_node_secret_record, load_node_secret_record_with_root,
    managed_node_config_path, save_node_secret_record, save_node_secret_record_with_root,
    try_handle_default_node_status, NodeConfig, NodeSecretError, NodeSecretRecord,
    NodeSecretWriteOptions, NodeUuidMode, FLUXBEE_NODE_NAME_ENV,
};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_api_adapter_config::{api_key_storage_key, IoApiAdapterConfigContract};
use io_common::io_control_plane::{
    build_io_config_get_response_payload, build_io_config_response_message,
    build_io_config_set_error_payload, build_io_config_set_ok_payload,
    ensure_config_version_advances, parse_and_validate_io_control_plane_request, IoConfigSource,
    IoControlPlaneErrorInfo, IoControlPlaneRequest, IoControlPlaneState, IoNodeLifecycleState,
};
use io_common::io_control_plane_bootstrap::bootstrap_io_control_plane_state;
use io_common::io_control_plane_logging::{
    log_config_get_served, log_config_set_applied, log_config_set_invalid_runtime_credentials,
    log_config_set_persist_error, log_config_set_stale_rejected,
    log_control_plane_request_rejected,
};
use io_common::io_control_plane_metrics::IoControlPlaneMetrics;
use io_common::io_control_plane_store::{default_state_dir, persist_io_control_plane_state};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,io_api=debug,fluxbee_sdk=info")
    });
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    tracing::info!(
        node_name = %config.node_name,
        island_id = %config.island_id,
        router_socket = %config.router_socket.display(),
        state_dir = %config.state_dir.display(),
        spawn_config_path = %config.spawn_config_path.display(),
        "io-api starting"
    );

    let (sender, mut receiver) = connect(&NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    })
    .await?;

    tracing::info!(
        full_name = %receiver.full_name(),
        vpn_id = %receiver.vpn_id(),
        "io-api connected to router"
    );

    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoApiAdapterConfigContract);
    let mut boot_state = bootstrap_io_control_plane_state(&config.state_dir, &config.node_name)
        .unwrap_or_else(|err| {
            tracing::warn!(
                error = %err,
                state_dir = %config.state_dir.display(),
                node_name = %config.node_name,
                "failed to bootstrap IO control-plane state; using UNCONFIGURED"
            );
            IoControlPlaneState::default()
        });
    let auth_registry = Arc::new(RwLock::new(ApiAuthRegistry::default()));

    if let Some(effective) = boot_state.effective_config.clone() {
        match prepare_runtime_api_config(&config.node_name, &effective, None) {
            Ok((sanitized_effective, registry)) => {
                tracing::info!(
                    key_count = registry.keys.len(),
                    config_source = boot_state.config_source.as_str(),
                    "io-api runtime auth initialized"
                );
                boot_state.effective_config = Some(sanitized_effective);
                *auth_registry.write().await = registry;
            }
            Err(err) => {
                boot_state.current_state = IoNodeLifecycleState::FailedConfig;
                boot_state.last_error = Some(IoControlPlaneErrorInfo {
                    code: "invalid_config".to_string(),
                    message: err.to_string(),
                });
                tracing::warn!(
                    node_name = %config.node_name,
                    error = %err,
                    "boot effective config is missing/invalid api auth config; starting in FAILED_CONFIG"
                );
            }
        }
    }

    let control_plane = Arc::new(RwLock::new(boot_state.clone()));
    let control_metrics = Arc::new(IoControlPlaneMetrics::with_initial_state(
        boot_state.current_state.as_str(),
        boot_state.config_version,
    ));
    let control_src = sender.full_name().to_string();

    loop {
        let msg = receiver.recv().await?;

        if try_handle_default_node_status(&sender, &msg).await? {
            continue;
        }

        if let Some(response) = handle_io_control_plane_message(
            &msg,
            &config.node_name,
            control_src.as_str(),
            &config.state_dir,
            control_plane.clone(),
            control_metrics.clone(),
            adapter_contract.as_ref(),
            auth_registry.clone(),
        )
        .await
        {
            sender.send(response).await?;
            continue;
        }

        tracing::debug!(
            msg_type = %msg.meta.msg_type,
            msg = %msg.meta.msg.as_deref().unwrap_or(""),
            trace_id = %msg.routing.trace_id,
            "io-api ignored non-control-plane message"
        );
    }
}

#[derive(Clone)]
struct Config {
    node_name: String,
    island_id: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    state_dir: PathBuf,
    spawn_config_path: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct ApiAuthRegistry {
    keys: Vec<ApiKeyRuntime>,
}

#[derive(Debug, Clone)]
struct ApiKeyRuntime {
    key_id: String,
    token: String,
    caller_identity: Option<Value>,
}

struct SpawnConfig {
    path: PathBuf,
    doc: Value,
}

impl Config {
    fn from_env() -> Result<Self> {
        let resolved_node_name = env(FLUXBEE_NODE_NAME_ENV).ok_or_else(|| {
            anyhow::anyhow!("missing required env {FLUXBEE_NODE_NAME_ENV} for managed spawn")
        })?;
        let resolved_island_id = hive_from_node_name(&resolved_node_name).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid {FLUXBEE_NODE_NAME_ENV}='{resolved_node_name}': expected <name>@<hive>"
            )
        })?;
        let spawn_cfg = load_spawn_config(&resolved_node_name)?;
        tracing::info!(path = %spawn_cfg.path.display(), "io-api loaded spawn config");
        let spawn_doc = Some(&spawn_cfg.doc);

        Ok(Self {
            node_name: resolved_node_name,
            island_id: resolved_island_id,
            node_version: env("NODE_VERSION")
                .or_else(|| {
                    json_get_string_opt(
                        spawn_doc,
                        &["_system.runtime_version", "runtime.version", "node.version"],
                    )
                })
                .unwrap_or_else(|| "0.1".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET")
                    .or_else(|| {
                        json_get_string_opt(spawn_doc, &["node.router_socket", "router_socket"])
                    })
                    .unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .or_else(|| {
                        json_get_string_opt(
                            spawn_doc,
                            &["node.uuid_persistence_dir", "uuid_persistence_dir"],
                        )
                    })
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR")
                    .or_else(|| json_get_string_opt(spawn_doc, &["node.config_dir", "config_dir"]))
                    .unwrap_or_else(|| "/etc/fluxbee".to_string()),
            ),
            state_dir: PathBuf::from(
                env("STATE_DIR")
                    .or_else(|| json_get_string_opt(spawn_doc, &["node.state_dir", "state_dir"]))
                    .unwrap_or_else(|| default_state_dir().display().to_string()),
            ),
            spawn_config_path: spawn_cfg.path,
        })
    }
}

fn load_spawn_config(node_name: &str) -> Result<SpawnConfig> {
    let path = managed_node_config_path(node_name)
        .map_err(|err| anyhow::anyhow!("failed to resolve managed config path: {err}"))?;
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        anyhow::anyhow!(
            "failed to read managed config file {}: {err}",
            path.display()
        )
    })?;
    let doc = serde_json::from_str::<Value>(&raw).map_err(|err| {
        anyhow::anyhow!(
            "failed to parse managed config JSON {}: {err}",
            path.display()
        )
    })?;
    Ok(SpawnConfig { path, doc })
}

fn hive_from_node_name(node_name: &str) -> Option<String> {
    node_name
        .split_once('@')
        .map(|(_, hive)| hive.trim())
        .filter(|hive| !hive.is_empty())
        .map(ToString::to_string)
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn json_get_string_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<String> {
    let doc = doc?;
    for path in dotted_paths {
        if let Some(value) = json_get_path(doc, path).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn json_get_path<'a>(root: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut current = root;
    for segment in dotted_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

async fn handle_io_control_plane_message(
    msg: &WireMessage,
    node_name: &str,
    control_src: &str,
    state_dir: &Path,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    auth_registry: Arc<RwLock<ApiAuthRegistry>>,
) -> Option<WireMessage> {
    let command = msg.meta.msg.as_deref().unwrap_or_default();
    if !is_control_plane_msg_type(&msg.meta.msg_type) {
        return None;
    }

    if command.eq_ignore_ascii_case("PING") {
        let state = control_plane.read().await.clone();
        let payload = serde_json::json!({
            "ok": true,
            "node_name": node_name,
            "state": state.current_state.as_str(),
        });
        return Some(build_system_reply(msg, control_src, "PONG", payload));
    }

    if command.eq_ignore_ascii_case("STATUS") {
        let state = control_plane.read().await.clone();
        let metrics = control_metrics.snapshot();
        let key_count = auth_registry.read().await.keys.len();
        let payload = serde_json::json!({
            "ok": true,
            "node_name": node_name,
            "state": state.current_state.as_str(),
            "config_source": state.config_source.as_str(),
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "last_error": state.last_error,
            "metrics": {
                "control_plane": metrics
            },
            "runtime": {
                "auth_key_count": key_count
            }
        });
        return Some(build_system_reply(
            msg,
            control_src,
            "STATUS_RESPONSE",
            payload,
        ));
    }

    if !command.eq_ignore_ascii_case("CONFIG_GET") && !command.eq_ignore_ascii_case("CONFIG_SET") {
        return None;
    }

    let payload = match parse_and_validate_io_control_plane_request(msg, node_name) {
        Ok(IoControlPlaneRequest::Get(_)) => {
            let state = control_plane.read().await.clone();
            let redacted = redact_state(&state, adapter_contract);
            log_config_get_served(msg.routing.trace_id.as_str(), node_name, &redacted);
            let mut payload = build_io_config_get_response_payload(
                node_name,
                &redacted,
                build_io_adapter_contract_payload(
                    adapter_contract,
                    state.effective_config.as_ref(),
                ),
            );
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "metrics".to_string(),
                    serde_json::json!({
                        "control_plane": control_metrics.snapshot(),
                        "auth": {
                            "key_count": auth_registry.read().await.keys.len()
                        }
                    }),
                );
            }
            payload
        }
        Ok(IoControlPlaneRequest::Set(set_payload)) => {
            apply_io_config_set(
                &set_payload,
                node_name,
                state_dir,
                control_plane.clone(),
                control_metrics,
                adapter_contract,
                auth_registry,
            )
            .await
        }
        Err(err) => {
            let state = control_plane.read().await.clone();
            let redacted = redact_state(&state, adapter_contract);
            let err_text = err.to_string();
            log_control_plane_request_rejected(
                msg.routing.trace_id.as_str(),
                node_name,
                err.code(),
                err_text.as_str(),
            );
            build_io_config_set_error_payload(node_name, &redacted, err.code(), err_text)
        }
    };

    let mut response = build_io_config_response_message(msg, payload);
    response.routing.src = control_src.to_string();
    Some(response)
}

fn is_control_plane_msg_type(msg_type: &str) -> bool {
    msg_type.eq_ignore_ascii_case(SYSTEM_KIND) || msg_type.eq_ignore_ascii_case("admin")
}

async fn apply_io_config_set(
    payload: &fluxbee_sdk::node_config::NodeConfigSetPayload,
    node_name: &str,
    state_dir: &Path,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    auth_registry: Arc<RwLock<ApiAuthRegistry>>,
) -> Value {
    let mut state = control_plane.write().await;

    if let Err(err) = ensure_config_version_advances(payload.config_version, state.config_version) {
        log_config_set_stale_rejected(node_name, payload.config_version, state.config_version);
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            state.config_version,
            err.code(),
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(
            node_name,
            &redacted,
            err.code(),
            err.to_string(),
        );
    }

    let candidate = match apply_adapter_config_replace(adapter_contract, &payload.config) {
        Ok(cfg) => cfg,
        Err(err) => {
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: err.code().to_string(),
                message: err.to_string(),
            });
            control_metrics.record_config_set_error(
                state.current_state.as_str(),
                payload.config_version,
                err.code(),
            );
            let redacted = redact_state(&state, adapter_contract);
            return build_io_config_set_error_payload(
                node_name,
                &redacted,
                err.code(),
                err.to_string(),
            );
        }
    };

    let (effective, registry) = match prepare_runtime_api_config(node_name, &candidate, None) {
        Ok(out) => out,
        Err(err) => {
            let err_text = err.to_string();
            log_config_set_invalid_runtime_credentials(
                node_name,
                payload.config_version,
                err_text.as_str(),
            );
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: err_text.clone(),
            });
            control_metrics.record_config_set_error(
                state.current_state.as_str(),
                payload.config_version,
                "invalid_config",
            );
            let redacted = redact_state(&state, adapter_contract);
            return build_io_config_set_error_payload(
                node_name,
                &redacted,
                "invalid_config",
                err_text,
            );
        }
    };

    let previous_effective = state.effective_config.clone();
    let mut apply_hot = Vec::<String>::new();
    let mut apply_reinit = Vec::<String>::new();
    let mut apply_restart_required = Vec::<String>::new();

    if section_changed(previous_effective.as_ref(), &effective, &["io"]) {
        apply_hot.push("io.*".to_string());
    }
    if section_changed(
        previous_effective.as_ref(),
        &effective,
        &["auth", "api_keys"],
    ) {
        apply_reinit.push("auth.api_keys".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["listen"]) {
        apply_restart_required.push("listen.*".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["ingress"]) {
        apply_restart_required.push("ingress.*".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["node"]) {
        apply_restart_required.push("node.*".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["runtime"]) {
        apply_restart_required.push("runtime.*".to_string());
    }

    state.current_state = IoNodeLifecycleState::Configured;
    state.config_source = IoConfigSource::Dynamic;
    state.schema_version = payload.schema_version;
    state.config_version = payload.config_version;
    state.effective_config = Some(effective.clone());
    state.last_error = None;

    if let Err(err) = persist_io_control_plane_state(state_dir, node_name, &state) {
        let err_text = err.to_string();
        log_config_set_persist_error(
            node_name,
            payload.schema_version,
            payload.config_version,
            err_text.as_str(),
        );
        state.current_state = IoNodeLifecycleState::FailedConfig;
        state.last_error = Some(IoControlPlaneErrorInfo {
            code: "config_persist_error".to_string(),
            message: err_text.clone(),
        });
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            payload.config_version,
            "config_persist_error",
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(
            node_name,
            &redacted,
            "config_persist_error",
            err_text,
        );
    }

    *auth_registry.write().await = registry.clone();
    tracing::info!(
        node_name = %node_name,
        key_count = registry.keys.len(),
        subject_mode = %extract_subject_mode(Some(&effective)).unwrap_or_else(|| "-".to_string()),
        "io-api auth registry hot-applied"
    );

    control_metrics.record_config_set_ok(state.current_state.as_str(), state.config_version);
    log_config_set_applied(
        node_name,
        payload.schema_version,
        payload.config_version,
        &apply_hot,
        &apply_reinit,
        &apply_restart_required,
    );

    let redacted = redact_state(&state, adapter_contract);
    let mut response = build_io_config_set_ok_payload(node_name, &redacted);
    if let Some(obj) = response.as_object_mut() {
        obj.insert(
            "apply".to_string(),
            serde_json::json!({
                "mode": "hot_reload",
                "hot_applied": apply_hot,
                "reinit_performed": apply_reinit,
                "restart_required": apply_restart_required
            }),
        );
    }
    response
}

fn prepare_runtime_api_config(
    node_name: &str,
    effective: &Value,
    secret_root: Option<&Path>,
) -> Result<(Value, ApiAuthRegistry)> {
    let sanitized = materialize_inline_api_keys(node_name, effective, secret_root)?;
    let registry = load_runtime_api_registry(node_name, &sanitized, secret_root)?;
    Ok((sanitized, registry))
}

fn materialize_inline_api_keys(
    node_name: &str,
    effective: &Value,
    secret_root: Option<&Path>,
) -> Result<Value> {
    let mut sanitized = effective.clone();
    let api_keys = sanitized
        .get_mut("auth")
        .and_then(Value::as_object_mut)
        .and_then(|auth| auth.get_mut("api_keys"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("missing config.auth.api_keys"))?;

    let mut secret_record = load_or_default_secret_record(node_name, secret_root)?;
    let mut secrets_changed = false;

    for entry in api_keys {
        let Some(entry_obj) = entry.as_object_mut() else {
            return Err(anyhow::anyhow!(
                "config.auth.api_keys[] entries must be objects"
            ));
        };
        let key_id = entry_obj
            .get("key_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[].key_id is required"))?;
        let inline_token = entry_obj
            .get("token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let Some(token) = inline_token else {
            continue;
        };

        let storage_key = api_key_storage_key(key_id);
        secret_record
            .secrets
            .insert(storage_key.clone(), Value::String(token));
        entry_obj.remove("token");
        entry_obj.insert(
            "token_ref".to_string(),
            Value::String(format!("local_file:{storage_key}")),
        );
        secrets_changed = true;
    }

    if secrets_changed {
        let record =
            build_node_secret_record(secret_record.secrets, &NodeSecretWriteOptions::default());
        save_secret_record(node_name, secret_root, &record)?;
    }

    Ok(sanitized)
}

fn load_runtime_api_registry(
    node_name: &str,
    effective: &Value,
    secret_root: Option<&Path>,
) -> Result<ApiAuthRegistry> {
    let api_keys = effective
        .get("auth")
        .and_then(|auth| auth.get("api_keys"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing config.auth.api_keys"))?;
    let mut registry = ApiAuthRegistry::default();
    let mut secret_record: Option<NodeSecretRecord> = None;

    for entry in api_keys {
        let Some(entry_obj) = entry.as_object() else {
            return Err(anyhow::anyhow!(
                "config.auth.api_keys[] entries must be objects"
            ));
        };
        let key_id = entry_obj
            .get("key_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[].key_id is required"))?;
        let token = resolve_api_key_token(node_name, entry_obj, &mut secret_record, secret_root)?;
        registry.keys.push(ApiKeyRuntime {
            key_id: key_id.to_string(),
            token,
            caller_identity: entry_obj.get("caller_identity").cloned(),
        });
    }

    Ok(registry)
}

fn resolve_api_key_token(
    node_name: &str,
    entry: &Map<String, Value>,
    secret_record: &mut Option<NodeSecretRecord>,
    secret_root: Option<&Path>,
) -> Result<String> {
    if let Some(token) = entry
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(token.to_string());
    }

    let reference = entry
        .get("token_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[] requires token or token_ref"))?;

    if let Some(var) = reference.strip_prefix("env:") {
        return env(var)
            .ok_or_else(|| anyhow::anyhow!("unresolved env secret reference '{reference}'"));
    }

    if let Some(storage_key) = reference.strip_prefix("local_file:") {
        if secret_record.is_none() {
            *secret_record = Some(load_or_default_secret_record(node_name, secret_root)?);
        }
        return secret_record
            .as_ref()
            .and_then(|record| record.secrets.get(storage_key))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| anyhow::anyhow!("missing local secret '{storage_key}'"));
    }

    Err(anyhow::anyhow!(
        "unsupported secret reference '{reference}' for config.auth.api_keys[].token_ref"
    ))
}

fn load_or_default_secret_record(
    node_name: &str,
    secret_root: Option<&Path>,
) -> Result<NodeSecretRecord> {
    match load_secret_record(node_name, secret_root) {
        Ok(record) => Ok(record),
        Err(NodeSecretError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(NodeSecretRecord::default())
        }
        Err(err) => Err(err.into()),
    }
}

fn load_secret_record(
    node_name: &str,
    secret_root: Option<&Path>,
) -> Result<NodeSecretRecord, NodeSecretError> {
    match secret_root {
        Some(root) => load_node_secret_record_with_root(node_name, root),
        None => load_node_secret_record(node_name),
    }
}

fn save_secret_record(
    node_name: &str,
    secret_root: Option<&Path>,
    record: &NodeSecretRecord,
) -> Result<()> {
    match secret_root {
        Some(root) => {
            save_node_secret_record_with_root(node_name, root, record)?;
        }
        None => {
            save_node_secret_record(node_name, record)?;
        }
    }
    Ok(())
}

fn section_changed(
    previous_effective: Option<&Value>,
    current_effective: &Value,
    path: &[&str],
) -> bool {
    value_at_path(previous_effective, path) != value_at_path(Some(current_effective), path)
}

fn value_at_path<'a>(root: Option<&'a Value>, path: &[&str]) -> Option<&'a Value> {
    let mut current = root?;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn extract_subject_mode(effective_config: Option<&Value>) -> Option<String> {
    effective_config
        .and_then(|cfg| cfg.get("ingress"))
        .and_then(|ingress| ingress.get("subject_mode"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn redact_state(
    state: &IoControlPlaneState,
    adapter_contract: &dyn IoAdapterConfigContract,
) -> IoControlPlaneState {
    let mut redacted = state.clone();
    redacted.effective_config = state
        .effective_config
        .as_ref()
        .map(|cfg| adapter_contract.redact_effective_config(cfg));
    redacted
}

fn build_system_reply(
    incoming: &WireMessage,
    control_src: &str,
    response_msg: &str,
    payload: Value,
) -> WireMessage {
    let dst = if incoming.routing.src.trim().is_empty() {
        Destination::Broadcast
    } else {
        Destination::Unicast(incoming.routing.src.clone())
    };
    WireMessage {
        routing: Routing {
            src: control_src.to_string(),
            dst,
            ttl: incoming.routing.ttl.max(1),
            trace_id: incoming.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(response_msg.to_string()),
            src_ilk: incoming.meta.src_ilk.clone(),
            dst_ilk: incoming.meta.dst_ilk.clone(),
            ich: incoming.meta.ich.clone(),
            thread_id: incoming.meta.thread_id.clone(),
            thread_seq: incoming.meta.thread_seq,
            ctx: None,
            ctx_seq: None,
            ctx_window: None,
            memory_package: incoming.meta.memory_package.clone(),
            scope: incoming.meta.scope.clone(),
            target: incoming.meta.target.clone(),
            action: Some(response_msg.to_string()),
            priority: incoming.meta.priority.clone(),
            context: incoming.meta.context.clone(),
            ..Meta::default()
        },
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or(0);
        let root =
            std::env::temp_dir().join(format!("io-api-{label}-{}-{now_ms}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    #[test]
    fn materialize_inline_api_key_moves_secret_to_local_file_ref() {
        let root = temp_root("materialize");
        let config = serde_json::json!({
            "auth": {
                "api_keys": [
                    {
                        "key_id": "partner-1",
                        "token": "secret-1"
                    }
                ]
            }
        });

        let sanitized =
            materialize_inline_api_keys("IO.api.demo@motherbee", &config, Some(root.as_path()))
                .expect("materialize");

        let entry = sanitized
            .get("auth")
            .and_then(|auth| auth.get("api_keys"))
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_object)
            .expect("entry");
        assert!(entry.get("token").is_none());
        assert_eq!(
            entry.get("token_ref").and_then(Value::as_str),
            Some("local_file:io_api_key__partner-1")
        );

        let record =
            load_node_secret_record_with_root("IO.api.demo@motherbee", &root).expect("load record");
        assert_eq!(
            record.secrets.get("io_api_key__partner-1"),
            Some(&Value::String("secret-1".to_string()))
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_runtime_api_registry_resolves_local_file_secret() {
        let root = temp_root("registry");
        let mut secrets = Map::new();
        secrets.insert(
            "io_api_key__partner_1".to_string(),
            Value::String("secret-2".to_string()),
        );
        let record = build_node_secret_record(secrets, &NodeSecretWriteOptions::default());
        save_node_secret_record_with_root("IO.api.demo@motherbee", &root, &record)
            .expect("save record");

        let config = serde_json::json!({
            "auth": {
                "api_keys": [
                    {
                        "key_id": "partner 1",
                        "token_ref": "local_file:io_api_key__partner_1",
                        "caller_identity": {
                            "external_user_id": "caller-1"
                        }
                    }
                ]
            }
        });

        let registry =
            load_runtime_api_registry("IO.api.demo@motherbee", &config, Some(root.as_path()))
                .expect("registry");

        assert_eq!(registry.keys.len(), 1);
        assert_eq!(registry.keys[0].key_id, "partner 1");
        assert_eq!(registry.keys[0].token, "secret-2");
        assert_eq!(
            registry.keys[0]
                .caller_identity
                .as_ref()
                .and_then(|value| value.get("external_user_id"))
                .and_then(Value::as_str),
            Some("caller-1")
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
