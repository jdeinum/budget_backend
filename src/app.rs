use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tokio::net::TcpListener;

use crate::config::AppConfig;
use crate::plaid::PlaidClient;
use crate::state::AppState;
use crate::utils::clock::SystemClock;

/// A fully built app: routes wired to a live DB pool, bound to a listener but
/// not yet serving. Splitting bind from serve lets callers (tests, mainly)
/// read back the OS-assigned port before traffic starts.
pub struct App {
    router: Router,
    listener: TcpListener,
}

impl App {
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub async fn run(self) -> anyhow::Result<()> {
        axum::serve(self.listener, self.router).await?;
        Ok(())
    }
}

pub async fn build(config: AppConfig) -> anyhow::Result<App> {
    if let Some(path) = config
        .database
        .url
        .strip_prefix("sqlite://")
        .or_else(|| config.database.url.strip_prefix("sqlite:"))
    {
        let path = path.split('?').next().unwrap_or(path);
        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
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
        clock: Arc::new(SystemClock),
    };

    let router = crate::routes::router().with_state(state);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    Ok(App { router, listener })
}
