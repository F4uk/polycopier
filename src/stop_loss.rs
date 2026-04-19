//! Dynamic tiered stop-loss + trailing take-profit module
//!
//! Fully adapted for Polymarket binary options (0–1 USDC price range).
//!
//! **Dynamic stop-loss tiers** (by entry price):
//! | Entry Price  | Initial SL | ≥20% profit → | ≥50% profit → |
//! |--------------|-----------|---------------|---------------|
//! | < 0.40       | 20%       | Breakeven     | Lock 20% gain |
//! | 0.40–0.55    | 15%       | Breakeven     | Lock 20% gain |
//! | 0.55–0.70    | 12%       | Breakeven     | Lock 20% gain |
//! | > 0.70       | 10%       | Breakeven     | Lock 20% gain |
//!
//! **Dynamic trailing take-profit tiers** (by entry price):
//! | Entry Price  | TP activate | Allowed drawdown |
//! |--------------|-------------|-----------------|
//! | < 0.40       | 80% profit  | 15% from peak   |
//! | 0.40–0.55    | 60% profit  | 12% from peak   |
//! | 0.55–0.70    | 40% profit  | 10% from peak   |
//! | > 0.70       | 25% profit  | 8% from peak    |
//!
//! **Force lines** (never modified):
//! - Price < 0.15 → immediate exit (prevent going to zero)
//! - Price > 0.95 → immediate exit (avoid last-minute crash)
//!
//! **Special conditions**:
//! - Market ends < 4h: TP threshold halved, drawdown halved (tighter trailing when active)
//! - Price swing > 30% from entry: drawdown +5% (wider room for volatile moves)
//! - SL and TP run in parallel, whichever triggers first wins

use crate::clients::OrderSubmitter;
use crate::config::{Config, StopLossTiersConfig};
use crate::models::{OrderRequest, TradeSide};
use crate::state::BotState;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Price tier parameters
// ---------------------------------------------------------------------------

/// Per-tier parameters for dynamic SL/TP.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PriceTier {
    /// Initial stop-loss percentage below entry (e.g. 0.20 = 20%).
    pub initial_sl_pct: Decimal,
    /// Profit threshold to activate trailing take-profit (e.g. 0.80 = 80%).
    pub tp_activate_pct: Decimal,
    /// Allowed drawdown from peak after TP activation (e.g. 0.15 = 15%).
    pub tp_drawdown_pct: Decimal,
}

/// Select the price tier based on entry price, driven by user-configurable [stop_loss_tiers].
fn get_tier(entry_price: Decimal, tiers: &StopLossTiersConfig) -> PriceTier {
    let tier = if entry_price < tiers.tier1.max_entry {
        &tiers.tier1
    } else if entry_price < tiers.tier2.max_entry {
        &tiers.tier2
    } else if entry_price < tiers.tier3.max_entry {
        &tiers.tier3
    } else {
        &tiers.tier4
    };
    PriceTier {
        initial_sl_pct: tier.initial_sl_pct,
        tp_activate_pct: tier.tp_activate_pct,
        tp_drawdown_pct: tier.tp_drawdown_pct,
    }
}

/// Stop-loss status label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlStatus {
    /// Initial stop-loss from entry price tier.
    Initial,
    /// Moved to breakeven (profit ≥ 20%).
    Breakeven,
    /// Locked 20% profit (profit ≥ 50%).
    LockProfit,
}

impl std::fmt::Display for SlStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlStatus::Initial => write!(f, "初始止损"),
            SlStatus::Breakeven => write!(f, "保本止损"),
            SlStatus::LockProfit => write!(f, "锁利止损"),
        }
    }
}

/// Close reason for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    InitialSl,
    BreakevenSl,
    LockProfitSl,
    ForceStop,
    TrailingTp,
    ForceClose,
}

impl std::fmt::Display for CloseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloseReason::InitialSl => write!(f, "初始止损"),
            CloseReason::BreakevenSl => write!(f, "保本止损"),
            CloseReason::LockProfitSl => write!(f, "锁利止损"),
            CloseReason::ForceStop => write!(f, "强制止损"),
            CloseReason::TrailingTp => write!(f, "回撤止盈"),
            CloseReason::ForceClose => write!(f, "强制止盈"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tracked position
// ---------------------------------------------------------------------------

/// Per-position dynamic tiered stop-loss / trailing take-profit record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedPosition {
    pub token_id: String,
    pub entry_price: Decimal,
    /// Current stop-loss price (moves up as profit grows).
    pub current_sl_price: Decimal,
    /// Current SL status label.
    pub sl_status: SlStatus,
    /// Price tier parameters (frozen at entry time).
    pub tier: PriceTier,
    /// Effective TP activate threshold (may be halved near expiry).
    pub effective_tp_activate_pct: Decimal,
    /// Effective drawdown allowed (may be halved near expiry or boosted by volatility).
    pub effective_drawdown_pct: Decimal,
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
    pub current_sl_price: Decimal,
    pub sl_status: SlStatus,
    pub trailing_activated: bool,
    pub peak_price: Option<Decimal>,
    /// The price at which trailing TP would be activated.
    pub tp_activate_price: Decimal,
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
    /// Force stop-loss price — below this, always exit (e.g. 0.15).
    pub force_stop_price: Decimal,
    /// Force take-profit price — above this, always exit (e.g. 0.95).
    pub force_close_price: Decimal,
    /// Check interval in seconds.
    pub check_interval_secs: u64,
    /// User-configurable price-tier parameters for dynamic SL/TP.
    pub tiers: StopLossTiersConfig,
    /// Tracked positions keyed by token_id.
    pub positions: HashMap<String, TrackedPosition>,
}

impl StopLossState {
    pub fn new(
        enabled: bool,
        force_stop_price: Decimal,
        force_close_price: Decimal,
        check_interval_secs: u64,
        tiers: StopLossTiersConfig,
    ) -> Self {
        Self {
            enabled,
            force_stop_price,
            force_close_price,
            check_interval_secs,
            tiers,
            positions: HashMap::new(),
        }
    }

    /// Record a new entry so the monitor can watch it.
    pub fn record_entry(&mut self, token_id: String, entry_price: Decimal) {
        let tier = get_tier(entry_price, &self.tiers);
        let current_sl_price = entry_price * (Decimal::ONE - tier.initial_sl_pct);
        let tp_activate_price = entry_price * (Decimal::ONE + tier.tp_activate_pct);

        let tracked = TrackedPosition {
            token_id: token_id.clone(),
            entry_price,
            current_sl_price,
            sl_status: SlStatus::Initial,
            tier,
            effective_tp_activate_pct: tier.tp_activate_pct,
            effective_drawdown_pct: tier.tp_drawdown_pct,
            trailing_activated: false,
            peak_price: None,
        };

        info!(
            "[SL/TP] Tracking token {}: entry={:.3}, sl={:.3} ({}, {}), tp_activate_at={:.3} ({}%), drawdown={}% | force_stop={:.2}, force_close={:.2}",
            &token_id[..token_id.len().min(12)],
            entry_price,
            current_sl_price,
            tracked.sl_status,
            format_pct(tier.initial_sl_pct),
            tp_activate_price,
            format_pct(tier.tp_activate_pct),
            format_pct(tier.tp_drawdown_pct),
            self.force_stop_price,
            self.force_close_price,
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
                let tp_activate_price =
                    tp.entry_price * (Decimal::ONE + tp.effective_tp_activate_pct);

                let drawdown_remaining_pct = if tp.trailing_activated {
                    if let (Some(peak), Some(cur)) = (tp.peak_price, cur_price) {
                        if peak > Decimal::ZERO {
                            let current_drawdown = (peak - cur) / peak;
                            Some(tp.effective_drawdown_pct - current_drawdown)
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
                    current_sl_price: tp.current_sl_price,
                    sl_status: tp.sl_status,
                    trailing_activated: tp.trailing_activated,
                    peak_price: tp.peak_price,
                    tp_activate_price,
                    drawdown_remaining_pct,
                }
            })
            .collect()
    }
}

/// Format a Decimal fraction as percentage string (e.g. 0.15 → "15.0").
fn format_pct(v: Decimal) -> String {
    (v * dec!(100)).to_string()
}

// ---------------------------------------------------------------------------
// Monitor background task
// ---------------------------------------------------------------------------

/// Start the dynamic tiered stop-loss / trailing take-profit monitor.
pub async fn start_stop_loss_monitor(
    state: Arc<RwLock<BotState>>,
    sl_state: Arc<Mutex<StopLossState>>,
    submitter: OrderSubmitter,
    _config: Config,
) {
    // Do not start if disabled
    {
        let guard = sl_state.lock().await;
        if !guard.enabled {
            info!("[SL/TP] Disabled in config — monitor not started.");
            return;
        }
    }

    let (check_interval, force_stop, force_close) = {
        let guard = sl_state.lock().await;
        (
            tokio::time::Duration::from_secs(guard.check_interval_secs.max(1)),
            guard.force_stop_price,
            guard.force_close_price,
        )
    };

    info!(
        "[SL/TP] Monitor started (checking every {}s, force_stop={:.2}, force_close={:.2}).",
        check_interval.as_secs(),
        force_stop,
        force_close
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

            // Get current prices + end dates from BotState
            // cur_price comes from Polymarket official API via wallet_sync
            #[allow(clippy::type_complexity)]
            let (current_prices, end_dates): (
                HashMap<String, Decimal>,
                HashMap<String, Option<chrono::DateTime<chrono::Utc>>>,
            ) = {
                let bot = state.read().await;
                let mut prices = HashMap::new();
                let mut ends = HashMap::new();
                for token_id in bot.positions.keys() {
                    if let Some(tp) = bot
                        .target_positions
                        .iter()
                        .find(|tp| &tp.token_id == token_id)
                    {
                        prices.insert(token_id.clone(), tp.cur_price);
                    }
                }
                // Get end_dates from pending_orders (which carry event_end_date)
                for (tid, pending) in &bot.pending_orders {
                    if !prices.contains_key(tid) {
                        prices.insert(tid.clone(), pending.price);
                    }
                    ends.insert(tid.clone(), pending.event_end_date);
                }
                (prices, ends)
            };

            for mut tracked_pos in tracked {
                let cur_price = match current_prices.get(&tracked_pos.token_id) {
                    Some(&p) => p,
                    None => continue,
                };

                // ── Special condition: near-expiry adjustments ────────────────
                let near_expiry = end_dates
                    .get(&tracked_pos.token_id)
                    .and_then(|ed| ed.as_ref())
                    .map(|ed| {
                        let now = chrono::Utc::now();
                        (*ed - now).num_hours() < 4
                    })
                    .unwrap_or(false);

                if near_expiry && !tracked_pos.trailing_activated {
                    // Halve TP threshold and drawdown when market ends < 4h
                    // Only apply if trailing hasn't activated yet — once active,
                    // the drawdown threshold is already set and shouldn't change.
                    tracked_pos.effective_tp_activate_pct =
                        tracked_pos.tier.tp_activate_pct / dec!(2);
                    tracked_pos.effective_drawdown_pct = tracked_pos.tier.tp_drawdown_pct / dec!(2);
                    let mut guard = sl_state.lock().await;
                    if let Some(pos) = guard.positions.get_mut(&tracked_pos.token_id) {
                        pos.effective_tp_activate_pct = tracked_pos.effective_tp_activate_pct;
                        pos.effective_drawdown_pct = tracked_pos.effective_drawdown_pct;
                    }
                } else if near_expiry && tracked_pos.trailing_activated {
                    // Near-expiry but trailing already active: only halve drawdown
                    // to tighten the trailing stop without resetting the activation state.
                    let halved_drawdown = tracked_pos.tier.tp_drawdown_pct / dec!(2);
                    if halved_drawdown < tracked_pos.effective_drawdown_pct {
                        tracked_pos.effective_drawdown_pct = halved_drawdown;
                        let mut guard = sl_state.lock().await;
                        if let Some(pos) = guard.positions.get_mut(&tracked_pos.token_id) {
                            pos.effective_drawdown_pct = halved_drawdown;
                        }
                    }
                }

                // ── Special condition: high volatility boost ──────────────────
                // Use actual price swing from entry (not inter-check delta which
                // is only seconds apart and not meaningful as "daily" volatility).
                if tracked_pos.entry_price > Decimal::ZERO {
                    let move_from_entry =
                        ((cur_price - tracked_pos.entry_price) / tracked_pos.entry_price).abs();
                    if move_from_entry > dec!(0.30) {
                        tracked_pos.effective_drawdown_pct =
                            tracked_pos.tier.tp_drawdown_pct + dec!(0.05);
                        let mut guard = sl_state.lock().await;
                        if let Some(pos) = guard.positions.get_mut(&tracked_pos.token_id) {
                            pos.effective_drawdown_pct = tracked_pos.effective_drawdown_pct;
                        }
                    }
                }

                // ── Force stop: price < force_stop_price ─────────────────────
                if cur_price < force_stop {
                    warn!(
                        "[SL/TP] FORCE STOP triggered for token {}! cur={:.3} < force_stop={:.2}",
                        &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                        cur_price,
                        force_stop
                    );
                    close_position(
                        &tracked_pos.token_id,
                        &state,
                        &submitter,
                        CloseReason::ForceStop,
                        cur_price,
                    )
                    .await;
                    sl_state.lock().await.remove(&tracked_pos.token_id);
                    continue;
                }

                // ── Force close: price > force_close_price ───────────────────
                if cur_price > force_close {
                    warn!(
                        "[SL/TP] FORCE CLOSE triggered for token {}! cur={:.3} > force_close={:.2}",
                        &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                        cur_price,
                        force_close
                    );
                    close_position(
                        &tracked_pos.token_id,
                        &state,
                        &submitter,
                        CloseReason::ForceClose,
                        cur_price,
                    )
                    .await;
                    sl_state.lock().await.remove(&tracked_pos.token_id);
                    continue;
                }

                // ── Update stop-loss tier (breakeven / lock-profit) ──────────
                let profit_pct = if tracked_pos.entry_price > Decimal::ZERO {
                    (cur_price - tracked_pos.entry_price) / tracked_pos.entry_price
                } else {
                    Decimal::ZERO
                };

                let new_sl_price;
                let new_sl_status;

                if profit_pct >= dec!(0.50) {
                    // Lock 20% profit: SL = entry × 1.2
                    new_sl_price = tracked_pos.entry_price * dec!(1.2);
                    new_sl_status = SlStatus::LockProfit;
                } else if profit_pct >= dec!(0.20) {
                    // Breakeven: SL = entry price
                    new_sl_price = tracked_pos.entry_price;
                    new_sl_status = SlStatus::Breakeven;
                } else {
                    new_sl_price = tracked_pos.current_sl_price;
                    new_sl_status = tracked_pos.sl_status;
                };

                // Only log if SL status changed
                if new_sl_status != tracked_pos.sl_status {
                    info!(
                        "[SL/TP] SL adjusted for token {}: {} → {} | sl_price={:.3} → {:.3} | entry={:.3} | profit={:.1}%",
                        &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                        tracked_pos.sl_status,
                        new_sl_status,
                        tracked_pos.current_sl_price,
                        new_sl_price,
                        tracked_pos.entry_price,
                        profit_pct * dec!(100)
                    );
                    tracked_pos.sl_status = new_sl_status;
                    tracked_pos.current_sl_price = new_sl_price;
                    let mut guard = sl_state.lock().await;
                    if let Some(pos) = guard.positions.get_mut(&tracked_pos.token_id) {
                        pos.sl_status = new_sl_status;
                        pos.current_sl_price = new_sl_price;
                    }
                }

                // ── Check stop-loss ──────────────────────────────────────────
                if cur_price <= tracked_pos.current_sl_price {
                    let loss_pct = if tracked_pos.entry_price > Decimal::ZERO {
                        ((tracked_pos.entry_price - cur_price) / tracked_pos.entry_price)
                            * dec!(100)
                    } else {
                        Decimal::ZERO
                    };
                    let close_reason = match tracked_pos.sl_status {
                        SlStatus::Initial => CloseReason::InitialSl,
                        SlStatus::Breakeven => CloseReason::BreakevenSl,
                        SlStatus::LockProfit => CloseReason::LockProfitSl,
                    };
                    warn!(
                        "[SL/TP] {} triggered for token {}! entry={:.3}, cur={:.3}, sl_price={:.3}, loss={:.1}%",
                        close_reason,
                        &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                        tracked_pos.entry_price,
                        cur_price,
                        tracked_pos.current_sl_price,
                        loss_pct
                    );
                    close_position(
                        &tracked_pos.token_id,
                        &state,
                        &submitter,
                        close_reason,
                        cur_price,
                    )
                    .await;
                    sl_state.lock().await.remove(&tracked_pos.token_id);
                    continue;
                }

                // ── Check trailing take-profit ───────────────────────────────
                if !tracked_pos.trailing_activated {
                    if profit_pct >= tracked_pos.effective_tp_activate_pct {
                        tracked_pos.trailing_activated = true;
                        tracked_pos.peak_price = Some(cur_price);
                        info!(
                            "[SL/TP] Trailing TP ACTIVATED for token {}! activate_price={:.3}, peak={:.3}, entry={:.3}, drawdown={}%{}",
                            &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                            cur_price,
                            cur_price,
                            tracked_pos.entry_price,
                            format_pct(tracked_pos.effective_drawdown_pct),
                            if near_expiry { " (near-expiry halved)" } else { "" }
                        );
                        let mut guard = sl_state.lock().await;
                        if let Some(pos) = guard.positions.get_mut(&tracked_pos.token_id) {
                            pos.trailing_activated = true;
                            pos.peak_price = Some(cur_price);
                        }
                    }
                } else {
                    // Trailing active — update peak and check drawdown
                    let mut should_sell = false;
                    let mut drawdown_pct = Decimal::ZERO;

                    if let Some(peak) = tracked_pos.peak_price {
                        if cur_price > peak {
                            info!(
                                "[SL/TP] Peak updated for token {}: old={:.3} → new={:.3}",
                                &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                                peak,
                                cur_price
                            );
                            tracked_pos.peak_price = Some(cur_price);
                            let mut guard = sl_state.lock().await;
                            if let Some(pos) = guard.positions.get_mut(&tracked_pos.token_id) {
                                pos.peak_price = Some(cur_price);
                            }
                        } else if peak > Decimal::ZERO {
                            drawdown_pct = (peak - cur_price) / peak;
                            if drawdown_pct >= tracked_pos.effective_drawdown_pct {
                                should_sell = true;
                            }
                        }
                    }

                    if should_sell {
                        warn!(
                            "[SL/TP] Trailing TP TRIGGERED for token {}! peak={:.3}, cur={:.3}, drawdown={:.1}%, threshold={:.1}%{} | exit_price={:.3}",
                            &tracked_pos.token_id[..tracked_pos.token_id.len().min(12)],
                            tracked_pos.peak_price.unwrap_or(Decimal::ZERO),
                            cur_price,
                            drawdown_pct * dec!(100),
                            tracked_pos.effective_drawdown_pct * dec!(100),
                            if near_expiry { " (near-expiry)" } else { "" },
                            cur_price
                        );
                        close_position(
                            &tracked_pos.token_id,
                            &state,
                            &submitter,
                            CloseReason::TrailingTp,
                            cur_price,
                        )
                        .await;
                        sl_state.lock().await.remove(&tracked_pos.token_id);
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
    }
}

// ---------------------------------------------------------------------------
// Helper: close a position via market sell
// ---------------------------------------------------------------------------

async fn close_position(
    token_id: &str,
    state: &Arc<RwLock<BotState>>,
    submitter: &OrderSubmitter,
    reason: CloseReason,
    exit_price: Decimal,
) {
    let held_size = {
        let bot = state.read().await;
        bot.positions
            .get(token_id)
            .map(|p| p.size)
            .unwrap_or(Decimal::ZERO)
    };

    if held_size <= Decimal::ZERO {
        return;
    }

    // Market sell via Polymarket SDK — use current price + 2% slippage buffer.
    // For very low-priced tokens, ensure at least 0.01 floor; cap at 0.99.
    let slippage = dec!(0.02);
    let sell_price = ((exit_price * (Decimal::ONE + slippage)).round_dp(2))
        .max(dec!(0.01))
        .min(dec!(0.99));
    let order = OrderRequest {
        token_id: token_id.to_string(),
        price: sell_price,
        size: held_size,
        side: TradeSide::SELL,
    };

    match submitter.clone()(order).await {
        Ok(()) => {
            info!(
                "[SL/TP] Closed token {} via {} at price {:.3}",
                &token_id[..token_id.len().min(12)],
                reason,
                exit_price
            );
        }
        Err(e) => {
            error!(
                "[SL/TP] Failed to close token {}: {}",
                &token_id[..token_id.len().min(12)],
                e
            );
        }
    }
}
