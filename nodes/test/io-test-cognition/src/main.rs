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

#[derive(Debug, Clone, Copy)]
struct CarrierValidation {
    trace_ok: bool,
    receiver_ok: bool,
    msg_type_ok: bool,
    thread_id_ok: bool,
    ich_ok: bool,
    context_ok: bool,
}

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
    let mut trace_ok_all = true;
    let mut receiver_ok_all = true;
    let mut msg_type_ok_all = true;
    let mut thread_id_ok_all = true;
    let mut ich_ok_all = true;
    let mut context_ok_all = true;
    let expected_ich = format!("io.test.cognition://{}", probe_id);

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
        let validation = validate_observation(
            &observation,
            &trace_id,
            &target,
            &probe_id,
            step,
            &thread_id,
            &expected_ich,
        )?;
        let memory_package_present = observation
            .get("memory_package_present")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        trace_ok_all &= validation.trace_ok;
        receiver_ok_all &= validation.receiver_ok;
        msg_type_ok_all &= validation.msg_type_ok;
        thread_id_ok_all &= validation.thread_id_ok;
        ich_ok_all &= validation.ich_ok;
        context_ok_all &= validation.context_ok;

        tracing::info!(
            step,
            trace_id = %trace_id,
            carrier_check = ?validation,
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
    println!("TRACE_ID_OK={trace_ok_all}");
    println!("ICH_OK={ich_ok_all}");
    println!("MSG_TYPE_OK={msg_type_ok_all}");
    println!("THREAD_ID_OK={thread_id_ok_all}");
    println!("RECEIVER_OK={receiver_ok_all}");
    println!("CONTEXT_OK={context_ok_all}");
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

fn validate_observation(
    observation: &Value,
    expected_trace_id: &str,
    expected_receiver: &str,
    expected_probe_id: &str,
    expected_probe_step: u64,
    expected_thread_id: &str,
    expected_ich: &str,
) -> Result<CarrierValidation, DynError> {
    let trace_ok = observation.get("trace_id").and_then(Value::as_str) == Some(expected_trace_id);
    let receiver_ok =
        observation.get("receiver").and_then(Value::as_str) == Some(expected_receiver);
    let probe_id_ok =
        observation.get("probe_id").and_then(Value::as_str) == Some(expected_probe_id);
    let probe_step_ok =
        observation.get("probe_step").and_then(Value::as_u64) == Some(expected_probe_step);
    let msg_type_ok = observation.get("msg_type").and_then(Value::as_str) == Some("user");
    let thread_id_ok =
        observation.get("thread_id").and_then(Value::as_str) == Some(expected_thread_id);
    let ich_ok = observation.get("ich").and_then(Value::as_str) == Some(expected_ich);
    let context = observation.get("context").unwrap_or(&Value::Null);
    let context_probe_id_ok =
        context.get("probe_id").and_then(Value::as_str) == Some(expected_probe_id);
    let context_probe_step_ok =
        context.get("probe_step").and_then(Value::as_u64) == Some(expected_probe_step);

    let context_ok = context_probe_id_ok && context_probe_step_ok;
    let ok = trace_ok
        && receiver_ok
        && probe_id_ok
        && probe_step_ok
        && msg_type_ok
        && thread_id_ok
        && ich_ok
        && context_ok;

    if !ok {
        return Err(format!(
            "carrier validation failed trace_ok={} receiver_ok={} probe_id_ok={} probe_step_ok={} msg_type_ok={} thread_id_ok={} ich_ok={} context_probe_id_ok={} context_probe_step_ok={} observation={}",
            trace_ok,
            receiver_ok,
            probe_id_ok,
            probe_step_ok,
            msg_type_ok,
            thread_id_ok,
            ich_ok,
            context_probe_id_ok,
            context_probe_step_ok,
            serde_json::to_string(observation)?
        )
        .into());
    }

    Ok(CarrierValidation {
        trace_ok,
        receiver_ok,
        msg_type_ok,
        thread_id_ok,
        ich_ok,
        context_ok,
    })
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
