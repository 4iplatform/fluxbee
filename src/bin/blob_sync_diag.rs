use std::error::Error;
use std::path::PathBuf;
use std::time::Instant;

use fluxbee_sdk::blob::{
    BlobConfig, BlobRef, BlobToolkit, PublishBlobRequest, ResolveRetryConfig, BLOB_NAME_MAX_CHARS,
    BLOB_SYNC_HINT_DEFAULT_TIMEOUT_MS,
};
use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::{connect, NodeConfig};
use tracing_subscriber::EnvFilter;

type DynError = Box<dyn Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let log_level = env_or("JSR_LOG_LEVEL", "info");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let mode = env_or("BLOB_SYNC_DIAG_MODE", "produce");
    match mode.as_str() {
        "produce" => run_produce().await,
        "consume" => run_consume().await,
        other => {
            Err(format!("invalid BLOB_SYNC_DIAG_MODE={other}; expected produce|consume").into())
        }
    }
}

async fn run_produce() -> Result<(), DynError> {
    let blob_root = PathBuf::from(env_or("BLOB_ROOT", "/var/lib/fluxbee/blob"));
    let max_blob_bytes = env_u64_opt("BLOB_MAX_BYTES");
    let filename = env_or("BLOB_DIAG_FILENAME", "blob-sync-diag.txt");
    let content = env_or("BLOB_DIAG_CONTENT", "fluxbee-blob-sync-diag");
    let mime = env_or("BLOB_DIAG_MIME", "text/plain");
    let payload_text = env_or("BLOB_DIAG_PAYLOAD_TEXT", "blob sync diag");
    let confirm_targets = env_list("BLOB_DIAG_CONFIRM_TARGETS");
    let confirm_required = env_bool("BLOB_DIAG_CONFIRM_REQUIRED", false);
    let confirm_wait_for_idle = env_bool("BLOB_DIAG_CONFIRM_WAIT_FOR_IDLE", true);
    let confirm_timeout_ms = env_u64(
        "BLOB_DIAG_CONFIRM_TIMEOUT_MS",
        BLOB_SYNC_HINT_DEFAULT_TIMEOUT_MS,
    );

    let toolkit = BlobToolkit::new(BlobConfig {
        blob_root: blob_root.clone(),
        name_max_chars: BLOB_NAME_MAX_CHARS,
        max_blob_bytes,
    })?;

    if confirm_required && confirm_targets.is_empty() {
        return Err(
            "BLOB_DIAG_CONFIRM_REQUIRED=1 requires BLOB_DIAG_CONFIRM_TARGETS (csv list)".into(),
        );
    }

    let (blob_ref, sync_targets, sync_timeout_ms) = if confirm_targets.is_empty() {
        let blob_ref = toolkit.put_bytes(content.as_bytes(), &filename, &mime)?;
        toolkit.promote(&blob_ref)?;
        (blob_ref, Vec::new(), 0_u64)
    } else {
        let node_name = env_or("BLOB_DIAG_SYSTEM_NODE_NAME", "WF.orch.diag");
        let node_version = env_or("BLOB_DIAG_SYSTEM_NODE_VERSION", "1.0");
        let node_config = NodeConfig {
            name: node_name,
            router_socket: json_router::paths::router_socket_dir(),
            uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
            uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
            config_dir: json_router::paths::config_dir(),
            version: node_version,
        };
        let (sender, mut receiver) = connect(&node_config).await?;
        let publish = toolkit
            .publish_blob_and_confirm(
                &sender,
                &mut receiver,
                PublishBlobRequest {
                    data: content.as_bytes(),
                    filename_original: &filename,
                    mime: &mime,
                    targets: confirm_targets.clone(),
                    wait_for_idle: confirm_wait_for_idle,
                    timeout_ms: confirm_timeout_ms,
                },
            )
            .await?;
        let _ = sender.close().await;
        (publish.blob_ref, publish.targets, publish.timeout_ms)
    };

    let active_path = toolkit.resolve(&blob_ref);

    let payload = TextV1Payload::new(payload_text, vec![blob_ref.clone()]);
    payload.validate()?;
    let payload_json = payload.to_value()?;
    let blob_ref_json = serde_json::to_string(&blob_ref)?;
    let payload_json_line = serde_json::to_string(&payload_json)?;
    let contract_signature = build_contract_signature(&blob_ref, &payload);

    tracing::info!(
        mode = "produce",
        blob_root = %blob_root.display(),
        blob_name = %blob_ref.blob_name,
        sync_targets = sync_targets.len(),
        active_path = %active_path.display(),
        "blob sync diag producer completed"
    );

    println!("STATUS=ok");
    println!("MODE=produce");
    println!("BLOB_ROOT={}", blob_root.display());
    println!("BLOB_REF_JSON={blob_ref_json}");
    println!("ACTIVE_PATH={}", active_path.display());
    println!("PAYLOAD_JSON={payload_json_line}");
    println!("CONTRACT_SIGNATURE={contract_signature}");
    if !sync_targets.is_empty() {
        println!("SYNC_HINT_TIMEOUT_MS={sync_timeout_ms}");
        println!(
            "SYNC_HINT_TARGETS={}",
            serde_json::to_string(&sync_targets)?
        );
    }
    println!(
        "BLOB_METRICS_JSON={}",
        serde_json::to_string(&BlobToolkit::metrics_snapshot())?
    );

    Ok(())
}

async fn run_consume() -> Result<(), DynError> {
    let blob_root = PathBuf::from(env_or("BLOB_ROOT", "/var/lib/fluxbee/blob"));
    let max_blob_bytes = env_u64_opt("BLOB_MAX_BYTES");
    let blob_ref_json = env_or("BLOB_DIAG_BLOB_REF_JSON", "");
    if blob_ref_json.trim().is_empty() {
        return Err("BLOB_DIAG_BLOB_REF_JSON is required for consume mode".into());
    }
    let expected_content = std::env::var("BLOB_DIAG_EXPECT_CONTENT")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let retry_cfg = ResolveRetryConfig {
        max_wait_ms: env_u64("BLOB_DIAG_RETRY_MAX_WAIT_MS", 15_000),
        initial_delay_ms: env_u64("BLOB_DIAG_RETRY_INITIAL_MS", 100),
        backoff_factor: env_f64("BLOB_DIAG_RETRY_BACKOFF", 2.0),
    };

    let toolkit = BlobToolkit::new(BlobConfig {
        blob_root: blob_root.clone(),
        name_max_chars: BLOB_NAME_MAX_CHARS,
        max_blob_bytes,
    })?;

    let blob_ref: BlobRef = serde_json::from_str(&blob_ref_json)?;
    let started = Instant::now();
    let resolved = toolkit.resolve_with_retry(&blob_ref, retry_cfg).await?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let bytes = std::fs::read(&resolved)?;

    if let Some(expected) = expected_content.as_ref() {
        if bytes != expected.as_bytes() {
            return Err(format!(
                "content mismatch: expected {} bytes got {} bytes",
                expected.len(),
                bytes.len()
            )
            .into());
        }
    }

    tracing::info!(
        mode = "consume",
        blob_root = %blob_root.display(),
        blob_name = %blob_ref.blob_name,
        resolved = %resolved.display(),
        elapsed_ms,
        bytes = bytes.len(),
        "blob sync diag consumer resolved blob"
    );

    println!("STATUS=ok");
    println!("MODE=consume");
    println!("BLOB_ROOT={}", blob_root.display());
    println!("RESOLVED_PATH={}", resolved.display());
    println!("RESOLVED_BYTES={}", bytes.len());
    println!("ELAPSED_MS={elapsed_ms}");
    println!(
        "BLOB_METRICS_JSON={}",
        serde_json::to_string(&BlobToolkit::metrics_snapshot())?
    );

    Ok(())
}

fn build_contract_signature(blob_ref: &BlobRef, payload: &TextV1Payload) -> String {
    let content_mode = if payload.content_ref.is_some() {
        "content_ref"
    } else {
        "content_inline"
    };
    format!(
        "blob_ref:{}|{}|{}|{}|{}|{};payload:{}|{}|attachments={}",
        blob_ref.ref_type,
        blob_ref.blob_name,
        blob_ref.size,
        blob_ref.mime,
        blob_ref.filename_original,
        blob_ref.spool_day,
        payload.payload_type,
        content_mode,
        payload.attachments.len()
    )
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_u64_opt(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.parse::<u64>().ok())
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| {
            let lowered = v.trim().to_ascii_lowercase();
            matches!(lowered.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(default)
}

fn env_list(key: &str) -> Vec<String> {
    std::env::var(key)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|part| part.trim())
                .filter(|part| !part.is_empty())
                .map(|part| part.to_string())
                .collect()
        })
        .unwrap_or_default()
}
