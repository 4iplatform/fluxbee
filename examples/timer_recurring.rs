#[path = "support/timer_example.rs"]
mod timer_example;

use fluxbee_sdk::{
    TimerClient, TimerClientConfig, TimerId, TimerInfo, TimerScheduleRecurringPayload,
    TimerStatus, TimerStatusFilter,
};
use serde_json::json;
use tokio::time::{sleep, Duration};

const FIRE_WAIT_TIMEOUT_SECS: u64 = 95;
const POST_FIRE_STATUS_ATTEMPTS: usize = 10;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("timer_recurring example supports only Linux targets.");
        std::process::exit(1);
    }

    timer_example::init_logging();

    let (sender, mut receiver, uuid_mode) =
        timer_example::connect_example_node("WF.timer.recurring").await?;
    println!(
        "connected as {} (uuid={}, vpn={}, uuid_mode={:?})",
        sender.full_name(),
        sender.uuid(),
        receiver.vpn_id(),
        uuid_mode,
    );

    let timer_id = schedule_recurring_and_inspect(&sender, &mut receiver).await?;
    println!("scheduled recurring timer_uuid={}", timer_id.as_str());

    let fired_event = timer_example::wait_for_timer_fired(
        &sender,
        &mut receiver,
        &timer_id,
        Duration::from_secs(FIRE_WAIT_TIMEOUT_SECS),
    )
    .await?;
    println!(
        "received recurring TIMER_FIRED timer_uuid={} actual_fire_at_utc_ms={} fire_count={} is_last_fire={}",
        fired_event.timer_uuid.as_str(),
        fired_event.actual_fire_at_utc_ms,
        fired_event.fire_count,
        fired_event.is_last_fire,
    );

    let requeued_state =
        wait_for_recurring_requeued_state(&sender, &mut receiver, &timer_id, fired_event.actual_fire_at_utc_ms)
            .await?;
    println!(
        "confirmed recurring timer requeued uuid={} status={:?} fire_count={} next_fire_at_utc_ms={}",
        requeued_state.uuid.as_str(),
        requeued_state.status,
        requeued_state.fire_count,
        requeued_state.fire_at_utc_ms,
    );

    cancel_and_confirm(&sender, &mut receiver, &timer_id).await?;
    println!("confirmed recurring timer canceled uuid={}", timer_id.as_str());

    Ok(())
}

async fn schedule_recurring_and_inspect(
    sender: &fluxbee_sdk::NodeSender,
    receiver: &mut fluxbee_sdk::NodeReceiver,
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
        .schedule_recurring(TimerScheduleRecurringPayload {
            cron_spec: "*/1 * * * *".to_string(),
            cron_tz: Some("UTC".to_string()),
            target_l2_name: Some(sender.full_name().to_string()),
            client_ref: Some(format!("rust-recurring-example-{}", now.now_utc_ms)),
            payload: json!({
                "source": "examples/timer_recurring.rs",
                "kind": "self_targeted_recurring",
            }),
            metadata: json!({
                "example": "timer_recurring",
                "cron_spec": "*/1 * * * *",
            }),
            ..TimerScheduleRecurringPayload::default()
        })
        .await
        .map_err(timer_example::explain_schedule_error)?;

    let current = client.get(timer_id.clone()).await?;
    println!(
        "get after recurring schedule uuid={} status={:?} fire_at_utc_ms={} cron_spec={:?} target={}",
        current.uuid.as_str(),
        current.status,
        current.fire_at_utc_ms,
        current.cron_spec,
        current.target_l2_name,
    );

    let listed = client
        .list_mine(fluxbee_sdk::TimerListFilter {
            status_filter: Some(TimerStatusFilter::Pending),
            limit: Some(10),
            ..fluxbee_sdk::TimerListFilter::default()
        })
        .await?;
    println!(
        "list_mine count={} contains_timer={}",
        listed.count,
        listed.timers.iter().any(|timer| timer.uuid == timer_id)
    );

    Ok(timer_id)
}

async fn wait_for_recurring_requeued_state(
    sender: &fluxbee_sdk::NodeSender,
    receiver: &mut fluxbee_sdk::NodeReceiver,
    timer_id: &TimerId,
    actual_fire_at_utc_ms: i64,
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
        if info.status == TimerStatus::Pending
            && info.fire_count >= 1
            && info.fire_at_utc_ms > actual_fire_at_utc_ms
        {
            return Ok(info);
        }
        last_info = Some(info);
    }
    Err(format!(
        "recurring timer {} did not converge to requeued pending state; last_state={:?}",
        timer_id.as_str(),
        last_info.as_ref().map(|value| (value.status, value.fire_count, value.fire_at_utc_ms))
    )
    .into())
}

async fn cancel_and_confirm(
    sender: &fluxbee_sdk::NodeSender,
    receiver: &mut fluxbee_sdk::NodeReceiver,
    timer_id: &TimerId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
    client.cancel(timer_id.clone()).await?;
    for delay in [
        Duration::from_millis(0),
        Duration::from_millis(100),
        Duration::from_millis(250),
        Duration::from_millis(500),
    ] {
        if !delay.is_zero() {
            sleep(delay).await;
        }
        let mut poll_client = TimerClient::new(sender, receiver, TimerClientConfig::default())?;
        let info = poll_client.get(timer_id.clone()).await?;
        if info.status == TimerStatus::Canceled {
            return Ok(());
        }
    }
    Err(format!("recurring timer {} did not converge to canceled state", timer_id.as_str()).into())
}
