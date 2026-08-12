use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct LinkTokenCreateRequest {
    pub user: LinkTokenUser,
    pub client_name: String,
    pub products: Vec<String>,
    pub country_codes: Vec<String>,
    pub language: String,
}

#[derive(Debug, Serialize)]
pub struct LinkTokenUser {
    pub client_user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct LinkTokenCreateResponse {
    pub link_token: String,
    pub expiration: String,
    #[allow(dead_code)]
    pub request_id: String,
}

#[derive(Debug, Serialize)]
pub struct PublicTokenExchangeRequest {
    pub public_token: String,
}

#[derive(Debug, Deserialize)]
pub struct PublicTokenExchangeResponse {
    pub access_token: String,
    pub item_id: String,
    #[allow(dead_code)]
    pub request_id: String,
}

#[derive(Debug, Serialize)]
pub struct TransactionsSyncRequest {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct TransactionsSyncResponse {
    pub accounts: Vec<PlaidAccount>,
    pub added: Vec<PlaidTransaction>,
    pub modified: Vec<PlaidTransaction>,
    pub removed: Vec<RemovedTransaction>,
    pub next_cursor: String,
    pub has_more: bool,
    #[allow(dead_code)]
    pub request_id: String,
}

/// The account metadata Plaid includes alongside every `/transactions/sync`
/// page — not per-transaction, but a flat list of every account touched by
/// the item. `name` is Plaid's own friendly account name (e.g. "Plaid
/// Checking"), used to seed a new account's display name on first sight
/// instead of the raw `account_id`.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaidAccount {
    pub account_id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct RemovedTransaction {
    pub transaction_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlaidTransaction {
    pub transaction_id: String,
    pub account_id: String,
    pub amount: f64,
    pub iso_currency_code: Option<String>,
    pub unofficial_currency_code: Option<String>,
    pub date: NaiveDate,
    pub datetime: Option<DateTime<Utc>>,
    pub name: Option<String>,
    pub merchant_name: Option<String>,
    /// Stable Plaid-generated merchant identifier — unlike `merchant_name`,
    /// this doesn't vary with formatting/branding changes in the raw
    /// description, so it's the preferred key for deduping merchants.
    pub merchant_entity_id: Option<String>,
    pub pending: bool,
    pub payment_channel: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ItemRemoveRequest {
    pub access_token: String,
}

#[derive(Debug, Deserialize)]
pub struct ItemRemoveResponse {
    #[allow(dead_code)]
    pub request_id: String,
}

/// Sandbox-only: mints a `public_token` without a real Plaid Link session, so
/// the exchange -> sync flow can be exercised end to end via curl.
#[derive(Debug, Serialize)]
pub struct SandboxPublicTokenCreateRequest {
    pub institution_id: String,
    pub initial_products: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SandboxPublicTokenCreateResponse {
    pub public_token: String,
}
