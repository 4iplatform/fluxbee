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
const UUID_MODE_ENV: &str = "JSR_TIMER_EXAMPLE_UUID_MODE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExampleMode {
    Fire,
    CancelByClientRef,
    RescheduleByClientRef,
    IdempotentClientRef,
}

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

    let (mode, delay_secs) = parse_args()?;
    let uuid_mode = example_uuid_mode();

    let config_dir = PathBuf::from(json_router::paths::CONFIG_DIR);
    let socket_dir = PathBuf::from(json_router::paths::ROUTER_SOCKET_DIR);
    let state_dir = PathBuf::from(json_router::paths::STATE_DIR);
    let nodes_dir = state_dir.join("nodes");

    let node_name = format!("WF.timer.example.{}", std::process::id());
    let node_config = NodeConfig {
        name: node_name,
        router_socket: socket_dir,
        uuid_persistence_dir: nodes_dir,
        uuid_mode,
        config_dir,
        version: "1.0".to_string(),
    };

    let (sender, mut receiver) =
        connect_with_retry(&node_config, StdDuration::from_secs(1)).await?;
    println!(
        "connected as {} (uuid={}, vpn={}, uuid_mode={:?})",
        sender.full_name(),
        sender.uuid(),
        receiver.vpn_id(),
        uuid_mode,
    );

    match mode {
        ExampleMode::Fire => {
            let client_ref = format!("rust-example-{}", unix_now_ms());
            let timer_id =
                schedule_and_inspect(&sender, &mut receiver, delay_secs, &client_ref).await?;
            println!("scheduled timer_uuid={} client_ref={}", timer_id.as_str(), client_ref);

            let fired_event =
                wait_for_timer_fired(&sender, &mut receiver, &timer_id, delay_secs).await?;
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
        }
        ExampleMode::CancelByClientRef => {
            run_cancel_by_client_ref(&sender, &mut receiver, delay_secs).await?;
        }
        ExampleMode::RescheduleByClientRef => {
            run_reschedule_by_client_ref(&sender, &mut receiver, delay_secs).await?;
        }
        ExampleMode::IdempotentClientRef => {
            run_idempotent_client_ref(&sender, &mut receiver, delay_secs).await?;
        }
    }

    Ok(())
}

async fn schedule_and_inspect(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    delay_secs: u64,
    client_ref: &str,
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
                client_ref: Some(client_ref.to_string()),
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
        .await
        .map_err(explain_schedule_error)?;

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
        listed.timers.iter().any(|timer| timer.uuid == timer_id)
    );

    Ok(timer_id)
}

async fn run_cancel_by_client_ref(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    delay_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let client_ref = format!("rust-cancel-example-{}", unix_now_ms());
    let timer_id = schedule_and_inspect(sender, receiver, delay_secs, &client_ref).await?;
    println!(
        "scheduled timer_uuid={} client_ref={} for cancel-by-client_ref flow",
        timer_id.as_str(),
        client_ref
    );

    let mut client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
    let response = client.cancel_by_client_ref(client_ref.as_str()).await?;
    println!("cancel response found={:?} status={:?}", response.found, response.status);
    let final_state = wait_for_status(sender, receiver, timer_id.as_ref(), TimerStatus::Canceled).await?;
    println!(
        "confirmed canceled timer state uuid={} status={:?}",
        final_state.uuid.as_str(),
        final_state.status,
    );
    Ok(())
}

async fn run_reschedule_by_client_ref(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    delay_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let client_ref = format!("rust-reschedule-example-{}", unix_now_ms());
    let timer_id = schedule_and_inspect(sender, receiver, delay_secs, &client_ref).await?;
    let original = {
        let mut client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
        client.get(timer_id.clone()).await?
    };
    let new_fire_at = original.fire_at_utc_ms + 120_000;
    let mut client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
    let response = client
        .reschedule_by_client_ref(client_ref.as_str(), new_fire_at)
        .await?;
    println!(
        "reschedule response found={:?} fire_at_utc_ms={:?}",
        response.found, response.fire_at_utc_ms
    );
    let updated = wait_for_fire_at(sender, receiver, timer_id.as_ref(), new_fire_at).await?;
    println!(
        "confirmed rescheduled timer uuid={} new_fire_at_utc_ms={}",
        updated.uuid.as_str(),
        updated.fire_at_utc_ms,
    );
    Ok(())
}

async fn run_idempotent_client_ref(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    delay_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let client_ref = format!("rust-idempotent-example-{}", unix_now_ms());
    let first = schedule_and_inspect(sender, receiver, delay_secs, &client_ref).await?;
    let second = schedule_and_inspect(sender, receiver, delay_secs, &client_ref).await?;
    println!(
        "idempotent schedule first_uuid={} second_uuid={} same={}",
        first.as_str(),
        second.as_str(),
        first == second
    );
    if first != second {
        return Err("idempotent schedule returned different timer uuids".into());
    }
    let listed = {
        let mut client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
        client
            .list_mine(TimerListFilter {
                limit: Some(20),
                ..TimerListFilter::default()
            })
            .await?
    };
    let matching: Vec<_> = listed
        .timers
        .iter()
        .filter(|timer| timer.client_ref.as_deref() == Some(client_ref.as_str()))
        .collect();
    println!(
        "idempotent list_mine matching_client_ref={} count={}",
        client_ref,
        matching.len()
    );
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one pending timer for client_ref {}, got {}",
            client_ref,
            matching.len()
        )
        .into());
    }
    Ok(())
}

fn parse_args() -> Result<(ExampleMode, u64), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    match first.as_deref() {
        None => Ok((ExampleMode::Fire, DEFAULT_DELAY_SECS)),
        Some(raw) if raw.chars().all(|c| c.is_ascii_digit()) => {
            Ok((ExampleMode::Fire, raw.parse::<u64>()?))
        }
        Some("fire") => Ok((ExampleMode::Fire, parse_delay_arg(args.next())?)),
        Some("cancel") => Ok((ExampleMode::CancelByClientRef, parse_delay_arg(args.next())?)),
        Some("reschedule") => Ok((ExampleMode::RescheduleByClientRef, parse_delay_arg(args.next())?)),
        Some("idempotent") => Ok((ExampleMode::IdempotentClientRef, parse_delay_arg(args.next())?)),
        Some(other) => Err(format!(
            "unknown mode {other}. expected one of: fire, cancel, reschedule, idempotent"
        )
        .into()),
    }
}

fn parse_delay_arg(raw: Option<String>) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(raw
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(DEFAULT_DELAY_SECS))
}

fn example_uuid_mode() -> NodeUuidMode {
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

fn explain_schedule_error(err: fluxbee_sdk::TimerClientError) -> Box<dyn std::error::Error> {
    match err {
        fluxbee_sdk::TimerClientError::ServiceError {
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
                    incoming.meta.msg_type, incoming.meta.msg, incoming.routing.trace_id
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

async fn wait_for_status(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    timer_id: &str,
    expected: TimerStatus,
) -> Result<TimerInfo, Box<dyn std::error::Error>> {
    let mut last_info = None;
    for delay in [
        Duration::from_millis(0),
        Duration::from_millis(100),
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_millis(750),
        Duration::from_secs(1),
    ] {
        if !delay.is_zero() {
            sleep(delay).await;
        }
        let mut client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
        let info = client.get(TimerId::from(timer_id)).await?;
        if info.status == expected {
            return Ok(info);
        }
        last_info = Some(info);
    }
    Err(format!(
        "timer {} did not converge to status {:?}; last_status={:?}",
        timer_id,
        expected,
        last_info.as_ref().map(|value| value.status)
    )
    .into())
}

async fn wait_for_fire_at(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    timer_id: &str,
    expected_fire_at: i64,
) -> Result<TimerInfo, Box<dyn std::error::Error>> {
    let mut last_info = None;
    for delay in [
        Duration::from_millis(0),
        Duration::from_millis(100),
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_millis(750),
        Duration::from_secs(1),
    ] {
        if !delay.is_zero() {
            sleep(delay).await;
        }
        let mut client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
        let info = client.get(TimerId::from(timer_id)).await?;
        if info.fire_at_utc_ms == expected_fire_at {
            return Ok(info);
        }
        last_info = Some(info);
    }
    Err(format!(
        "timer {} did not converge to fire_at_utc_ms={}; last={:?}",
        timer_id,
        expected_fire_at,
        last_info.as_ref().map(|value| value.fire_at_utc_ms)
    )
    .into())
}

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
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
