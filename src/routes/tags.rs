use axum::Json;
use axum::extract::State;
use serde::Deserialize;

use crate::error::AppResult;
use crate::repo::tag::{self, Tag};
use crate::state::AppState;

pub async fn list_tags(State(state): State<AppState>) -> AppResult<Json<Vec<Tag>>> {
    Ok(Json(tag::list_all(&state.pool).await?))
}

#[derive(Deserialize)]
pub struct CreateTagRequest {
    pub name: String,
    pub value: String,
}

/// Creates a tag `(name, value)` up front, e.g. to populate a picker in the
/// frontend before it's ever attached to anything. Attaching a tag to a
/// transaction/merchant also creates it if it doesn't exist yet, so calling
/// this first is optional.
pub async fn create_tag(
    State(state): State<AppState>,
    Json(req): Json<CreateTagRequest>,
) -> AppResult<Json<Tag>> {
    Ok(Json(
        tag::get_or_create_tag(&state.pool, &req.name, &req.value).await?,
    ))
}

/// Shared request body for attaching a tag to a transaction or merchant.
#[derive(Deserialize)]
pub struct AttachTagRequest {
    pub name: String,
    pub value: String,
}
