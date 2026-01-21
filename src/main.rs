use json_router::config::RouterConfig;
use json_router::router::Router;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = RouterConfig::load_from_env()?;
    let router = Router::new(cfg);
    router.run().await?;
    Ok(())
}
