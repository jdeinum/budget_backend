use axum::Json;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::repo::account::{self, Account};
use crate::repo::tag::{self, TagValue};
use crate::repo::transaction;
use crate::repo::transaction_rule;
use crate::routes::tags::AttachTagRequest;
use crate::source::Source;
use crate::state::AppState;
use crate::statement::{self, StatementImporter};

pub async fn list_accounts(State(state): State<AppState>) -> AppResult<Json<Vec<Account>>> {
    Ok(Json(account::list_accounts(&state.pool).await?))
}

#[derive(Deserialize)]
pub struct CreateAccountRequest {
    pub source: Source,
    pub name: String,
}

/// Creates an account with no backing Plaid item. `source` declares which
/// issuer's statements it'll receive — must be `Source::Neo` or
/// `Source::Amex` (the only issuers supported today; `Source::Plaid`
/// accounts are auto-created by connecting a bank, and `Source::Manual`
/// isn't a real institution — see `Source`'s docs).
pub async fn create_account(
    State(state): State<AppState>,
    Json(req): Json<CreateAccountRequest>,
) -> AppResult<Json<Account>> {
    match req.source {
        Source::Neo | Source::Amex => {}
        Source::Plaid | Source::Manual => {
            return Err(AppError::BadRequest(format!(
                "accounts can be created as neo or amex, not {}",
                req.source.label().to_lowercase()
            )));
        }
    }
    let account = account::create_manual_account(&state.pool, req.source, &req.name).await?;
    Ok(Json(account))
}

/// Deletes a manually-created account and its transactions. A Plaid
/// account can't be removed this way — see `account::delete_account` — and
/// this returns 404 for one the same as for a nonexistent id, since from
/// the caller's perspective it's not a deletable account either way.
pub async fn delete_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    if !account::delete_account(&state.pool, &id).await? {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The importer for an account's declared institution. Plaid and Manual
/// aren't real statement formats — the former syncs, the latter is always
/// one hand-entered row at a time (see `routes::transactions::create_transaction`) —
/// so statement uploads are rejected for both.
fn importer_for(source: Source) -> AppResult<Box<dyn StatementImporter>> {
    match source {
        Source::Neo => Ok(Box::new(statement::NeoImporter)),
        Source::Amex => Ok(Box::new(statement::AmexImporter)),
        Source::Plaid => Err(AppError::BadRequest(
            "this account syncs via Plaid — statement uploads are for Neo/Amex accounts".into(),
        )),
        Source::Manual => Err(AppError::BadRequest(
            "this account has no declared institution to match a statement format against".into(),
        )),
    }
}

#[derive(Serialize, Default)]
pub struct FailedFile {
    pub filename: String,
    pub error: String,
}

#[derive(Serialize, Default)]
pub struct UploadStatementsResponse {
    pub inserted: i64,
    pub skipped_duplicates: i64,
    pub failed: Vec<FailedFile>,
}

/// Validates `bytes`' header matches `importer` (giving a clear "wrong
/// format" error rather than a confusing per-row parse failure) before
/// parsing and importing it into `account_id`.
async fn import_one_file(
    state: &AppState,
    account_id: &str,
    source: Source,
    importer: &dyn StatementImporter,
    bytes: &[u8],
) -> Result<transaction::ImportSummary, String> {
    let mut reader = csv::ReaderBuilder::new().from_reader(bytes);
    let header = reader.headers().map_err(|err| err.to_string())?.clone();
    if !importer.detect(&header) {
        return Err(format!("doesn't look like a {} export", source.label()));
    }

    let rows = importer.parse(bytes).map_err(|err| err.to_string())?;
    transaction::import_transactions(&state.pool, account_id, source, &rows)
        .await
        .map_err(|err| err.to_string())
}

/// Imports transactions from one or more uploaded statement exports (CSV
/// today — see `crate::statement`) into an existing account, using the
/// importer for the account's own declared institution (see
/// `importer_for`) rather than guessing per file — so uploading, say, an
/// Amex file to a Neo account is rejected instead of silently tagging
/// those rows `source=amex` on a "Neo" account.
///
/// The upload is one or more multipart fields all named `file`. Each is
/// validated and imported independently — one bad file doesn't stop the
/// others — with per-file failures reported back rather than swallowed.
/// Rows already present (by the natural-key dedup in
/// `transaction::import_transactions`) are silently skipped rather than
/// re-imported, so the same file can be uploaded more than once safely,
/// and an overlapping statement only adds what's actually new.
pub async fn upload_statements(
    State(state): State<AppState>,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> AppResult<Json<UploadStatementsResponse>> {
    let account = account::get_account(&state.pool, &id).await?.ok_or(AppError::NotFound)?;
    let importer = importer_for(account.source)?;

    let mut response = UploadStatementsResponse::default();
    let mut file_count = 0;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| AppError::BadRequest(err.to_string()))?
    {
        if field.name() != Some("file") {
            continue;
        }
        file_count += 1;
        let filename = field.file_name().unwrap_or("file").to_string();

        let bytes = match field.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                response.failed.push(FailedFile { filename, error: err.to_string() });
                continue;
            }
        };

        match import_one_file(&state, &id, account.source, importer.as_ref(), &bytes).await {
            Ok(summary) => {
                response.inserted += summary.inserted;
                response.skipped_duplicates += summary.skipped_duplicates;
            }
            Err(error) => response.failed.push(FailedFile { filename, error }),
        }
    }

    if file_count == 0 {
        return Err(AppError::BadRequest("missing \"file\" field".into()));
    }

    // A tag/account rule can match transactions this import just added —
    // see `transaction_rule::reevaluate_ignored`'s other call sites.
    transaction_rule::reevaluate_ignored(&state.pool).await?;

    Ok(Json(response))
}

#[derive(Deserialize)]
pub struct RenameAccountRequest {
    pub name: String,
}

pub async fn rename_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RenameAccountRequest>,
) -> AppResult<Json<Account>> {
    let account = account::rename_account(&state.pool, &id, &req.name)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(account))
}

/// Attaches `name` -> `value` to an account, creating the tag if it's new.
/// Returns the account's full tag set after the change.
pub async fn tag_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AttachTagRequest>,
) -> AppResult<Json<Vec<TagValue>>> {
    account::get_account(&state.pool, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    tag::tag_account(&state.pool, &id, &req.name, &req.value).await?;
    transaction_rule::reevaluate_ignored(&state.pool).await?;
    Ok(Json(tag::list_for_account(&state.pool, &id).await?))
}

/// Removes whatever tag is set under `name` on an account, if any.
/// Returns the account's full tag set after the change.
pub async fn untag_account(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> AppResult<Json<Vec<TagValue>>> {
    account::get_account(&state.pool, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    tag::untag_account(&state.pool, &id, &name).await?;
    transaction_rule::reevaluate_ignored(&state.pool).await?;
    Ok(Json(tag::list_for_account(&state.pool, &id).await?))
}
