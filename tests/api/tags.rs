use serde_json::json;

use crate::utils::spawn_app;

#[tokio::test]
async fn create_tag_then_list_returns_it() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let created: serde_json::Value = app
        .api_client
        .post(format!("{}/tags", app.address))
        .json(&json!({ "name": "category", "value": "FOOD_AND_DRINK" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(created["name"], "category");
    assert_eq!(created["value"], "FOOD_AND_DRINK");

    let tags: serde_json::Value = app
        .api_client
        .get(format!("{}/tags", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(tags.as_array().unwrap().len(), 1);
    assert_eq!(tags[0]["name"], "category");

    Ok(())
}

#[tokio::test]
async fn list_tags_on_a_fresh_db_is_empty() -> anyhow::Result<()> {
    let app = spawn_app().await?;

    let tags: serde_json::Value = app
        .api_client
        .get(format!("{}/tags", app.address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(tags.as_array().unwrap().len(), 0);

    Ok(())
}
