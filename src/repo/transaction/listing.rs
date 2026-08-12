use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use std::collections::HashMap;
use uuid::Uuid;

use crate::repo::tag::{self, TagValue};
use crate::utils::db_uuid::DbUuid;
use crate::utils::search::fts5_query;

use super::TransactionWithMerchant;

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
    use crate::repo::transaction::upsert_transaction;
    use crate::repo::{account, item, tag};
    use chrono::Utc;

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
    async fn paginates_by_date_desc_with_keyset_cursor(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, Utc::now(), "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();

        let d1 = NaiveDate::from_ymd_opt(2026, 1, 3).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
        let d3 = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        for (id, date) in [("t1", d1), ("t2", d2), ("t3", d3)] {
            upsert_transaction(&pool, Utc::now(), item.id.into(), None, &tx(id, date))
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

    #[sqlx::test]
    async fn paginates_by_amount_asc_with_keyset_cursor(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, Utc::now(), "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        for (id, amount) in [("cheap", 5.0), ("mid", 20.0), ("pricey", 100.0)] {
            let mut t = tx(id, d);
            t.amount = amount;
            upsert_transaction(&pool, Utc::now(), item.id.into(), None, &t)
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

    #[sqlx::test]
    async fn filters_by_tags_with_and_semantics(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, Utc::now(), "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        for id in ["both", "one"] {
            upsert_transaction(&pool, Utc::now(), item.id.into(), None, &tx(id, d))
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

    #[sqlx::test]
    async fn excludes_transactions_matching_any_excluded_tag(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, Utc::now(), "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        let mut ids = std::collections::HashMap::new();
        for id in ["keep", "drop_a", "drop_b"] {
            upsert_transaction(&pool, Utc::now(), item.id.into(), None, &tx(id, d))
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

    #[sqlx::test]
    async fn a_transaction_inherits_its_merchants_and_accounts_tags(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, Utc::now(), "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        tag::tag_account(&pool, "acc_1", "kind", "business")
            .await
            .unwrap();
        let merchant =
            crate::repo::merchant::upsert_merchant(&pool, Utc::now(), "Whole Foods", None)
                .await
                .unwrap();
        tag::tag_merchant(&pool, merchant.id.into(), "category", "FOOD_AND_DRINK")
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

    #[sqlx::test]
    async fn filters_by_a_tag_that_only_lives_on_the_merchant(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, Utc::now(), "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let merchant = crate::repo::merchant::upsert_merchant(&pool, Utc::now(), "Netflix", None)
            .await
            .unwrap();
        tag::tag_merchant(&pool, merchant.id.into(), "type", "subscription")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let tagged = upsert_transaction(
            &pool,
            Utc::now(),
            item.id.into(),
            Some(merchant.id.into()),
            &tx("t1", d),
        )
        .await
        .unwrap();
        upsert_transaction(&pool, Utc::now(), item.id.into(), None, &tx("t2", d))
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

    #[sqlx::test]
    async fn excludes_by_a_tag_that_only_lives_on_the_account(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, Utc::now(), "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        tag::tag_account(&pool, "acc_1", "kind", "business")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        upsert_transaction(&pool, Utc::now(), item.id.into(), None, &tx("t1", d))
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

    #[sqlx::test]
    async fn searches_by_name_via_fts5(pool: SqlitePool) {
        let item = item::upsert_item(&pool, Utc::now(), "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, Utc::now(), "acc_1", item.id.into(), "acc_1")
            .await
            .unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        let mut coffee = tx("coffee_tx", d);
        coffee.name = Some("Blue Bottle Coffee".to_string());
        upsert_transaction(&pool, Utc::now(), item.id.into(), None, &coffee)
            .await
            .unwrap();

        let mut grocery = tx("grocery_tx", d);
        grocery.name = Some("Whole Foods Market".to_string());
        upsert_transaction(&pool, Utc::now(), item.id.into(), None, &grocery)
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
}
