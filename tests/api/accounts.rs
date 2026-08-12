use serde_json::json;

use crate::utils::spawn_app;

/// `Source::Plaid`/`Source::Manual` accounts can't be created directly
/// through this route (Plaid accounts are created by connecting a bank;
/// Manual isn't a real institution) — only Neo/Amex are.
#[tokio::test]
async fn create_account_accepts_or_rejects_by_source() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let cases = [
        ("neo", 200, "Neo is a real statement-import institution"),
        ("amex", 200, "Amex is a real statement-import institution"),
        (
            "plaid",
            400,
            "Plaid accounts are created by connecting a bank, not this route",
        ),
        (
            "manual",
            400,
            "manual is not a real institution to declare an account under",
        ),
    ];

    for (source, expected_status, message) in cases {
        let resp = app
            .api_client
            .post(format!("{}/accounts", app.address))
            .json(&json!({ "source": source, "name": format!("{source} account") }))
            .send()
            .await?;

        assert_eq!(resp.status().as_u16(), expected_status, "{message}");
    }

    Ok(())
}

#[tokio::test]
async fn create_account_then_list_returns_it() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/accounts", app.address))
        .json(&json!({ "source": "neo", "name": "Neo Mastercard" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(created["name"], "Neo Mastercard");
    assert_eq!(created["source"], "neo");
    assert!(created["item_id"].is_null());

    let accounts: serde_json::Value = app
        .api_client
        .get(format!("{}/accounts", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(accounts.as_array().unwrap().len(), 1);
    assert_eq!(accounts[0]["id"], created["id"]);

    Ok(())
}

#[tokio::test]
async fn rename_account_updates_the_name() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/accounts", app.address))
        .json(&json!({ "source": "amex", "name": "Amex" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = created["id"].as_str().unwrap();

    let renamed: serde_json::Value = app
        .api_client
        .post(format!("{}/accounts/{id}/rename", app.address))
        .json(&json!({ "name": "Amex Gold" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(renamed["name"], "Amex Gold");

    Ok(())
}

#[tokio::test]
async fn rename_a_nonexistent_account_returns_404() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let resp = app
        .api_client
        .post(format!("{}/accounts/does-not-exist/rename", app.address))
        .json(&json!({ "name": "New Name" }))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 404);

    Ok(())
}

#[tokio::test]
async fn delete_account_removes_it_from_the_list() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/accounts", app.address))
        .json(&json!({ "source": "neo", "name": "Neo Mastercard" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = created["id"].as_str().unwrap();

    let resp = app
        .api_client
        .delete(format!("{}/accounts/{id}", app.address))
        .send()
        .await?;
    assert_eq!(resp.status().as_u16(), 204);

    let accounts: serde_json::Value = app
        .api_client
        .get(format!("{}/accounts", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(accounts.as_array().unwrap().len(), 0);

    Ok(())
}

#[tokio::test]
async fn delete_a_nonexistent_account_returns_404() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let resp = app
        .api_client
        .delete(format!("{}/accounts/does-not-exist", app.address))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 404);

    Ok(())
}

#[tokio::test]
async fn tag_then_untag_an_account_round_trips_through_the_list() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/accounts", app.address))
        .json(&json!({ "source": "neo", "name": "Neo Mastercard" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = created["id"].as_str().unwrap();

    let tags: serde_json::Value = app
        .api_client
        .post(format!("{}/accounts/{id}/tags", app.address))
        .json(&json!({ "name": "kind", "value": "credit" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(tags, json!([{ "name": "kind", "value": "credit" }]));

    let accounts: serde_json::Value = app
        .api_client
        .get(format!("{}/accounts", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        accounts[0]["tags"],
        json!([{ "name": "kind", "value": "credit" }])
    );

    let tags: serde_json::Value = app
        .api_client
        .delete(format!("{}/accounts/{id}/tags/kind", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(tags, json!([]));

    Ok(())
}

#[tokio::test]
async fn tagging_a_nonexistent_account_returns_404() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let resp = app
        .api_client
        .post(format!("{}/accounts/does-not-exist/tags", app.address))
        .json(&json!({ "name": "kind", "value": "credit" }))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 404);

    Ok(())
}
