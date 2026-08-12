mod config;
mod cursor;
mod db_uuid;
mod error;
mod plaid;
mod repo;
mod routes;
mod search;
mod source;
mod state;
mod statement;
mod sync;

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tracing_subscriber::EnvFilter;

use crate::config::AppConfig;
use crate::plaid::PlaidClient;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = AppConfig::load()?;

    if let Some(path) = config
        .database
        .url
        .strip_prefix("sqlite://")
        .or_else(|| config.database.url.strip_prefix("sqlite:"))
    {
        let path = path.split('?').next().unwrap_or(path);
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
    }

    let options: SqliteConnectOptions = config.database.url.parse()?;
    let options = options
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await?;

    sqlx::migrate!().run(&pool).await?;

    let plaid = PlaidClient::new(&config.plaid);

    let state = AppState {
        pool,
        plaid: Arc::new(plaid),
        config: Arc::new(config.clone()),
    };

    let app = routes::router().with_state(state);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!(%addr, "starting server");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
