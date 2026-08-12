use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::repo::merchant::{self, Merchant, MerchantCursor};
use crate::repo::merchant_category::{self, MerchantCategory};
use crate::repo::tag::{self, TagValue};
use crate::repo::transaction_rule;
use crate::routes::tags::AttachTagRequest;
use crate::state::AppState;
use crate::utils::cursor;

const fn default_limit() -> i64 {
    50
}

const MAX_LIMIT: i64 = 200;

#[derive(Deserialize)]
pub struct ListMerchantsQuery {
    pub cursor: Option<String>,
    /// Free-text search against a merchant's `name`, via FTS5.
    pub q: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

#[derive(Serialize)]
pub struct MerchantsPage {
    pub items: Vec<Merchant>,
    pub next_cursor: Option<String>,
}

pub async fn list_merchants(
    State(state): State<AppState>,
    Query(query): Query<ListMerchantsQuery>,
) -> AppResult<Json<MerchantsPage>> {
    let cursor = query
        .cursor
        .as_deref()
        .map(cursor::decode::<MerchantCursor>)
        .transpose()?;
    let limit = query.limit.clamp(1, MAX_LIMIT);
    let q = query.q.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let (items, next_cursor) = merchant::list_paginated(&state.pool, cursor, q, limit).await?;

    Ok(Json(MerchantsPage {
        items,
        next_cursor: next_cursor.map(|c| cursor::encode(&c)),
    }))
}

/// Full category history for a merchant, most recent first.
pub async fn merchant_categories(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<MerchantCategory>>> {
    merchant::get_merchant(&state.pool, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(
        merchant_category::list_for_merchant(&state.pool, id).await?,
    ))
}

/// Attaches `name` -> `value` to a merchant, creating the tag if it's new.
/// Returns the merchant's full tag set after the change.
pub async fn tag_merchant(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<AttachTagRequest>,
) -> AppResult<Json<Vec<TagValue>>> {
    merchant::get_merchant(&state.pool, id)
        .await?
        .ok_or(AppError::NotFound)?;
    tag::tag_merchant(&state.pool, id, &req.name, &req.value).await?;
    transaction_rule::reevaluate_ignored(&state.pool).await?;
    Ok(Json(tag::list_for_merchant(&state.pool, id).await?))
}

/// Removes whatever tag is set under `name` on a merchant, if any.
/// Returns the merchant's full tag set after the change.
pub async fn untag_merchant(
    State(state): State<AppState>,
    Path((id, name)): Path<(Uuid, String)>,
) -> AppResult<Json<Vec<TagValue>>> {
    merchant::get_merchant(&state.pool, id)
        .await?
        .ok_or(AppError::NotFound)?;
    tag::untag_merchant(&state.pool, id, &name).await?;
    transaction_rule::reevaluate_ignored(&state.pool).await?;
    Ok(Json(tag::list_for_merchant(&state.pool, id).await?))
}
