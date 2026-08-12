use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use uuid::Uuid;

use crate::utils::db_uuid::DbUuid;

/// A tag is a single, reusable `(name, value)` pair — e.g. `("category",
/// "FOOD_AND_DRINK")` — shared by every subject tagged with it, rather than
/// each attachment carrying its own copy of the value.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct Tag {
    pub id: DbUuid,
    pub name: String,
    pub value: String,
}

/// A tag's `(name, value)` as returned alongside a tagged subject; distinct
/// from [`Tag`] only in that it omits the tag's own `id`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize)]
pub struct TagValue {
    pub name: String,
    pub value: String,
}

pub async fn list_all(pool: &SqlitePool) -> sqlx::Result<Vec<Tag>> {
    sqlx::query_as::<_, Tag>("SELECT * FROM tags ORDER BY name, value")
        .fetch_all(pool)
        .await
}

pub async fn get_or_create_tag(pool: &SqlitePool, name: &str, value: &str) -> sqlx::Result<Tag> {
    sqlx::query_as::<_, Tag>(
        r#"
        INSERT INTO tags (id, name, value)
        VALUES (?1, ?2, ?3)
        ON CONFLICT (name, value) DO UPDATE SET name = excluded.name
        RETURNING *
        "#,
    )
    .bind(DbUuid::from(Uuid::new_v4()))
    .bind(name)
    .bind(value)
    .fetch_one(pool)
    .await
}

/// Sets `name` -> `value` on a transaction, treating `name` as single-valued:
/// any existing tag under the same `name` (with a different value) is
/// replaced rather than left attached alongside the new one.
pub async fn tag_transaction(
    pool: &SqlitePool,
    transaction_id: Uuid,
    name: &str,
    value: &str,
) -> sqlx::Result<()> {
    let tag = get_or_create_tag(pool, name, value).await?;

    sqlx::query(
        r#"
        DELETE FROM transaction_tags
        WHERE transaction_id = ?1
          AND tag_id IN (SELECT id FROM tags WHERE name = ?2 AND id <> ?3)
        "#,
    )
    .bind(DbUuid::from(transaction_id))
    .bind(name)
    .bind(tag.id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO transaction_tags (transaction_id, tag_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
    )
    .bind(DbUuid::from(transaction_id))
    .bind(tag.id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Removes whatever tag is set under `name` on a transaction, if any.
pub async fn untag_transaction(
    pool: &SqlitePool,
    transaction_id: Uuid,
    name: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        r#"
        DELETE FROM transaction_tags
        WHERE transaction_id = ?1
          AND tag_id IN (SELECT id FROM tags WHERE name = ?2)
        "#,
    )
    .bind(DbUuid::from(transaction_id))
    .bind(name)
    .execute(pool)
    .await?;
    Ok(())
}

/// Sets `name` -> `value` on a merchant; same single-valued-per-name
/// replacement semantics as [`tag_transaction`].
pub async fn tag_merchant(
    pool: &SqlitePool,
    merchant_id: Uuid,
    name: &str,
    value: &str,
) -> sqlx::Result<()> {
    let tag = get_or_create_tag(pool, name, value).await?;

    sqlx::query(
        r#"
        DELETE FROM merchant_tags
        WHERE merchant_id = ?1
          AND tag_id IN (SELECT id FROM tags WHERE name = ?2 AND id <> ?3)
        "#,
    )
    .bind(DbUuid::from(merchant_id))
    .bind(name)
    .bind(tag.id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO merchant_tags (merchant_id, tag_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
    )
    .bind(DbUuid::from(merchant_id))
    .bind(tag.id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Removes whatever tag is set under `name` on a merchant, if any.
pub async fn untag_merchant(pool: &SqlitePool, merchant_id: Uuid, name: &str) -> sqlx::Result<()> {
    sqlx::query(
        r#"
        DELETE FROM merchant_tags
        WHERE merchant_id = ?1
          AND tag_id IN (SELECT id FROM tags WHERE name = ?2)
        "#,
    )
    .bind(DbUuid::from(merchant_id))
    .bind(name)
    .execute(pool)
    .await?;
    Ok(())
}

/// Sets `name` -> `value` on an account; same single-valued-per-name
/// replacement semantics as [`tag_transaction`].
pub async fn tag_account(
    pool: &SqlitePool,
    account_id: &str,
    name: &str,
    value: &str,
) -> sqlx::Result<()> {
    let tag = get_or_create_tag(pool, name, value).await?;

    sqlx::query(
        r#"
        DELETE FROM account_tags
        WHERE account_id = ?1
          AND tag_id IN (SELECT id FROM tags WHERE name = ?2 AND id <> ?3)
        "#,
    )
    .bind(account_id)
    .bind(name)
    .bind(tag.id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO account_tags (account_id, tag_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
    )
    .bind(account_id)
    .bind(tag.id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Removes whatever tag is set under `name` on an account, if any.
pub async fn untag_account(pool: &SqlitePool, account_id: &str, name: &str) -> sqlx::Result<()> {
    sqlx::query(
        r#"
        DELETE FROM account_tags
        WHERE account_id = ?1
          AND tag_id IN (SELECT id FROM tags WHERE name = ?2)
        "#,
    )
    .bind(account_id)
    .bind(name)
    .execute(pool)
    .await?;
    Ok(())
}

/// A single transaction's tags — for the tag-attach endpoint's response.
pub async fn list_for_transaction(
    pool: &SqlitePool,
    transaction_id: Uuid,
) -> sqlx::Result<Vec<TagValue>> {
    sqlx::query_as::<_, TagValue>(
        r#"
        SELECT tag.name, tag.value
        FROM transaction_tags tt
        JOIN tags tag ON tag.id = tt.tag_id
        WHERE tt.transaction_id = ?1
        ORDER BY tag.name
        "#,
    )
    .bind(DbUuid::from(transaction_id))
    .fetch_all(pool)
    .await
}

/// Tags for every transaction in `transaction_ids`, as `(transaction_id, name, value)`
/// rows — for batch-enriching a page of transactions in one query instead of N+1.
#[derive(Debug, sqlx::FromRow)]
pub struct TransactionTagRow {
    pub transaction_id: DbUuid,
    pub name: String,
    pub value: String,
}

pub async fn list_for_transactions(
    pool: &SqlitePool,
    transaction_ids: &[DbUuid],
) -> sqlx::Result<Vec<TransactionTagRow>> {
    if transaction_ids.is_empty() {
        return Ok(vec![]);
    }
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT tt.transaction_id, tag.name, tag.value \
         FROM transaction_tags tt JOIN tags tag ON tag.id = tt.tag_id \
         WHERE tt.transaction_id IN (",
    );
    let mut separated = qb.separated(", ");
    for id in transaction_ids {
        separated.push_bind(*id);
    }
    qb.push(")");
    qb.build_query_as().fetch_all(pool).await
}

/// A single merchant's tags — for the tag-attach endpoint's response.
pub async fn list_for_merchant(
    pool: &SqlitePool,
    merchant_id: Uuid,
) -> sqlx::Result<Vec<TagValue>> {
    sqlx::query_as::<_, TagValue>(
        r#"
        SELECT tag.name, tag.value
        FROM merchant_tags mt
        JOIN tags tag ON tag.id = mt.tag_id
        WHERE mt.merchant_id = ?1
        ORDER BY tag.name
        "#,
    )
    .bind(DbUuid::from(merchant_id))
    .fetch_all(pool)
    .await
}

/// Tags for every merchant in `merchant_ids`, as `(merchant_id, name, value)`
/// rows — for batch-enriching a page of merchants in one query instead of N+1.
#[derive(Debug, sqlx::FromRow)]
pub struct MerchantTagRow {
    pub merchant_id: DbUuid,
    pub name: String,
    pub value: String,
}

pub async fn list_for_merchants(
    pool: &SqlitePool,
    merchant_ids: &[DbUuid],
) -> sqlx::Result<Vec<MerchantTagRow>> {
    if merchant_ids.is_empty() {
        return Ok(vec![]);
    }
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT mt.merchant_id, tag.name, tag.value \
         FROM merchant_tags mt JOIN tags tag ON tag.id = mt.tag_id \
         WHERE mt.merchant_id IN (",
    );
    let mut separated = qb.separated(", ");
    for id in merchant_ids {
        separated.push_bind(*id);
    }
    qb.push(")");
    qb.build_query_as().fetch_all(pool).await
}

/// Merges tag layers in ascending precedence order — a later layer's tag
/// overrides an earlier layer's tag of the same `name`. Backs the account <
/// vendor < transaction tag hierarchy: pass account tags, then merchant
/// tags, then the transaction's own tags, and the result is what that
/// transaction should be treated as carrying everywhere (filtering, search,
/// category charts, ...). Returned sorted by name, matching every other
/// tag-listing function here.
pub fn merge_layers(layers: &[&[TagValue]]) -> Vec<TagValue> {
    let mut merged: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for layer in layers {
        for tag in *layer {
            merged.insert(&tag.name, &tag.value);
        }
    }
    merged
        .into_iter()
        .map(|(name, value)| TagValue {
            name: name.to_string(),
            value: value.to_string(),
        })
        .collect()
}

/// A single account's tags — for the tag-attach endpoint's response.
pub async fn list_for_account(pool: &SqlitePool, account_id: &str) -> sqlx::Result<Vec<TagValue>> {
    sqlx::query_as::<_, TagValue>(
        r#"
        SELECT tag.name, tag.value
        FROM account_tags act
        JOIN tags tag ON tag.id = act.tag_id
        WHERE act.account_id = ?1
        ORDER BY tag.name
        "#,
    )
    .bind(account_id)
    .fetch_all(pool)
    .await
}

/// Tags for every account in `account_ids`, as `(account_id, name, value)`
/// rows — for batch-enriching the accounts list in one query instead of N+1.
#[derive(Debug, sqlx::FromRow)]
pub struct AccountTagRow {
    pub account_id: String,
    pub name: String,
    pub value: String,
}

pub async fn list_for_accounts(
    pool: &SqlitePool,
    account_ids: &[String],
) -> sqlx::Result<Vec<AccountTagRow>> {
    if account_ids.is_empty() {
        return Ok(vec![]);
    }
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT act.account_id, tag.name, tag.value \
         FROM account_tags act JOIN tags tag ON tag.id = act.tag_id \
         WHERE act.account_id IN (",
    );
    let mut separated = qb.separated(", ");
    for id in account_ids {
        separated.push_bind(id.clone());
    }
    qb.push(")");
    qb.build_query_as().fetch_all(pool).await
}

#[cfg(test)]
mod tests {
    use quickcheck::{Arbitrary, Gen};

    use super::*;

    impl Arbitrary for TagValue {
        fn arbitrary(g: &mut Gen) -> Self {
            TagValue {
                name: String::arbitrary(g),
                value: String::arbitrary(g),
            }
        }
    }

    fn tv(name: &str, value: &str) -> TagValue {
        TagValue {
            name: name.into(),
            value: value.into(),
        }
    }

    #[test]
    fn higher_layers_override_same_named_tags_from_lower_ones() {
        let account = vec![tv("category", "GENERAL"), tv("kind", "business")];
        let merchant = vec![tv("category", "FOOD_AND_DRINK")];
        let transaction = vec![tv("category", "TRAVEL"), tv("flag", "reviewed")];

        let merged = merge_layers(&[&account, &merchant, &transaction]);

        assert_eq!(
            merged,
            vec![
                tv("category", "TRAVEL"),
                tv("flag", "reviewed"),
                tv("kind", "business")
            ]
        );
    }

    #[test]
    fn a_layer_with_no_tag_under_a_name_leaves_the_lower_layer_in_effect() {
        let account = vec![tv("category", "GENERAL")];
        let merchant: Vec<TagValue> = vec![];
        let transaction: Vec<TagValue> = vec![];

        let merged = merge_layers(&[&account, &merchant, &transaction]);

        assert_eq!(merged, vec![tv("category", "GENERAL")]);
    }

    /// The doc comment promises the result is "sorted by name" with one
    /// entry per name — for arbitrary layers, not just the handful of
    /// examples above.
    fn prop_merged_tags_are_sorted_by_name_with_no_duplicates(layers: Vec<Vec<TagValue>>) -> bool {
        let layer_refs: Vec<&[TagValue]> = layers.iter().map(Vec::as_slice).collect();
        let merged = merge_layers(&layer_refs);
        let names: Vec<&str> = merged.iter().map(|t| t.name.as_str()).collect();

        let mut sorted = names.clone();
        sorted.sort_unstable();
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();

        names == sorted && unique.len() == names.len()
    }

    #[test]
    fn merged_tags_are_sorted_by_name_with_no_duplicates() {
        crate::utils::proptest_support::run(
            "merged_tags_are_sorted_by_name_with_no_duplicates",
            prop_merged_tags_are_sorted_by_name_with_no_duplicates
                as fn(Vec<Vec<TagValue>>) -> bool,
        );
    }

    /// Every merged tag's value is whichever occurrence of that name came
    /// last across the flattened (layer, tag) sequence — "later layers
    /// win", and within a layer, a later tag wins over an earlier one
    /// under the same name (matches the repeated-`insert` implementation).
    fn prop_a_merged_tags_value_is_the_last_occurrence_of_its_name(
        layers: Vec<Vec<TagValue>>,
    ) -> bool {
        let layer_refs: Vec<&[TagValue]> = layers.iter().map(Vec::as_slice).collect();
        let flattened: Vec<&TagValue> = layers.iter().flatten().collect();

        merge_layers(&layer_refs).iter().all(|tag| {
            let expected = flattened
                .iter()
                .rev()
                .find(|t| t.name == tag.name)
                .map(|t| t.value.as_str());
            expected == Some(tag.value.as_str())
        })
    }

    #[test]
    fn a_merged_tags_value_is_the_last_occurrence_of_its_name() {
        crate::utils::proptest_support::run(
            "a_merged_tags_value_is_the_last_occurrence_of_its_name",
            prop_a_merged_tags_value_is_the_last_occurrence_of_its_name
                as fn(Vec<Vec<TagValue>>) -> bool,
        );
    }
}
