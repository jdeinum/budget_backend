use tracing_subscriber::EnvFilter;

use budget::config::AppConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = AppConfig::load()?;
    let app = budget::build(config).await?;

    tracing::info!(addr = %app.local_addr()?, "starting server");
    app.run().await
}
