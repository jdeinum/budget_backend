use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use std::collections::HashMap;
use uuid::Uuid;

use crate::db_uuid::DbUuid;
use crate::repo::tag::{self, TagValue};
use crate::search::fts5_query;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct Merchant {
    pub id: DbUuid,
    pub name: String,
    pub entity_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[sqlx(skip)]
    pub tags: Vec<TagValue>,
}

/// Upserts a merchant for the given display `name`, keyed on `entity_id`
/// when Plaid supplies one (the stable identity) and falling back to `name`
/// otherwise. `name` is always refreshed to the latest observed value.
pub async fn upsert_merchant(
    pool: &SqlitePool,
    name: &str,
    entity_id: Option<&str>,
) -> sqlx::Result<Merchant> {
    match entity_id {
        Some(entity_id) => upsert_by_entity_id(pool, name, entity_id).await,
        None => upsert_by_name(pool, name).await,
    }
}

/// Reuses the row already keyed on this entity_id, else claims a pre-existing
/// name-only row for it (learning its entity_id for the first time), else
/// inserts fresh — tried in order inside one transaction.
///
/// This can't be a single `ON CONFLICT` upsert (SQLite or Postgres): a
/// partial-index conflict target only fires when the row *being inserted*
/// itself satisfies that index's predicate. Since the row here always has a
/// non-null `entity_id`, it can never match `idx_merchants_name_no_entity`'s
/// `WHERE entity_id IS NULL`, so a chained-upsert attempt at the "backfill"
/// step silently never triggers and inserts a duplicate row instead.
async fn upsert_by_entity_id(pool: &SqlitePool, name: &str, entity_id: &str) -> sqlx::Result<Merchant> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;

    if let Some(m) = sqlx::query_as::<_, Merchant>(
        "UPDATE merchants SET name = ?1, updated_at = ?2 WHERE entity_id = ?3 RETURNING *",
    )
    .bind(name)
    .bind(now)
    .bind(entity_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok(m);
    }

    if let Some(m) = sqlx::query_as::<_, Merchant>(
        r#"
        UPDATE merchants SET entity_id = ?1, name = ?2, updated_at = ?3
        WHERE name = ?2 AND entity_id IS NULL
        RETURNING *
        "#,
    )
    .bind(entity_id)
    .bind(name)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok(m);
    }

    let m = sqlx::query_as::<_, Merchant>(
        "INSERT INTO merchants (id, name, entity_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4) RETURNING *",
    )
    .bind(DbUuid::from(Uuid::new_v4()))
    .bind(name)
    .bind(entity_id)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(m)
}

async fn upsert_by_name(pool: &SqlitePool, name: &str) -> sqlx::Result<Merchant> {
    let now = Utc::now();
    sqlx::query_as::<_, Merchant>(
        r#"
        INSERT INTO merchants (id, name, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?3)
        ON CONFLICT (name) WHERE entity_id IS NULL DO UPDATE SET updated_at = excluded.updated_at
        RETURNING *
        "#,
    )
    .bind(DbUuid::from(Uuid::new_v4()))
    .bind(name)
    .bind(now)
    .fetch_one(pool)
    .await
}

/// Keyset-pagination cursor: the `(name, id)` of the last row on the
/// previous page, ordered by `name ASC, id ASC`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerchantCursor {
    pub name: String,
    pub id: Uuid,
}

pub async fn list_paginated(
    pool: &SqlitePool,
    cursor: Option<MerchantCursor>,
    q: Option<&str>,
    limit: i64,
) -> sqlx::Result<(Vec<Merchant>, Option<MerchantCursor>)> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT * FROM merchants WHERE 1 = 1");

    if let Some(c) = &cursor {
        qb.push(" AND (name, id) > (")
            .push_bind(c.name.clone())
            .push(", ")
            .push_bind(DbUuid::from(c.id))
            .push(")");
    }
    if let Some(q) = q {
        qb.push(" AND rowid IN (SELECT rowid FROM merchants_fts WHERE merchants_fts MATCH ")
            .push_bind(fts5_query(q))
            .push(")");
    }
    qb.push(" ORDER BY name ASC, id ASC LIMIT ").push_bind(limit + 1);

    let mut merchants: Vec<Merchant> = qb.build_query_as().fetch_all(pool).await?;

    let next_cursor = if merchants.len() as i64 > limit {
        merchants.truncate(limit as usize);
        merchants
            .last()
            .map(|m| MerchantCursor { name: m.name.clone(), id: m.id.into() })
    } else {
        None
    };

    let ids: Vec<DbUuid> = merchants.iter().map(|m| m.id).collect();
    let tag_rows = tag::list_for_merchants(pool, &ids).await?;
    let mut tags_by_merchant: HashMap<DbUuid, Vec<TagValue>> = HashMap::new();
    for row in tag_rows {
        tags_by_merchant
            .entry(row.merchant_id)
            .or_default()
            .push(TagValue { name: row.name, value: row.value });
    }
    for m in &mut merchants {
        m.tags = tags_by_merchant.remove(&m.id).unwrap_or_default();
    }

    Ok((merchants, next_cursor))
}

pub async fn get_merchant(pool: &SqlitePool, id: Uuid) -> sqlx::Result<Option<Merchant>> {
    sqlx::query_as::<_, Merchant>("SELECT * FROM merchants WHERE id = ?1")
        .bind(DbUuid::from(id))
        .fetch_optional(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
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
    async fn dedupes_by_entity_id_and_refreshes_name() {
        let pool = pool().await;
        let entity_id = format!("ent_{}", Uuid::new_v4());

        let first = upsert_merchant(&pool, "UBER *TRIP", Some(&entity_id))
            .await
            .unwrap();
        let second = upsert_merchant(&pool, "Uber", Some(&entity_id))
            .await
            .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(second.name, "Uber");
        assert_eq!(second.entity_id.as_deref(), Some(entity_id.as_str()));
    }

    #[tokio::test]
    async fn backfills_entity_id_onto_existing_name_only_merchant() {
        let pool = pool().await;
        let name = format!("Test Cafe {}", Uuid::new_v4());
        let entity_id = format!("ent_{}", Uuid::new_v4());

        let name_only = upsert_merchant(&pool, &name, None).await.unwrap();
        assert_eq!(name_only.entity_id, None);

        let claimed = upsert_merchant(&pool, &name, Some(&entity_id))
            .await
            .unwrap();
        assert_eq!(claimed.id, name_only.id);
        assert_eq!(claimed.entity_id.as_deref(), Some(entity_id.as_str()));
    }

    #[tokio::test]
    async fn distinct_entity_ids_stay_distinct_even_with_same_name() {
        let pool = pool().await;
        let shared_name = format!("Chain Store {}", Uuid::new_v4());
        let entity_a = format!("ent_a_{}", Uuid::new_v4());
        let entity_b = format!("ent_b_{}", Uuid::new_v4());

        let a = upsert_merchant(&pool, &shared_name, Some(&entity_a))
            .await
            .unwrap();
        let b = upsert_merchant(&pool, &shared_name, Some(&entity_b))
            .await
            .unwrap();

        assert_ne!(a.id, b.id);
    }
}
