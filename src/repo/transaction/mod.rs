mod import;
mod listing;

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::plaid::models::PlaidTransaction;
use crate::repo::tag::{self, TagValue};
use crate::utils::db_uuid::DbUuid;
use crate::utils::source::Source;

pub use import::{ImportSummary, import_transactions};
pub use listing::{SortDir, SortField, TransactionCursor, list_paginated};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct Transaction {
    pub id: DbUuid,
    /// `None` for a manually-imported transaction (see `source`) — Plaid
    /// transactions always have one.
    pub item_id: Option<DbUuid>,
    pub source: Source,
    pub plaid_transaction_id: Option<String>,
    /// Only meaningful for non-Plaid sources — see `import::import_transactions`
    /// and `idx_transactions_import_dedup` in `0004_manual_accounts_transactions.sql`.
    pub occurrence: i64,
    pub account_id: String,
    pub amount: f64,
    pub iso_currency_code: Option<String>,
    pub unofficial_currency_code: Option<String>,
    pub date: NaiveDate,
    pub datetime: Option<DateTime<Utc>>,
    pub name: Option<String>,
    pub merchant_name: Option<String>,
    pub pending: bool,
    pub payment_channel: Option<String>,
    pub merchant_id: Option<DbUuid>,
    pub ignored: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A transaction joined with its merchant's name and tags, for listing views.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct TransactionWithMerchant {
    pub id: DbUuid,
    pub item_id: Option<DbUuid>,
    pub source: Source,
    pub account_id: String,
    pub amount: f64,
    pub iso_currency_code: Option<String>,
    pub date: NaiveDate,
    pub datetime: Option<DateTime<Utc>>,
    pub name: Option<String>,
    pub merchant_name: Option<String>,
    pub pending: bool,
    pub payment_channel: Option<String>,
    pub merchant_id: Option<DbUuid>,
    pub ignored: bool,
    #[sqlx(skip)]
    pub tags: Vec<TagValue>,
}

pub async fn upsert_transaction(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    item_id: Uuid,
    merchant_id: Option<Uuid>,
    tx: &PlaidTransaction,
) -> sqlx::Result<Uuid> {
    let (id,): (DbUuid,) = sqlx::query_as(
        r"
        INSERT INTO transactions (
            id, item_id, plaid_transaction_id, account_id, amount,
            iso_currency_code, unofficial_currency_code, date, datetime,
            name, merchant_name, pending, payment_channel, merchant_id,
            created_at, updated_at
        )
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15)
        ON CONFLICT (plaid_transaction_id) WHERE plaid_transaction_id IS NOT NULL DO UPDATE SET
            amount = excluded.amount,
            iso_currency_code = excluded.iso_currency_code,
            unofficial_currency_code = excluded.unofficial_currency_code,
            date = excluded.date,
            datetime = excluded.datetime,
            name = excluded.name,
            merchant_name = excluded.merchant_name,
            pending = excluded.pending,
            payment_channel = excluded.payment_channel,
            merchant_id = excluded.merchant_id,
            updated_at = excluded.updated_at
        RETURNING id
        ",
    )
    .bind(DbUuid::from(Uuid::new_v4()))
    .bind(DbUuid::from(item_id))
    .bind(&tx.transaction_id)
    .bind(&tx.account_id)
    .bind(tx.amount)
    .bind(&tx.iso_currency_code)
    .bind(&tx.unofficial_currency_code)
    .bind(tx.date)
    .bind(tx.datetime)
    .bind(&tx.name)
    .bind(&tx.merchant_name)
    .bind(tx.pending)
    .bind(&tx.payment_channel)
    .bind(merchant_id.map(DbUuid::from))
    .bind(now)
    .fetch_one(pool)
    .await?;

    Ok(id.into())
}

/// Adds a single hand-entered transaction to `account_id` — the entry
/// point behind the "Add transaction" button, as opposed to a batch
/// statement import. `occurrence` still has to be computed (the dedup
/// index in `0004_manual_accounts_transactions.sql` covers every non-Plaid
/// source, not just imports) but there's no batch to dedupe within here —
/// this is an intentional single add, not something to skip — so it's
/// computed directly as `MAX(occurrence) + 1` in the same INSERT rather
/// than via the app-side grouping `import::import_transactions` needs.
///
/// `merchant_id`, if given, links the transaction to that existing merchant
/// directly instead of the usual by-name fallback — so it picks up that
/// merchant's tags (category, etc.) through the tag hierarchy rather than
/// spawning a new merchant named after this one transaction's description.
/// The caller is expected to have already checked it exists.
pub async fn create_manual_transaction(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    account_id: &str,
    date: NaiveDate,
    amount: f64,
    name: &str,
    merchant_id: Option<Uuid>,
) -> sqlx::Result<Transaction> {
    // There's no separate merchant field on a hand-entered transaction —
    // absent an explicit `merchant_id`, `name` doubles as both, same
    // fallback every importer uses when its export has no distinct
    // merchant column of its own.
    let merchant_id: DbUuid = match merchant_id {
        Some(id) => DbUuid::from(id),
        None => {
            crate::repo::merchant::upsert_merchant(pool, now, name, None)
                .await?
                .id
        }
    };

    sqlx::query_as::<_, Transaction>(
        r"
        INSERT INTO transactions (
            id, item_id, source, plaid_transaction_id, occurrence,
            account_id, amount, date, name, merchant_name, merchant_id, created_at, updated_at
        )
        VALUES (
            ?1, NULL, 'manual', NULL,
            COALESCE(
                (SELECT MAX(occurrence) + 1 FROM transactions
                 WHERE account_id = ?2 AND date = ?3 AND amount = ?4 AND name = ?5 AND source != 'plaid'),
                0
            ),
            ?2, ?4, ?3, ?5, ?5, ?6, ?7, ?7
        )
        RETURNING *
        ",
    )
    .bind(DbUuid::from(Uuid::new_v4()))
    .bind(account_id)
    .bind(date)
    .bind(amount)
    .bind(name)
    .bind(merchant_id)
    .bind(now)
    .fetch_one(pool)
    .await
}

/// Fetches one transaction in the same tag-enriched shape the `/transactions`
/// listing uses, for returning a freshly-created or freshly-mutated row to
/// the caller without them having to re-derive it from the plain `Transaction`
/// row (which lacks tags and the `source`-driven fields listing callers expect).
pub async fn get(pool: &SqlitePool, id: Uuid) -> sqlx::Result<Option<TransactionWithMerchant>> {
    let row = sqlx::query_as::<_, TransactionWithMerchant>(
        "SELECT id, item_id, source, account_id, amount, iso_currency_code, date, datetime, \
         name, merchant_name, pending, payment_channel, merchant_id, ignored \
         FROM transactions WHERE id = ?1",
    )
    .bind(DbUuid::from(id))
    .fetch_optional(pool)
    .await?;

    let Some(mut row) = row else { return Ok(None) };

    let account_tags = tag::list_for_account(pool, &row.account_id).await?;
    let merchant_tags = match row.merchant_id {
        Some(merchant_id) => tag::list_for_merchant(pool, merchant_id.into()).await?,
        None => vec![],
    };
    let own_tags = tag::list_for_transaction(pool, id).await?;
    row.tags = tag::merge_layers(&[&account_tags, &merchant_tags, &own_tags]);

    Ok(Some(row))
}

pub async fn exists(pool: &SqlitePool, id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM transactions WHERE id = ?1)")
        .bind(DbUuid::from(id))
        .fetch_one(pool)
        .await
}

pub async fn delete_transaction(pool: &SqlitePool, plaid_transaction_id: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM transactions WHERE plaid_transaction_id = ?1")
        .bind(plaid_transaction_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_transactions_for_item(
    pool: &SqlitePool,
    item_id: Uuid,
) -> sqlx::Result<Vec<Transaction>> {
    sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions WHERE item_id = ?1 ORDER BY date DESC, created_at DESC",
    )
    .bind(DbUuid::from(item_id))
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plaid::models::PlaidTransaction;
    use crate::repo::{account, item, tag};

    fn tx(id: &str, date: NaiveDate) -> PlaidTransaction {
        PlaidTransaction {
            transaction_id: id.to_string(),
            account_id: "acc_1".to_string(),
            amount: 12.34,
            iso_currency_code: Some("USD".to_string()),
            unofficial_currency_code: None,
            date,
            datetime: None,
            name: Some("Test Tx".to_string()),
            merchant_name: None,
            merchant_entity_id: None,
            pending: false,
            payment_channel: Some("online".to_string()),
        }
    }

    #[sqlx::test]
    async fn a_transactions_own_tag_overrides_its_merchants_tag_of_the_same_name(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, Utc::now(), "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let merchant = crate::repo::merchant::upsert_merchant(&pool, Utc::now(), "Costco", None)
            .await
            .unwrap();
        tag::tag_merchant(&pool, merchant.id.into(), "category", "GENERAL_MERCHANDISE")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let id = upsert_transaction(
            &pool,
            Utc::now(),
            item.id.into(),
            Some(merchant.id.into()),
            &tx("t1", d),
        )
        .await
        .unwrap();
        tag::tag_transaction(&pool, id, "category", "TRAVEL")
            .await
            .unwrap();

        let fetched = get(&pool, id).await.unwrap().unwrap();

        assert_eq!(
            fetched.tags,
            vec![TagValue {
                name: "category".into(),
                value: "TRAVEL".into()
            }]
        );
    }

    // 42.5 round-trips exactly through SQLite's REAL with no arithmetic in
    // between, so exact equality is the right check here.
    #[allow(clippy::float_cmp)]
    #[sqlx::test]
    async fn creates_a_manual_transaction(pool: SqlitePool) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();

        let created =
            create_manual_transaction(&pool, Utc::now(), &account.id, d, 42.5, "Cash tip", None)
                .await
                .unwrap();

        assert_eq!(created.source, Source::Manual);
        assert_eq!(created.account_id, account.id);
        assert_eq!(created.amount, 42.5);
        assert_eq!(created.date, d);
        assert_eq!(created.name.as_deref(), Some("Cash tip"));
        assert_eq!(created.item_id, None);
        assert_eq!(created.plaid_transaction_id, None);
    }

    #[sqlx::test]
    async fn manual_transactions_get_distinct_occurrences_when_they_collide(pool: SqlitePool) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();

        let first =
            create_manual_transaction(&pool, Utc::now(), &account.id, d, 4.50, "Coffee", None)
                .await
                .unwrap();
        let second =
            create_manual_transaction(&pool, Utc::now(), &account.id, d, 4.50, "Coffee", None)
                .await
                .unwrap();

        assert_eq!(first.occurrence, 0);
        assert_eq!(second.occurrence, 1);
    }

    #[sqlx::test]
    async fn get_returns_a_tag_enriched_transaction(pool: SqlitePool) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Amex, "Amex")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 7).unwrap();
        let created = create_manual_transaction(
            &pool,
            Utc::now(),
            &account.id,
            d,
            15.99,
            "Membership fee",
            None,
        )
        .await
        .unwrap();
        tag::tag_transaction(&pool, created.id.into(), "category", "FEES")
            .await
            .unwrap();

        let fetched = get(&pool, created.id.into()).await.unwrap().unwrap();

        assert_eq!(fetched.name.as_deref(), Some("Membership fee"));
        assert_eq!(fetched.source, Source::Manual);
        assert_eq!(
            fetched.tags,
            vec![TagValue {
                name: "category".into(),
                value: "FEES".into()
            }]
        );
    }

    #[sqlx::test]
    async fn get_returns_none_for_a_nonexistent_id(pool: SqlitePool) {
        assert!(get(&pool, Uuid::new_v4()).await.unwrap().is_none());
    }

    #[sqlx::test]
    async fn create_manual_transaction_links_a_merchant(pool: SqlitePool) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Amex, "Amex")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 7).unwrap();

        let created =
            create_manual_transaction(&pool, Utc::now(), &account.id, d, 15.99, "Starbucks", None)
                .await
                .unwrap();

        assert_eq!(created.merchant_name.as_deref(), Some("Starbucks"));
        let merchant_id = created.merchant_id.expect("merchant should be linked");

        let merchant = sqlx::query_scalar::<_, String>("SELECT name FROM merchants WHERE id = ?1")
            .bind(merchant_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(merchant, "Starbucks");
    }

    #[sqlx::test]
    async fn create_manual_transaction_links_an_explicitly_chosen_merchant(pool: SqlitePool) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Amex, "Amex")
            .await
            .unwrap();
        let merchant = crate::repo::merchant::upsert_merchant(&pool, Utc::now(), "Starbucks", None)
            .await
            .unwrap();
        tag::tag_merchant(&pool, merchant.id.into(), "category", "FOOD_AND_DRINK")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 7).unwrap();

        // The free-text description differs from the merchant's name — an
        // explicit `merchant_id` should still link to the chosen merchant
        // rather than creating a new one named after the description.
        let created = create_manual_transaction(
            &pool,
            Utc::now(),
            &account.id,
            d,
            6.25,
            "Coffee run",
            Some(merchant.id.into()),
        )
        .await
        .unwrap();

        assert_eq!(created.merchant_id, Some(merchant.id));
        let merchant_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merchants")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(merchant_count, 1);

        let fetched = get(&pool, created.id.into()).await.unwrap().unwrap();
        assert_eq!(
            fetched.tags,
            vec![TagValue {
                name: "category".into(),
                value: "FOOD_AND_DRINK".into()
            }]
        );
    }
}
