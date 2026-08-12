use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::PlaidEnv;
use crate::error::{AppError, AppResult};
use crate::repo::{item, transaction};
use crate::state::AppState;
use crate::sync::{self, SyncSummary};

#[derive(Deserialize)]
pub struct CreateLinkTokenRequest {
    pub client_user_id: String,
}

#[derive(Serialize)]
pub struct CreateLinkTokenResponse {
    pub link_token: String,
    pub expiration: String,
}

pub async fn create_link_token(
    State(state): State<AppState>,
    Json(req): Json<CreateLinkTokenRequest>,
) -> AppResult<Json<CreateLinkTokenResponse>> {
    let resp = state.plaid.create_link_token(&req.client_user_id).await?;
    Ok(Json(CreateLinkTokenResponse {
        link_token: resp.link_token,
        expiration: resp.expiration,
    }))
}

#[derive(Deserialize)]
pub struct SandboxPublicTokenRequest {
    /// Defaults to `ins_109508`, Plaid's "First Platypus Bank" sandbox institution.
    #[serde(default = "default_sandbox_institution")]
    pub institution_id: String,
}

fn default_sandbox_institution() -> String {
    "ins_109508".to_string()
}

#[derive(Serialize)]
pub struct SandboxPublicTokenResponse {
    pub public_token: String,
}

/// Sandbox-only helper so the exchange -> sync flow can be tested with curl,
/// without standing up Plaid Link in a browser.
pub async fn sandbox_public_token(
    State(state): State<AppState>,
    Json(req): Json<SandboxPublicTokenRequest>,
) -> AppResult<Json<SandboxPublicTokenResponse>> {
    if state.config.plaid.env != PlaidEnv::Sandbox {
        return Err(AppError::BadRequest(
            "sandbox endpoints are only available when plaid.env = sandbox".into(),
        ));
    }
    let resp = state
        .plaid
        .sandbox_create_public_token(&req.institution_id)
        .await?;
    Ok(Json(SandboxPublicTokenResponse {
        public_token: resp.public_token,
    }))
}

#[derive(Deserialize)]
pub struct CreateItemRequest {
    pub public_token: String,
}

#[derive(Serialize)]
pub struct CreateItemResponse {
    pub item: item::Item,
    pub sync: SyncSummary,
}

/// Exchanges a Link `public_token` for an access token, persists the Item,
/// then runs an initial transactions sync so the item has data immediately.
pub async fn create_item(
    State(state): State<AppState>,
    Json(req): Json<CreateItemRequest>,
) -> AppResult<Json<CreateItemResponse>> {
    let exchange = state.plaid.exchange_public_token(&req.public_token).await?;

    let saved = item::upsert_item(
        &state.pool,
        &exchange.item_id,
        &exchange.access_token,
        None,
    )
    .await?;

    let summary = sync::sync_item_transactions(&state.pool, &state.plaid, &saved).await?;

    Ok(Json(CreateItemResponse {
        item: saved,
        sync: summary,
    }))
}

pub async fn list_items(State(state): State<AppState>) -> AppResult<Json<Vec<item::Item>>> {
    Ok(Json(item::list_items(&state.pool).await?))
}

/// Disconnects an item: revokes it on Plaid's side first, then deletes it
/// locally, which cascades to its accounts, transactions, and their tags
/// (see `0001_init.sql`) — merchants and any transaction rules referencing a
/// *different* account are untouched, but rules scoped to one of this item's
/// accounts are cascade-deleted along with the account itself.
pub async fn delete_item(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let saved = item::get_item(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    state.plaid.remove_item(&saved.access_token).await?;
    item::delete_item(&state.pool, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn sync_item(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<SyncSummary>> {
    let saved = item::get_item(&state.pool, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let summary = sync::sync_item_transactions(&state.pool, &state.plaid, &saved).await?;
    Ok(Json(summary))
}

#[derive(Serialize)]
pub struct ItemSyncResult {
    pub item_id: Uuid,
    pub plaid_item_id: String,
    pub summary: Option<SyncSummary>,
    pub error: Option<String>,
}

/// Syncs every stored item. One item failing (e.g. a revoked access token)
/// doesn't stop the others — each item's outcome is reported individually.
pub async fn sync_all_items(State(state): State<AppState>) -> AppResult<Json<Vec<ItemSyncResult>>> {
    let items = item::list_items(&state.pool).await?;
    let mut results = Vec::with_capacity(items.len());

    for item in &items {
        let (summary, error) =
            match sync::sync_item_transactions(&state.pool, &state.plaid, item).await {
                Ok(summary) => (Some(summary), None),
                Err(err) => (None, Some(err.to_string())),
            };
        results.push(ItemSyncResult {
            item_id: item.id.into(),
            plaid_item_id: item.plaid_item_id.clone(),
            summary,
            error,
        });
    }

    Ok(Json(results))
}

pub async fn list_item_transactions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<transaction::Transaction>>> {
    item::get_item(&state.pool, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(
        transaction::list_transactions_for_item(&state.pool, id).await?,
    ))
}
