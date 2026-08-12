use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, Request, ResponseTemplate};

use crate::utils::spawn_app;

fn sync_page(cursor: &str, has_more: bool, added: &[serde_json::Value]) -> serde_json::Value {
    json!({
        "accounts": [{ "account_id": "acc_1", "name": "Plaid Checking" }],
        "added": added,
        "modified": [],
        "removed": [],
        "next_cursor": cursor,
        "has_more": has_more,
        "request_id": "req_1",
    })
}

fn plaid_tx(id: &str, name: &str, amount: f64) -> serde_json::Value {
    json!({
        "transaction_id": id,
        "account_id": "acc_1",
        "amount": amount,
        "iso_currency_code": "USD",
        "unofficial_currency_code": null,
        "date": "2026-08-07",
        "datetime": null,
        "name": name,
        "merchant_name": null,
        "merchant_entity_id": null,
        "pending": false,
        "payment_channel": "online",
    })
}

/// Matches a `/transactions/sync` request that carries no `cursor` field —
/// i.e. the first page of a sync, since `PlaidClient` omits it (rather than
/// sending `null`) when there's none yet.
struct NoCursor;
impl wiremock::Match for NoCursor {
    fn matches(&self, request: &Request) -> bool {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
        body.get("cursor").is_none()
    }
}

async fn mount_exchange(app: &crate::utils::TestApp) {
    Mock::given(method("POST"))
        .and(path("/item/public_token/exchange"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-sandbox-token",
            "item_id": "plaid-item-1",
            "request_id": "req_0",
        })))
        .mount(&app.plaid_mock)
        .await;
}

#[tokio::test]
async fn create_item_exchanges_the_token_and_runs_an_initial_sync() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    mount_exchange(&app).await;
    Mock::given(method("POST"))
        .and(path("/transactions/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sync_page(
            "cursor_1",
            false,
            &[plaid_tx("tx_1", "Coffee Shop", 4.5)],
        )))
        .mount(&app.plaid_mock)
        .await;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/plaid/items", app.address))
        .json(&json!({ "public_token": "public-sandbox-token" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(created["item"]["plaid_item_id"], "plaid-item-1");
    assert_eq!(created["sync"]["added"], 1);

    let items: serde_json::Value = app
        .api_client
        .get(format!("{}/plaid/items", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(items.as_array().unwrap().len(), 1);

    let accounts: serde_json::Value = app
        .api_client
        .get(format!("{}/accounts", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(accounts[0]["name"], "Plaid Checking");

    Ok(())
}

#[tokio::test]
async fn create_item_surfaces_a_plaid_error_as_a_502() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    Mock::given(method("POST"))
        .and(path("/item/public_token/exchange"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error_code": "INVALID_PUBLIC_TOKEN",
            "error_message": "the public token is expired",
        })))
        .mount(&app.plaid_mock)
        .await;

    let resp = app
        .api_client
        .post(format!("{}/plaid/items", app.address))
        .json(&json!({ "public_token": "expired-token" }))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 502);

    Ok(())
}

#[tokio::test]
async fn sync_item_pages_through_has_more_until_the_final_page() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    mount_exchange(&app).await;
    Mock::given(method("POST"))
        .and(path("/transactions/sync"))
        .and(NoCursor)
        .respond_with(ResponseTemplate::new(200).set_body_json(sync_page(
            "cursor_page_2",
            true,
            &[plaid_tx("tx_1", "Coffee Shop", 4.5)],
        )))
        .mount(&app.plaid_mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/transactions/sync"))
        .and(body_partial_json(json!({ "cursor": "cursor_page_2" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(sync_page(
            "cursor_page_3",
            false,
            &[plaid_tx("tx_2", "Groceries", 30.0)],
        )))
        .mount(&app.plaid_mock)
        .await;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/plaid/items", app.address))
        .json(&json!({ "public_token": "public-sandbox-token" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    // Both pages' transactions landed, across the two mocked /transactions/sync calls.
    assert_eq!(created["sync"]["added"], 2, "both pages should be consumed");

    let transactions: serde_json::Value = app
        .api_client
        .get(format!("{}/transactions", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(transactions["items"].as_array().unwrap().len(), 2);

    Ok(())
}

#[tokio::test]
async fn sync_item_on_a_nonexistent_item_returns_404() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let resp = app
        .api_client
        .post(format!(
            "{}/plaid/items/{}/sync",
            app.address,
            Uuid::new_v4()
        ))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 404);

    Ok(())
}

#[tokio::test]
async fn delete_item_revokes_on_plaid_then_removes_it_locally() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    mount_exchange(&app).await;
    Mock::given(method("POST"))
        .and(path("/transactions/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sync_page("cursor_1", false, &[])))
        .mount(&app.plaid_mock)
        .await;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/plaid/items", app.address))
        .json(&json!({ "public_token": "public-sandbox-token" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let item_id = created["item"]["id"].as_str().unwrap();

    Mock::given(method("POST"))
        .and(path("/item/remove"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "request_id": "req_2" })))
        .mount(&app.plaid_mock)
        .await;

    let resp = app
        .api_client
        .delete(format!("{}/plaid/items/{item_id}", app.address))
        .send()
        .await?;
    assert_eq!(resp.status().as_u16(), 204);

    let items: serde_json::Value = app
        .api_client
        .get(format!("{}/plaid/items", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(items.as_array().unwrap().len(), 0);

    Ok(())
}

/// Plaid reporting the item already gone (e.g. the user revoked access from
/// their bank's side first) must still let the local disconnect succeed —
/// see `PlaidClient::remove_item`'s doc comment.
#[tokio::test]
async fn delete_item_still_succeeds_when_plaid_reports_item_not_found() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    mount_exchange(&app).await;
    Mock::given(method("POST"))
        .and(path("/transactions/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sync_page("cursor_1", false, &[])))
        .mount(&app.plaid_mock)
        .await;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/plaid/items", app.address))
        .json(&json!({ "public_token": "public-sandbox-token" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let item_id = created["item"]["id"].as_str().unwrap();

    Mock::given(method("POST"))
        .and(path("/item/remove"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error_code": "ITEM_NOT_FOUND",
            "error_message": "the item was not found",
        })))
        .mount(&app.plaid_mock)
        .await;

    let resp = app
        .api_client
        .delete(format!("{}/plaid/items/{item_id}", app.address))
        .send()
        .await?;
    assert_eq!(resp.status().as_u16(), 204);

    Ok(())
}

#[tokio::test]
async fn sandbox_public_token_is_rejected_outside_sandbox_env() -> anyhow::Result<()> {
    let app = crate::utils::spawn_app_with(|cfg| {
        cfg.plaid.env = budget::config::PlaidEnv::Production;
    })
    .await?;

    let resp = app
        .api_client
        .post(format!("{}/plaid/sandbox/public-token", app.address))
        .json(&json!({}))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 400);

    Ok(())
}

#[tokio::test]
async fn sandbox_public_token_returns_a_token_in_sandbox_env() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    Mock::given(method("POST"))
        .and(path("/sandbox/public_token/create"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "public_token": "public-sandbox-token",
        })))
        .mount(&app.plaid_mock)
        .await;

    let resp: serde_json::Value = app
        .api_client
        .post(format!("{}/plaid/sandbox/public-token", app.address))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(resp["public_token"], "public-sandbox-token");

    Ok(())
}
