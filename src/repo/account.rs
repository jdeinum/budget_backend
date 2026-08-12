use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::repo::tag::{self, TagValue};
use crate::utils::db_uuid::DbUuid;
use crate::utils::source::Source;

/// An account, either synced from a Plaid item (keyed on Plaid's own
/// `account_id` — there's no separate identity to dedupe the way merchants
/// have) or created manually for statement-import accounts, which have no
/// item and get a generated id instead.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct Account {
    pub id: String,
    /// `None` for a manually-created account — see `source`.
    pub item_id: Option<DbUuid>,
    /// `Source::Plaid` for a synced account, or the issuer it was declared
    /// as when created manually (`Source::Manual` never appears here — see
    /// `Source`'s docs).
    pub source: Source,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[sqlx(skip)]
    pub tags: Vec<TagValue>,
}

/// Ensures an account row exists for `account_id`, auto-creating it the
/// first time a transaction references it (mirrors how merchants are
/// auto-created). `name` seeds the row the first time it's created — the
/// caller passes Plaid's own friendly account name where available (see
/// `PlaidAccount` in `plaid/models.rs`), falling back to the raw
/// `account_id` if not.
///
/// On conflict, `name` is only applied if the existing row's name still
/// equals its own `id` — i.e. it was never actually renamed, just created
/// before a Plaid name was available (or without one). A genuine user rename
/// (or a genuine Plaid name, which is never identical to the raw id) is left
/// untouched, so re-syncing an account you've renamed never clobbers it,
/// while accounts stuck with the old id-as-name fallback pick up a real name
/// on their next sync.
pub async fn upsert_account(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    account_id: &str,
    item_id: Uuid,
    name: &str,
) -> sqlx::Result<Account> {
    sqlx::query_as::<_, Account>(
        r#"
        INSERT INTO accounts (id, item_id, name, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?4)
        ON CONFLICT (id) DO UPDATE SET
            name = CASE WHEN accounts.name = accounts.id THEN excluded.name ELSE accounts.name END,
            updated_at = excluded.updated_at
        RETURNING *
        "#,
    )
    .bind(account_id)
    .bind(DbUuid::from(item_id))
    .bind(name)
    .bind(now)
    .fetch_one(pool)
    .await
}

/// Creates an account with no backing Plaid item — the entry point for
/// statement-import accounts. `source` declares which issuer's format its
/// statements are expected to be in (picks the importer — see
/// `routes::accounts::importer_for`) and must be `Source::Neo` or
/// `Source::Amex`; the caller (the route) is responsible for rejecting
/// anything else.
pub async fn create_manual_account(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    source: Source,
    name: &str,
) -> sqlx::Result<Account> {
    sqlx::query_as::<_, Account>(
        r#"
        INSERT INTO accounts (id, item_id, source, name, created_at, updated_at)
        VALUES (?1, NULL, ?2, ?3, ?4, ?4)
        RETURNING *
        "#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(source)
    .bind(name)
    .bind(now)
    .fetch_one(pool)
    .await
}

/// Deletes a manually-created account, its transactions, and its tags.
/// Scoped to non-Plaid accounts — a Plaid account is only ever removed by
/// disconnecting its item (`item::delete_item`), which also revokes access
/// on Plaid's side first. Returns whether it existed (and was eligible for
/// direct deletion).
///
/// Unlike `item::delete_item`, this can't lean on `ON DELETE CASCADE`
/// alone: `transactions.account_id` has no cascade (only `account_tags`
/// does — see `0001_init.sql`), since a transaction losing its account out
/// from under it is normally a bug, not something to shrug off. So
/// transactions are deleted explicitly first, in the same transaction.
pub async fn delete_account(pool: &SqlitePool, id: &str) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;

    let target: Option<(String,)> =
        sqlx::query_as("SELECT id FROM accounts WHERE id = ?1 AND source != 'plaid'")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if target.is_none() {
        return Ok(false);
    }

    sqlx::query("DELETE FROM transactions WHERE account_id = ?1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM accounts WHERE id = ?1")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(true)
}

pub async fn list_accounts(pool: &SqlitePool) -> sqlx::Result<Vec<Account>> {
    let mut accounts: Vec<Account> =
        sqlx::query_as::<_, Account>("SELECT * FROM accounts ORDER BY name ASC")
            .fetch_all(pool)
            .await?;

    let ids: Vec<String> = accounts.iter().map(|a| a.id.clone()).collect();
    let tag_rows = tag::list_for_accounts(pool, &ids).await?;
    let mut tags_by_account: HashMap<String, Vec<TagValue>> = HashMap::new();
    for row in tag_rows {
        tags_by_account
            .entry(row.account_id)
            .or_default()
            .push(TagValue {
                name: row.name,
                value: row.value,
            });
    }
    for a in &mut accounts {
        a.tags = tags_by_account.remove(&a.id).unwrap_or_default();
    }

    Ok(accounts)
}

pub async fn get_account(pool: &SqlitePool, id: &str) -> sqlx::Result<Option<Account>> {
    sqlx::query_as::<_, Account>("SELECT * FROM accounts WHERE id = ?1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub async fn rename_account(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    id: &str,
    name: &str,
) -> sqlx::Result<Option<Account>> {
    sqlx::query_as::<_, Account>(
        "UPDATE accounts SET name = ?1, updated_at = ?2 WHERE id = ?3 RETURNING *",
    )
    .bind(name)
    .bind(now)
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::item;

    #[sqlx::test]
    async fn upsert_seeds_name_from_the_caller_on_first_sight(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();

        let account = upsert_account(
            &pool,
            Utc::now(),
            "acc_123",
            item.id.into(),
            "Plaid Checking",
        )
        .await
        .unwrap();

        assert_eq!(account.id, "acc_123");
        assert_eq!(account.name, "Plaid Checking");
    }

    #[sqlx::test]
    async fn id_as_name_fallback_is_backfilled_on_next_sync(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();

        // Created before a Plaid name was available (or without one) —
        // name falls back to the raw id, same as pre-existing accounts.
        let account = upsert_account(&pool, Utc::now(), "acc_123", item.id.into(), "acc_123")
            .await
            .unwrap();
        assert_eq!(account.name, "acc_123");

        // The next sync has a real Plaid name — it should backfill, since
        // the account was never actually renamed.
        let resynced = upsert_account(
            &pool,
            Utc::now(),
            "acc_123",
            item.id.into(),
            "Plaid Checking",
        )
        .await
        .unwrap();
        assert_eq!(resynced.name, "Plaid Checking");
    }

    #[sqlx::test]
    async fn rename_survives_a_resync(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();

        upsert_account(
            &pool,
            Utc::now(),
            "acc_123",
            item.id.into(),
            "Plaid Checking",
        )
        .await
        .unwrap();
        let renamed = rename_account(&pool, Utc::now(), "acc_123", "Checking")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(renamed.name, "Checking");

        // A re-sync upserts again — this must NOT clobber the rename, even
        // though Plaid's own name is passed again.
        let resynced = upsert_account(
            &pool,
            Utc::now(),
            "acc_123",
            item.id.into(),
            "Plaid Checking",
        )
        .await
        .unwrap();
        assert_eq!(resynced.name, "Checking");
    }

    #[sqlx::test]
    async fn lists_accounts_with_tags(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        upsert_account(
            &pool,
            Utc::now(),
            "acc_123",
            item.id.into(),
            "Plaid Checking",
        )
        .await
        .unwrap();
        tag::tag_account(&pool, "acc_123", "type", "checking")
            .await
            .unwrap();

        let accounts = list_accounts(&pool).await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(
            accounts[0].tags,
            vec![TagValue {
                name: "type".into(),
                value: "checking".into()
            }]
        );
    }

    #[sqlx::test]
    async fn deletes_a_manual_account_and_its_transactions(pool: SqlitePool) {
        let account = create_manual_account(&pool, Utc::now(), Source::Amex, "Amex")
            .await
            .unwrap();
        crate::repo::transaction::import_transactions(
            &pool,
            Utc::now(),
            &account.id,
            Source::Amex,
            &[crate::statement::ParsedTransaction {
                date: chrono::NaiveDate::from_ymd_opt(2026, 8, 7).unwrap(),
                amount: 15.99,
                description: "MEMBERSHIP FEE".into(),
                merchant: "MEMBERSHIP FEE".into(),
            }],
        )
        .await
        .unwrap();

        let deleted = delete_account(&pool, &account.id).await.unwrap();
        assert!(deleted);
        assert!(get_account(&pool, &account.id).await.unwrap().is_none());

        let remaining_transactions =
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transactions WHERE account_id = ?1")
                .bind(&account.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining_transactions, 0);
    }

    #[sqlx::test]
    async fn refuses_to_delete_a_plaid_account(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        upsert_account(
            &pool,
            Utc::now(),
            "acc_123",
            item.id.into(),
            "Plaid Checking",
        )
        .await
        .unwrap();

        let deleted = delete_account(&pool, "acc_123").await.unwrap();
        assert!(!deleted);
        assert!(get_account(&pool, "acc_123").await.unwrap().is_some());
    }
}
