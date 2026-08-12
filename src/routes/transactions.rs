use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cursor;
use crate::error::{AppError, AppResult};
use crate::repo::account;
use crate::repo::merchant;
use crate::repo::tag::{self, TagValue};
use crate::repo::transaction::{
    self, SortDir, SortField, TransactionCursor, TransactionWithMerchant,
};
use crate::repo::transaction_rule;
use crate::routes::tags::AttachTagRequest;
use crate::state::AppState;

fn default_limit() -> i64 {
    50
}

const MAX_LIMIT: i64 = 200;

#[derive(Deserialize)]
pub struct CreateTransactionRequest {
    pub account_id: String,
    pub date: NaiveDate,
    pub amount: f64,
    pub name: String,
    /// Links the transaction to an existing merchant instead of the usual
    /// by-name fallback — optional, so it picks up that merchant's tags
    /// through the account < vendor < transaction hierarchy.
    pub merchant_id: Option<Uuid>,
}

/// Adds a single hand-entered transaction to an existing account — the
/// "Add transaction" button's endpoint, as opposed to a batch statement
/// import (`accounts::upload_statements`). Works for any account
/// regardless of its own `source`: a Plaid-synced or statement-imported
/// account can still have one-off cash transactions Plaid or a statement
/// never saw.
pub async fn create_transaction(
    State(state): State<AppState>,
    Json(req): Json<CreateTransactionRequest>,
) -> AppResult<Json<TransactionWithMerchant>> {
    account::get_account(&state.pool, &req.account_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if let Some(merchant_id) = req.merchant_id {
        merchant::get_merchant(&state.pool, merchant_id)
            .await?
            .ok_or(AppError::NotFound)?;
    }

    let created = transaction::create_manual_transaction(
        &state.pool,
        &req.account_id,
        req.date,
        req.amount,
        &req.name,
        req.merchant_id,
    )
    .await?;

    // A tag/account rule can already match this transaction (e.g. an
    // "account" kind rule ignoring everything in this account).
    transaction_rule::reevaluate_ignored(&state.pool).await?;

    let with_merchant = transaction::get(&state.pool, created.id.into())
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(with_merchant))
}

fn default_sort() -> String {
    "date:desc".to_string()
}

#[derive(Deserialize)]
pub struct ListTransactionsQuery {
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    /// Comma-separated `name:value` pairs, e.g.
    /// `tags=category:FOOD_AND_DRINK,category_detail:COFFEE`. A transaction
    /// must carry every listed pair (AND semantics) to match.
    pub tags: Option<String>,
    /// Same `name:value` format as `tags`, but a transaction carrying *any*
    /// one of these is dropped (OR semantics on exclusion).
    pub exclude_tags: Option<String>,
    /// Free-text search against a transaction's `name`/`merchant_name`, via FTS5.
    pub q: Option<String>,
    /// `field:dir`, e.g. `amount:asc`. Fields: `date`, `amount`. Dirs: `asc`, `desc`.
    #[serde(default = "default_sort")]
    pub sort: String,
    /// Opaque cursor from a previous response's `next_cursor`. Must have been
    /// minted under the same `sort` as this request — a cursor from a
    /// `date:desc` page can't be reused after switching to `amount:asc`.
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// When true, returns only ignored transactions (see
    /// `transaction_rules`) instead of the normal default of excluding them.
    #[serde(default)]
    pub ignored_only: bool,
}

fn parse_tags(raw: &str) -> AppResult<Vec<(String, String)>> {
    raw.split(',')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            pair.split_once(':')
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "invalid tag filter {pair:?}, expected name:value"
                    ))
                })
        })
        .collect()
}

fn parse_sort(raw: &str) -> AppResult<(SortField, SortDir)> {
    let (field, dir) = raw
        .split_once(':')
        .ok_or_else(|| AppError::BadRequest(format!("invalid sort {raw:?}, expected field:dir")))?;

    let field = match field {
        "date" => SortField::Date,
        "amount" => SortField::Amount,
        other => {
            return Err(AppError::BadRequest(format!(
                "invalid sort field {other:?}, expected date or amount"
            )));
        }
    };
    let dir = match dir {
        "asc" => SortDir::Asc,
        "desc" => SortDir::Desc,
        other => {
            return Err(AppError::BadRequest(format!(
                "invalid sort direction {other:?}, expected asc or desc"
            )));
        }
    };
    Ok((field, dir))
}

#[derive(Serialize)]
pub struct TransactionsPage {
    pub items: Vec<TransactionWithMerchant>,
    pub next_cursor: Option<String>,
}

pub async fn list_transactions(
    State(state): State<AppState>,
    Query(query): Query<ListTransactionsQuery>,
) -> AppResult<Json<TransactionsPage>> {
    let tags = query
        .tags
        .as_deref()
        .map(parse_tags)
        .transpose()?
        .unwrap_or_default();
    let exclude_tags = query
        .exclude_tags
        .as_deref()
        .map(parse_tags)
        .transpose()?
        .unwrap_or_default();
    let (sort_field, sort_dir) = parse_sort(&query.sort)?;
    let cursor = query
        .cursor
        .as_deref()
        .map(cursor::decode::<TransactionCursor>)
        .transpose()?;
    if let Some(c) = &cursor
        && (c.sort_field != sort_field || c.sort_dir != sort_dir)
    {
        return Err(AppError::BadRequest(
            "cursor was minted under a different sort; drop it when changing sort".into(),
        ));
    }
    let limit = query.limit.clamp(1, MAX_LIMIT);
    let q = query.q.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let (items, next_cursor) = transaction::list_paginated(
        &state.pool,
        query.start_date,
        query.end_date,
        &tags,
        &exclude_tags,
        q,
        sort_field,
        sort_dir,
        cursor,
        limit,
        query.ignored_only,
    )
    .await?;

    Ok(Json(TransactionsPage {
        items,
        next_cursor: next_cursor.map(|c| cursor::encode(&c)),
    }))
}

/// Attaches `name` -> `value` to a transaction, creating the tag if it's new.
/// Returns the transaction's full tag set after the change.
pub async fn tag_transaction(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<AttachTagRequest>,
) -> AppResult<Json<Vec<TagValue>>> {
    if !transaction::exists(&state.pool, id).await? {
        return Err(AppError::NotFound);
    }
    tag::tag_transaction(&state.pool, id, &req.name, &req.value).await?;
    // A `tag`-kind ignore rule matches against a transaction's own tags, so
    // adding one can change whether this transaction should be ignored.
    transaction_rule::reevaluate_ignored(&state.pool).await?;
    Ok(Json(tag::list_for_transaction(&state.pool, id).await?))
}

/// Removes whatever tag is set under `name` on a transaction, if any.
/// Returns the transaction's full tag set after the change.
pub async fn untag_transaction(
    State(state): State<AppState>,
    Path((id, name)): Path<(Uuid, String)>,
) -> AppResult<Json<Vec<TagValue>>> {
    if !transaction::exists(&state.pool, id).await? {
        return Err(AppError::NotFound);
    }
    tag::untag_transaction(&state.pool, id, &name).await?;
    transaction_rule::reevaluate_ignored(&state.pool).await?;
    Ok(Json(tag::list_for_transaction(&state.pool, id).await?))
}
