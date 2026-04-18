use crate::models::{ActiveApiOrder, EvaluatedTrade, Position, QueuedOrder, TargetPosition};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

/// Per-wallet win/loss statistics for the AI stats panel.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WalletStats {
    pub total_copies: u32,
    pub wins: u32,
    pub losses: u32,
    pub total_pnl: Decimal,
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
        self.today_copies += 1;
        self.today_wins += 1;
        self.today_pnl += pnl;
        self.maybe_reset_today();
    }

    /// Record a losing close for `wallet`, with the given realized PnL.
    pub fn record_loss(&mut self, wallet: &str, pnl: Decimal) {
        let stats = self.wallet_stats.entry(wallet.to_lowercase()).or_default();
        stats.total_copies += 1;
        stats.losses += 1;
        stats.total_pnl += pnl;
        self.today_copies += 1;
        self.today_losses += 1;
        self.today_pnl += pnl;
        self.maybe_reset_today();
    }

    fn maybe_reset_today(&mut self) {
        let today = chrono::Utc::now().date_naive().to_string();
        if self.today_date != today {
            self.today_copies = 0;
            self.today_wins = 0;
            self.today_losses = 0;
            self.today_pnl = Decimal::from(0);
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
