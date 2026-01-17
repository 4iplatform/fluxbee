mod config;
mod shm;
mod socket;
mod opa;
mod protocol;
mod router;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("json_router=info")
        .init();
    tracing::info!("json-router starting");
    let cfg = config::Config::from_env();
    let router = router::Router::new(cfg)?;
    router.run().await?;
    Ok(())
}
