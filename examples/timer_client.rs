use std::path::PathBuf;
use std::time::Duration as StdDuration;

use fluxbee_sdk::{
    connect, parse_timer_fired_event, try_handle_default_node_status, NodeConfig, NodeError,
    NodeReceiver, NodeSender, NodeUuidMode, TimerClient, TimerClientConfig, TimerId, TimerInfo,
    TimerListFilter, TimerSchedulePayload, TimerStatus,
};
use serde_json::json;
use tokio::time::{sleep, Duration, Instant as TokioInstant};
use tracing_subscriber::EnvFilter;

const DEFAULT_DELAY_SECS: u64 = 65;
const POST_FIRE_STATUS_ATTEMPTS: usize = 10;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("timer_client example supports only Linux targets.");
        std::process::exit(1);
    }

    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let delay_secs = std::env::args()
        .nth(1)
        .map(|raw| raw.parse::<u64>())
        .transpose()?
        .unwrap_or(DEFAULT_DELAY_SECS);

    let config_dir = PathBuf::from(json_router::paths::CONFIG_DIR);
    let socket_dir = PathBuf::from(json_router::paths::ROUTER_SOCKET_DIR);
    let state_dir = PathBuf::from(json_router::paths::STATE_DIR);
    let nodes_dir = state_dir.join("nodes");

    let node_name = format!("WF.timer.example.{}", std::process::id());
    let node_config = NodeConfig {
        name: node_name,
        router_socket: socket_dir,
        uuid_persistence_dir: nodes_dir,
        uuid_mode: NodeUuidMode::Ephemeral,
        config_dir,
        version: "1.0".to_string(),
    };

    let (sender, mut receiver) =
        connect_with_retry(&node_config, StdDuration::from_secs(1)).await?;
    println!(
        "connected as {} (uuid={}, vpn={})",
        sender.full_name(),
        sender.uuid(),
        receiver.vpn_id(),
    );

    let timer_id = schedule_and_inspect(&sender, &mut receiver, delay_secs).await?;
    println!("scheduled timer_uuid={}", timer_id.as_str());

    let fired_event = wait_for_timer_fired(&sender, &mut receiver, &timer_id, delay_secs).await?;
    println!(
        "received TIMER_FIRED timer_uuid={} actual_fire_at_utc_ms={} fire_count={}",
        fired_event.timer_uuid.as_str(),
        fired_event.actual_fire_at_utc_ms,
        fired_event.fire_count,
    );

    let final_state = wait_for_fired_state(&sender, &mut receiver, &timer_id).await?;
    println!(
        "confirmed final timer state uuid={} status={:?} fire_count={} last_fired_at={:?}",
        final_state.uuid.as_str(),
        final_state.status,
        final_state.fire_count,
        final_state.last_fired_at_utc_ms,
    );

    Ok(())
}

async fn schedule_and_inspect(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    delay_secs: u64,
) -> Result<TimerId, Box<dyn std::error::Error>> {
    let mut client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
    let now = client.now().await?;
    println!(
        "timer target={} now_utc_ms={} now_utc_iso={}",
        client.target(),
        now.now_utc_ms,
        now.now_utc_iso
    );

    let timer_id = client
        .schedule_in(
            Duration::from_secs(delay_secs),
            TimerSchedulePayload {
                target_l2_name: Some(sender.full_name().to_string()),
                client_ref: Some(format!("rust-example-{}", now.now_utc_ms)),
                payload: json!({
                    "source": "examples/timer_client.rs",
                    "kind": "self_targeted_one_shot",
                }),
                metadata: json!({
                    "example": "timer_client",
                    "requested_delay_secs": delay_secs,
                }),
                ..TimerSchedulePayload::default()
            },
        )
        .await?;

    let current = client.get(timer_id.clone()).await?;
    println!(
        "get after schedule uuid={} status={:?} fire_at_utc_ms={} target={}",
        current.uuid.as_str(),
        current.status,
        current.fire_at_utc_ms,
        current.target_l2_name,
    );

    let listed = client
        .list_mine(TimerListFilter {
            limit: Some(10),
            ..TimerListFilter::default()
        })
        .await?;
    println!(
        "list_mine count={} contains_timer={}",
        listed.count,
        listed
            .timers
            .iter()
            .any(|timer| timer.uuid == timer_id)
    );

    Ok(timer_id)
}

async fn wait_for_timer_fired(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    timer_id: &TimerId,
    delay_secs: u64,
) -> Result<fluxbee_sdk::FiredEvent, Box<dyn std::error::Error>> {
    let timeout = Duration::from_secs(delay_secs + 30);
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
                    incoming.meta.msg_type,
                    incoming.meta.msg,
                    incoming.routing.trace_id
                );
            }
        }
    }
}

async fn wait_for_fired_state(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    timer_id: &TimerId,
) -> Result<TimerInfo, Box<dyn std::error::Error>> {
    let delays = [
        Duration::from_millis(0),
        Duration::from_millis(100),
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_millis(750),
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
    ];
    let mut last_info = None;
    for delay in delays.into_iter().take(POST_FIRE_STATUS_ATTEMPTS) {
        if !delay.is_zero() {
            sleep(delay).await;
        }
        let mut client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
        let info = client.get(timer_id.clone()).await?;
        if info.status == TimerStatus::Fired {
            return Ok(info);
        }
        last_info = Some(info);
    }
    Err(format!(
        "timer {} fired event arrived but final status did not converge to fired; last_status={:?}",
        timer_id.as_str(),
        last_info.as_ref().map(|value| value.status)
    )
    .into())
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
