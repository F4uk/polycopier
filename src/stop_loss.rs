//! Local hard stop-loss / take-profit module
//!
//! On each successful BUY entry the strategy engine calls `record_entry()`
//! to save the entry price and the computed stop-loss / take-profit prices.
//!
//! A background task spawned by `start_stop_loss_monitor()` wakes every
//! `check_interval_secs` seconds, scans all tracked positions, and
//! submits a SELL order when the current price hits either boundary.

use crate::clients::OrderSubmitter;
use crate::config::Config;
use crate::models::{OrderRequest, TradeSide};
use crate::state::BotState;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Tracked position
// ---------------------------------------------------------------------------

/// Per-position stop-loss / take-profit record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedPosition {
    pub token_id: String,
    pub entry_price: Decimal,
    pub stop_loss_price: Decimal,
    pub take_profit_price: Decimal,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Shared stop-loss / take-profit state.
pub struct StopLossState {
    /// Enabled flag — read from config.
    pub enabled: bool,
    /// Stop-loss percentage (e.g. 0.15 = 15% below entry).
    pub stop_loss_pct: Decimal,
    /// Take-profit percentage (e.g. 0.30 = 30% above entry).
    pub take_profit_pct: Decimal,
    /// Check interval in seconds.
    pub check_interval_secs: u64,
    /// Tracked positions keyed by token_id.
    pub positions: HashMap<String, TrackedPosition>,
}

impl StopLossState {
    pub fn new(
        enabled: bool,
        stop_loss_pct: Decimal,
        take_profit_pct: Decimal,
        check_interval_secs: u64,
    ) -> Self {
        Self {
            enabled,
            stop_loss_pct,
            take_profit_pct,
            check_interval_secs,
            positions: HashMap::new(),
        }
    }

    /// Record a new entry so the monitor can watch it.
    pub fn record_entry(&mut self, token_id: String, entry_price: Decimal) {
        let stop_loss_price = entry_price * (Decimal::ONE - self.stop_loss_pct);
        let take_profit_price = entry_price * (Decimal::ONE + self.take_profit_pct);
        let tracked = TrackedPosition {
            token_id: token_id.clone(),
            entry_price,
            stop_loss_price,
            take_profit_price,
        };
        info!(
            "[STOP-LOSS] Tracking token {}: entry={}, SL={}, TP={}",
            &token_id[..token_id.len().min(12)],
            entry_price,
            stop_loss_price,
            take_profit_price
        );
        self.positions.insert(token_id, tracked);
    }

    /// Remove a token (e.g. after it has been closed).
    pub fn remove(&mut self, token_id: &str) {
        self.positions.remove(token_id);
    }
}

// ---------------------------------------------------------------------------
// Monitor background task
// ---------------------------------------------------------------------------

/// Start the stop-loss / take-profit monitor.
///
/// This spawns a background tokio task that wakes every
/// `check_interval_secs` seconds and checks all tracked positions.
pub fn start_stop_loss_monitor(
    state: Arc<RwLock<BotState>>,
    sl_state: Arc<Mutex<StopLossState>>,
    submitter: OrderSubmitter,
    _config: Config,
) {
    // Do not start if disabled
    {
        let guard = sl_state.blocking_lock();
        if !guard.enabled {
            info!("[STOP-LOSS] Disabled in config — monitor not started.");
            return;
        }
    }

    tokio::spawn(async move {
        let check_interval = {
            let guard = sl_state.lock().await;
            tokio::time::Duration::from_secs(guard.check_interval_secs.max(1))
        };

        info!(
            "[STOP-LOSS] Monitor started (checking every {}s).",
            check_interval.as_secs()
        );

        let mut interval = tokio::time::interval(check_interval);

        loop {
            interval.tick().await;

            // Snapshot tracked positions
            let tracked: Vec<TrackedPosition> = {
                let guard = sl_state.lock().await;
                if !guard.enabled {
                    continue;
                }
                guard.positions.values().cloned().collect()
            };

            // Get current prices from BotState
            let current_prices: HashMap<String, Decimal> = {
                let bot = state.read().await;
                let mut prices = HashMap::new();
                for token_id in bot.positions.keys() {
                    // Use target_positions to find current price if available
                    if let Some(tp) = bot
                        .target_positions
                        .iter()
                        .find(|tp| &tp.token_id == token_id)
                    {
                        prices.insert(token_id.clone(), tp.cur_price);
                    }
                }
                prices
            };

            for tracked_pos in tracked {
                let cur_price = match current_prices.get(&tracked_pos.token_id) {
                    Some(&p) => p,
                    None => continue, // no price data available yet
                };

                // Check stop-loss
                if cur_price <= tracked_pos.stop_loss_price {
                    warn!(
                        "[STOP-LOSS] STOP-LOSS triggered for token {}! price={} <= SL={}",
                        &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                        cur_price,
                        tracked_pos.stop_loss_price
                    );

                    // Get held size
                    let held_size = {
                        let bot = state.read().await;
                        bot.positions
                            .get(&tracked_pos.token_id)
                            .map(|p| p.size)
                            .unwrap_or(Decimal::ZERO)
                    };

                    if held_size > Decimal::ZERO {
                        let sell_price = Decimal::from_str("0.99").unwrap_or(dec!(0.99));
                        let order = OrderRequest {
                            token_id: tracked_pos.token_id.clone(),
                            price: sell_price,
                            size: held_size,
                            side: TradeSide::SELL,
                        };

                        match submitter.clone()(order).await {
                            Ok(()) => {
                                info!(
                                    "[STOP-LOSS] Successfully closed token {} via stop-loss",
                                    &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)]
                                );
                            }
                            Err(e) => {
                                error!(
                                    "[STOP-LOSS] Failed to close token {}: {}",
                                    &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                                    e
                                );
                            }
                        }
                    }

                    // Remove from tracking regardless
                    {
                        let mut guard = sl_state.lock().await;
                        guard.remove(&tracked_pos.token_id);
                    }
                    continue;
                }

                // Check take-profit
                if cur_price >= tracked_pos.take_profit_price {
                    warn!(
                        "[STOP-LOSS] TAKE-PROFIT triggered for token {}! price={} >= TP={}",
                        &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                        cur_price,
                        tracked_pos.take_profit_price
                    );

                    let held_size = {
                        let bot = state.read().await;
                        bot.positions
                            .get(&tracked_pos.token_id)
                            .map(|p| p.size)
                            .unwrap_or(Decimal::ZERO)
                    };

                    if held_size > Decimal::ZERO {
                        let sell_price = Decimal::from_str("0.99").unwrap_or(dec!(0.99));
                        let order = OrderRequest {
                            token_id: tracked_pos.token_id.clone(),
                            price: sell_price,
                            size: held_size,
                            side: TradeSide::SELL,
                        };

                        match submitter.clone()(order).await {
                            Ok(()) => {
                                info!(
                                    "[STOP-LOSS] Successfully closed token {} via take-profit",
                                    &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)]
                                );
                            }
                            Err(e) => {
                                error!(
                                    "[STOP-LOSS] Failed to close token {}: {}",
                                    &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                                    e
                                );
                            }
                        }
                    }

                    {
                        let mut guard = sl_state.lock().await;
                        guard.remove(&tracked_pos.token_id);
                    }
                    continue;
                }
            }

            // Periodic cleanup — remove tracked positions that no longer exist in BotState
            {
                let bot = state.read().await;
                let mut guard = sl_state.lock().await;
                guard
                    .positions
                    .retain(|token_id, _| bot.positions.contains_key(token_id));
            }
        }
    });
}
