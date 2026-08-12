use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::db_uuid::DbUuid;

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

/// The open (effective_to IS NULL) category period for a merchant, if any.
pub async fn get_current(pool: &SqlitePool, merchant_id: Uuid) -> sqlx::Result<Option<MerchantCategory>> {
    sqlx::query_as::<_, MerchantCategory>(
        "SELECT * FROM merchant_categories WHERE merchant_id = ?1 AND effective_to IS NULL",
    )
    .bind(DbUuid::from(merchant_id))
    .fetch_optional(pool)
    .await
}

pub async fn close_current(pool: &SqlitePool, id: Uuid, effective_to: NaiveDate) -> sqlx::Result<()> {
    sqlx::query("UPDATE merchant_categories SET effective_to = ?1 WHERE id = ?2")
        .bind(effective_to)
        .bind(DbUuid::from(id))
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn open_new(
    pool: &SqlitePool,
    merchant_id: Uuid,
    category_primary: Option<&str>,
    category_detailed: Option<&str>,
    effective_from: NaiveDate,
) -> sqlx::Result<MerchantCategory> {
    sqlx::query_as::<_, MerchantCategory>(
        r#"
        INSERT INTO merchant_categories (id, merchant_id, category_primary, category_detailed, effective_from, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        RETURNING *
        "#,
    )
    .bind(DbUuid::from(Uuid::new_v4()))
    .bind(DbUuid::from(merchant_id))
    .bind(category_primary)
    .bind(category_detailed)
    .bind(effective_from)
    .bind(Utc::now())
    .fetch_one(pool)
    .await
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

/// Records `(category_primary, category_detailed)` as observed on `observed_on`
/// for `merchant_id`, opening a new history row only if it differs from the
/// currently open one (and closing that one out first).
pub async fn record_observation(
    pool: &SqlitePool,
    merchant_id: Uuid,
    category_primary: Option<&str>,
    category_detailed: Option<&str>,
    observed_on: NaiveDate,
) -> sqlx::Result<()> {
    let current = get_current(pool, merchant_id).await?;

    match current {
        Some(current)
            if current.category_primary.as_deref() == category_primary
                && current.category_detailed.as_deref() == category_detailed =>
        {
            // Already up to date.
        }
        Some(current) => {
            if observed_on > current.effective_from {
                close_current(pool, current.id.into(), observed_on).await?;
                open_new(
                    pool,
                    merchant_id,
                    category_primary,
                    category_detailed,
                    observed_on,
                )
                .await?;
            }
            // An observation older than the current period's start is out of
            // order (e.g. a late-arriving backfill); leave history as-is
            // rather than rewriting it under an earlier date.
        }
        None => {
            open_new(
                pool,
                merchant_id,
                category_primary,
                category_detailed,
                observed_on,
            )
            .await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::merchant;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn tracks_category_changes_over_time() {
        let pool = pool().await;
        let name = format!("Test Merchant {}", Uuid::new_v4());
        let m = merchant::upsert_merchant(&pool, &name, None).await.unwrap();

        let d1 = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        let d3 = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();

        // First observation opens a history row.
        record_observation(&pool, m.id.into(), Some("FOOD_AND_DRINK"), Some("COFFEE"), d1)
            .await
            .unwrap();
        let history = list_for_merchant(&pool, m.id.into()).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].category_primary.as_deref(), Some("FOOD_AND_DRINK"));
        assert_eq!(history[0].effective_to, None);

        // A repeat of the same category on a later date is a no-op.
        record_observation(&pool, m.id.into(), Some("FOOD_AND_DRINK"), Some("COFFEE"), d2)
            .await
            .unwrap();
        assert_eq!(list_for_merchant(&pool, m.id.into()).await.unwrap().len(), 1);

        // A different category on a later date closes the old row and opens a new one.
        record_observation(&pool, m.id.into(), Some("GENERAL_MERCHANDISE"), None, d3)
            .await
            .unwrap();
        let history = list_for_merchant(&pool, m.id.into()).await.unwrap();
        assert_eq!(history.len(), 2);
        let current = history.iter().find(|h| h.effective_to.is_none()).unwrap();
        assert_eq!(current.category_primary.as_deref(), Some("GENERAL_MERCHANDISE"));
        assert_eq!(current.effective_from, d3);
        let closed = history.iter().find(|h| h.effective_to.is_some()).unwrap();
        assert_eq!(closed.category_primary.as_deref(), Some("FOOD_AND_DRINK"));
        assert_eq!(closed.effective_to, Some(d3));

        // An out-of-order (earlier) observation with a different category does
        // not rewrite history.
        record_observation(&pool, m.id.into(), Some("TRANSPORTATION"), None, d1)
            .await
            .unwrap();
        assert_eq!(list_for_merchant(&pool, m.id.into()).await.unwrap().len(), 2);
    }
}
