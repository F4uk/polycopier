use crate::models::{ActiveApiOrder, EvaluatedTrade, Position, QueuedOrder, TargetPosition};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr;
use std::time::Instant;

/// Per-wallet win/loss statistics for the AI stats panel.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WalletStats {
    pub total_copies: u32,
    pub wins: u32,
    pub losses: u32,
    pub total_pnl: Decimal,
    /// Consecutive losses currently (reset on win).
    pub consecutive_losses: u32,
    /// Wallet weight/scalar from config (e.g. 0.7 or 1.0). Default 1.0.
    pub weight: Decimal,
}

/// Performance metrics tracked in real-time.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PerfMetrics {
    /// When the bot started (Unix timestamp).
    pub started_at_secs: u64,
    /// Number of API calls made today (reset at UTC midnight).
    pub today_api_calls: u64,
    /// Last measured API latency in milliseconds.
    pub last_api_latency_ms: u64,
    /// Average API latency over last 10 calls.
    pub avg_api_latency_ms: u64,
    /// Last measured copy-trade latency in milliseconds (time from WS event to order submit).
    pub last_copy_latency_ms: u64,
    /// Average copy latency over last 10 calls.
    pub avg_copy_latency_ms: u64,
}

/// PnL snapshot for equity curve charting.
#[derive(Debug, Clone, Serialize)]
pub struct PnlSnapshot {
    pub timestamp_secs: u64,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_balance: Decimal,
}

pub struct BotState {
    pub positions: HashMap<String, Position>,
    pub live_feed: VecDeque<EvaluatedTrade>,
    pub active_orders: Vec<ActiveApiOrder>,
    pub total_balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub realized_pnl: Decimal,
    pub started: bool,
    pub target_positions: Vec<TargetPosition>,
    pub copies_executed: u32,
    pub trades_skipped: u32,

    /// Number of positions WE currently hold that the TARGET also holds.
    /// Set by a dedicated background task that queries both wallets via the API
    /// every 30 seconds -- never inferred from local scanner state.
    pub copied_count: usize,
    /// When the position scanner last completed a full cycle (wall clock).
    /// None until the first scan finishes.
    pub last_scan_at: Option<Instant>,
    /// How many seconds until the next scan is scheduled (set just before sleeping).
    pub next_scan_secs: u64,
    /// When target_positions.cur_price was last refreshed via the dedicated price
    /// refresh task (runs every 20s, independent of scanner urgency).
    pub last_price_refresh_at: Option<Instant>,
    /// Token IDs for which we have a live GTC order in the CLOB that has NOT
    /// yet been filled. Seeded from open CLOB orders at boot, updated by the
    /// strategy engine on submission and by the order watcher on cancellation.
    /// The scanner uses this alongside `positions` to prevent duplicate orders
    /// across bot restarts.
    pub pending_orders: HashMap<String, QueuedOrder>,
    /// When the order watcher last completed a cycle.
    pub last_watcher_run_at: Option<Instant>,
    /// Per-wallet win/loss statistics for AI stats panel.
    pub wallet_stats: HashMap<String, WalletStats>,
    /// Token IDs of muted markets (load/save from muted_markets.json).
    pub muted_markets: HashSet<String>,
    /// Today's copy stats (auto-reset at UTC midnight).
    pub today_copies: u32,
    pub today_wins: u32,
    pub today_losses: u32,
    pub today_pnl: Decimal,
    pub today_date: String,
    /// When the trading freeze (from /ai/freeze) expires. None = not frozen.
    pub freeze_until: Option<Instant>,
    /// Today's cumulative realized loss (reset at UTC midnight). Used for daily loss circuit-breaker.
    pub today_realized_loss: Decimal,
    /// Starting balance at UTC midnight (for daily loss % calculation).
    pub daily_start_balance: Decimal,
    /// Whether daily loss circuit-breaker has been triggered.
    pub daily_loss_triggered: bool,
    /// Wallet addresses that are temporarily blacklisted from copy-trading.
    pub wallet_blacklist: HashSet<String>,
    /// Performance metrics for the monitoring panel.
    pub perf: PerfMetrics,
    /// PnL snapshots for equity curve (sampled every 60s).
    pub pnl_history: Vec<PnlSnapshot>,
    /// Token ownership strategy currently active.
    pub token_ownership_strategy: String,
    /// Whether partial close is enabled.
    pub enable_partial_close: bool,
}

const MUTED_FILE: &str = "muted_markets.json";

fn load_muted_markets() -> HashSet<String> {
    std::fs::read_to_string(MUTED_FILE)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_muted_markets(markets: &HashSet<String>) {
    if let Ok(s) = serde_json::to_string(markets) {
        let _ = std::fs::write(MUTED_FILE, s);
    }
}

impl BotState {
    pub fn new(is_sim: bool, sim_balance: Option<Decimal>) -> Self {
        // Initialize balance to $10,000 for simulation (or override)
        let initial_balance = if is_sim {
            sim_balance.unwrap_or(Decimal::from(10000))
        } else {
            Decimal::from(0)
        };

        Self {
            positions: HashMap::new(),
            live_feed: VecDeque::with_capacity(100),
            active_orders: Vec::new(),
            total_balance: initial_balance,
            unrealized_pnl: Decimal::from(0),
            realized_pnl: Decimal::from(0),
            started: false,
            target_positions: Vec::new(),
            copies_executed: 0,
            trades_skipped: 0,
            copied_count: 0,
            last_scan_at: None,
            next_scan_secs: 0,
            last_price_refresh_at: None,
            pending_orders: HashMap::new(),
            last_watcher_run_at: None,
            wallet_stats: HashMap::new(),
            muted_markets: load_muted_markets(),
            today_copies: 0,
            today_wins: 0,
            today_losses: 0,
            today_pnl: Decimal::from(0),
            today_date: chrono::Utc::now().date_naive().to_string(),
            freeze_until: None,
            today_realized_loss: Decimal::ZERO,
            daily_start_balance: initial_balance,
            daily_loss_triggered: false,
            wallet_blacklist: HashSet::new(),
            perf: PerfMetrics {
                started_at_secs: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                ..Default::default()
            },
            pnl_history: Vec::new(),
            token_ownership_strategy: "first_come".to_string(),
            enable_partial_close: true,
        }
    }

    pub fn push_evaluated_trade(&mut self, trade: EvaluatedTrade) {
        // Anti-Spam: The bot evaluates target positions aggressively every ~5 seconds.
        // If a trade is skipped (e.g., insufficient balance, limits), we do NOT want
        // to infinitely increment the skips counter or flood the UI feed with IDENTICAL
        // rejection events every single cycle.
        let is_duplicate = self.live_feed.iter().any(|existing| {
            existing.validated == trade.validated
                && existing.original_event.token_id == trade.original_event.token_id
                && existing.original_event.side == trade.original_event.side
                && existing.reason == trade.reason
        });

        if !is_duplicate {
            if trade.validated {
                self.copies_executed += 1;
            } else {
                self.trades_skipped += 1;
            }

            if self.live_feed.len() == 100 {
                self.live_feed.pop_back();
            }
            self.live_feed.push_front(trade);
        }
    }

    /// Record a winning close for `wallet`, with the given realized PnL.
    pub fn record_win(&mut self, wallet: &str, pnl: Decimal) {
        let stats = self.wallet_stats.entry(wallet.to_lowercase()).or_default();
        stats.total_copies += 1;
        stats.wins += 1;
        stats.total_pnl += pnl;
        stats.consecutive_losses = 0; // reset on win
        self.today_copies += 1;
        self.today_wins += 1;
        self.today_pnl += pnl;
        // If pnl > 0, it's a realized gain — no loss tracking needed
        self.maybe_reset_today();
    }

    /// Record a losing close for `wallet`, with the given realized PnL.
    pub fn record_loss(&mut self, wallet: &str, pnl: Decimal) {
        let stats = self.wallet_stats.entry(wallet.to_lowercase()).or_default();
        stats.total_copies += 1;
        stats.losses += 1;
        stats.total_pnl += pnl;
        stats.consecutive_losses += 1;
        self.today_copies += 1;
        self.today_losses += 1;
        self.today_pnl += pnl;
        // Track realized loss for daily circuit-breaker
        if pnl < Decimal::ZERO {
            self.today_realized_loss += pnl.abs();
        }
        self.maybe_reset_today();
    }

    /// Check if daily loss circuit-breaker should trigger.
    /// Returns true if today's realized loss exceeds max_daily_loss_pct of starting balance.
    pub fn check_daily_loss_circuit_breaker(&mut self, max_daily_loss_pct: Decimal) -> bool {
        if max_daily_loss_pct <= Decimal::ZERO || self.daily_start_balance <= Decimal::ZERO {
            return false;
        }
        let threshold = self.daily_start_balance * max_daily_loss_pct;
        if self.today_realized_loss >= threshold && !self.daily_loss_triggered {
            self.daily_loss_triggered = true;
            tracing::warn!(
                "[RiskGuard] Daily loss circuit-breaker triggered: loss={} >= threshold={}",
                self.today_realized_loss,
                threshold
            );
            return true;
        }
        false
    }

    /// Check if a wallet should be auto-blacklisted based on consecutive losses or win rate.
    /// Returns true if the wallet was newly blacklisted.
    pub fn check_wallet_blacklist(
        &mut self,
        wallet: &str,
        max_consecutive: u32,
        min_win_rate: Decimal,
    ) -> bool {
        let key = wallet.to_lowercase();
        if self.wallet_blacklist.contains(&key) {
            return false;
        }
        let stats = match self.wallet_stats.get(&key) {
            Some(s) => s,
            None => return false,
        };
        let should_blacklist = if max_consecutive > 0 && stats.consecutive_losses >= max_consecutive
        {
            true
        } else if min_win_rate > Decimal::ZERO && stats.wins + stats.losses >= 3 {
            let win_rate = if stats.wins + stats.losses > 0 {
                Decimal::from(stats.wins) / Decimal::from(stats.wins + stats.losses)
            } else {
                Decimal::ONE
            };
            win_rate < min_win_rate
        } else {
            false
        };
        if should_blacklist {
            self.wallet_blacklist.insert(key.clone());
            tracing::warn!(
                "[RiskGuard] Wallet {} auto-blacklisted (consecutive_losses={}, win_rate={:.1}%)",
                &key[..key.len().min(10)],
                stats.consecutive_losses,
                if stats.wins + stats.losses > 0 {
                    stats.wins as f64 / (stats.wins + stats.losses) as f64 * 100.0
                } else {
                    0.0
                }
            );
            return true;
        }
        false
    }

    /// Whether the given wallet is currently blacklisted.
    pub fn is_wallet_blacklisted(&self, wallet: &str) -> bool {
        self.wallet_blacklist.contains(&wallet.to_lowercase())
    }

    /// Record an API call for performance tracking.
    pub fn record_api_call(&mut self, latency_ms: u64) {
        self.perf.today_api_calls += 1;
        self.perf.last_api_latency_ms = latency_ms;
        // Running average over last 10 calls
        self.perf.avg_api_latency_ms = (self.perf.avg_api_latency_ms * 9 + latency_ms) / 10;
    }

    /// Record a copy-trade latency measurement.
    pub fn record_copy_latency(&mut self, latency_ms: u64) {
        self.perf.last_copy_latency_ms = latency_ms;
        self.perf.avg_copy_latency_ms = (self.perf.avg_copy_latency_ms * 9 + latency_ms) / 10;
    }

    /// Record a PnL snapshot for the equity curve.
    pub fn record_pnl_snapshot(&mut self) {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.pnl_history.push(PnlSnapshot {
            timestamp_secs: now_secs,
            realized_pnl: self.realized_pnl,
            unrealized_pnl: self.unrealized_pnl,
            total_balance: self.total_balance,
        });
        // Keep last 60480 snapshots (7 days at 10s intervals) — enough for chart display
        if self.pnl_history.len() > 60480 {
            self.pnl_history.remove(0);
        }
    }

    /// Returns the equity curve data formatted for frontend chart consumption.
    /// Each entry: { timestamp: "2026-04-19T12:00:00Z", equity: 1050.50, pnl: 50.50 }
    pub fn get_pnl_history_for_chart(&self) -> Vec<serde_json::Value> {
        self.pnl_history
            .iter()
            .map(|s| {
                let ts = chrono::DateTime::from_timestamp(s.timestamp_secs as i64, 0)
                    .unwrap_or_else(chrono::Utc::now);
                let equity = rust_decimal::Decimal::from_str(&format!("{:.2}", s.total_balance))
                    .unwrap_or(s.total_balance);
                let pnl = s.realized_pnl + s.unrealized_pnl;
                serde_json::json!({
                    "timestamp": ts.to_rfc3339(),
                    "equity": f64::try_from(equity).unwrap_or(0.0),
                    "pnl": f64::try_from(pnl).unwrap_or(0.0),
                })
            })
            .collect()
    }

    fn maybe_reset_today(&mut self) {
        let today = chrono::Utc::now().date_naive().to_string();
        if self.today_date != today {
            self.today_copies = 0;
            self.today_wins = 0;
            self.today_losses = 0;
            self.today_pnl = Decimal::from(0);
            self.today_realized_loss = Decimal::ZERO;
            self.daily_start_balance = self.total_balance;
            self.daily_loss_triggered = false;
            self.perf.today_api_calls = 0;
            self.today_date = today;
        }
    }

    /// Whether the given token_id market is currently muted.
    pub fn is_market_muted(&self, token_id: &str) -> bool {
        self.muted_markets.contains(token_id)
    }

    /// Toggle mute state for a market. Returns the new mute state (true = muted).
    pub fn toggle_market_mute(&mut self, token_id: &str) -> bool {
        let now_muted = if self.muted_markets.contains(token_id) {
            self.muted_markets.remove(token_id);
            false
        } else {
            self.muted_markets.insert(token_id.to_string());
            true
        };
        save_muted_markets(&self.muted_markets);
        now_muted
    }

    /// Returns true if trading is currently frozen (BUY entries blocked).
    pub fn is_frozen(&self) -> bool {
        self.freeze_until
            .map(|t| Instant::now() < t)
            .unwrap_or(false)
    }

    /// Set freeze for `duration_secs` seconds from now.
    pub fn freeze_for(&mut self, duration_secs: u64) {
        self.freeze_until = Some(Instant::now() + std::time::Duration::from_secs(duration_secs));
        tracing::info!("[State] Trading frozen for {}s.", duration_secs);
    }
}

impl Default for BotState {
    fn default() -> Self {
        Self::new(false, None)
    }
}
