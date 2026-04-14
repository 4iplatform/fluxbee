use std::path::PathBuf;
use std::time::Duration as StdDuration;

use fluxbee_sdk::{
    connect, parse_timer_fired_event, try_handle_default_node_status, NodeConfig, NodeError,
    NodeReceiver, NodeSender, NodeUuidMode, TimerClientError, TimerId,
};
use tokio::time::{sleep, Duration, Instant as TokioInstant};
use tracing_subscriber::EnvFilter;

pub const UUID_MODE_ENV: &str = "JSR_TIMER_EXAMPLE_UUID_MODE";

pub fn init_logging() {
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .try_init();
}

#[allow(dead_code)]
pub fn parse_delay_secs(default_delay_secs: u64) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(std::env::args()
        .nth(1)
        .map(|raw| raw.parse::<u64>())
        .transpose()?
        .unwrap_or(default_delay_secs))
}

pub fn example_uuid_mode() -> NodeUuidMode {
    match std::env::var(UUID_MODE_ENV)
        .unwrap_or_else(|_| "persistent".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "ephemeral" => NodeUuidMode::Ephemeral,
        _ => NodeUuidMode::Persistent,
    }
}

pub async fn connect_example_node(
    prefix: &str,
) -> Result<(NodeSender, NodeReceiver, NodeUuidMode), Box<dyn std::error::Error>> {
    let uuid_mode = example_uuid_mode();
    let config_dir = PathBuf::from(json_router::paths::CONFIG_DIR);
    let socket_dir = PathBuf::from(json_router::paths::ROUTER_SOCKET_DIR);
    let state_dir = PathBuf::from(json_router::paths::STATE_DIR);
    let nodes_dir = state_dir.join("nodes");
    let node_name = format!("{prefix}.{}@motherbee", std::process::id());
    let node_config = NodeConfig {
        name: node_name,
        router_socket: socket_dir,
        uuid_persistence_dir: nodes_dir,
        uuid_mode,
        config_dir,
        version: "1.0".to_string(),
    };
    let (sender, receiver) = connect_with_retry(&node_config, StdDuration::from_secs(1)).await?;
    Ok((sender, receiver, uuid_mode))
}

pub fn explain_schedule_error(err: TimerClientError) -> Box<dyn std::error::Error> {
    match err {
        TimerClientError::ServiceError {
            verb,
            code,
            message,
        } if code == "TIMER_INTERNAL" && message.contains("/var/lib/fluxbee/state/nodes") => {
            format!(
                "{verb} failed because the target SY.timer still looks up requester UUIDs via legacy state files. \
Rebuild/restart sy-timer on the host to pick up the router SHM resolver, or rerun this example with the default persistent UUID mode."
            )
            .into()
        }
        other => other.into(),
    }
}

pub async fn wait_for_timer_fired(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    timer_id: &TimerId,
    timeout: Duration,
) -> Result<fluxbee_sdk::FiredEvent, Box<dyn std::error::Error>> {
    let deadline = TokioInstant::now() + timeout;
    loop {
        let now = TokioInstant::now();
        if now >= deadline {
            return Err(format!(
                "timeout waiting TIMER_FIRED for {} after {}s",
                timer_id.as_str(),
                timeout.as_secs()
            )
            .into());
        }
        let remaining = deadline - now;
        let incoming = receiver.recv_timeout(remaining).await?;
        if try_handle_default_node_status(sender, &incoming).await? {
            continue;
        }
        match parse_timer_fired_event(&incoming) {
            Ok(event) if event.timer_uuid == *timer_id => return Ok(event),
            Ok(event) => {
                println!(
                    "ignoring TIMER_FIRED for another timer uuid={}",
                    event.timer_uuid.as_str()
                );
            }
            Err(_) => {
                println!(
                    "ignoring message kind={} msg={:?} trace_id={}",
                    incoming.meta.msg_type, incoming.meta.msg, incoming.routing.trace_id
                );
            }
        }
    }
}

async fn connect_with_retry(
    config: &NodeConfig,
    delay: StdDuration,
) -> Result<(NodeSender, NodeReceiver), NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                eprintln!("connect failed: {err}");
                sleep(Duration::from_millis(delay.as_millis() as u64)).await;
            }
        }
    }
}
