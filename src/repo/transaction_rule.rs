use chrono::{DateTime, Utc};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Sqlite, SqlitePool};
use uuid::Uuid;

use crate::db_uuid::DbUuid;

/// A transaction-ignore rule's condition. One rule is one condition; several
/// rules combine with OR (see [`reevaluate_ignored`]). Stored as `TEXT`
/// (mirrors [`DbUuid`](crate::db_uuid::DbUuid)'s manual `Type`/`Encode`/
/// `Decode` pattern) matching the CHECK constraint values in
/// `0002_transaction_rules.sql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    MerchantContains,
    Tag,
    Account,
    Transfer,
}

impl RuleKind {
    fn as_str(self) -> &'static str {
        match self {
            RuleKind::MerchantContains => "merchant_contains",
            RuleKind::Tag => "tag",
            RuleKind::Account => "account",
            RuleKind::Transfer => "transfer",
        }
    }
}

impl sqlx::Type<Sqlite> for RuleKind {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as sqlx::Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as sqlx::Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> sqlx::Encode<'q, Sqlite> for RuleKind {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <String as sqlx::Encode<'q, Sqlite>>::encode(self.as_str().to_string(), buf)
    }
}

impl<'r> sqlx::Decode<'r, Sqlite> for RuleKind {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <String as sqlx::Decode<'r, Sqlite>>::decode(value)?;
        match s.as_str() {
            "merchant_contains" => Ok(RuleKind::MerchantContains),
            "tag" => Ok(RuleKind::Tag),
            "account" => Ok(RuleKind::Account),
            "transfer" => Ok(RuleKind::Transfer),
            other => Err(format!("invalid rule kind: {other:?}").into()),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct TransactionRule {
    pub id: DbUuid,
    pub kind: RuleKind,
    pub pattern: Option<String>,
    pub tag_name: Option<String>,
    pub tag_value: Option<String>,
    pub account_id: Option<String>,
    pub source_account_id: Option<String>,
    pub target_account_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
pub async fn create_rule(
    pool: &SqlitePool,
    kind: RuleKind,
    pattern: Option<&str>,
    tag_name: Option<&str>,
    tag_value: Option<&str>,
    account_id: Option<&str>,
    source_account_id: Option<&str>,
    target_account_id: Option<&str>,
) -> sqlx::Result<TransactionRule> {
    let rule = sqlx::query_as::<_, TransactionRule>(
        r#"
        INSERT INTO transaction_rules (
            id, kind, pattern, tag_name, tag_value, account_id,
            source_account_id, target_account_id, created_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        RETURNING *
        "#,
    )
    .bind(DbUuid::from(Uuid::new_v4()))
    .bind(kind)
    .bind(pattern)
    .bind(tag_name)
    .bind(tag_value)
    .bind(account_id)
    .bind(source_account_id)
    .bind(target_account_id)
    .bind(Utc::now())
    .fetch_one(pool)
    .await?;

    reevaluate_ignored(pool).await?;

    Ok(rule)
}

pub async fn list_rules(pool: &SqlitePool) -> sqlx::Result<Vec<TransactionRule>> {
    sqlx::query_as::<_, TransactionRule>("SELECT * FROM transaction_rules ORDER BY created_at ASC")
        .fetch_all(pool)
        .await
}

pub async fn delete_rule(pool: &SqlitePool, id: Uuid) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM transaction_rules WHERE id = ?1")
        .bind(DbUuid::from(id))
        .execute(pool)
        .await?;

    reevaluate_ignored(pool).await?;

    Ok(result.rows_affected() > 0)
}

/// Recomputes `transactions.ignored` for every transaction against the
/// current rule set (OR semantics — a transaction matching any rule is
/// ignored). A full reset-then-reapply rather than per-transaction
/// attribution bookkeeping, run inside one transaction for atomicity. Called
/// after every rule create/delete (making rule changes retroactive), once
/// per sync (making it apply to newly-synced transactions too), and after
/// any transaction/merchant/account tag change (since a `tag` rule can match
/// through any of the three — see below).
pub async fn reevaluate_ignored(pool: &SqlitePool) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("UPDATE transactions SET ignored = 0")
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        r#"
        UPDATE transactions SET ignored = 1 WHERE id IN (
            SELECT t.id FROM transactions t JOIN transaction_rules r ON (
                (r.kind = 'merchant_contains' AND (
                    (t.name IS NOT NULL AND lower(t.name) LIKE '%' || lower(r.pattern) || '%') OR
                    (t.merchant_name IS NOT NULL AND lower(t.merchant_name) LIKE '%' || lower(r.pattern) || '%')
                ))
                OR (r.kind = 'account' AND t.account_id = r.account_id)
                OR (r.kind = 'tag' AND (
                    EXISTS (
                        SELECT 1 FROM transaction_tags tt JOIN tags tag ON tag.id = tt.tag_id
                        WHERE tt.transaction_id = t.id AND tag.name = r.tag_name AND tag.value = r.tag_value
                    )
                    OR (t.merchant_id IS NOT NULL AND EXISTS (
                        SELECT 1 FROM merchant_tags mt JOIN tags tag ON tag.id = mt.tag_id
                        WHERE mt.merchant_id = t.merchant_id AND tag.name = r.tag_name AND tag.value = r.tag_value
                    ))
                    OR EXISTS (
                        SELECT 1 FROM account_tags act JOIN tags tag ON tag.id = act.tag_id
                        WHERE act.account_id = t.account_id AND tag.name = r.tag_name AND tag.value = r.tag_value
                    )
                ))
                OR (r.kind = 'transfer' AND t.account_id IN (r.source_account_id, r.target_account_id) AND (
                    (r.pattern IS NULL AND r.tag_name IS NULL)
                    OR (r.pattern IS NOT NULL AND (
                        (t.name IS NOT NULL AND lower(t.name) LIKE '%' || lower(r.pattern) || '%') OR
                        (t.merchant_name IS NOT NULL AND lower(t.merchant_name) LIKE '%' || lower(r.pattern) || '%')
                    ))
                    OR (r.tag_name IS NOT NULL AND (
                        EXISTS (
                            SELECT 1 FROM transaction_tags tt JOIN tags tag ON tag.id = tt.tag_id
                            WHERE tt.transaction_id = t.id AND tag.name = r.tag_name AND tag.value = r.tag_value
                        )
                        OR (t.merchant_id IS NOT NULL AND EXISTS (
                            SELECT 1 FROM merchant_tags mt JOIN tags tag ON tag.id = mt.tag_id
                            WHERE mt.merchant_id = t.merchant_id AND tag.name = r.tag_name AND tag.value = r.tag_value
                        ))
                        OR EXISTS (
                            SELECT 1 FROM account_tags act JOIN tags tag ON tag.id = act.tag_id
                            WHERE act.account_id = t.account_id AND tag.name = r.tag_name AND tag.value = r.tag_value
                        )
                    ))
                ))
            )
        )
        "#,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plaid::models::PlaidTransaction;
    use crate::repo::{account, item, tag, transaction};
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

    fn tx(id: &str, name: &str) -> PlaidTransaction {
        PlaidTransaction {
            transaction_id: id.to_string(),
            account_id: "acc_1".to_string(),
            amount: 12.34,
            iso_currency_code: Some("USD".to_string()),
            unofficial_currency_code: None,
            date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            datetime: None,
            name: Some(name.to_string()),
            merchant_name: None,
            merchant_entity_id: None,
            pending: false,
            payment_channel: Some("online".to_string()),
            personal_finance_category: None,
        }
    }

    async fn is_ignored(pool: &SqlitePool, plaid_transaction_id: &str) -> bool {
        sqlx::query_scalar::<_, bool>(
            "SELECT ignored FROM transactions WHERE plaid_transaction_id = ?1",
        )
        .bind(plaid_transaction_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn merchant_contains_rule_ignores_matching_transactions_retroactively() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1").await.unwrap();
        transaction::upsert_transaction(&pool, item.id.into(), None, &tx("venmo_tx", "Venmo Payment"))
            .await
            .unwrap();
        transaction::upsert_transaction(&pool, item.id.into(), None, &tx("other_tx", "Whole Foods"))
            .await
            .unwrap();

        create_rule(&pool, RuleKind::MerchantContains, Some("venmo"), None, None, None, None, None)
            .await
            .unwrap();

        assert!(is_ignored(&pool, "venmo_tx").await);
        assert!(!is_ignored(&pool, "other_tx").await);
    }

    #[tokio::test]
    async fn account_rule_ignores_all_transactions_on_that_account() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1").await.unwrap();
        transaction::upsert_transaction(&pool, item.id.into(), None, &tx("t1", "Whole Foods"))
            .await
            .unwrap();

        create_rule(&pool, RuleKind::Account, None, None, None, Some("acc_1"), None, None)
            .await
            .unwrap();

        assert!(is_ignored(&pool, "t1").await);
    }

    #[tokio::test]
    async fn tag_rule_ignores_transactions_carrying_that_tag() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1").await.unwrap();
        let id = transaction::upsert_transaction(&pool, item.id.into(), None, &tx("t1", "Transfer"))
            .await
            .unwrap();
        tag::tag_transaction(&pool, id, "category", "TRANSFER").await.unwrap();
        transaction::upsert_transaction(&pool, item.id.into(), None, &tx("t2", "Whole Foods"))
            .await
            .unwrap();

        create_rule(&pool, RuleKind::Tag, None, Some("category"), Some("TRANSFER"), None, None, None)
            .await
            .unwrap();

        assert!(is_ignored(&pool, "t1").await);
        assert!(!is_ignored(&pool, "t2").await);
    }

    #[tokio::test]
    async fn tag_rule_matches_through_the_transactions_merchant() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1").await.unwrap();
        let merchant = crate::repo::merchant::upsert_merchant(&pool, "Netflix", None)
            .await
            .unwrap();
        tag::tag_merchant(&pool, merchant.id.into(), "type", "subscription")
            .await
            .unwrap();

        transaction::upsert_transaction(&pool, item.id.into(), Some(merchant.id.into()), &tx("t1", "Netflix"))
            .await
            .unwrap();
        transaction::upsert_transaction(&pool, item.id.into(), None, &tx("t2", "Whole Foods"))
            .await
            .unwrap();

        create_rule(&pool, RuleKind::Tag, None, Some("type"), Some("subscription"), None, None, None)
            .await
            .unwrap();

        assert!(is_ignored(&pool, "t1").await);
        assert!(!is_ignored(&pool, "t2").await);
    }

    #[tokio::test]
    async fn tag_rule_matches_through_the_transactions_account() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1").await.unwrap();
        tag::tag_account(&pool, "acc_1", "kind", "business").await.unwrap();

        transaction::upsert_transaction(&pool, item.id.into(), None, &tx("t1", "Anything"))
            .await
            .unwrap();

        create_rule(&pool, RuleKind::Tag, None, Some("kind"), Some("business"), None, None, None)
            .await
            .unwrap();

        assert!(is_ignored(&pool, "t1").await);
    }

    #[tokio::test]
    async fn transfer_rule_ignores_transactions_on_either_account_with_no_refinement() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "checking", item.id.into(), "checking").await.unwrap();
        account::upsert_account(&pool, "savings", item.id.into(), "savings").await.unwrap();
        account::upsert_account(&pool, "credit_card", item.id.into(), "credit_card").await.unwrap();

        let mut t1 = tx("t1", "Transfer out");
        t1.account_id = "checking".to_string();
        transaction::upsert_transaction(&pool, item.id.into(), None, &t1).await.unwrap();

        let mut t2 = tx("t2", "Transfer in");
        t2.account_id = "savings".to_string();
        transaction::upsert_transaction(&pool, item.id.into(), None, &t2).await.unwrap();

        let mut t3 = tx("t3", "Groceries");
        t3.account_id = "credit_card".to_string();
        transaction::upsert_transaction(&pool, item.id.into(), None, &t3).await.unwrap();

        create_rule(&pool, RuleKind::Transfer, None, None, None, None, Some("checking"), Some("savings"))
            .await
            .unwrap();

        assert!(is_ignored(&pool, "t1").await);
        assert!(is_ignored(&pool, "t2").await);
        assert!(!is_ignored(&pool, "t3").await);
    }

    #[tokio::test]
    async fn transfer_rule_with_tag_refinement_only_matches_tagged_transactions() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "checking", item.id.into(), "checking").await.unwrap();
        account::upsert_account(&pool, "savings", item.id.into(), "savings").await.unwrap();

        let mut tagged = tx("tagged_tx", "Transfer out");
        tagged.account_id = "checking".to_string();
        let tagged_id = transaction::upsert_transaction(&pool, item.id.into(), None, &tagged)
            .await
            .unwrap();
        tag::tag_transaction(&pool, tagged_id, "category", "TRANSFER").await.unwrap();

        let mut untagged = tx("untagged_tx", "Groceries");
        untagged.account_id = "savings".to_string();
        transaction::upsert_transaction(&pool, item.id.into(), None, &untagged)
            .await
            .unwrap();

        create_rule(
            &pool,
            RuleKind::Transfer,
            None,
            Some("category"),
            Some("TRANSFER"),
            None,
            Some("checking"),
            Some("savings"),
        )
        .await
        .unwrap();

        assert!(is_ignored(&pool, "tagged_tx").await);
        assert!(!is_ignored(&pool, "untagged_tx").await);
    }

    #[tokio::test]
    async fn deleting_a_rule_unignores_its_former_matches() {
        let pool = pool().await;
        let item = item::upsert_item(&pool, "plaid_item_1", "access-token", None)
            .await
            .unwrap();
        account::upsert_account(&pool, "acc_1", item.id.into(), "acc_1").await.unwrap();
        transaction::upsert_transaction(&pool, item.id.into(), None, &tx("venmo_tx", "Venmo Payment"))
            .await
            .unwrap();

        let rule = create_rule(&pool, RuleKind::MerchantContains, Some("venmo"), None, None, None, None, None)
            .await
            .unwrap();
        assert!(is_ignored(&pool, "venmo_tx").await);

        let existed = delete_rule(&pool, rule.id.into()).await.unwrap();
        assert!(existed);
        assert!(!is_ignored(&pool, "venmo_tx").await);
    }
}
