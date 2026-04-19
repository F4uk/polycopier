//! Local hard stop-loss / trailing take-profit module
//!
//! On each successful BUY entry the strategy engine calls `record_entry()`
//! to save the entry price and the computed stop-loss price.
//!
//! **Stop-loss**: Fixed — exits when price drops `stop_loss_pct` below entry.
//!
//! **Trailing take-profit**:
//!   a. After entry, only stop-loss is checked.
//!   b. When profit ≥ `take_profit_pct` (e.g. 35%), trailing is **activated**.
//!   c. The highest price seen since activation is tracked as `peak_price`.
//!   d. Every check interval: if price drops ≥ `take_profit_drawdown_pct`
//!      from `peak_price`, exit immediately.
//!   e. If price keeps rising, `peak_price` is continuously updated.
//!
//! This lets winners run while locking in gains once a pullback starts.

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

/// Per-position stop-loss / trailing take-profit record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedPosition {
    pub token_id: String,
    pub entry_price: Decimal,
    pub stop_loss_price: Decimal,
    /// Target profit % to activate trailing (e.g. 0.35 = 35%).
    pub take_profit_pct: Decimal,
    /// Allowed drawdown from peak after activation (e.g. 0.10 = 10%).
    pub take_profit_drawdown_pct: Decimal,
    /// Whether trailing take-profit has been activated.
    pub trailing_activated: bool,
    /// Highest price seen since trailing activation (None until activated).
    pub peak_price: Option<Decimal>,
}

/// Status info for a tracked position (for UI display).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedPositionStatus {
    pub token_id: String,
    pub entry_price: Decimal,
    pub stop_loss_price: Decimal,
    pub trailing_activated: bool,
    pub peak_price: Option<Decimal>,
    /// Distance from peak to drawdown threshold (remaining % before TP triggers).
    pub drawdown_remaining_pct: Option<Decimal>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Shared stop-loss / trailing take-profit state.
pub struct StopLossState {
    /// Enabled flag — read from config.
    pub enabled: bool,
    /// Stop-loss percentage (e.g. 0.10 = 10% below entry).
    pub stop_loss_pct: Decimal,
    /// Take-profit target % to activate trailing (e.g. 0.35 = 35% above entry).
    pub take_profit_pct: Decimal,
    /// Allowed drawdown from peak after trailing activation (e.g. 0.10 = 10%).
    pub take_profit_drawdown_pct: Decimal,
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
        take_profit_drawdown_pct: Decimal,
        check_interval_secs: u64,
    ) -> Self {
        Self {
            enabled,
            stop_loss_pct,
            take_profit_pct,
            take_profit_drawdown_pct,
            check_interval_secs,
            positions: HashMap::new(),
        }
    }

    /// Record a new entry so the monitor can watch it.
    pub fn record_entry(&mut self, token_id: String, entry_price: Decimal) {
        let stop_loss_price = entry_price * (Decimal::ONE - self.stop_loss_pct);
        let take_profit_activate_price = entry_price * (Decimal::ONE + self.take_profit_pct);
        let tracked = TrackedPosition {
            token_id: token_id.clone(),
            entry_price,
            stop_loss_price,
            take_profit_pct: self.take_profit_pct,
            take_profit_drawdown_pct: self.take_profit_drawdown_pct,
            trailing_activated: false,
            peak_price: None,
        };
        info!(
            "[STOP-LOSS] Tracking token {}: entry={}, stop_loss={}, trailing_tp_activate_at={}, drawdown_pct={}",
            &token_id[..token_id.len().min(12)],
            entry_price,
            stop_loss_price,
            take_profit_activate_price,
            self.take_profit_drawdown_pct
        );
        self.positions.insert(token_id, tracked);
    }

    /// Remove a token (e.g. after it has been closed).
    pub fn remove(&mut self, token_id: &str) {
        self.positions.remove(token_id);
    }

    /// Get status info for all tracked positions (for UI).
    pub fn get_all_status(
        &self,
        current_prices: &HashMap<String, Decimal>,
    ) -> Vec<TrackedPositionStatus> {
        self.positions
            .values()
            .map(|tp| {
                let cur_price = current_prices.get(&tp.token_id).copied();
                let drawdown_remaining_pct = if tp.trailing_activated {
                    if let (Some(peak), Some(cur)) = (tp.peak_price, cur_price) {
                        if peak > Decimal::ZERO {
                            let current_drawdown = (peak - cur) / peak;
                            Some(tp.take_profit_drawdown_pct - current_drawdown)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                TrackedPositionStatus {
                    token_id: tp.token_id.clone(),
                    entry_price: tp.entry_price,
                    stop_loss_price: tp.stop_loss_price,
                    trailing_activated: tp.trailing_activated,
                    peak_price: tp.peak_price,
                    drawdown_remaining_pct,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Monitor background task
// ---------------------------------------------------------------------------

/// Start the stop-loss / trailing take-profit monitor.
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

            // Get current prices from BotState — uses cur_price from Polymarket official API
            // (populated by wallet_sync position sync task)
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

            for mut tracked_pos in tracked {
                let cur_price = match current_prices.get(&tracked_pos.token_id) {
                    Some(&p) => p,
                    None => continue, // no price data available yet
                };

                // ── Check stop-loss (always active) ──────────────────────────
                if cur_price <= tracked_pos.stop_loss_price {
                    let loss_pct = if tracked_pos.entry_price > Decimal::ZERO {
                        ((tracked_pos.entry_price - cur_price) / tracked_pos.entry_price)
                            * dec!(100)
                    } else {
                        Decimal::ZERO
                    };
                    warn!(
                        "[STOP-LOSS] STOP-LOSS triggered for token {}! entry={}, cur={}, loss={:.1}%, sl_price={}",
                        &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                        tracked_pos.entry_price,
                        cur_price,
                        loss_pct,
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
                        // Market sell via Polymarket SDK — price 0.99 ensures fill
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
                                    "[STOP-LOSS] Successfully closed token {} via stop-loss at price {}",
                                    &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                                    cur_price
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

                // ── Check trailing take-profit ──────────────────────────────
                let profit_pct = if tracked_pos.entry_price > Decimal::ZERO {
                    (cur_price - tracked_pos.entry_price) / tracked_pos.entry_price
                } else {
                    Decimal::ZERO
                };

                if !tracked_pos.trailing_activated {
                    // Step a: Check if profit has reached the activation threshold
                    if profit_pct >= tracked_pos.take_profit_pct {
                        tracked_pos.trailing_activated = true;
                        tracked_pos.peak_price = Some(cur_price);
                        info!(
                            "[STOP-LOSS] Trailing TP ACTIVATED for token {}! activate_price={}, peak={}, entry={}",
                            &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                            cur_price,
                            cur_price,
                            tracked_pos.entry_price
                        );
                        // Update the tracked position in shared state
                        {
                            let mut guard = sl_state.lock().await;
                            if let Some(pos) = guard.positions.get_mut(&tracked_pos.token_id) {
                                pos.trailing_activated = true;
                                pos.peak_price = Some(cur_price);
                            }
                        }
                    }
                } else {
                    // Trailing is active — update peak and check drawdown
                    let mut should_sell = false;
                    let mut drawdown_pct = Decimal::ZERO;

                    if let Some(peak) = tracked_pos.peak_price {
                        if cur_price > peak {
                            // New peak — update
                            info!(
                                "[STOP-LOSS] Trailing TP peak updated for token {}: old_peak={} → new_peak={}",
                                &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                                peak,
                                cur_price
                            );
                            tracked_pos.peak_price = Some(cur_price);
                            {
                                let mut guard = sl_state.lock().await;
                                if let Some(pos) = guard.positions.get_mut(&tracked_pos.token_id) {
                                    pos.peak_price = Some(cur_price);
                                }
                            }
                        } else if peak > Decimal::ZERO {
                            // Check drawdown from peak
                            drawdown_pct = (peak - cur_price) / peak;
                            if drawdown_pct >= tracked_pos.take_profit_drawdown_pct {
                                should_sell = true;
                            }
                        }
                    }

                    if should_sell {
                        warn!(
                            "[STOP-LOSS] Trailing TP TRIGGERED for token {}! peak={}, cur={}, drawdown={:.1}%, threshold={:.1}%, exit_price={}",
                            &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                            tracked_pos.peak_price.unwrap_or(Decimal::ZERO),
                            cur_price,
                            drawdown_pct * dec!(100),
                            tracked_pos.take_profit_drawdown_pct * dec!(100),
                            cur_price
                        );

                        let held_size = {
                            let bot = state.read().await;
                            bot.positions
                                .get(&tracked_pos.token_id)
                                .map(|p| p.size)
                                .unwrap_or(Decimal::ZERO)
                        };

                        if held_size > Decimal::ZERO {
                            // Market sell via Polymarket SDK
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
                                        "[STOP-LOSS] Successfully closed token {} via trailing take-profit at price {}",
                                        &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                                        cur_price
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
