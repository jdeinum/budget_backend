use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::repo::transaction_rule::{self, RuleKind, TransactionRule};
use crate::state::AppState;

pub async fn list_transaction_rules(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<TransactionRule>>> {
    Ok(Json(transaction_rule::list_rules(&state.pool).await?))
}

#[derive(Deserialize)]
pub struct CreateRuleRequest {
    pub kind: RuleKind,
    pub pattern: Option<String>,
    pub tag_name: Option<String>,
    pub tag_value: Option<String>,
    pub account_id: Option<String>,
    pub source_account_id: Option<String>,
    pub target_account_id: Option<String>,
}

fn non_empty(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|s| !s.trim().is_empty())
}

pub async fn create_transaction_rule(
    State(state): State<AppState>,
    Json(req): Json<CreateRuleRequest>,
) -> AppResult<Json<TransactionRule>> {
    match req.kind {
        RuleKind::MerchantContains => {
            if !non_empty(&req.pattern) {
                return Err(AppError::BadRequest(
                    "merchant_contains rules require a non-empty pattern".into(),
                ));
            }
        }
        RuleKind::Tag => {
            if !non_empty(&req.tag_name) || !non_empty(&req.tag_value) {
                return Err(AppError::BadRequest(
                    "tag rules require both tag_name and tag_value".into(),
                ));
            }
        }
        RuleKind::Account => {
            if !non_empty(&req.account_id) {
                return Err(AppError::BadRequest(
                    "account rules require a non-empty account_id".into(),
                ));
            }
        }
        RuleKind::Transfer => {
            if !non_empty(&req.source_account_id) || !non_empty(&req.target_account_id) {
                return Err(AppError::BadRequest(
                    "transfer rules require both source_account_id and target_account_id".into(),
                ));
            }
            if req.source_account_id == req.target_account_id {
                return Err(AppError::BadRequest(
                    "transfer rules require source_account_id and target_account_id to differ"
                        .into(),
                ));
            }
        }
    }

    let rule = transaction_rule::create_rule(
        &state.pool,
        req.kind,
        req.pattern.as_deref(),
        req.tag_name.as_deref(),
        req.tag_value.as_deref(),
        req.account_id.as_deref(),
        req.source_account_id.as_deref(),
        req.target_account_id.as_deref(),
    )
    .await?;

    Ok(Json(rule))
}

pub async fn delete_transaction_rule(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    if !transaction_rule::delete_rule(&state.pool, id).await? {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
