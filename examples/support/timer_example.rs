use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use fluxbee_sdk::{
    parse_timer_fired_event, try_handle_default_node_status, NodeConfig, NodeError, NodeUuidMode,
    OperationalRouteProfile, RouterDispatcher, RpcCommandReceiver, TimerClientError, TimerId,
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

/// Returns `(dispatcher, fired_events_receiver, uuid_mode)`.
///
/// The dispatcher owns the router connection. `TIMER_FIRED` broadcasts
/// flow into the `incoming` command channel, which this helper exposes
/// to the caller so `wait_for_timer_fired` can drain them while other
/// timer RPCs go through `dispatcher.send_with_matcher`.
pub async fn connect_example_node(
    prefix: &str,
) -> Result<(Arc<RouterDispatcher>, RpcCommandReceiver, NodeUuidMode), Box<dyn std::error::Error>> {
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
    let profile = OperationalRouteProfile::builder()
        .command_channel("incoming")
        .post_pending_rule(
            fluxbee_sdk::RouteMatch::Any,
            fluxbee_sdk::RouteTarget::Command("incoming"),
        )
        .build()?;
    let dispatcher =
        connect_dispatcher_with_retry(node_config, StdDuration::from_secs(1), profile).await?;
    let incoming = dispatcher.take_command_receiver("incoming").await?;
    Ok((dispatcher, incoming, uuid_mode))
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
    dispatcher: &Arc<RouterDispatcher>,
    incoming: &mut RpcCommandReceiver,
    timer_id: &TimerId,
    timeout: Duration,
) -> Result<fluxbee_sdk::FiredEvent, Box<dyn std::error::Error>> {
    let sender = dispatcher.sender_snapshot();
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
        let incoming_msg = tokio::time::timeout(remaining, incoming.recv())
            .await
            .map_err(|_| {
                format!(
                    "timeout waiting TIMER_FIRED for {} after {}s",
                    timer_id.as_str(),
                    timeout.as_secs()
                )
            })?
            .ok_or_else::<Box<dyn std::error::Error>, _>(|| {
                "incoming channel closed before TIMER_FIRED arrived".into()
            })?;
        if try_handle_default_node_status(&sender, &incoming_msg).await? {
            continue;
        }
        match parse_timer_fired_event(&incoming_msg) {
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
                    incoming_msg.meta.msg_type,
                    incoming_msg.meta.msg,
                    incoming_msg.routing.trace_id
                );
            }
        }
    }
}

async fn connect_dispatcher_with_retry(
    config: NodeConfig,
    delay: StdDuration,
    profile: OperationalRouteProfile,
) -> Result<Arc<RouterDispatcher>, NodeError> {
    let mut config = config;
    loop {
        match RouterDispatcher::connect_with_retry(config.clone(), delay, profile.clone()).await {
            Ok(dispatcher) => return Ok(dispatcher),
            Err(err) => {
                eprintln!("connect failed: {err}");
                sleep(Duration::from_millis(delay.as_millis() as u64)).await;
                // Force the loop to allow retry; the SDK's own retry handles
                // mid-flight reconnects, but a connect() failure here can
                // surface and we want to give the operator a chance to fix
                // their environment.
                let _ = &mut config;
            }
        }
    }
}
