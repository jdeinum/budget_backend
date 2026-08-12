use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::config::PlaidField;
use crate::error::AppError;

use super::models::*;

/// Thin wrapper around the Plaid REST API. `client_id`/`secret` are injected
/// into every request body, matching Plaid's body-auth convention (as opposed
/// to the `PLAID-CLIENT-ID`/`PLAID-SECRET` header alternative).
#[derive(Clone)]
pub struct PlaidClient {
    http: reqwest::Client,
    base_url: &'static str,
    client_id: String,
    secret: String,
}

impl PlaidClient {
    pub fn new(config: &PlaidField) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: config.env.base_url(),
            client_id: config.client_id.clone(),
            secret: config.secret.clone(),
        }
    }

    async fn post<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        payload: T,
    ) -> Result<R, AppError> {
        let mut body = serde_json::to_value(payload).expect("request payload is serializable");
        if let Value::Object(map) = &mut body {
            map.insert("client_id".into(), Value::String(self.client_id.clone()));
            map.insert("secret".into(), Value::String(self.secret.clone()));
        }

        let resp = self
            .http
            .post(format!("{}{}", self.base_url, path))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.json::<Value>().await.unwrap_or(Value::Null);
            return Err(AppError::Plaid { status, body });
        }

        Ok(resp.json::<R>().await?)
    }

    pub async fn create_link_token(
        &self,
        client_user_id: &str,
    ) -> Result<LinkTokenCreateResponse, AppError> {
        self.post(
            "/link/token/create",
            LinkTokenCreateRequest {
                user: LinkTokenUser {
                    client_user_id: client_user_id.to_string(),
                },
                client_name: "Budget".to_string(),
                products: vec!["transactions".to_string()],
                country_codes: vec!["US".to_string(), "CA".to_string()],
                language: "en".to_string(),
            },
        )
        .await
    }

    pub async fn exchange_public_token(
        &self,
        public_token: &str,
    ) -> Result<PublicTokenExchangeResponse, AppError> {
        self.post(
            "/item/public_token/exchange",
            PublicTokenExchangeRequest {
                public_token: public_token.to_string(),
            },
        )
        .await
    }

    pub async fn sync_transactions(
        &self,
        access_token: &str,
        cursor: Option<&str>,
    ) -> Result<TransactionsSyncResponse, AppError> {
        self.post(
            "/transactions/sync",
            TransactionsSyncRequest {
                access_token: access_token.to_string(),
                cursor: cursor.map(str::to_string),
                count: Some(500),
            },
        )
        .await
    }

    /// Revokes the item's access on Plaid's side — must be called before
    /// deleting the item locally, so a disconnect actually unlinks the bank
    /// rather than just forgetting the access token while Plaid keeps the
    /// item (and any per-item billing) active. Returns `Ok` if Plaid reports
    /// the item is already gone (`ITEM_NOT_FOUND`), so a disconnect still
    /// succeeds locally when the user revoked access from their bank's side
    /// first.
    pub async fn remove_item(&self, access_token: &str) -> Result<(), AppError> {
        let result = self
            .post::<_, ItemRemoveResponse>(
                "/item/remove",
                ItemRemoveRequest { access_token: access_token.to_string() },
            )
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(AppError::Plaid { body, .. })
                if body.get("error_code").and_then(Value::as_str) == Some("ITEM_NOT_FOUND") =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub async fn sandbox_create_public_token(
        &self,
        institution_id: &str,
    ) -> Result<SandboxPublicTokenCreateResponse, AppError> {
        self.post(
            "/sandbox/public_token/create",
            SandboxPublicTokenCreateRequest {
                institution_id: institution_id.to_string(),
                initial_products: vec!["transactions".to_string()],
            },
        )
        .await
    }
}
