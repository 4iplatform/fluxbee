use json_router::config::RouterConfig;
use json_router::router::Router;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let cfg = RouterConfig::load_from_env()?;
    let router = Router::new(cfg);
    router.run().await?;
    Ok(())
}
