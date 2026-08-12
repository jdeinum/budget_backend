use serde::Deserialize;

/// Plaid API credentials and environment selection.
///
/// `client_id` and `secret` are secrets and should be supplied via environment
/// variables (`BUDGET__PLAID__CLIENT_ID`, `BUDGET__PLAID__SECRET`), not committed
/// to `config/*.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaidField {
    pub client_id: String,
    pub secret: String,
    pub env: PlaidEnv,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlaidEnv {
    Sandbox,
    Production,
}

impl PlaidEnv {
    pub fn base_url(self) -> &'static str {
        match self {
            PlaidEnv::Sandbox => "https://sandbox.plaid.com",
            PlaidEnv::Production => "https://production.plaid.com",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub plaid: PlaidField,
}

impl AppConfig {
    /// Loads configuration from `config/default.toml`, an optional
    /// `config/{RUN_MODE}.toml` override, and environment variables prefixed
    /// with `BUDGET` (double underscore separated, e.g. `BUDGET__PLAID__SECRET`
    /// sets `plaid.secret`). Env vars take precedence over files.
    pub fn load() -> Result<Self, config::ConfigError> {
        let run_mode = std::env::var("RUN_MODE").unwrap_or_else(|_| "development".into());

        let cfg = config::Config::builder()
            .add_source(config::File::with_name("config/default"))
            .add_source(config::File::with_name(&format!("config/{run_mode}")).required(false))
            .add_source(
                config::Environment::with_prefix("BUDGET")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        cfg.try_deserialize()
    }
}
