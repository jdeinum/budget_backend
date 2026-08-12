use serde::Serialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::AppResult;
use crate::plaid::PlaidClient;
use crate::plaid::models::PlaidTransaction;
use crate::repo::{
    account, item, item::Item, merchant, merchant_category, tag, transaction, transaction_rule,
};

#[derive(Debug, Default, Serialize)]
pub struct SyncSummary {
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
}

/// Pages through `/transactions/sync` until `has_more` is false, persisting
/// the item's cursor after every page so a crash mid-sync resumes rather than
/// re-fetching from scratch.
pub async fn sync_item_transactions(
    pool: &SqlitePool,
    plaid: &PlaidClient,
    item: &Item,
) -> AppResult<SyncSummary> {
    let mut cursor = item.cursor.clone();
    let mut summary = SyncSummary::default();

    loop {
        let page = plaid
            .sync_transactions(&item.access_token, cursor.as_deref())
            .await?;

        // Plaid's own friendly account names, keyed by account_id — included
        // alongside every sync page (not per-transaction). Falls back to the
        // raw account_id for any account this map doesn't cover.
        let account_names: std::collections::HashMap<&str, &str> = page
            .accounts
            .iter()
            .map(|a| (a.account_id.as_str(), a.name.as_str()))
            .collect();
        let account_name_for = |account_id: &str| -> String {
            account_names
                .get(account_id)
                .copied()
                .unwrap_or(account_id)
                .to_string()
        };

        for tx in &page.added {
            account::upsert_account(
                pool,
                &tx.account_id,
                item.id.into(),
                &account_name_for(&tx.account_id),
            )
            .await?;
            let merchant_id = resolve_merchant(pool, tx).await?;
            let transaction_id =
                transaction::upsert_transaction(pool, item.id.into(), merchant_id, tx).await?;
            tag_category(pool, transaction_id, tx).await?;
            summary.added += 1;
        }
        for tx in &page.modified {
            account::upsert_account(
                pool,
                &tx.account_id,
                item.id.into(),
                &account_name_for(&tx.account_id),
            )
            .await?;
            let merchant_id = resolve_merchant(pool, tx).await?;
            let transaction_id =
                transaction::upsert_transaction(pool, item.id.into(), merchant_id, tx).await?;
            tag_category(pool, transaction_id, tx).await?;
            summary.modified += 1;
        }
        for removed in &page.removed {
            transaction::delete_transaction(pool, &removed.transaction_id).await?;
            summary.removed += 1;
        }

        item::update_cursor(pool, item.id.into(), &page.next_cursor).await?;
        cursor = Some(page.next_cursor);

        if !page.has_more {
            break;
        }
    }

    transaction_rule::reevaluate_ignored(pool).await?;

    Ok(summary)
}

/// Upserts the transaction's merchant (by name) and records its observed
/// category against that merchant's category history, so `merchant_categories`
/// tracks how a merchant's category changes over time rather than just what
/// each individual transaction happened to be tagged with.
async fn resolve_merchant(pool: &SqlitePool, tx: &PlaidTransaction) -> AppResult<Option<Uuid>> {
    let Some(name) = tx.merchant_name.as_deref().or(tx.name.as_deref()) else {
        return Ok(None);
    };

    let merchant = merchant::upsert_merchant(pool, name, tx.merchant_entity_id.as_deref()).await?;

    let (category_primary, category_detailed) = tx
        .personal_finance_category
        .as_ref()
        .map(|c| (c.primary.as_deref(), c.detailed.as_deref()))
        .unwrap_or((None, None));

    merchant_category::record_observation(
        pool,
        merchant.id.into(),
        category_primary,
        category_detailed,
        tx.date,
    )
    .await?;

    Ok(Some(merchant.id.into()))
}

/// Records the transaction's own Plaid category as `category`/`category_detail`
/// tags on it — this is the per-transaction observation, distinct from
/// `merchant_categories`' longer-lived per-merchant history.
async fn tag_category(
    pool: &SqlitePool,
    transaction_id: Uuid,
    tx: &PlaidTransaction,
) -> AppResult<()> {
    let Some(category) = &tx.personal_finance_category else {
        return Ok(());
    };
    if let Some(primary) = &category.primary {
        tag::tag_transaction(pool, transaction_id, "category", primary).await?;
    }
    if let Some(detailed) = &category.detailed {
        tag::tag_transaction(pool, transaction_id, "category_detail", detailed).await?;
    }
    Ok(())
}
