//! Risk engine — stateful pre-trade checks run by the strategy engine.
//!
//! ## Checks implemented
//!
//! | Check | Env var | Default | Gap |
//! |---|---|---|---|
//! | Micro-trade anti-spoofing | — | $1 minimum | original |
//! | Daily volume limit | `MAX_DAILY_VOLUME_USD` | 0 (disabled) | 12 |
//! | Consecutive-loss circuit breaker | `MAX_CONSECUTIVE_LOSSES` | 0 (disabled) | 12 |
//! | Rapid-flip guard | — | 60s cooldown per token | 12 |
//! | Per-category position limit | `[risk_by_category]` | 0 (disabled) | new |

use crate::config::Config;
use crate::models::{TradeEvent, TradeSide};
use crate::state::BotState;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::time::Instant;

pub struct RiskEngine {
    config: Config,

    // -- Daily volume tracker (Gap 12) ------------------------------------
    /// Total USD notional traded today (UTC day).
    daily_volume_usd: Decimal,
    /// UTC date when `daily_volume_usd` was last reset.
    daily_reset_date: chrono::NaiveDate,

    // -- Consecutive-loss circuit breaker (Gap 12) ------------------------
    /// How many consecutive BUY-side trade evaluations have been flagged as losses.
    /// A "loss" is defined as the risk engine rejecting a trade for a non-spoofing reason
    /// (i.e., the engine itself prevents placing it).  Reset on any successful approval.
    consecutive_losses: u32,
    /// If `Some(until)`, all new trades are blocked until `Instant::now() >= until`.
    cooldown_until: Option<Instant>,

    // -- Rapid-flip guard (Gap 12) ----------------------------------------
    /// token_id → time of last BUY approved for that token.
    last_buy_at: HashMap<String, Instant>,
}

impl RiskEngine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            daily_volume_usd: Decimal::ZERO,
            daily_reset_date: chrono::Utc::now().date_naive(),
            consecutive_losses: 0,
            cooldown_until: None,
            last_buy_at: HashMap::new(),
        }
    }

    /// Run all risk checks against an incoming `TradeEvent`.
    ///
    /// Returns `Ok(())` if the trade may proceed, or `Err(reason)` to reject it.
    /// Internally updates daily-volume and consecutive-loss state.
    pub fn check_trade(&mut self, trade: &TradeEvent) -> Result<(), String> {
        // === 0. Reset daily volume at UTC midnight ===
        let today = chrono::Utc::now().date_naive();
        if today != self.daily_reset_date {
            self.daily_volume_usd = Decimal::ZERO;
            self.daily_reset_date = today;
            self.consecutive_losses = 0; // fresh day resets loss streak too
            tracing::info!("Risk: daily volume/loss counters reset for {today}.");
        }

        let trade_value = trade.size * trade.price;

        // Only apply entry restrictions (spoofing, cooldowns) to BUY events.
        // We MUST always allow closing trades to proceed, regardless of size or cooldown.
        if trade.side == crate::models::TradeSide::BUY {
            // === 1. Anti-spoofing: minimum $1 notional ===
            if trade_value < Decimal::from(1) {
                return Err("Trade value is too small (spoofing protection)".to_string());
            }

            // === 2. Consecutive-loss cooldown (Gap 12) ===
            if let Some(until) = self.cooldown_until {
                if std::time::Instant::now() < until {
                    let remaining = until.duration_since(std::time::Instant::now()).as_secs();
                    return Err(format!(
                        "Risk cooldown active ({remaining}s remaining) after {} consecutive losses",
                        self.config.max_consecutive_losses
                    ));
                } else {
                    // Cooldown expired — reset
                    self.cooldown_until = None;
                    self.consecutive_losses = 0;
                    tracing::info!("Risk: consecutive-loss cooldown expired — resuming.");
                }
            }
        }

        // === 3. Daily volume limit (Gap 12) ===
        if self.config.max_daily_volume_usd > Decimal::ZERO
            && self.daily_volume_usd + trade_value > self.config.max_daily_volume_usd
        {
            return Err(format!(
                "Daily volume limit ${:.2} would be exceeded (used ${:.2}, trade ${:.2})",
                self.config.max_daily_volume_usd, self.daily_volume_usd, trade_value
            ));
        }

        // === 4. Rapid-flip guard (Gap 12): no re-entry within 60 s ===
        if trade.side == TradeSide::BUY {
            if let Some(&last) = self.last_buy_at.get(&trade.token_id) {
                let age_secs = last.elapsed().as_secs();
                if age_secs < 60 {
                    return Err(format!(
                        "Rapid-flip guard: token {} was entered {}s ago — cooldown 60s",
                        &trade.token_id[..trade.token_id.len().min(12)],
                        age_secs
                    ));
                }
            }
        }

        // === All checks passed — update state ===
        self.daily_volume_usd += trade_value;
        if trade.side == TradeSide::BUY {
            self.last_buy_at
                .insert(trade.token_id.clone(), Instant::now());
            // Successful BUY approval resets consecutive-loss counter.
            self.consecutive_losses = 0;
        }

        Ok(())
    }

    /// Record that a trade we approved ultimately resulted in a loss
    /// (called by the order-watcher or strategy engine when an order is
    /// cancelled due to the target's position dropping past max_copy_loss_pct).
    ///
    /// Increments the consecutive-loss counter and triggers a cooldown if
    /// the configured threshold is reached.
    pub fn record_loss(&mut self) {
        if self.config.max_consecutive_losses == 0 {
            return; // feature disabled
        }
        self.consecutive_losses += 1;
        tracing::warn!(
            "Risk: consecutive losses = {} / {}",
            self.consecutive_losses,
            self.config.max_consecutive_losses
        );
        if self.consecutive_losses >= self.config.max_consecutive_losses {
            let until =
                Instant::now() + std::time::Duration::from_secs(self.config.loss_cooldown_secs);
            self.cooldown_until = Some(until);
            tracing::warn!(
                "Risk: consecutive-loss limit hit — cooling down for {}s.",
                self.config.loss_cooldown_secs
            );
        }
    }

    /// Compute total position size (in USDC) for a given category.
    /// Sums up: for each position in state.positions,
    /// find its TargetPosition (via token_id) and check if it belongs to the given category.
    /// Position size * cur_price = USDC value.
    fn get_category_position_size(&self, state: &BotState, category: &str) -> Decimal {
        let mut total = Decimal::ZERO;
        for (token_id, position) in &state.positions {
            if let Some(target_pos) = state
                .target_positions
                .iter()
                .find(|tp| &tp.token_id == token_id)
            {
                if target_pos.category == category {
                    let size_usd = position.size * target_pos.cur_price;
                    total += size_usd;
                }
            }
        }
        total
    }

    /// Check if a trade is allowed under category position limits.
    /// Returns Ok(()) if allowed, or Err(message) if blocked.
    /// Only applies to BUY (entry) trades.
    pub fn check_category_limit(
        &self,
        _token_id: &str,
        category: &str,
        trade_value_usd: Decimal,
        state: &BotState,
    ) -> Result<(), String> {
        if !self.config.risk_by_category_enabled {
            return Ok(());
        }
        // If no explicit limits configured, skip
        if self.config.risk_by_category_limits.is_empty() {
            return Ok(());
        }

        let limit = self
            .config
            .risk_by_category_limits
            .get(category)
            .copied()
            .unwrap_or(self.config.risk_by_category_default);

        if limit == Decimal::ZERO {
            return Err(format!(
                "Category '{}' is completely disabled (position limit = 0)",
                category
            ));
        }

        let current_size = self.get_category_position_size(state, category);
        let new_size = current_size + trade_value_usd;

        if new_size > limit {
            tracing::warn!(
                "[Risk/Category] Skipping open: category '{}' would exceed position limit \
                 (current ${:.2} + trade ${:.2} > limit ${:.2})",
                category,
                current_size,
                trade_value_usd,
                limit
            );
            return Err(format!(
                "Category '{}' position limit would be exceeded: current ${:.2} + trade ${:.2} > limit ${:.2}",
                category,
                current_size,
                trade_value_usd,
                limit
            ));
        }

        tracing::info!(
            "[Risk/Category] Category '{}': current=${:.2}, trade=${:.2}, limit=${:.2}",
            category,
            current_size,
            trade_value_usd,
            limit
        );

        Ok(())
    }

    /// Extended trade check that includes category limit validation.
    /// Call this from the strategy engine for BUY entries where category is known.
    pub fn check_trade_with_category(
        &mut self,
        trade: &TradeEvent,
        category: &str,
        state: &BotState,
    ) -> Result<(), String> {
        self.check_trade(trade)?; // existing checks first
        if trade.side == TradeSide::BUY {
            let trade_value = trade.size * trade.price;
            self.check_category_limit(&trade.token_id, category, trade_value, state)?;
        }
        Ok(())
    }
}
