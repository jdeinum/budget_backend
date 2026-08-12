use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::state::AppState;

mod accounts;
mod merchants;
mod plaid;
mod tags;
mod transaction_rules;
mod transactions;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/accounts",
            get(accounts::list_accounts).post(accounts::create_account),
        )
        .route("/accounts/{id}", delete(accounts::delete_account))
        .route("/accounts/{id}/rename", post(accounts::rename_account))
        .route("/accounts/{id}/tags", post(accounts::tag_account))
        .route(
            "/accounts/{id}/tags/{name}",
            delete(accounts::untag_account),
        )
        .route(
            "/accounts/{id}/statements",
            post(accounts::upload_statements),
        )
        .route("/plaid/link-token", post(plaid::create_link_token))
        .route(
            "/plaid/sandbox/public-token",
            post(plaid::sandbox_public_token),
        )
        .route(
            "/plaid/items",
            post(plaid::create_item).get(plaid::list_items),
        )
        .route("/plaid/items/{id}", delete(plaid::delete_item))
        .route("/plaid/items/sync", post(plaid::sync_all_items))
        .route("/plaid/items/{id}/sync", post(plaid::sync_item))
        .route(
            "/plaid/items/{id}/transactions",
            get(plaid::list_item_transactions),
        )
        .route(
            "/transactions",
            get(transactions::list_transactions).post(transactions::create_transaction),
        )
        .route(
            "/transactions/{id}/tags",
            post(transactions::tag_transaction),
        )
        .route(
            "/transactions/{id}/tags/{name}",
            delete(transactions::untag_transaction),
        )
        .route("/merchants", get(merchants::list_merchants))
        .route(
            "/merchants/{id}/categories",
            get(merchants::merchant_categories),
        )
        .route("/merchants/{id}/tags", post(merchants::tag_merchant))
        .route(
            "/merchants/{id}/tags/{name}",
            delete(merchants::untag_merchant),
        )
        .route("/tags", get(tags::list_tags).post(tags::create_tag))
        .route(
            "/transaction-rules",
            get(transaction_rules::list_transaction_rules)
                .post(transaction_rules::create_transaction_rule),
        )
        .route(
            "/transaction-rules/{id}",
            delete(transaction_rules::delete_transaction_rule),
        )
}
