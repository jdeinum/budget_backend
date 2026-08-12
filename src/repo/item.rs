use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::db_uuid::DbUuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct Item {
    pub id: DbUuid,
    pub plaid_item_id: String,
    #[serde(skip_serializing)]
    pub access_token: String,
    pub institution_id: Option<String>,
    pub cursor: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn upsert_item(
    pool: &SqlitePool,
    plaid_item_id: &str,
    access_token: &str,
    institution_id: Option<&str>,
) -> sqlx::Result<Item> {
    let now = Utc::now();
    sqlx::query_as::<_, Item>(
        r#"
        INSERT INTO items (id, plaid_item_id, access_token, institution_id, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?5)
        ON CONFLICT (plaid_item_id) DO UPDATE SET
            access_token = excluded.access_token,
            institution_id = excluded.institution_id,
            updated_at = excluded.updated_at
        RETURNING *
        "#,
    )
    .bind(DbUuid::from(Uuid::new_v4()))
    .bind(plaid_item_id)
    .bind(access_token)
    .bind(institution_id)
    .bind(now)
    .fetch_one(pool)
    .await
}

pub async fn list_items(pool: &SqlitePool) -> sqlx::Result<Vec<Item>> {
    sqlx::query_as::<_, Item>("SELECT * FROM items ORDER BY created_at DESC")
        .fetch_all(pool)
        .await
}

pub async fn get_item(pool: &SqlitePool, id: Uuid) -> sqlx::Result<Option<Item>> {
    sqlx::query_as::<_, Item>("SELECT * FROM items WHERE id = ?1")
        .bind(DbUuid::from(id))
        .fetch_optional(pool)
        .await
}

/// Deletes the item and, via `ON DELETE CASCADE`, everything scoped to it
/// (accounts, transactions, and their tags) — see `0001_init.sql`. Merchants
/// aren't item-scoped and are left as-is. Returns whether it existed.
pub async fn delete_item(pool: &SqlitePool, id: Uuid) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM items WHERE id = ?1")
        .bind(DbUuid::from(id))
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn update_cursor(pool: &SqlitePool, id: Uuid, cursor: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE items SET cursor = ?1, updated_at = ?2 WHERE id = ?3")
        .bind(cursor)
        .bind(Utc::now())
        .bind(DbUuid::from(id))
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plaid::models::PlaidTransaction;
    use crate::repo::{account, tag, transaction};
    use chrono::NaiveDate;
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
    async fn deleting_an_item_cascades_to_its_accounts_transactions_and_tags() {
        let pool = pool().await;
        let item = upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1").await.unwrap();

        let tx = PlaidTransaction {
            transaction_id: "tx_1".to_string(),
            account_id: "acc_1".to_string(),
            amount: 12.34,
            iso_currency_code: Some("USD".to_string()),
            unofficial_currency_code: None,
            date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            datetime: None,
            name: Some("Test Tx".to_string()),
            merchant_name: None,
            merchant_entity_id: None,
            pending: false,
            payment_channel: Some("online".to_string()),
            personal_finance_category: None,
        };
        let transaction_id = transaction::upsert_transaction(&pool, item.id.into(), None, &tx)
            .await
            .unwrap();
        tag::tag_transaction(&pool, transaction_id, "category", "TEST").await.unwrap();

        let existed = delete_item(&pool, item.id.into()).await.unwrap();
        assert!(existed);

        let accounts = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM accounts")
            .fetch_one(&pool)
            .await
            .unwrap();
        let transactions = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transactions")
            .fetch_one(&pool)
            .await
            .unwrap();
        let transaction_tags = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transaction_tags")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(accounts, 0);
        assert_eq!(transactions, 0);
        assert_eq!(transaction_tags, 0);
    }

    #[tokio::test]
    async fn deleting_a_nonexistent_item_returns_false() {
        let pool = pool().await;
        let existed = delete_item(&pool, Uuid::new_v4()).await.unwrap();
        assert!(!existed);
    }
}
