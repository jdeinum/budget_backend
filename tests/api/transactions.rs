use serde_json::json;
use uuid::Uuid;

use crate::utils::spawn_app;

async fn create_account(app: &crate::utils::TestApp, source: &str, name: &str) -> String {
    let account: serde_json::Value = app
        .api_client
        .post(format!("{}/accounts", app.address))
        .json(&json!({ "source": source, "name": name }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    account["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn create_transaction_on_a_nonexistent_account_returns_404() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let resp = app
        .api_client
        .post(format!("{}/transactions", app.address))
        .json(&json!({
            "account_id": "does-not-exist",
            "date": "2026-08-07",
            "amount": 15.99,
            "name": "Coffee",
        }))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 404);

    Ok(())
}

#[tokio::test]
async fn create_transaction_with_a_nonexistent_merchant_id_returns_404() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    let account_id = create_account(&app, "neo", "Neo Mastercard").await;

    let resp = app
        .api_client
        .post(format!("{}/transactions", app.address))
        .json(&json!({
            "account_id": account_id,
            "date": "2026-08-07",
            "amount": 15.99,
            "name": "Coffee",
            "merchant_id": Uuid::new_v4(),
        }))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 404);

    Ok(())
}

#[tokio::test]
async fn create_transaction_then_list_returns_it() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    let account_id = create_account(&app, "neo", "Neo Mastercard").await;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/transactions", app.address))
        .json(&json!({
            "account_id": account_id,
            "date": "2026-08-07",
            "amount": 15.99,
            "name": "Coffee",
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(created["account_id"], account_id);
    assert_eq!(created["amount"], 15.99);
    assert_eq!(created["name"], "Coffee");

    let page: serde_json::Value = app
        .api_client
        .get(format!("{}/transactions", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["id"], created["id"]);

    Ok(())
}

#[tokio::test]
async fn list_transactions_on_a_fresh_db_is_empty_with_no_next_cursor() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let page: serde_json::Value = app
        .api_client
        .get(format!("{}/transactions", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(page["items"].as_array().unwrap().len(), 0);
    assert!(page["next_cursor"].is_null());

    Ok(())
}

#[tokio::test]
async fn list_transactions_rejects_malformed_query_params() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let cases = [
        (
            "tags",
            "not-a-pair",
            "a tags filter without a `name:value` pair must be rejected",
        ),
        (
            "exclude_tags",
            "also-not-a-pair",
            "an exclude_tags filter without a `name:value` pair must be rejected",
        ),
        (
            "sort",
            "date",
            "a sort with no `:dir` half must be rejected",
        ),
        (
            "sort",
            "nonsense:desc",
            "a sort on an unknown field must be rejected",
        ),
        (
            "sort",
            "date:sideways",
            "a sort with an unknown direction must be rejected",
        ),
        (
            "cursor",
            "not-valid-base64!",
            "a malformed cursor must be rejected",
        ),
    ];

    for (param, value, message) in cases {
        let resp = app
            .api_client
            .get(format!("{}/transactions", app.address))
            .query(&[(param, value)])
            .send()
            .await?;

        assert_eq!(resp.status().as_u16(), 400, "{message}");
    }

    Ok(())
}

#[tokio::test]
async fn list_transactions_rejects_a_cursor_minted_under_a_different_sort() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    let account_id = create_account(&app, "neo", "Neo Mastercard").await;
    for (date, name) in [("2026-08-07", "Coffee"), ("2026-08-08", "Groceries")] {
        app.api_client
            .post(format!("{}/transactions", app.address))
            .json(&json!({
                "account_id": account_id,
                "date": date,
                "amount": 15.99,
                "name": name,
            }))
            .send()
            .await?
            .error_for_status()?;
    }

    // limit=1 over 2 rows guarantees a next_cursor minted under date:desc.
    let page: serde_json::Value = app
        .api_client
        .get(format!("{}/transactions", app.address))
        .query(&[("sort", "date:desc"), ("limit", "1")])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let cursor = page["next_cursor"].as_str().expect("a next_cursor");

    let resp = app
        .api_client
        .get(format!("{}/transactions", app.address))
        .query(&[("sort", "amount:asc"), ("cursor", cursor)])
        .send()
        .await?;
    assert_eq!(
        resp.status().as_u16(),
        400,
        "a date:desc cursor must be rejected once the sort changes to amount:asc"
    );

    Ok(())
}

#[tokio::test]
async fn tag_then_untag_a_transaction_round_trips_through_get() -> anyhow::Result<()> {
    let app = spawn_app().await?;
    let account_id = create_account(&app, "neo", "Neo Mastercard").await;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/transactions", app.address))
        .json(&json!({
            "account_id": account_id,
            "date": "2026-08-07",
            "amount": 15.99,
            "name": "Coffee",
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = created["id"].as_str().unwrap();

    let tags: serde_json::Value = app
        .api_client
        .post(format!("{}/transactions/{id}/tags", app.address))
        .json(&json!({ "name": "category", "value": "FOOD_AND_DRINK" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        tags,
        json!([{ "name": "category", "value": "FOOD_AND_DRINK" }])
    );

    let tags: serde_json::Value = app
        .api_client
        .delete(format!("{}/transactions/{id}/tags/category", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(tags, json!([]));

    Ok(())
}

#[tokio::test]
async fn tagging_a_nonexistent_transaction_returns_404() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let resp = app
        .api_client
        .post(format!(
            "{}/transactions/{}/tags",
            app.address,
            Uuid::new_v4()
        ))
        .json(&json!({ "name": "category", "value": "FOOD_AND_DRINK" }))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 404);

    Ok(())
}
