pub mod clients;
pub mod config;
pub mod listener;
pub mod models;
pub mod position_scanner;
pub mod risk;
pub mod state;
pub mod strategy;
pub mod ui;
pub mod utils;

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize logging — set to WARN to prevent log lines bleeding into the TUI.
    //    For diagnostic detail run: RUST_LOG=debug cargo run
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::WARN)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    // 2. Load Configuration via Prompt Wizard
    let config = config::Config::load_or_prompt().await?;

    // 3. Initialize Shared State
    let state = Arc::new(RwLock::new(state::BotState::new()));

    // 4. Initialize Risk Engine
    let risk_engine = risk::RiskEngine::new(config.clone());

    // 5. Initialize API / RPC Clients
    let (poly_submitter, balance_fetcher) = clients::build_order_submitter(&config).await?;

    // 6. Connect WebSocket Listener (Spawns Task)
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(100);
    listener::start_ws_listener(&config, event_tx.clone()).await?;

    // 7. Start Strategy & Execution Engines
    strategy::start_strategy_engine(
        event_rx,
        state.clone(),
        risk_engine,
        poly_submitter,
        config.clone(),
    );

    // 8. Seed OUR own open positions on startup so the scanner doesn't re-enter
    //    positions we already hold from a previous session.
    {
        use alloy::primitives::Address;
        use polymarket_client_sdk::data::types::request::PositionsRequest;
        use polymarket_client_sdk::data::Client as DataClient;
        use std::str::FromStr;

        let funder = config.funder_address.clone();
        let state_seed = state.clone();
        tokio::spawn(async move {
            let data_client = DataClient::default();
            if let Ok(addr) = Address::from_str(&funder) {
                let req = PositionsRequest::builder().user(addr).build();
                match data_client.positions(&req).await {
                    Ok(positions) => {
                        let mut guard = state_seed.write().await;
                        for p in positions {
                            let token_id = p.asset.to_string();
                            guard.positions.insert(
                                token_id.clone(),
                                crate::models::Position {
                                    token_id,
                                    size: p.size,
                                    average_entry_price: p.avg_price,
                                },
                            );
                        }
                        tracing::warn!(
                            "Seeded {} existing position(s) from wallet — scanner will skip these.",
                            guard.positions.len()
                        );
                    }
                    Err(e) => tracing::warn!("Could not seed own positions on startup: {}", e),
                }
            }
        });
    }

    // 9. Scan target wallets for pre-existing open positions (startup + adaptive interval)
    position_scanner::start_position_scanner(config.clone(), state.clone(), event_tx);

    // 8. Poll live USDC balance every 10 seconds and update TUI
    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                match balance_fetcher().await {
                    Ok(balance) => {
                        let mut guard = state.write().await;
                        guard.total_balance = balance;
                    }
                    Err(e) => tracing::warn!("Balance fetch failed: {}", e),
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        });
    }

    // 8. Start Terminal UI (Blocks main thread)
    ui::start_tui(state.clone(), config.clone()).await?;

    Ok(())
}
