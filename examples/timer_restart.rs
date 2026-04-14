#[path = "support/timer_example.rs"]
mod timer_example;

use fluxbee_sdk::{
    TimerClient, TimerClientConfig, TimerId, TimerInfo, TimerListFilter, TimerSchedulePayload,
    TimerStatus, TimerStatusFilter,
};
use serde_json::json;
use tokio::time::{sleep, Duration};

const DEFAULT_DELAY_SECS: u64 = 90;
const POST_FIRE_STATUS_ATTEMPTS: usize = 10;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("timer_restart example supports only Linux targets.");
        std::process::exit(1);
    }

    timer_example::init_logging();

    let delay_secs = timer_example::parse_delay_secs(DEFAULT_DELAY_SECS)?;
    let (sender, mut receiver, uuid_mode) =
        timer_example::connect_example_node("WF.timer.restart").await?;
    println!(
        "connected as {} (uuid={}, vpn={}, uuid_mode={:?})",
        sender.full_name(),
        sender.uuid(),
        receiver.vpn_id(),
        uuid_mode,
    );

    let timer_id = schedule_restart_probe(&sender, &mut receiver, delay_secs).await?;
    println!("scheduled restart probe timer_uuid={}", timer_id.as_str());
    println!(
        "manual step: restart sy-timer before the fire deadline to validate replay/restart:\n  sudo systemctl restart sy-timer"
    );
    println!("the example will keep waiting for TIMER_FIRED and then confirm final state.");

    let fired_event = timer_example::wait_for_timer_fired(
        &sender,
        &mut receiver,
        &timer_id,
        Duration::from_secs(delay_secs + 90),
    )
    .await?;
    println!(
        "received TIMER_FIRED after restart probe timer_uuid={} actual_fire_at_utc_ms={} fire_count={}",
        fired_event.timer_uuid.as_str(),
        fired_event.actual_fire_at_utc_ms,
        fired_event.fire_count,
    );

    let final_state = wait_for_fired_state(&sender, &mut receiver, &timer_id).await?;
    println!(
        "confirmed replay/restart final state uuid={} status={:?} fire_count={} last_fired_at={:?}",
        final_state.uuid.as_str(),
        final_state.status,
        final_state.fire_count,
        final_state.last_fired_at_utc_ms,
    );

    Ok(())
}

async fn schedule_restart_probe(
    sender: &fluxbee_sdk::NodeSender,
    receiver: &mut fluxbee_sdk::NodeReceiver,
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
                client_ref: Some(format!("rust-restart-example-{}", now.now_utc_ms)),
                payload: json!({
                    "source": "examples/timer_restart.rs",
                    "kind": "self_targeted_restart_probe",
                }),
                metadata: json!({
                    "example": "timer_restart",
                    "requested_delay_secs": delay_secs,
                    "manual_step": "restart sy-timer before fire",
                }),
                ..TimerSchedulePayload::default()
            },
        )
        .await
        .map_err(timer_example::explain_schedule_error)?;

    let current = client.get(timer_id.clone()).await?;
    println!(
        "get after restart schedule uuid={} status={:?} fire_at_utc_ms={} target={}",
        current.uuid.as_str(),
        current.status,
        current.fire_at_utc_ms,
        current.target_l2_name,
    );

    let listed = client
        .list_mine(TimerListFilter {
            status_filter: Some(TimerStatusFilter::Pending),
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

async fn wait_for_fired_state(
    sender: &fluxbee_sdk::NodeSender,
    receiver: &mut fluxbee_sdk::NodeReceiver,
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
