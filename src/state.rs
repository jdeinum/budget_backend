use std::sync::Arc;

use sqlx::SqlitePool;

use crate::config::AppConfig;
use crate::plaid::PlaidClient;
use crate::utils::clock::Clock;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub plaid: Arc<PlaidClient>,
    pub config: Arc<AppConfig>,
    pub clock: Arc<dyn Clock>,
}
