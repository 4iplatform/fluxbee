#[cfg(target_os = "linux")]
use json_router::config::RouterConfig;
#[cfg(target_os = "linux")]
use json_router::router::Router;
#[cfg(target_os = "linux")]
use tokio::signal::unix::{signal, SignalKind};
#[cfg(target_os = "linux")]
use tracing_subscriber::EnvFilter;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("fluxbee-router supports only Linux targets.");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let cfg = RouterConfig::load_from_env()?;
    let nats_mode = cfg.nats_mode.clone();
    let nats_url = cfg.nats_url.clone();
    let router = Router::new(cfg);
    let mut router_task = tokio::spawn(async move { router.run().await });

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    tokio::select! {
        result = &mut router_task => {
            let run_result = result.map_err(|err| format!("router task join error: {err}"))?;
            run_result?;
        }
        _ = sigterm.recv() => {
            tracing::warn!("SIGTERM received; shutting down rt-gateway");
            if nats_mode == "embedded" {
                if let Err(err) = json_router::nats::stop_embedded_broker(&nats_url).await {
                    tracing::warn!(endpoint = %nats_url, error = %err, "embedded nats stop failed");
                }
            }
            router_task.abort();
            let _ = router_task.await;
        }
        _ = sigint.recv() => {
            tracing::warn!("SIGINT received; shutting down rt-gateway");
            if nats_mode == "embedded" {
                if let Err(err) = json_router::nats::stop_embedded_broker(&nats_url).await {
                    tracing::warn!(endpoint = %nats_url, error = %err, "embedded nats stop failed");
                }
            }
            router_task.abort();
            let _ = router_task.await;
        }
    }

    Ok(())
}
