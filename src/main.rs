mod config;
mod shm;
mod socket;
mod opa;
mod protocol;
mod router;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = config::Config::load()?;
    let filter = if cfg.log_level.contains('=') {
        cfg.log_level.clone()
    } else {
        format!("json_router={}", cfg.log_level)
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();
    tracing::info!("json-router starting");
    let router = router::Router::new(cfg)?;
    router.run().await?;
    Ok(())
}
