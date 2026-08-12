use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::utils::db_uuid::DbUuid;
use crate::utils::source::Source;

/// Outcome of importing a batch of parsed statement rows into one account.
#[derive(Debug, Serialize)]
pub struct ImportSummary {
    pub inserted: i64,
    pub skipped_duplicates: i64,
}

/// Inserts a batch of manually-imported transactions (parsed from a CSV/PDF
/// statement — see `crate::statement`) into `account_id`, skipping rows
/// that duplicate ones already stored. `source` must not be `Source::Plaid`
/// — Plaid rows go through `super::upsert_transaction` and dedupe on
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
    now: DateTime<Utc>,
    account_id: &str,
    source: Source,
    rows: &[crate::statement::ParsedTransaction],
) -> sqlx::Result<ImportSummary> {
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
        let merchant =
            crate::repo::merchant::upsert_merchant(pool, now, &row.merchant, None).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::account;

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

    #[sqlx::test]
    async fn imports_a_batch_and_reports_the_count(pool: SqlitePool) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();

        let summary = import_transactions(
            &pool,
            Utc::now(),
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

    #[sqlx::test]
    async fn reimporting_the_same_file_skips_everything_as_duplicates(pool: SqlitePool) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();
        let rows = [
            parsed(d, 26.35, "REAL CDN LIQUOR"),
            parsed(d, 37.04, "REAL CDN SUPERSTORE"),
        ];

        import_transactions(&pool, Utc::now(), &account.id, Source::Neo, &rows)
            .await
            .unwrap();
        let summary = import_transactions(&pool, Utc::now(), &account.id, Source::Neo, &rows)
            .await
            .unwrap();

        assert_eq!(summary.inserted, 0);
        assert_eq!(summary.skipped_duplicates, 2);
    }

    #[sqlx::test]
    async fn distinguishes_genuine_same_day_duplicates_by_occurrence(pool: SqlitePool) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Amex, "Amex")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 4).unwrap();

        // Two identical $4.50 coffees on the same day are two real
        // transactions, not one duplicated — both must be inserted.
        let first_visit = [parsed(d, 4.50, "COFFEE SHOP")];
        let summary1 =
            import_transactions(&pool, Utc::now(), &account.id, Source::Amex, &first_visit)
                .await
                .unwrap();
        assert_eq!(summary1.inserted, 1);

        let both_visits = [
            parsed(d, 4.50, "COFFEE SHOP"),
            parsed(d, 4.50, "COFFEE SHOP"),
        ];
        let summary2 =
            import_transactions(&pool, Utc::now(), &account.id, Source::Amex, &both_visits)
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

    #[sqlx::test]
    async fn a_later_statement_with_an_overlapping_date_only_imports_the_new_rows(
        pool: SqlitePool,
    ) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Neo, "Neo")
            .await
            .unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 8, 2).unwrap();

        import_transactions(
            &pool,
            Utc::now(),
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
            Utc::now(),
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

    #[sqlx::test]
    async fn import_transactions_links_a_merchant_per_row(pool: SqlitePool) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Neo, "Neo")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();

        import_transactions(
            &pool,
            Utc::now(),
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

    #[sqlx::test]
    async fn reimporting_the_same_merchant_reuses_the_existing_row_not_a_duplicate(
        pool: SqlitePool,
    ) {
        let account = account::create_manual_account(&pool, Utc::now(), Source::Neo, "Neo")
            .await
            .unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();

        import_transactions(
            &pool,
            Utc::now(),
            &account.id,
            Source::Neo,
            &[parsed(d1, 26.35, "REAL CDN LIQUOR")],
        )
        .await
        .unwrap();
        import_transactions(
            &pool,
            Utc::now(),
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
