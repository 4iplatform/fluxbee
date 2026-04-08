use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::time;
use tracing_subscriber::EnvFilter;

use fluxbee_sdk::nats::resolve_local_nats_endpoint;
use fluxbee_sdk::protocol::{Message, SYSTEM_KIND};
use fluxbee_sdk::{
    action_class_requires_result, connect, managed_node_name, try_handle_default_node_status,
    ActionResult, NodeConfig, NodeReceiver, NodeSender, NodeUuidMode,
};
use json_router::nats::{NatsSubscriber as RouterNatsSubscriber, SUBJECT_STORAGE_TURNS};

type PolicyError = Box<dyn std::error::Error + Send + Sync>;

const POLICY_NODE_BASE_NAME: &str = "SY.policy";
const POLICY_NODE_VERSION: &str = "0.1";
const POLICY_TURNS_SID: u32 = 29;
const DURABLE_QUEUE_TURNS: &str = "durable.sy-policy.turns";
const NATS_ERROR_LOG_EVERY: u64 = 20;

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    nats: Option<NatsSection>,
}

#[derive(Debug, Deserialize)]
struct NatsSection {
    mode: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct PolicyRuntimeState {
    processed_turns_total: u64,
    contract_ready_turns_total: u64,
    invalid_turns_total: u64,
    missing_action_class_total: u64,
    missing_action_result_total: u64,
    missing_result_origin_total: u64,
    last_trace_id: Option<String>,
    last_action_class: Option<String>,
    last_action_result: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), PolicyError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_policy supports only Linux targets.");
        std::process::exit(1);
    }

    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let hive = load_hive(&config_dir).await?;
    let endpoint = resolve_local_nats_endpoint(&config_dir)?;
    let use_durable_consumer = hive
        .nats
        .as_ref()
        .and_then(|nats| nats.mode.as_deref())
        .map(|mode| mode.trim().eq_ignore_ascii_case("embedded"))
        .unwrap_or(true);
    let node_base_name = managed_node_name(POLICY_NODE_BASE_NAME, &["SY_POLICY_NODE_NAME"]);
    let node_name = ensure_l2_name(&node_base_name, &hive.hive_id);
    let node_config = NodeConfig {
        name: node_base_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: POLICY_NODE_VERSION.to_string(),
    };
    let runtime_state = Arc::new(Mutex::new(PolicyRuntimeState::default()));
    let nats_subscribe_errors = Arc::new(AtomicU64::new(0));

    let (mut sender, mut receiver) =
        connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!(
        node_name = %node_name,
        endpoint = %endpoint,
        use_durable_consumer,
        "sy.policy connected to router"
    );

    std::mem::drop(tokio::spawn(run_turns_loop(
        endpoint,
        use_durable_consumer,
        Arc::clone(&runtime_state),
        Arc::clone(&nats_subscribe_errors),
    )));

    let mut heartbeat = time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                let snapshot = runtime_state.lock().await;
                tracing::info!(
                    node_name = %node_name,
                    processed_turns_total = snapshot.processed_turns_total,
                    contract_ready_turns_total = snapshot.contract_ready_turns_total,
                    missing_action_class_total = snapshot.missing_action_class_total,
                    missing_action_result_total = snapshot.missing_action_result_total,
                    missing_result_origin_total = snapshot.missing_result_origin_total,
                    invalid_turns_total = snapshot.invalid_turns_total,
                    nats_subscribe_failures = nats_subscribe_errors.load(Ordering::Relaxed),
                    last_trace_id = ?snapshot.last_trace_id,
                    last_action_class = ?snapshot.last_action_class,
                    last_action_result = ?snapshot.last_action_result,
                    "sy.policy heartbeat"
                );
            }
            received = receiver.recv() => {
                let msg = match received {
                    Ok(msg) => msg,
                    Err(err) => {
                        tracing::warn!(error = %err, "sy.policy recv error; reconnecting");
                        let (new_sender, new_receiver) =
                            connect_with_retry(&node_config, Duration::from_secs(1)).await?;
                        sender = new_sender;
                        receiver = new_receiver;
                        tracing::info!(node_name = %sender.full_name(), "sy.policy reconnected to router");
                        continue;
                    }
                };
                if let Err(err) = process_router_message(&sender, &msg).await {
                    tracing::warn!(error = %err, msg_type = %msg.meta.msg_type, msg = ?msg.meta.msg, "sy.policy router message handling failed");
                }
            }
        }
    }
}

async fn run_turns_loop(
    endpoint: String,
    use_durable_consumer: bool,
    runtime_state: Arc<Mutex<PolicyRuntimeState>>,
    nats_subscribe_errors: Arc<AtomicU64>,
) {
    loop {
        let subscriber = if use_durable_consumer {
            RouterNatsSubscriber::new(
                endpoint.clone(),
                SUBJECT_STORAGE_TURNS.to_string(),
                POLICY_TURNS_SID,
            )
            .with_queue(DURABLE_QUEUE_TURNS)
        } else {
            RouterNatsSubscriber::new(
                endpoint.clone(),
                SUBJECT_STORAGE_TURNS.to_string(),
                POLICY_TURNS_SID,
            )
        };

        let run_state = Arc::clone(&runtime_state);
        let run_result = subscriber
            .run(move |payload| {
                let runtime_state = Arc::clone(&run_state);
                async move { handle_turn_payload(payload, runtime_state).await }
            })
            .await;

        if let Err(err) = run_result {
            let count = nats_subscribe_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if count == 1 || count % NATS_ERROR_LOG_EVERY == 0 {
                tracing::warn!(
                    subject = SUBJECT_STORAGE_TURNS,
                    error = %err,
                    failures = count,
                    "sy.policy turns subscribe loop failed; retrying"
                );
            }
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

async fn handle_turn_payload(
    payload: Vec<u8>,
    runtime_state: Arc<Mutex<PolicyRuntimeState>>,
) -> Result<(), std::io::Error> {
    let msg: Message = match serde_json::from_slice(&payload) {
        Ok(msg) => msg,
        Err(err) => {
            let mut state = runtime_state.lock().await;
            state.invalid_turns_total = state.invalid_turns_total.saturating_add(1);
            tracing::warn!(error = %err, "sy.policy received invalid turn payload");
            return Ok(());
        }
    };

    let mut state = runtime_state.lock().await;
    state.processed_turns_total = state.processed_turns_total.saturating_add(1);
    state.last_trace_id = Some(msg.routing.trace_id.clone());
    state.last_action_class = msg.meta.action_class.map(|value| value.to_string());
    state.last_action_result = msg.meta.action_result.map(|value| value.to_string());

    let Some(action_class) = msg.meta.action_class else {
        state.missing_action_class_total = state.missing_action_class_total.saturating_add(1);
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            msg_type = %msg.meta.msg_type,
            msg = ?msg.meta.msg,
            action = ?msg.meta.action,
            "sy.policy received turn without action_class; skipping"
        );
        return Ok(());
    };

    if action_class_requires_result(action_class) && msg.meta.action_result.is_none() {
        state.missing_action_result_total = state.missing_action_result_total.saturating_add(1);
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            action_class = %action_class,
            msg_type = %msg.meta.msg_type,
            msg = ?msg.meta.msg,
            action = ?msg.meta.action,
            "sy.policy received evaluable turn without action_result; skipping"
        );
        return Ok(());
    }

    if msg.meta.action_result == Some(ActionResult::Blocked) && msg.meta.result_origin.is_none() {
        state.missing_result_origin_total = state.missing_result_origin_total.saturating_add(1);
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            action_class = %action_class,
            msg_type = %msg.meta.msg_type,
            msg = ?msg.meta.msg,
            action = ?msg.meta.action,
            "sy.policy received blocked turn without result_origin; skipping"
        );
        return Ok(());
    }

    state.contract_ready_turns_total = state.contract_ready_turns_total.saturating_add(1);
    Ok(())
}

async fn process_router_message(sender: &NodeSender, msg: &Message) -> Result<(), PolicyError> {
    if try_handle_default_node_status(sender, msg).await? {
        return Ok(());
    }
    if msg.meta.msg_type != SYSTEM_KIND {
        return Ok(());
    }
    Ok(())
}

async fn load_hive(config_dir: &Path) -> Result<HiveFile, PolicyError> {
    let data = tokio::fs::read_to_string(config_dir.join("hive.yaml")).await?;
    Ok(serde_yaml::from_str(&data)?)
}

async fn connect_with_retry(
    config: &NodeConfig,
    delay: Duration,
) -> Result<(NodeSender, NodeReceiver), fluxbee_sdk::NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                tracing::warn!(error = %err, "sy.policy router connect failed; retrying");
                time::sleep(delay).await;
            }
        }
    }
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{name}@{hive_id}")
    }
}
