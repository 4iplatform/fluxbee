#[cfg(target_os = "linux")]
use json_router::config::RouterConfig;
#[cfg(target_os = "linux")]
use json_router::router::{hold_instance_lock, Router};
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
    // systemd starts rt-gateway with no argument. One started by hand with any, `--version` among
    // them, refuses before it touches anything (FINDINGS A-177).
    if std::env::args_os().len() > 1 {
        eprintln!(
            "rt-gateway takes no arguments: systemd starts it (rt-gateway.service). The installed \
             version is in GET /versions on SY.admin."
        );
        std::process::exit(2);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    fluxbee_sdk::logging::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let cfg = RouterConfig::load_from_env()?;
    // Before the router opens its shared memory, its stream and its sockets: a second instance of
    // this router exits here, naming the lock, held until the process ends (FINDINGS A-177).
    hold_instance_lock(&cfg)?;
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
            router_task.abort();
            let _ = router_task.await;
        }
        _ = sigint.recv() => {
            tracing::warn!("SIGINT received; shutting down rt-gateway");
            router_task.abort();
            let _ = router_task.await;
        }
    }

    Ok(())
}
