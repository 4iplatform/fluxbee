use std::path::PathBuf;
use std::time::{Duration, Instant};

use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use fluxbee_sdk::thread::ThreadIdInput;
use fluxbee_sdk::{
    compute_thread_id, connect, load_hive_id, try_handle_default_node_status, NodeConfig,
    NodeReceiver, NodeSender,
};
use serde_json::{json, Value};
use tokio::time::timeout;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DynError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(env_or("JSR_LOG_LEVEL", "info")))
        .init();

    let cfg = build_node_config("IO.test.cognition", "0.1.0", "IO_TEST_COGNITION");
    let hive_id = load_hive_id(&cfg.config_dir)?;
    let target = env_or(
        "IO_TEST_COGNITION_TARGET",
        &format!("AI.test.cognition@{hive_id}"),
    );
    let probe_id = env_or(
        "IO_TEST_COGNITION_PROBE_ID",
        &format!("probe-cognition-{}", Uuid::new_v4().simple()),
    );
    let thread_id = resolve_thread_id(&hive_id, &probe_id)?;
    let turn_delay_ms = env_u64("IO_TEST_COGNITION_TURN_DELAY_MS", 2_000);
    let max_steps = env_u64("IO_TEST_COGNITION_MAX_STEPS", 4).max(2);
    let require_memory_package = env_bool("IO_TEST_COGNITION_REQUIRE_MEMORY_PACKAGE", true);

    tracing::info!(
        node = %cfg.name,
        target = %target,
        probe_id = %probe_id,
        thread_id = %thread_id,
        max_steps,
        "starting IO.test.cognition"
    );

    let (sender, mut receiver) = connect(&cfg).await?;

    let mut first_observation = None;
    let mut final_observation = None;
    let mut success_step = None;

    for step in 1..=max_steps {
        if step > 1 {
            tokio::time::sleep(Duration::from_millis(turn_delay_ms)).await;
        }
        let trace_id = send_probe(&sender, &target, &probe_id, &thread_id, step).await?;
        let reply = wait_for_reply(&sender, &mut receiver, &trace_id, 8_000).await?;
        let observation = reply
            .payload
            .get("observation")
            .cloned()
            .unwrap_or(Value::Null);
        let memory_package_present = observation
            .get("memory_package_present")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        tracing::info!(
            step,
            trace_id = %trace_id,
            memory_package_present,
            observation = %observation,
            "IO.test.cognition received reply"
        );

        if first_observation.is_none() {
            first_observation = Some(observation.clone());
        }
        final_observation = Some(observation.clone());
        if memory_package_present {
            success_step = Some(step);
            break;
        }
    }

    let final_observation = final_observation.unwrap_or(Value::Null);
    let memory_package_present = final_observation
        .get("memory_package_present")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if require_memory_package && !memory_package_present {
        return Err(format!(
            "memory_package not observed after {} steps for thread {}",
            max_steps, thread_id
        )
        .into());
    }

    println!("STATUS=ok");
    println!("TARGET={target}");
    println!("PROBE_ID={probe_id}");
    println!("THREAD_ID={thread_id}");
    println!("STEPS_USED={}", success_step.unwrap_or(max_steps));
    println!("MEMORY_PACKAGE_PRESENT={memory_package_present}");
    if let Some(observation) = first_observation {
        println!(
            "FIRST_THREAD_SEQ={}",
            observation
                .get("thread_seq")
                .and_then(Value::as_u64)
                .map(|value| value.to_string())
                .unwrap_or_default()
        );
    }
    println!(
        "FINAL_THREAD_SEQ={}",
        final_observation
            .get("thread_seq")
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_default()
    );
    println!(
        "DOMINANT_CONTEXT={}",
        final_observation
            .get("memory_package")
            .and_then(|value| value.get("dominant_context"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    );
    println!(
        "DOMINANT_REASON={}",
        final_observation
            .get("memory_package")
            .and_then(|value| value.get("dominant_reason"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    );
    println!(
        "FINAL_OBSERVATION={}",
        serde_json::to_string(&final_observation)?
    );
    Ok(())
}

fn resolve_thread_id(hive_id: &str, probe_id: &str) -> Result<String, DynError> {
    if let Some(explicit) = env_opt("IO_TEST_COGNITION_THREAD_ID") {
        return Ok(explicit);
    }
    compute_thread_id(ThreadIdInput::PersistentChannel {
        channel_type: "io.test.cognition",
        entrypoint_id: Some(hive_id),
        conversation_id: probe_id,
    })
    .map_err(|err| err.into())
}

async fn send_probe(
    sender: &NodeSender,
    target: &str,
    probe_id: &str,
    thread_id: &str,
    step: u64,
) -> Result<String, DynError> {
    let trace_id = Uuid::new_v4().to_string();
    let text = if step == 1 {
        env_or(
            "IO_TEST_COGNITION_TEXT_STEP1",
            "please help, urgent refund, I was charged twice and this is broken and unacceptable",
        )
    } else {
        env_or(
            "IO_TEST_COGNITION_TEXT_STEPN",
            "please resolve this refund issue now, it is still broken and I may escalate the complaint",
        )
    };
    let payload = TextV1Payload::new(text.clone(), vec![]).to_value()?;
    let send_started = Instant::now();
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: None,
            src_ilk: None,
            dst_ilk: None,
            ich: Some(format!("io.test.cognition://{}", probe_id)),
            thread_id: Some(thread_id.to_string()),
            thread_seq: None,
            ctx: None,
            ctx_seq: None,
            ctx_window: None,
            memory_package: None,
            scope: None,
            target: Some("io.test.cognition.probe".to_string()),
            action: None,
            priority: None,
            context: Some(json!({
                "probe_id": probe_id,
                "probe_step": step,
            })),
        },
        payload,
    };
    sender.send(msg).await?;
    tracing::info!(
        trace_id = %trace_id,
        probe_id = %probe_id,
        thread_id = %thread_id,
        step,
        elapsed_us = send_started.elapsed().as_micros() as u64,
        "IO.test.cognition sent probe"
    );
    Ok(trace_id)
}

async fn wait_for_reply(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    expected_trace_id: &str,
    timeout_ms: u64,
) -> Result<Message, DynError> {
    let wait_started = Instant::now();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("timeout waiting reply trace_id={expected_trace_id}").into());
        }
        let incoming = match timeout(remaining, receiver.recv()).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => {
                return Err(format!("timeout waiting reply trace_id={expected_trace_id}").into())
            }
        };

        tracing::info!(
            expected_trace_id = %expected_trace_id,
            incoming_trace_id = %incoming.routing.trace_id,
            incoming_type = %incoming.meta.msg_type,
            incoming_msg = incoming.meta.msg.as_deref().unwrap_or(""),
            elapsed_us = wait_started.elapsed().as_micros() as u64,
            "IO.test.cognition received message while waiting for reply"
        );

        if try_handle_default_node_status(sender, &incoming).await? {
            continue;
        }
        if incoming.routing.trace_id != expected_trace_id {
            continue;
        }
        if incoming.meta.msg_type != "user" {
            continue;
        }
        return Ok(incoming);
    }
}

fn build_node_config(default_name: &str, default_version: &str, prefix: &str) -> NodeConfig {
    let node_name_env = format!("{prefix}_NODE_NAME");
    NodeConfig {
        name: fluxbee_sdk::managed_node_name(default_name, &[node_name_env.as_str()]),
        router_socket: PathBuf::from(env_or(
            &format!("{prefix}_ROUTER_SOCKET_DIR"),
            "/var/run/fluxbee/routers",
        )),
        uuid_persistence_dir: PathBuf::from(env_or(
            &format!("{prefix}_UUID_PERSISTENCE_DIR"),
            "/var/lib/fluxbee/state/nodes",
        )),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: PathBuf::from(env_or(&format!("{prefix}_CONFIG_DIR"), "/etc/fluxbee")),
        version: env_or(&format!("{prefix}_NODE_VERSION"), default_version),
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}
