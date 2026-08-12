use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use std::collections::HashMap;
use uuid::Uuid;

use crate::db_uuid::DbUuid;
use crate::plaid::models::PlaidTransaction;
use crate::repo::tag::{self, TagValue};
use crate::search::fts5_query;
use crate::source::Source;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct Transaction {
    pub id: DbUuid,
    /// `None` for a manually-imported transaction (see `source`) — Plaid
    /// transactions always have one.
    pub item_id: Option<DbUuid>,
    pub source: Source,
    pub plaid_transaction_id: Option<String>,
    /// Only meaningful for non-Plaid sources — see `import_transactions`
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
    item_id: Uuid,
    merchant_id: Option<Uuid>,
    tx: &PlaidTransaction,
) -> sqlx::Result<Uuid> {
    let now = Utc::now();
    let (id,): (DbUuid,) = sqlx::query_as(
        r#"
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
        "#,
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

/// Outcome of importing a batch of parsed statement rows into one account.
#[derive(Debug, Serialize)]
pub struct ImportSummary {
    pub inserted: i64,
    pub skipped_duplicates: i64,
}

/// Inserts a batch of manually-imported transactions (parsed from a CSV/PDF
/// statement — see `crate::statement`) into `account_id`, skipping rows
/// that duplicate ones already stored. `source` must not be `Source::Plaid`
/// — Plaid rows go through `upsert_transaction` and dedupe on
/// `plaid_transaction_id`, a stable id this path doesn't have.
///
/// Duplicates are detected by `(account_id, date, amount, name, occurrence)`
/// (see `idx_transactions_import_dedup`), where `occurrence` is each row's
/// position among rows sharing the same `(date, amount, description)`
/// *within this batch*. Re-importing an identical file reproduces the same
/// occurrence numbers and is skipped by `ON CONFLICT ... DO NOTHING`; a new
/// file with one additional same-day/same-amount/same-description charge
/// (e.g. two identical coffees) gets the next occurrence number instead of
/// being mistaken for a duplicate.
pub async fn import_transactions(
    pool: &SqlitePool,
    account_id: &str,
    source: Source,
    rows: &[crate::statement::ParsedTransaction],
) -> sqlx::Result<ImportSummary> {
    let now = Utc::now();
    let mut occurrence_of: HashMap<(NaiveDate, u64, String), i64> = HashMap::new();
    let mut inserted = 0i64;
    let mut skipped_duplicates = 0i64;

    for row in rows {
        let key = (row.date, row.amount.to_bits(), row.description.clone());
        let occurrence = occurrence_of
            .entry(key)
            .and_modify(|n| *n += 1)
            .or_insert(0);

        // Upserting even for a row that turns out to be a duplicate (and
        // gets DO NOTHING'd below) is a bit wasteful, but there's no way to
        // know which it'll be until the insert is attempted, and
        // `upsert_merchant` is cheap and idempotent.
        let merchant = crate::repo::merchant::upsert_merchant(pool, &row.merchant, None).await?;

        let newly_inserted: Option<(DbUuid,)> = sqlx::query_as(
            r#"
            INSERT INTO transactions (
                id, item_id, source, plaid_transaction_id, occurrence,
                account_id, amount, date, name, merchant_name, merchant_id, created_at, updated_at
            )
            VALUES (?1, NULL, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
            ON CONFLICT (account_id, date, amount, name, occurrence) WHERE source != 'plaid'
                DO NOTHING
            RETURNING id
            "#,
        )
        .bind(DbUuid::from(Uuid::new_v4()))
        .bind(source)
        .bind(*occurrence)
        .bind(account_id)
        .bind(row.amount)
        .bind(row.date)
        .bind(&row.description)
        .bind(&row.merchant)
        .bind(merchant.id)
        .bind(now)
        .fetch_optional(pool)
        .await?;

        if newly_inserted.is_some() {
            inserted += 1;
        } else {
            skipped_duplicates += 1;
        }
    }

    Ok(ImportSummary {
        inserted,
        skipped_duplicates,
    })
}

/// Adds a single hand-entered transaction to `account_id` — the entry
/// point behind the "Add transaction" button, as opposed to a batch
/// statement import. `occurrence` still has to be computed (the dedup
/// index in `0004_manual_accounts_transactions.sql` covers every non-Plaid
/// source, not just imports) but there's no batch to dedupe within here —
/// this is an intentional single add, not something to skip — so it's
/// computed directly as `MAX(occurrence) + 1` in the same INSERT rather
/// than via the app-side grouping `import_transactions` needs.
///
/// `merchant_id`, if given, links the transaction to that existing merchant
/// directly instead of the usual by-name fallback — so it picks up that
/// merchant's tags (category, etc.) through the tag hierarchy rather than
/// spawning a new merchant named after this one transaction's description.
/// The caller is expected to have already checked it exists.
pub async fn create_manual_transaction(
    pool: &SqlitePool,
    account_id: &str,
    date: NaiveDate,
    amount: f64,
    name: &str,
    merchant_id: Option<Uuid>,
) -> sqlx::Result<Transaction> {
    let now = Utc::now();
    // There's no separate merchant field on a hand-entered transaction —
    // absent an explicit `merchant_id`, `name` doubles as both, same
    // fallback every importer uses when its export has no distinct
    // merchant column of its own.
    let merchant_id: DbUuid = match merchant_id {
        Some(id) => DbUuid::from(id),
        None => {
            crate::repo::merchant::upsert_merchant(pool, name, None)
                .await?
                .id
        }
    };

    sqlx::query_as::<_, Transaction>(
        r#"
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
        "#,
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

/// A column the `/transactions` listing can be sorted by. Deliberately a
/// small closed set (not an arbitrary column name string) so it's safe to
/// interpolate the matching SQL identifier into a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    Date,
    Amount,
}

impl SortField {
    fn column(self) -> &'static str {
        match self {
            SortField::Date => "t.date",
            SortField::Amount => "t.amount",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    fn sql(self) -> &'static str {
        match self {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        }
    }

    /// The row-comparison operator that yields "rows after this one" for
    /// this direction, per standard keyset-pagination.
    fn keyset_op(self) -> &'static str {
        match self {
            SortDir::Asc => ">",
            SortDir::Desc => "<",
        }
    }
}

/// The sorted column's value on the cursor's row — type depends on
/// [`SortField`], so this can't just be a `NaiveDate` like it was when
/// `date DESC` was the only supported order.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CursorSortValue {
    Date(NaiveDate),
    Amount(f64),
}

/// Keyset-pagination cursor: the `(sort_value, id)` of the last row on the
/// previous page, plus the sort itself — a cursor is only valid for the
/// exact `(sort_field, sort_dir)` it was minted under.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TransactionCursor {
    pub sort_field: SortField,
    pub sort_dir: SortDir,
    pub sort_value: CursorSortValue,
    pub id: Uuid,
}

/// Lists transactions across all items, tag-enriched, with keyset pagination,
/// sorting, and optional filters. Every returned transaction's `tags` (and
/// every `tags`/`exclude_tags` match below) is its *effective* tag set under
/// the account < vendor < transaction hierarchy — its own tags, falling back
/// to its merchant's, falling back to its account's, per `name` (see
/// `tag::merge_layers`) — not just the tags attached to the transaction row
/// itself. `tags` requires every listed `(name, value)` pair to be present
/// (AND semantics); `exclude_tags` requires every listed pair to be absent
/// (a transaction matching any one of them is dropped). `q`, if present, is
/// matched against `name`/`merchant_name` via the `transactions_fts` FTS5
/// index.
///
/// `cursor`, if present, must have been minted under the same
/// `(sort_field, sort_dir)` passed here — callers are expected to have
/// checked this (see the route handler); mismatches WHERE-filter against the
/// wrong column and simply won't paginate correctly.
///
/// `ignored_only` selects which side of `transactions.ignored` to return:
/// `false` (the normal case) excludes ignored transactions, `true` returns
/// only ignored ones (used by the settings page's ignored-transactions
/// review list). See [`crate::repo::transaction_rule::reevaluate_ignored`].
#[allow(clippy::too_many_arguments)]
pub async fn list_paginated(
    pool: &SqlitePool,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
    tags: &[(String, String)],
    exclude_tags: &[(String, String)],
    q: Option<&str>,
    sort_field: SortField,
    sort_dir: SortDir,
    cursor: Option<TransactionCursor>,
    limit: i64,
    ignored_only: bool,
) -> sqlx::Result<(Vec<TransactionWithMerchant>, Option<TransactionCursor>)> {
    let column = sort_field.column();

    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT t.id, t.item_id, t.source, t.account_id, t.amount, t.iso_currency_code, \
         t.date, t.datetime, t.name, t.merchant_name, t.pending, t.payment_channel, t.merchant_id, t.ignored \
         FROM transactions t WHERE 1 = 1",
    );

    qb.push(if ignored_only {
        " AND t.ignored = 1"
    } else {
        " AND t.ignored = 0"
    });

    if let Some(start) = start_date {
        qb.push(" AND t.date >= ").push_bind(start);
    }
    if let Some(end) = end_date {
        qb.push(" AND t.date <= ").push_bind(end);
    }
    if let Some(q) = q {
        qb.push(
            " AND t.rowid IN (SELECT rowid FROM transactions_fts WHERE transactions_fts MATCH ",
        )
        .push_bind(fts5_query(q))
        .push(")");
    }
    if let Some(cursor) = cursor {
        qb.push(format!(" AND ({column}, t.id) {} (", sort_dir.keyset_op()));
        match cursor.sort_value {
            CursorSortValue::Date(d) => {
                qb.push_bind(d);
            }
            CursorSortValue::Amount(a) => {
                qb.push_bind(a);
            }
        }
        qb.push(", ").push_bind(DbUuid::from(cursor.id)).push(")");
    }
    // A transaction's *effective* value for `name` under the account < vendor
    // < transaction hierarchy: its own tag if it has one, else its
    // merchant's, else its account's — mirrors `tag::merge_layers`, just
    // expressed as SQL so it can be pushed down into the WHERE clause instead
    // of fetched and filtered after the fact.
    fn push_effective_value(qb: &mut QueryBuilder<Sqlite>, name: &str) {
        qb.push(
            "COALESCE(\
              (SELECT tag.value FROM transaction_tags tt JOIN tags tag ON tag.id = tt.tag_id \
               WHERE tt.transaction_id = t.id AND tag.name = ",
        )
        .push_bind(name.to_string())
        .push(
            "), \
              (SELECT tag.value FROM merchant_tags mt JOIN tags tag ON tag.id = mt.tag_id \
               WHERE mt.merchant_id = t.merchant_id AND tag.name = ",
        )
        .push_bind(name.to_string())
        .push(
            "), \
              (SELECT tag.value FROM account_tags act JOIN tags tag ON tag.id = act.tag_id \
               WHERE act.account_id = t.account_id AND tag.name = ",
        )
        .push_bind(name.to_string())
        .push("))");
    }

    for (name, value) in tags {
        qb.push(" AND ");
        push_effective_value(&mut qb, name);
        qb.push(" = ").push_bind(value.clone());
    }
    for (name, value) in exclude_tags {
        // `IS NOT` (rather than `!=`) so a transaction with no tag under
        // `name` at any level — a NULL effective value — counts as not
        // carrying the excluded value, instead of being dropped by SQL's
        // NULL-comparison-is-NULL rule.
        qb.push(" AND ");
        push_effective_value(&mut qb, name);
        qb.push(" IS NOT ").push_bind(value.clone());
    }

    qb.push(format!(
        " ORDER BY {column} {dir}, t.id {dir} LIMIT ",
        dir = sort_dir.sql()
    ))
    .push_bind(limit + 1);

    let mut rows: Vec<TransactionWithMerchant> = qb.build_query_as().fetch_all(pool).await?;

    let next_cursor = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|t| TransactionCursor {
            sort_field,
            sort_dir,
            sort_value: match sort_field {
                SortField::Date => CursorSortValue::Date(t.date),
                SortField::Amount => CursorSortValue::Amount(t.amount),
            },
            id: t.id.into(),
        })
    } else {
        None
    };

    let ids: Vec<DbUuid> = rows.iter().map(|t| t.id).collect();
    let merchant_ids: Vec<DbUuid> = rows.iter().filter_map(|t| t.merchant_id).collect();
    let account_ids: Vec<String> = {
        let mut ids: Vec<String> = rows.iter().map(|t| t.account_id.clone()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };

    let mut own_by_transaction: HashMap<DbUuid, Vec<TagValue>> = HashMap::new();
    for row in tag::list_for_transactions(pool, &ids).await? {
        own_by_transaction
            .entry(row.transaction_id)
            .or_default()
            .push(TagValue {
                name: row.name,
                value: row.value,
            });
    }
    let mut merchant_by_id: HashMap<DbUuid, Vec<TagValue>> = HashMap::new();
    for row in tag::list_for_merchants(pool, &merchant_ids).await? {
        merchant_by_id
            .entry(row.merchant_id)
            .or_default()
            .push(TagValue {
                name: row.name,
                value: row.value,
            });
    }
    let mut account_by_id: HashMap<String, Vec<TagValue>> = HashMap::new();
    for row in tag::list_for_accounts(pool, &account_ids).await? {
        account_by_id
            .entry(row.account_id)
            .or_default()
            .push(TagValue {
                name: row.name,
                value: row.value,
            });
    }

    for t in &mut rows {
        let account_tags = account_by_id
            .get(&t.account_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let merchant_tags = t
            .merchant_id
            .and_then(|id| merchant_by_id.get(&id))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let own_tags = own_by_transaction
            .get(&t.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        t.tags = tag::merge_layers(&[account_tags, merchant_tags, own_tags]);
    }

    Ok((rows, next_cursor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plaid::models::PlaidTransaction;
    use crate::repo::{account, item, tag};
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
            personal_finance_category: None,
        }
    }

    #[tokio::test]
    async fn paginates_by_date_desc_with_keyset_cursor() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();

        let d1 = NaiveDate::from_ymd_opt(2026, 1, 3).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
        let d3 = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        for (id, date) in [("t1", d1), ("t2", d2), ("t3", d3)] {
            upsert_transaction(&pool, item.id.into(), None, &tx(id, date))
                .await
                .unwrap();
        }

        let (page1, cursor1) = list_paginated(
            &pool,
            None,
            None,
            &[],
            &[],
            None,
            SortField::Date,
            SortDir::Desc,
            None,
            2,
            false,
        )
        .await
        .unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].date, d1);
        assert_eq!(page1[1].date, d2);
        assert!(
            cursor1.is_some(),
            "expected a next_cursor with more rows remaining"
        );

        let (page2, cursor2) = list_paginated(
            &pool,
            None,
            None,
            &[],
            &[],
            None,
            SortField::Date,
            SortDir::Desc,
            cursor1,
            2,
            false,
        )
        .await
        .unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].date, d3);
        assert!(
            cursor2.is_none(),
            "last page should not carry a next_cursor"
        );
    }

    #[tokio::test]
    async fn paginates_by_amount_asc_with_keyset_cursor() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        for (id, amount) in [("cheap", 5.0), ("mid", 20.0), ("pricey", 100.0)] {
            let mut t = tx(id, d);
            t.amount = amount;
            upsert_transaction(&pool, item.id.into(), None, &t)
                .await
                .unwrap();
        }

        let (page1, cursor1) = list_paginated(
            &pool,
            None,
            None,
            &[],
            &[],
            None,
            SortField::Amount,
            SortDir::Asc,
            None,
            2,
            false,
        )
        .await
        .unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].amount, 5.0);
        assert_eq!(page1[1].amount, 20.0);
        assert!(cursor1.is_some());

        let (page2, cursor2) = list_paginated(
            &pool,
            None,
            None,
            &[],
            &[],
            None,
            SortField::Amount,
            SortDir::Asc,
            cursor1,
            2,
            false,
        )
        .await
        .unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].amount, 100.0);
        assert!(cursor2.is_none());
    }

    #[tokio::test]
    async fn filters_by_tags_with_and_semantics() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        for id in ["both", "one"] {
            upsert_transaction(&pool, item.id.into(), None, &tx(id, d))
                .await
                .unwrap();
        }
        let both_id = sqlx::query_scalar::<_, DbUuid>(
            "SELECT id FROM transactions WHERE plaid_transaction_id = ?1",
        )
        .bind("both")
        .fetch_one(&pool)
        .await
        .unwrap();
        let one_id = sqlx::query_scalar::<_, DbUuid>(
            "SELECT id FROM transactions WHERE plaid_transaction_id = ?1",
        )
        .bind("one")
        .fetch_one(&pool)
        .await
        .unwrap();

        tag::tag_transaction(&pool, both_id.into(), "category", "FOOD_AND_DRINK")
            .await
            .unwrap();
        tag::tag_transaction(&pool, both_id.into(), "flag", "reviewed")
            .await
            .unwrap();
        tag::tag_transaction(&pool, one_id.into(), "category", "FOOD_AND_DRINK")
            .await
            .unwrap();

        let filters = vec![
            ("category".to_string(), "FOOD_AND_DRINK".to_string()),
            ("flag".to_string(), "reviewed".to_string()),
        ];
        let (matched, _) = list_paginated(
            &pool,
            None,
            None,
            &filters,
            &[],
            None,
            SortField::Date,
            SortDir::Desc,
            None,
            50,
            false,
        )
        .await
        .unwrap();
        let matched_ids: Vec<DbUuid> = matched.iter().map(|t| t.id).collect();

        assert!(matched_ids.contains(&both_id));
        assert!(!matched_ids.contains(&one_id));
    }

    #[tokio::test]
    async fn excludes_transactions_matching_any_excluded_tag() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        let mut ids = std::collections::HashMap::new();
        for id in ["keep", "drop_a", "drop_b"] {
            upsert_transaction(&pool, item.id.into(), None, &tx(id, d))
                .await
                .unwrap();
            let transaction_id = sqlx::query_scalar::<_, DbUuid>(
                "SELECT id FROM transactions WHERE plaid_transaction_id = ?1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
            ids.insert(id, transaction_id);
        }

        tag::tag_transaction(&pool, ids["drop_a"].into(), "category", "TRAVEL")
            .await
            .unwrap();
        tag::tag_transaction(&pool, ids["drop_b"].into(), "flag", "reviewed")
            .await
            .unwrap();

        let excluded = vec![
            ("category".to_string(), "TRAVEL".to_string()),
            ("flag".to_string(), "reviewed".to_string()),
        ];

        let (matched, _) = list_paginated(
            &pool,
            None,
            None,
            &[],
            &excluded,
            None,
            SortField::Date,
            SortDir::Desc,
            None,
            50,
            false,
        )
        .await
        .unwrap();
        let matched_ids: Vec<DbUuid> = matched.iter().map(|t| t.id).collect();

        assert!(matched_ids.contains(&ids["keep"]));
        assert!(!matched_ids.contains(&ids["drop_a"]));
        assert!(!matched_ids.contains(&ids["drop_b"]));
    }

    #[tokio::test]
    async fn a_transaction_inherits_its_merchants_and_accounts_tags() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        tag::tag_account(&pool, "acc_1", "kind", "business")
            .await
            .unwrap();
        let merchant = crate::repo::merchant::upsert_merchant(&pool, "Whole Foods", None)
            .await
            .unwrap();
        tag::tag_merchant(&pool, merchant.id.into(), "category", "FOOD_AND_DRINK")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let id = upsert_transaction(
            &pool,
            item.id.into(),
            Some(merchant.id.into()),
            &tx("t1", d),
        )
        .await
        .unwrap();

        let (matched, _) = list_paginated(
            &pool,
            None,
            None,
            &[],
            &[],
            None,
            SortField::Date,
            SortDir::Desc,
            None,
            50,
            false,
        )
        .await
        .unwrap();
        let found = matched.iter().find(|t| t.id == id.into()).unwrap();

        assert_eq!(
            found.tags,
            vec![
                TagValue {
                    name: "category".into(),
                    value: "FOOD_AND_DRINK".into()
                },
                TagValue {
                    name: "kind".into(),
                    value: "business".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_transactions_own_tag_overrides_its_merchants_tag_of_the_same_name() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let merchant = crate::repo::merchant::upsert_merchant(&pool, "Costco", None)
            .await
            .unwrap();
        tag::tag_merchant(&pool, merchant.id.into(), "category", "GENERAL_MERCHANDISE")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let id = upsert_transaction(
            &pool,
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

    #[tokio::test]
    async fn filters_by_a_tag_that_only_lives_on_the_merchant() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let merchant = crate::repo::merchant::upsert_merchant(&pool, "Netflix", None)
            .await
            .unwrap();
        tag::tag_merchant(&pool, merchant.id.into(), "type", "subscription")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let tagged = upsert_transaction(
            &pool,
            item.id.into(),
            Some(merchant.id.into()),
            &tx("t1", d),
        )
        .await
        .unwrap();
        upsert_transaction(&pool, item.id.into(), None, &tx("t2", d))
            .await
            .unwrap();

        let filters = vec![("type".to_string(), "subscription".to_string())];
        let (matched, _) = list_paginated(
            &pool,
            None,
            None,
            &filters,
            &[],
            None,
            SortField::Date,
            SortDir::Desc,
            None,
            50,
            false,
        )
        .await
        .unwrap();
        let matched_ids: Vec<DbUuid> = matched.iter().map(|t| t.id).collect();

        assert_eq!(matched_ids, vec![tagged.into()]);
    }

    #[tokio::test]
    async fn excludes_by_a_tag_that_only_lives_on_the_account() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        tag::tag_account(&pool, "acc_1", "kind", "business")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        upsert_transaction(&pool, item.id.into(), None, &tx("t1", d))
            .await
            .unwrap();

        let excluded = vec![("kind".to_string(), "business".to_string())];
        let (matched, _) = list_paginated(
            &pool,
            None,
            None,
            &[],
            &excluded,
            None,
            SortField::Date,
            SortDir::Desc,
            None,
            50,
            false,
        )
        .await
        .unwrap();

        assert!(matched.is_empty());
    }

    #[tokio::test]
    async fn searches_by_name_via_fts5() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        let mut coffee = tx("coffee_tx", d);
        coffee.name = Some("Blue Bottle Coffee".to_string());
        upsert_transaction(&pool, item.id.into(), None, &coffee)
            .await
            .unwrap();

        let mut grocery = tx("grocery_tx", d);
        grocery.name = Some("Whole Foods Market".to_string());
        upsert_transaction(&pool, item.id.into(), None, &grocery)
            .await
            .unwrap();

        let (matched, _) = list_paginated(
            &pool,
            None,
            None,
            &[],
            &[],
            Some("coffee"),
            SortField::Date,
            SortDir::Desc,
            None,
            50,
            false,
        )
        .await
        .unwrap();

        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].name.as_deref(), Some("Blue Bottle Coffee"));
    }

    fn parsed(
        date: NaiveDate,
        amount: f64,
        description: &str,
    ) -> crate::statement::ParsedTransaction {
        crate::statement::ParsedTransaction {
            date,
            amount,
            description: description.to_string(),
            merchant: description.to_string(),
        }
    }

    #[tokio::test]
    async fn imports_a_batch_and_reports_the_count() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();

        let summary = import_transactions(
            &pool,
            &account.id,
            Source::Neo,
            &[
                parsed(d, 26.35, "REAL CDN LIQUOR"),
                parsed(d, 37.04, "REAL CDN SUPERSTORE"),
            ],
        )
        .await
        .unwrap();

        assert_eq!(summary.inserted, 2);
        assert_eq!(summary.skipped_duplicates, 0);
    }

    #[tokio::test]
    async fn reimporting_the_same_file_skips_everything_as_duplicates() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();
        let rows = [
            parsed(d, 26.35, "REAL CDN LIQUOR"),
            parsed(d, 37.04, "REAL CDN SUPERSTORE"),
        ];

        import_transactions(&pool, &account.id, Source::Neo, &rows)
            .await
            .unwrap();
        let summary = import_transactions(&pool, &account.id, Source::Neo, &rows)
            .await
            .unwrap();

        assert_eq!(summary.inserted, 0);
        assert_eq!(summary.skipped_duplicates, 2);
    }

    #[tokio::test]
    async fn distinguishes_genuine_same_day_duplicates_by_occurrence() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Amex, "Amex")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 4).unwrap();

        // Two identical $4.50 coffees on the same day are two real
        // transactions, not one duplicated — both must be inserted.
        let first_visit = [parsed(d, 4.50, "COFFEE SHOP")];
        let summary1 = import_transactions(&pool, &account.id, Source::Amex, &first_visit)
            .await
            .unwrap();
        assert_eq!(summary1.inserted, 1);

        let both_visits = [
            parsed(d, 4.50, "COFFEE SHOP"),
            parsed(d, 4.50, "COFFEE SHOP"),
        ];
        let summary2 = import_transactions(&pool, &account.id, Source::Amex, &both_visits)
            .await
            .unwrap();
        // The first of the two collides with the one already imported
        // above (same occurrence 0); only the second is genuinely new.
        assert_eq!(summary2.inserted, 1);
        assert_eq!(summary2.skipped_duplicates, 1);

        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM transactions WHERE account_id = ?1 AND name = 'COFFEE SHOP'",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn a_later_statement_with_an_overlapping_date_only_imports_the_new_rows() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Neo, "Neo")
            .await
            .unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 8, 2).unwrap();

        import_transactions(
            &pool,
            &account.id,
            Source::Neo,
            &[parsed(d1, 10.0, "A"), parsed(d2, 20.0, "B")],
        )
        .await
        .unwrap();

        // A second statement covering an overlapping range plus one new day.
        let d3 = NaiveDate::from_ymd_opt(2026, 8, 3).unwrap();
        let summary = import_transactions(
            &pool,
            &account.id,
            Source::Neo,
            &[
                parsed(d1, 10.0, "A"),
                parsed(d2, 20.0, "B"),
                parsed(d3, 30.0, "C"),
            ],
        )
        .await
        .unwrap();

        assert_eq!(summary.inserted, 1);
        assert_eq!(summary.skipped_duplicates, 2);
    }

    #[tokio::test]
    async fn creates_a_manual_transaction() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();

        let created = create_manual_transaction(&pool, &account.id, d, 42.5, "Cash tip", None)
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

    #[tokio::test]
    async fn manual_transactions_get_distinct_occurrences_when_they_collide() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();

        let first = create_manual_transaction(&pool, &account.id, d, 4.50, "Coffee", None)
            .await
            .unwrap();
        let second = create_manual_transaction(&pool, &account.id, d, 4.50, "Coffee", None)
            .await
            .unwrap();

        assert_eq!(first.occurrence, 0);
        assert_eq!(second.occurrence, 1);
    }

    #[tokio::test]
    async fn get_returns_a_tag_enriched_transaction() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Amex, "Amex")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 7).unwrap();
        let created =
            create_manual_transaction(&pool, &account.id, d, 15.99, "Membership fee", None)
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

    #[tokio::test]
    async fn get_returns_none_for_a_nonexistent_id() {
        let pool = pool().await;
        assert!(get(&pool, Uuid::new_v4()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn create_manual_transaction_links_a_merchant() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Amex, "Amex")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 7).unwrap();

        let created = create_manual_transaction(&pool, &account.id, d, 15.99, "Starbucks", None)
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

    #[tokio::test]
    async fn create_manual_transaction_links_an_explicitly_chosen_merchant() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Amex, "Amex")
            .await
            .unwrap();
        let merchant = crate::repo::merchant::upsert_merchant(&pool, "Starbucks", None)
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

    #[tokio::test]
    async fn import_transactions_links_a_merchant_per_row() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();

        import_transactions(
            &pool,
            &account.id,
            Source::Neo,
            &[parsed(d, 26.35, "REAL CDN LIQUOR")],
        )
        .await
        .unwrap();

        let (merchant_name, merchant_id): (Option<String>, Option<DbUuid>) = sqlx::query_as(
            "SELECT merchant_name, merchant_id FROM transactions WHERE account_id = ?1",
        )
        .bind(&account.id)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(merchant_name.as_deref(), Some("REAL CDN LIQUOR"));
        assert!(merchant_id.is_some());
    }

    #[tokio::test]
    async fn reimporting_the_same_merchant_reuses_the_existing_row_not_a_duplicate() {
        let pool = pool().await;
        let account = account::create_manual_account(&pool, Source::Neo, "Neo")
            .await
            .unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();

        import_transactions(
            &pool,
            &account.id,
            Source::Neo,
            &[parsed(d1, 26.35, "REAL CDN LIQUOR")],
        )
        .await
        .unwrap();
        import_transactions(
            &pool,
            &account.id,
            Source::Neo,
            &[parsed(d2, 9.99, "REAL CDN LIQUOR")],
        )
        .await
        .unwrap();

        let merchant_count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM merchants WHERE name = 'REAL CDN LIQUOR'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(merchant_count, 1);
    }
}
