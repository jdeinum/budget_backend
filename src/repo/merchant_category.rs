use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::utils::db_uuid::DbUuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct MerchantCategory {
    pub id: DbUuid,
    pub merchant_id: DbUuid,
    pub category_primary: Option<String>,
    pub category_detailed: Option<String>,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
}

pub async fn list_for_merchant(
    pool: &SqlitePool,
    merchant_id: Uuid,
) -> sqlx::Result<Vec<MerchantCategory>> {
    sqlx::query_as::<_, MerchantCategory>(
        "SELECT * FROM merchant_categories WHERE merchant_id = ?1 ORDER BY effective_from DESC",
    )
    .bind(DbUuid::from(merchant_id))
    .fetch_all(pool)
    .await
}
