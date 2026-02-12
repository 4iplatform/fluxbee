use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::time;
use tracing_subscriber::EnvFilter;

use json_router::nats::{NatsSubscriber, SUBJECT_STORAGE_TURNS};

type StorageError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
    role: Option<String>,
    nats: Option<NatsSection>,
}

#[derive(Debug, Deserialize)]
struct NatsSection {
    mode: Option<String>,
    port: Option<u16>,
    url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), StorageError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_storage supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let storage_root = json_router::paths::storage_root_dir();
    let island = load_island(&config_dir).await?;
    if !is_mother_role(island.role.as_deref()) {
        tracing::warn!(
            island = %island.island_id,
            "SY.storage solo corre en motherbee; role != motherbee"
        );
        return Ok(());
    }

    let endpoint = nats_endpoint(&island);
    let storage_dir = storage_root.join("storage");
    fs::create_dir_all(&storage_dir).await?;
    let turns_log = storage_dir.join("turns.ndjson");

    tracing::info!(
        island = %island.island_id,
        endpoint = %endpoint,
        file = %turns_log.display(),
        "sy.storage started"
    );

    loop {
        let subscriber =
            NatsSubscriber::new(endpoint.clone(), SUBJECT_STORAGE_TURNS.to_string(), 10);
        let file_path = turns_log.clone();
        let run_result = subscriber
            .run(move |payload| {
                let file_path = file_path.clone();
                async move { append_line(&file_path, &payload).await }
            })
            .await;
        if let Err(err) = run_result {
            tracing::warn!(error = %err, "nats subscribe loop failed; retrying");
            time::sleep(Duration::from_secs(1)).await;
        }
    }
}

async fn load_island(config_dir: &Path) -> Result<IslandFile, StorageError> {
    let data = fs::read_to_string(config_dir.join("island.yaml")).await?;
    Ok(serde_yaml::from_str(&data)?)
}

fn nats_endpoint(island: &IslandFile) -> String {
    let Some(nats) = island.nats.as_ref() else {
        return "nats://127.0.0.1:4222".to_string();
    };
    if let Some(url) = nats.url.as_ref() {
        let url = url.trim();
        if !url.is_empty() {
            return url.to_string();
        }
    }
    let mode = nats
        .mode
        .as_deref()
        .unwrap_or("embedded")
        .trim()
        .to_ascii_lowercase();
    let port = nats.port.unwrap_or(4222);
    if mode == "embedded" || mode == "client" {
        format!("nats://127.0.0.1:{port}")
    } else {
        "nats://127.0.0.1:4222".to_string()
    }
}

async fn append_line(path: &PathBuf, payload: &[u8]) -> Result<(), std::io::Error> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(payload).await?;
    file.write_all(b"\n").await?;
    file.flush().await?;
    Ok(())
}

fn is_mother_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee" || r == "mother")
}
