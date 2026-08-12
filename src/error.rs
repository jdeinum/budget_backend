use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("http error calling Plaid: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Plaid API error: {status} {body}")]
    Plaid {
        status: reqwest::StatusCode,
        body: serde_json::Value,
    },

    #[error("not found")]
    NotFound,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("config error: {0}")]
    Config(#[from] config::ConfigError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Plaid { .. } => (StatusCode::BAD_GATEWAY, self.to_string()),
            AppError::Database(_) | AppError::Http(_) | AppError::Config(_) => {
                tracing::error!(error = %self, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
