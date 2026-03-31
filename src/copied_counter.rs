/// Dedicated "Copied" counter: queries our wallet and each target wallet via
/// the Polymarket Data API, computes the intersection of held token IDs, and
/// returns the count.
///
/// This is the authoritative source for the TUI "Copied" counter. It is
/// drive by a background task in main.rs that calls `run_copied_counter_loop`
/// on startup and every 30 seconds thereafter.
use alloy::primitives::Address;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::Client as DataClient;
use std::collections::HashSet;
use std::str::FromStr;

// -- Pure intersection helper (extracted for testability) ---------------------

/// Count how many token IDs in `target_tokens` are also present in `our_tokens`.
///
/// This is a pure function with no I/O -- all API results are pre-fetched and
/// passed in as slices so unit tests can drive it without network access.
///
/// # Arguments
/// * `our_tokens`    - token IDs we currently hold (from our wallet API response)
/// * `target_tokens` - token IDs the target wallet currently holds
///
/// # Returns
/// Number of token IDs that appear in both sets (the intersection size).
pub fn count_intersection(our_tokens: &HashSet<String>, target_tokens: &[String]) -> usize {
    target_tokens
        .iter()
        .filter(|t| our_tokens.contains(*t))
        .count()
}

// -- Live API helper ----------------------------------------------------------

/// Fetch all token IDs held by `wallet_str` from the Polymarket Data API.
/// Returns an empty set on any error (logged as WARN).
pub async fn fetch_token_ids(client: &DataClient, wallet_str: &str) -> HashSet<String> {
    let addr = match Address::from_str(wallet_str.trim()) {
        Ok(a) => a,
        Err(_) => {
            tracing::warn!("copied_counter: invalid wallet address: {}", wallet_str);
            return HashSet::new();
        }
    };
    let req = PositionsRequest::builder().user(addr).build();
    match client.positions(&req).await {
        Ok(ps) => ps.into_iter().map(|p| p.asset.to_string()).collect(),
        Err(e) => {
            tracing::warn!("copied_counter: failed to fetch {}: {}", wallet_str, e);
            HashSet::new()
        }
    }
}

/// Fetch our positions and each target's positions, then return the total
/// number of positions WE hold that any target ALSO holds.
///
/// Used by the background task in main.rs.
pub async fn compute_copied_count(
    client: &DataClient,
    our_wallet: &str,
    target_wallets: &[String],
) -> usize {
    let our_tokens = fetch_token_ids(client, our_wallet).await;
    if our_tokens.is_empty() {
        return 0;
    }

    let mut total = 0usize;
    for wallet_str in target_wallets {
        let target_tokens: Vec<String> = fetch_token_ids(client, wallet_str)
            .await
            .into_iter()
            .collect();
        total += count_intersection(&our_tokens, &target_tokens);
    }
    total
}

// -- Background task entry point ----------------------------------------------

/// Spawns a loop that calls `compute_copied_count` immediately, then every
/// `interval_secs` seconds, writing the result to `state.copied_count`.
///
/// Call this from main.rs after all clients are initialized.
pub fn start_copied_counter(
    our_wallet: String,
    target_wallets: Vec<String>,
    state: std::sync::Arc<tokio::sync::RwLock<crate::state::BotState>>,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let client = DataClient::default();
        loop {
            let count = compute_copied_count(&client, &our_wallet, &target_wallets).await;

            {
                let mut g = state.write().await;
                g.copied_count = count;
            }
            tracing::debug!(
                "copied_counter: {} position(s) mirrored from target(s)",
                count
            );

            tokio::time::sleep(tokio::time::Duration::from_secs(interval_secs)).await;
        }
    });
}
