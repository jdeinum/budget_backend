use std::sync::LazyLock;

use budget::config::{AppConfig, DatabaseConfig, PlaidEnv, PlaidField, ServerConfig};

static TRACING: LazyLock<()> = LazyLock::new(|| {
    if std::env::var("TESTING_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .init();
    }
});

pub struct TestApp {
    pub address: String,
    pub api_client: reqwest::Client,
    // Held so the backing file isn't deleted while the test runs.
    _db_file: tempfile::TempPath,
}

/// Builds and starts the real app (same `budget::build` main.rs uses) against
/// a throwaway SQLite file and an OS-assigned port, and returns an HTTP
/// client pointed at it.
pub async fn spawn_app() -> anyhow::Result<TestApp> {
    LazyLock::force(&TRACING);

    let db_file = tempfile::NamedTempFile::new()?.into_temp_path();

    let config = AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
        },
        database: DatabaseConfig {
            url: format!("sqlite://{}", db_file.display()),
        },
        plaid: PlaidField {
            client_id: "test-client-id".into(),
            secret: "test-secret".into(),
            env: PlaidEnv::Sandbox,
        },
    };

    let app = budget::build(config).await?;
    let address = format!("http://{}", app.local_addr()?);

    tokio::spawn(app.run());

    let api_client = reqwest::Client::new();

    Ok(TestApp {
        address,
        api_client,
        _db_file: db_file,
    })
}
