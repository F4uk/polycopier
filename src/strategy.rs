use crate::clients::OrderSubmitter;
use crate::config::Config;
use crate::copy_ledger::CopyLedger;
use crate::models::{EvaluatedTrade, OrderRequest, SizingMode, TradeEvent, TradeSide};
use crate::risk::RiskEngine;
use crate::slippage_guard;
use crate::state::BotState;
use crate::wash_trade_filter;
use alloy::primitives::Address;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::Client as DataClient;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::Duration;
use tracing::{debug, info, warn};

// -- Pure helpers (extracted for testability) ----------------------------------

/// Applies slippage to a price to produce the limit order price.
pub fn calculate_limit_price(price: Decimal, side: TradeSide, slippage_pct: Decimal) -> Decimal {
    // Most Polymarket markets use tick size 0.01 (2 decimal places).
    // A small number of high-liquidity markets use 0.001 (3 dp).
    // Rounding to 2 dp works for all markets: the CLOB accepts 2 dp
    // even when tick size is 0.001, but rejects 3 dp on 0.01-tick markets.
    let max_price = Decimal::new(99, 2); // 0.99
    let min_price = Decimal::new(1, 2); // 0.01
    match side {
        TradeSide::BUY => (price + price * slippage_pct).round_dp(2).min(max_price),
        TradeSide::SELL => (price - price * slippage_pct).round_dp(2).max(min_price),
    }
}

/// Caps entry size to max_trade_usd / price, returns the original size if within budget.
pub fn calculate_entry_size(size: Decimal, price: Decimal, max_trade_usd: Decimal) -> Decimal {
    let cost = size * price;
    if cost > max_trade_usd {
        max_trade_usd / price
    } else {
        size
    }
}

/// Minimum share count the Polymarket CLOB enforces per order.
/// Any order with size < 5 shares gets a 400: "Size (X) lower than the minimum: 5"
pub const MIN_ORDER_SHARES: Decimal = Decimal::from_parts(5, 0, 0, false, 0);

/// Dollar floor (secondary sanity guard — the real constraint is MIN_ORDER_SHARES).
pub const MIN_ORDER_USD: Decimal = Decimal::from_parts(1, 0, 0, false, 0);

/// Compute the USD budget for a single BUY order according to the active [`SizingMode`].
///
/// | Mode | Formula |
/// |---|---|
/// | `Fixed` | `max_trade_usd` (constant) |
/// | `SelfPct` | `our_balance * copy_size_pct`, capped at `max_trade_usd` |
/// | `TargetUsd` | `target_notional` (exact $ the target bet), capped at `max_trade_usd` |
///
/// Returns **`Decimal::ZERO`** when the computed budget is below `MIN_ORDER_USD` ($1).
/// Callers treat zero as "skip this order". This respects the user's configured
/// percentage exactly: 7% of $39 = $2.73 ≥ $1 → order placed correctly.
pub fn compute_order_usd(
    our_balance: Decimal,
    sizing_mode: &SizingMode,
    copy_size_pct: Option<Decimal>,
    wallet_scalar: Decimal,
    mut max_trade_usd: Decimal,
    target_notional: Decimal,
) -> Decimal {
    // If the user's available balance is smaller than the hard max ceiling,
    // gracefully scale down the max ceiling to their available balance
    // minus an estimated 2% fee overhead buffer (divide by 1.02),
    // ensuring we still enter the trade with "all we have" rather than erroring out.
    let usable_balance = our_balance / Decimal::from_str("1.02").unwrap();
    if usable_balance < max_trade_usd {
        max_trade_usd = usable_balance;
    }

    let desired = match sizing_mode {
        SizingMode::Fixed => max_trade_usd,
        SizingMode::SelfPct => {
            let pct =
                copy_size_pct.unwrap_or_else(|| max_trade_usd / our_balance.max(Decimal::ONE));
            our_balance * pct
        }
        SizingMode::TargetUsd => target_notional,
        SizingMode::TargetScalar => target_notional * wallet_scalar,
    };
    let capped = desired.min(max_trade_usd);
    // Return ZERO to signal "skip" when below the CLOB minimum.
    // Never floor UP — that silently overrides the user's sizing config.
    if capped < MIN_ORDER_USD {
        Decimal::ZERO
    } else {
        capped
    }
}

// ---------------------------------------------------------------------------
// Debounce cache with TTL eviction (Gap 5)
// ---------------------------------------------------------------------------

/// Maximum number of entries the debounce cache may hold simultaneously.
/// At ~3 events/s per target × 4 targets, 512 covers >40 seconds of burst.
const DEBOUNCE_CACHE_CAP: usize = 512;
/// Entries older than this are evicted unconditionally.
const DEBOUNCE_STALE_SECS: u64 = 5;
/// Events for the same key within this window are coalesced (size accumulated).
const DEBOUNCE_WINDOW_SECS: i64 = 1;

struct DebounceCache {
    map: HashMap<String, (TradeEvent, Instant)>,
}

impl DebounceCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Evict all entries older than `DEBOUNCE_STALE_SECS`.
    fn purge_stale(&mut self) {
        self.map
            .retain(|_, (_, inserted)| inserted.elapsed().as_secs() < DEBOUNCE_STALE_SECS);
    }

    /// Insert or accumulate an event.  Returns `true` if the event was debounced
    /// (accumulated into an existing entry) so the caller should `continue`.
    fn insert_or_accumulate(&mut self, key: String, event: TradeEvent) -> bool {
        // Evict stale entries before checking capacity
        self.purge_stale();

        // Enforce capacity ceiling — evict oldest entry if at cap
        if self.map.len() >= DEBOUNCE_CACHE_CAP && !self.map.contains_key(&key) {
            // Remove the entry with the oldest insertion time
            if let Some(oldest_key) = self
                .map
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest_key);
            }
        }

        if let Some((existing, inserted)) = self.map.get_mut(&key) {
            let age_secs = chrono::Utc::now().timestamp() - existing.timestamp;
            if age_secs < DEBOUNCE_WINDOW_SECS {
                // Accumulate size — still within the debounce window
                existing.size += event.size;
                debug!(
                    "Debounced fragmented fill for {}. New size: {}",
                    existing.token_id, existing.size
                );
                return true; // caller should `continue`
            } else {
                // Window expired — replace with fresh event
                *existing = event;
                *inserted = Instant::now();
                return false;
            }
        }

        self.map.insert(key, (event, Instant::now()));
        false
    }
}

// ---------------------------------------------------------------------------
// Live query cache (Gap 13)
// ---------------------------------------------------------------------------

/// Cache TTL in seconds for `holds_query` results.  Fresh enough that a trade
/// arriving 3s after a prior one for the same wallet re-uses the cached result,
/// but stale enough that a fast-moving market doesn't use a 10s-old snapshot.
const LIVE_QUERY_CACHE_TTL_SECS: u64 = 3;

struct LiveQueryCache {
    inner: HashMap<String, (bool, Instant)>,
}

impl LiveQueryCache {
    fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    fn get(&self, wallet: &str, token_id: &str) -> Option<bool> {
        let key = format!("{wallet}:{token_id}");
        self.inner.get(&key).and_then(|(result, inserted)| {
            if inserted.elapsed().as_secs() < LIVE_QUERY_CACHE_TTL_SECS {
                Some(*result)
            } else {
                None
            }
        })
    }

    fn set(&mut self, wallet: &str, token_id: &str, holds: bool) {
        let key = format!("{wallet}:{token_id}");
        self.inner.insert(key, (holds, Instant::now()));
    }

    /// Evict expired entries to keep memory bounded.
    fn evict_expired(&mut self) {
        self.inner
            .retain(|_, (_, t)| t.elapsed().as_secs() < LIVE_QUERY_CACHE_TTL_SECS * 4);
    }
}

// ---------------------------------------------------------------------------
// Local state cache (optimization: avoid redundant API calls)
// ---------------------------------------------------------------------------

/// Cached position state for a wallet+token pair.
/// Avoids repeated API calls within the configured TTL window.
struct LocalStateCache {
    /// Maps "wallet:token_id" → (holds: bool, inserted_at: Instant)
    inner: HashMap<String, (bool, Instant)>,
    /// TTL in seconds (from config.trading.local_cache_ttl_secs).
    ttl_secs: u64,
}

impl LocalStateCache {
    fn new(ttl_secs: u64) -> Self {
        Self {
            inner: HashMap::new(),
            ttl_secs,
        }
    }

    fn get(&self, wallet: &str, token_id: &str) -> Option<bool> {
        let key = format!("{wallet}:{token_id}");
        self.inner.get(&key).and_then(|(holds, inserted)| {
            if inserted.elapsed().as_secs() < self.ttl_secs.max(1) {
                Some(*holds)
            } else {
                None
            }
        })
    }

    fn set(&mut self, wallet: &str, token_id: &str, holds: bool) {
        let key = format!("{wallet}:{token_id}");
        self.inner.insert(key, (holds, Instant::now()));
    }

    fn evict_expired(&mut self) {
        let ttl = self.ttl_secs.max(1) * 4;
        self.inner.retain(|_, (_, t)| t.elapsed().as_secs() < ttl);
    }
}

// ---------------------------------------------------------------------------
// Preload cache (optimization: pre-warm target wallet state)
// ---------------------------------------------------------------------------

/// Tracks which wallets we've recently preloaded for a given token,
/// so we don't redundantly preload the same wallet within the cooldown.
struct PreloadTracker {
    /// Maps "wallet:token_id" → Instant of last preload
    inner: HashMap<String, Instant>,
}

impl PreloadTracker {
    fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Returns true if this wallet+token was preloaded recently (< 10s).
    fn was_recently_preloaded(&self, wallet: &str, token_id: &str) -> bool {
        let key = format!("{wallet}:{token_id}");
        self.inner
            .get(&key)
            .map(|t| t.elapsed().as_secs() < 10)
            .unwrap_or(false)
    }

    fn mark_preloaded(&mut self, wallet: &str, token_id: &str) {
        let key = format!("{wallet}:{token_id}");
        self.inner.insert(key, Instant::now());
    }
}

// ---------------------------------------------------------------------------
// Token ownership strategy
// ---------------------------------------------------------------------------

/// Token ownership strategy determines which wallet "owns" a token when
/// multiple targets hold it simultaneously.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenOwnershipStrategy {
    /// First wallet to BUY owns the token (original behavior).
    FirstCome,
    /// Highest win-rate wallet owns the token.
    WinRatePriority,
    /// Average across all wallets holding the token.
    MultiWalletAverage,
    /// Only copy from whitelisted wallets (those with a :weight suffix in config).
    WhitelistOnly,
}

impl TokenOwnershipStrategy {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "win_rate_priority" => Self::WinRatePriority,
            "multi_wallet_average" => Self::MultiWalletAverage,
            "whitelist_only" => Self::WhitelistOnly,
            _ => Self::FirstCome,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FirstCome => "first_come",
            Self::WinRatePriority => "win_rate_priority",
            Self::MultiWalletAverage => "multi_wallet_average",
            Self::WhitelistOnly => "whitelist_only",
        }
    }
}

// ---------------------------------------------------------------------------
// Injectable live position checker
// ---------------------------------------------------------------------------

/// Returns `Some(true)` if `wallet` holds `token_id`, `Some(false)` if not,
/// `None` on error or timeout.
///
/// Injected via [`start_strategy_engine`] so tests can provide a no-op
/// without touching the real network.
pub type HoldsQuery =
    Arc<dyn Fn(String, String) -> Pin<Box<dyn Future<Output = Option<bool>> + Send>> + Send + Sync>;

/// Production implementation — makes a live Polymarket Data API call with a
/// 5-second timeout.
pub fn make_live_holds_query() -> HoldsQuery {
    Arc::new(|wallet: String, token_id: String| {
        Box::pin(async move {
            let client = DataClient::default();
            let Ok(addr) = Address::from_str(&wallet) else {
                return None;
            };
            let req = PositionsRequest::builder().user(addr).build();
            match tokio::time::timeout(Duration::from_secs(5), client.positions(&req)).await {
                Ok(Ok(positions)) => {
                    Some(positions.iter().any(|p| p.asset.to_string() == token_id))
                }
                Ok(Err(e)) => {
                    warn!(
                        "Live position query failed for {}: {e}",
                        &wallet[..wallet.len().min(10)]
                    );
                    None
                }
                Err(_) => {
                    warn!(
                        "Live position query timed out for {}",
                        &wallet[..wallet.len().min(10)]
                    );
                    None
                }
            }
        })
    })
}

/// No-op implementation for use in integration tests.  Returns `None`
/// immediately, triggering the scanner-cache fallback in the engine.
pub fn make_no_op_holds_query() -> HoldsQuery {
    Arc::new(|_wallet: String, _token_id: String| Box::pin(async { None::<bool> }))
}

// ---------------------------------------------------------------------------
// Injectable market end-date checker
// ---------------------------------------------------------------------------

pub type EndDateQuery = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<chrono::DateTime<chrono::Utc>>> + Send>>
        + Send
        + Sync,
>;

pub fn make_live_end_date_query() -> EndDateQuery {
    Arc::new(|token_id: String| {
        Box::pin(async move {
            let client = polymarket_client_sdk::gamma::Client::default();
            let Ok(u) = polymarket_client_sdk::types::U256::from_str(&token_id) else {
                return None;
            };
            let req = polymarket_client_sdk::gamma::types::request::MarketsRequest::builder()
                .clob_token_ids(vec![u])
                .build();
            match tokio::time::timeout(Duration::from_secs(5), client.markets(&req)).await {
                Ok(Ok(markets)) => markets.into_iter().next().and_then(|m| m.end_date),
                Ok(Err(e)) => {
                    warn!("Live end_date query failed for {}: {}", &token_id[..10], e);
                    None
                }
                Err(_) => {
                    warn!("Live end_date query timed out for {}", &token_id[..10]);
                    None
                }
            }
        })
    })
}

pub fn make_no_op_end_date_query() -> EndDateQuery {
    Arc::new(|_token_id: String| Box::pin(async { None }))
}

// ---------------------------------------------------------------------------
// End Date Cache
// ---------------------------------------------------------------------------

struct EndDateCache {
    inner: HashMap<String, chrono::DateTime<chrono::Utc>>,
}

impl EndDateCache {
    fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    async fn get_or_fetch(
        &mut self,
        token_id: &str,
        query: &EndDateQuery,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        if let Some(ed) = self.inner.get(token_id) {
            return Some(*ed);
        }
        if let Some(ed) = query(token_id.to_string()).await {
            self.inner.insert(token_id.to_string(), ed);
            return Some(ed);
        }
        None
    }
}

#[allow(clippy::too_many_arguments)]
pub fn start_strategy_engine(
    mut rx: mpsc::Receiver<TradeEvent>,
    state: Arc<RwLock<BotState>>,
    mut risk_engine: RiskEngine,
    submitter: OrderSubmitter,
    config: Config,
    copy_ledger: Arc<Mutex<CopyLedger>>,
    holds_query: HoldsQuery,
    end_date_query: EndDateQuery,
    sl_state: Arc<Mutex<crate::stop_loss::StopLossState>>,
) {
    tokio::spawn(async move {
        info!("Strategy Engine Started. Monitoring edge cases (debouncing, closures...)");

        // Initialize wallet weights in BotState from config.target_scalars
        {
            let mut guard = state.write().await;
            for wallet in &config.target_wallets {
                let weight = config
                    .target_scalars
                    .get(wallet)
                    .cloned()
                    .unwrap_or(Decimal::ONE);
                let stats = guard.wallet_stats.entry(wallet.clone()).or_default();
                stats.weight = weight;
            }
        }

        let mut debounce = DebounceCache::new();
        let mut live_cache = LiveQueryCache::new();
        let mut end_date_cache = EndDateCache::new();
        // Wash-trade filter: detects coordinated same-address / same-token patterns
        let mut wash_filter = wash_trade_filter::WashTradeFilter::new();
        // Periodic cache maintenance counter
        let mut event_count: u32 = 0;

        // --- Optimization: local state cache (1.5s TTL, configurable) ---
        let mut local_cache = LocalStateCache::new(config.local_cache_ttl_secs);
        // --- Optimization: preload tracker ---
        let mut preload_tracker = PreloadTracker::new();
        // --- Optimization: token ownership strategy ---
        let ownership_strategy = TokenOwnershipStrategy::parse(&config.token_ownership_strategy);
        info!(
            "Token ownership strategy: {} | partial_close={} | local_cache_ttl={}s | api_degrade={}s",
            ownership_strategy.as_str(),
            config.enable_partial_close,
            config.local_cache_ttl_secs,
            config.api_timeout_degrade_secs,
        );
        // --- Optimization: API degradation state ---
        let mut api_degraded: bool = false;

        while let Some(event) = rx.recv().await {
            event_count += 1;
            // Evict expired live-query cache entries every 50 events (Gap 13)
            if event_count.is_multiple_of(50) {
                live_cache.evict_expired();
                local_cache.evict_expired();
            }

            let mut eval = EvaluatedTrade {
                original_event: event.clone(),
                validated: true,
                reason: None,
            };

            // 1. Is it a filled trade from the target wallet list?
            //    Also: Optimization — whitelist_only strategy blocks non-whitelisted wallets.
            if !config.target_wallets.contains(&event.taker_address) {
                eval.validated = false;
                eval.reason = Some("Wallet mismatch".to_string());
            } else if ownership_strategy == TokenOwnershipStrategy::WhitelistOnly {
                // whitelist_only: only copy from wallets that have a :weight suffix
                if !config.target_scalars.contains_key(&event.taker_address) {
                    eval.validated = false;
                    eval.reason =
                        Some("Wallet not in whitelist (whitelist_only strategy)".to_string());
                }
            }

            // --- Optimization: pre-warm target wallet state ---
            // When we see a trade from a target, preload their position state
            // so the subsequent live verification can use cached data.
            if eval.validated
                && !preload_tracker.was_recently_preloaded(&event.taker_address, &event.token_id)
            {
                let _ = holds_query(event.taker_address.clone(), event.token_id.clone()).await;
                preload_tracker.mark_preloaded(&event.taker_address, &event.token_id);
            }

            // 2. Fragmented fill debounce (Gap 5 — bounded cache with TTL eviction)
            let cache_key = format!(
                "{}_{}_{:?}",
                event.taker_address, event.token_id, event.side
            );
            if eval.validated && debounce.insert_or_accumulate(cache_key, event.clone()) {
                continue; // accumulated — skip this event
            }

            // 3. Risk bounds
            if eval.validated {
                if let Err(reason) = risk_engine.check_trade(&event) {
                    eval.validated = false;
                    eval.reason = Some(reason);
                }
            }

            // 4. Market mute check — skip if this market is muted
            if eval.validated {
                let is_muted = {
                    let guard = state.read().await;
                    guard.is_market_muted(&event.token_id)
                };
                if is_muted {
                    eval.validated = false;
                    eval.reason = Some("Market is muted".to_string());
                }
            }

            // 5. AI freeze check — block BUY entries when frozen (from /ai/freeze)
            if eval.validated && event.side == TradeSide::BUY {
                let is_frozen = {
                    let guard = state.read().await;
                    guard.is_frozen()
                };
                if is_frozen {
                    eval.validated = false;
                    eval.reason = Some("Trading is frozen (AI freeze active)".to_string());
                }
            }

            // 6. Wallet blacklist check — skip if wallet is auto-blacklisted
            if eval.validated {
                let is_blacklisted = {
                    let guard = state.read().await;
                    guard.is_wallet_blacklisted(&event.taker_address)
                };
                if is_blacklisted {
                    eval.validated = false;
                    eval.reason = Some("Wallet is blacklisted (poor performance)".to_string());
                }
            }

            // 7. Daily loss circuit-breaker — block all BUYs if triggered
            if eval.validated && event.side == TradeSide::BUY {
                let (loss_triggered, _max_daily_loss_pct) = {
                    let guard = state.read().await;
                    (guard.daily_loss_triggered, config.max_daily_loss_pct)
                };
                if loss_triggered {
                    eval.validated = false;
                    eval.reason =
                        Some("Daily loss circuit-breaker triggered — BUYs blocked".to_string());
                }
            }

            // 8. Market category blacklist — skip if market title matches blacklisted category
            if eval.validated
                && event.side == TradeSide::BUY
                && !config.category_blacklist.is_empty()
            {
                let title = {
                    let guard = state.read().await;
                    guard
                        .target_positions
                        .iter()
                        .find(|p| p.token_id == event.token_id)
                        .map(|p| p.title.to_lowercase())
                        .unwrap_or_default()
                };
                let blocked = config
                    .category_blacklist
                    .iter()
                    .any(|cat| title.contains(&cat.to_lowercase()));
                if blocked {
                    eval.validated = false;
                    eval.reason = Some("Market category is blacklisted".to_string());
                }
            }

            // -- Intent classification: live API + copy ledger ------------------
            //
            // Rules:
            //   ONE-POSITION-PER-TOKEN: once we hold a token (from any target),
            //   ignore BUY events from all other targets for that token.
            //
            //   SELL gating: only the target we originally copied from can trigger
            //   a close.  A SELL from a different target is irrelevant to our
            //   position.
            //
            //   LIVE VERIFICATION (Gap 13 — cached): for BUY events we query the
            //   target's wallet live; for SELL events we query OUR wallet live.
            //   Results are cached for LIVE_QUERY_CACHE_TTL_SECS seconds to reduce
            //   API calls and latency for burst activity.

            let mut resolved_end_date = None;

            if eval.validated {
                // Check market closing soon (before resolving holdings to save time if skipped)
                if let Some(skip_mins) = config.ignore_closing_in_mins {
                    let ed = end_date_cache
                        .get_or_fetch(&event.token_id, &end_date_query)
                        .await;
                    resolved_end_date = ed;
                    if let Some(end_date) = ed {
                        let cutoff =
                            chrono::Utc::now() + chrono::Duration::minutes(skip_mins as i64);
                        if end_date <= cutoff && event.side == TradeSide::BUY {
                            eval.validated = false;
                            eval.reason = Some(format!(
                                "Market closes in < {} mins (at {})",
                                skip_mins,
                                end_date.format("%H:%M UTC")
                            ));
                            warn!("Trade skipped: {}", eval.reason.as_ref().unwrap());
                        }
                    }
                }
            }

            if eval.validated {
                // --- Resolve live state for this token ---

                // Our position: check local cache first, then BotState, then live API
                let cache_we_hold = {
                    let guard = state.read().await;
                    guard.positions.contains_key(&event.token_id)
                };
                let cache_target_holds = {
                    let guard = state.read().await;
                    guard.target_positions.iter().any(|p| {
                        p.token_id == event.token_id && p.source_wallet == event.taker_address
                    })
                };

                // --- Optimization: local state cache (avoids BotState locks + API calls) ---
                let local_we_hold = local_cache.get(&config.funder_address, &event.token_id);
                let local_target_holds = local_cache.get(&event.taker_address, &event.token_id);

                // Check live-query cache before making API calls (Gap 13)
                let our_cached = live_cache.get(&config.funder_address, &event.token_id);
                let target_cached = live_cache.get(&event.taker_address, &event.token_id);

                // --- Optimization: smart degradation ---
                // If API has been degraded (too slow), prioritize local data + ledger.
                // Otherwise, run live API queries in parallel.
                let (live_we_hold, live_target_holds) = if api_degraded {
                    // Degraded mode: use local cache + ledger, skip live API
                    let we_hold = local_we_hold.or(our_cached).unwrap_or(cache_we_hold);
                    let target_holds = local_target_holds
                        .or(target_cached)
                        .unwrap_or(cache_target_holds);
                    (we_hold, target_holds)
                } else if local_we_hold.is_some() && local_target_holds.is_some() {
                    // Both available in local cache — no API call needed
                    (
                        local_we_hold.unwrap_or(cache_we_hold),
                        local_target_holds.unwrap_or(cache_target_holds),
                    )
                } else if our_cached.is_some() && target_cached.is_some() {
                    // Both are in live-query cache — no API call needed
                    (
                        our_cached.unwrap_or(cache_we_hold),
                        target_cached.unwrap_or(cache_target_holds),
                    )
                } else {
                    // Run whichever queries are needed in parallel with timeout
                    let api_start = Instant::now();
                    let (live_we_hold_opt, live_target_holds_opt) = tokio::join!(
                        async {
                            if let Some(cached) = our_cached {
                                Some(cached)
                            } else {
                                holds_query(config.funder_address.clone(), event.token_id.clone())
                                    .await
                            }
                        },
                        async {
                            if let Some(cached) = target_cached {
                                Some(cached)
                            } else {
                                holds_query(event.taker_address.clone(), event.token_id.clone())
                                    .await
                            }
                        },
                    );

                    let api_elapsed = api_start.elapsed().as_secs();
                    // --- Optimization: smart degradation check ---
                    if api_elapsed >= config.api_timeout_degrade_secs {
                        if !api_degraded {
                            warn!(
                                "[Degradation] API latency {}s >= threshold {}s — switching to local-ledger mode",
                                api_elapsed, config.api_timeout_degrade_secs
                            );
                            api_degraded = true;
                        }
                    } else if api_degraded {
                        // API recovered — exit degradation mode
                        info!(
                            "[Degradation] API latency recovered ({}s) — exiting degraded mode",
                            api_elapsed
                        );
                        api_degraded = false;
                    }

                    let we_hold = live_we_hold_opt.unwrap_or(cache_we_hold);
                    let target_holds = live_target_holds_opt.unwrap_or(cache_target_holds);

                    // Populate both caches with fresh results
                    live_cache.set(&config.funder_address, &event.token_id, we_hold);
                    live_cache.set(&event.taker_address, &event.token_id, target_holds);
                    local_cache.set(&config.funder_address, &event.token_id, we_hold);
                    local_cache.set(&event.taker_address, &event.token_id, target_holds);

                    (we_hold, target_holds)
                };

                // Ledger lookup for this token (who we copied it from, if anyone).
                let ledger_entry = {
                    let ledger = copy_ledger.lock().await;
                    ledger.find_active_for_token(&event.token_id).cloned()
                };
                let already_in_token = ledger_entry.is_some();

                // --- Token ownership strategy ---
                // When we already hold a token from wallet A and wallet B also buys:
                // - first_come: skip (original behavior)
                // - win_rate_priority: if wallet B has higher win rate, transfer ownership
                // - multi_wallet_average: allow multiple wallets, average sizing
                // - whitelist_only: already filtered above, treat as first_come here
                let skip_reason: Option<String> = match event.side {
                    // ---- BUY -----------------------------------------------
                    TradeSide::BUY => {
                        if already_in_token {
                            let from = ledger_entry
                                .as_ref()
                                .map(|e| &e.source_wallet[..e.source_wallet.len().min(10)])
                                .unwrap_or("unknown");
                            match ownership_strategy {
                                TokenOwnershipStrategy::WinRatePriority => {
                                    // Check if this wallet has a better win rate
                                    let current_wallet =
                                        ledger_entry.as_ref().map(|e| e.source_wallet.clone());
                                    let should_transfer = if let Some(ref cur_wallet) =
                                        current_wallet
                                    {
                                        let guard = state.read().await;
                                        let cur_stats = guard.wallet_stats.get(cur_wallet);
                                        let new_stats =
                                            guard.wallet_stats.get(&event.taker_address);
                                        match (cur_stats, new_stats) {
                                            (Some(cs), Some(ns)) => {
                                                let cur_wr = if cs.wins + cs.losses > 0 {
                                                    cs.wins as f64 / (cs.wins + cs.losses) as f64
                                                } else {
                                                    0.5
                                                };
                                                let new_wr = if ns.wins + ns.losses > 0 {
                                                    ns.wins as f64 / (ns.wins + ns.losses) as f64
                                                } else {
                                                    0.5
                                                };
                                                new_wr > cur_wr
                                            }
                                            (None, Some(_)) => true, // new wallet has stats, current doesn't
                                            _ => false,
                                        }
                                    } else {
                                        false
                                    };
                                    if should_transfer {
                                        info!(
                                            "[Ownership] Transferring token {} from {} to {} (win_rate_priority)",
                                            &event.token_id[..event.token_id.len().min(12)],
                                            from,
                                            &event.taker_address[..event.taker_address.len().min(10)],
                                        );
                                        // Transfer ownership: update the ledger entry's source_wallet
                                        {
                                            let mut ledger = copy_ledger.lock().await;
                                            if let Some(entry) =
                                                ledger.entries.iter_mut().rev().find(|e| {
                                                    !e.closed && e.token_id == event.token_id
                                                })
                                            {
                                                entry.source_wallet = event.taker_address.clone();
                                                ledger.save();
                                            }
                                        }
                                        None // Allow: ownership transferred
                                    } else {
                                        Some(format!(
                                            "BUY skipped: already holding token {} (entered from {}, win_rate_priority: current is better)",
                                            &event.token_id[..event.token_id.len().min(12)],
                                            from
                                        ))
                                    }
                                }
                                TokenOwnershipStrategy::MultiWalletAverage => {
                                    // Allow multiple wallets to hold the same token
                                    // Don't skip — we'll just track the new source
                                    info!(
                                        "[Ownership] Multi-wallet: token {} also held by {} (multi_wallet_average)",
                                        &event.token_id[..event.token_id.len().min(12)],
                                        &event.taker_address[..event.taker_address.len().min(10)],
                                    );
                                    None // Allow: multi-wallet
                                }
                                _ => {
                                    // first_come / whitelist_only: original behavior — skip
                                    Some(format!(
                                        "BUY skipped: already holding token {} (entered from {})",
                                        &event.token_id[..event.token_id.len().min(12)],
                                        from
                                    ))
                                }
                            }
                        } else if !live_target_holds && live_we_hold {
                            Some(
                                "BUY skipped: we hold long but target has no position \
                                 (likely closing their short)"
                                    .to_string(),
                            )
                        } else {
                            None // Fresh long entry → copy
                        }
                    }
                    // ---- SELL ----------------------------------------------
                    TradeSide::SELL => {
                        // --- Optimization: partial close support ---
                        // If enable_partial_close and the target only partially reduced,
                        // we reduce our position proportionally instead of closing entirely.
                        if live_we_hold {
                            match &ledger_entry {
                                Some(entry) if entry.source_wallet == event.taker_address => {
                                    // Correct source is selling — check for partial close
                                    let mut is_partial_close = false;
                                    if config.enable_partial_close {
                                        let our_held_size = {
                                            let guard = state.read().await;
                                            guard
                                                .positions
                                                .get(&event.token_id)
                                                .map(|p| p.size)
                                                .unwrap_or(Decimal::ZERO)
                                        };
                                        // Target's current size in this token (from target_positions)
                                        let target_current_size = {
                                            let guard = state.read().await;
                                            guard
                                                .target_positions
                                                .iter()
                                                .find(|p| {
                                                    p.token_id == event.token_id
                                                        && p.source_wallet == event.taker_address
                                                })
                                                .map(|p| p.size)
                                                .unwrap_or(Decimal::ZERO)
                                        };
                                        // If target still holds shares after the SELL, this is a partial reduction
                                        if target_current_size > Decimal::ZERO
                                            && our_held_size > Decimal::ZERO
                                        {
                                            let total_target_before =
                                                target_current_size + event.size;
                                            if total_target_before > Decimal::ZERO {
                                                let reduction_ratio =
                                                    event.size / total_target_before;
                                                let our_reduction =
                                                    (our_held_size * reduction_ratio).round_dp(2);
                                                if our_reduction >= Decimal::from(5)
                                                    && our_reduction < our_held_size
                                                {
                                                    is_partial_close = true;
                                                    // Partial close: reduce our position proportionally
                                                    info!(
                                                        "[PartialClose] Token {} reducing by {:.2} shares ({:.1}% of our {:.2}), target reduced {:.1}%",
                                                        &event.token_id[..event.token_id.len().min(12)],
                                                        our_reduction,
                                                        reduction_ratio * dec!(100),
                                                        our_held_size,
                                                        reduction_ratio * dec!(100),
                                                    );
                                                    // Submit partial sell order
                                                    let partial_limit_price = calculate_limit_price(
                                                        event.price,
                                                        TradeSide::SELL,
                                                        config.max_slippage_pct,
                                                    );
                                                    let partial_order = OrderRequest {
                                                        token_id: event.token_id.clone(),
                                                        price: partial_limit_price,
                                                        size: our_reduction,
                                                        side: TradeSide::SELL,
                                                    };
                                                    let partial_submitter = submitter.clone();
                                                    let partial_token_id = event.token_id.clone();
                                                    let partial_source =
                                                        event.taker_address.clone();
                                                    let partial_ledger = copy_ledger.clone();
                                                    let partial_state = state.clone();
                                                    let partial_sl = sl_state.clone();
                                                    let partial_entry_price = entry.entry_price;
                                                    tokio::spawn(async move {
                                                        match partial_submitter(partial_order).await
                                                        {
                                                            Ok(()) => {
                                                                info!(
                                                                    "[PartialClose] Sold {:.2} shares of {}",
                                                                    our_reduction,
                                                                    &partial_token_id[..partial_token_id.len().min(12)],
                                                                );
                                                                // Don't close ledger entry — keep it open for future partials
                                                                // Update the filled_size in the ledger
                                                                {
                                                                    let mut ledger =
                                                                        partial_ledger.lock().await;
                                                                    ledger.update_fill(
                                                                        &partial_token_id,
                                                                        our_held_size
                                                                            - our_reduction,
                                                                    );
                                                                }
                                                                // Update SL state: remove and re-record with adjusted position
                                                                {
                                                                    let mut sl_guard =
                                                                        partial_sl.lock().await;
                                                                    sl_guard
                                                                        .remove(&partial_token_id);
                                                                    // wallet_sync will re-record if needed
                                                                }
                                                                // Record realized PnL for the partial close
                                                                let partial_pnl = (event.price
                                                                    - partial_entry_price)
                                                                    * our_reduction;
                                                                let mut guard =
                                                                    partial_state.write().await;
                                                                guard.realized_pnl += partial_pnl;
                                                                if partial_pnl >= Decimal::ZERO {
                                                                    guard.record_win(
                                                                        &partial_source,
                                                                        partial_pnl,
                                                                    );
                                                                } else {
                                                                    guard.record_loss(
                                                                        &partial_source,
                                                                        partial_pnl,
                                                                    );
                                                                }
                                                            }
                                                            Err(e) => {
                                                                warn!(
                                                                    "[PartialClose] Failed to sell partial of {}: {}",
                                                                    &partial_token_id[..partial_token_id.len().min(12)],
                                                                    e
                                                                );
                                                            }
                                                        }
                                                    });
                                                }
                                            }
                                        }
                                    }
                                    if is_partial_close {
                                        // Partial close handled — skip the normal full-close flow
                                        Some("Partial close executed".to_string())
                                    } else {
                                        None // Full close → proceed normally
                                    }
                                }
                                Some(entry) => Some(format!(
                                    "SELL skipped: {} sold but we copied from {} — \
                                     keeping position",
                                    &event.taker_address[..event.taker_address.len().min(10)],
                                    &entry.source_wallet[..entry.source_wallet.len().min(10)],
                                )),
                                None => {
                                    warn!(
                                        "SELL: we hold token {} with no ledger entry — \
                                         closing defensively.",
                                        &event.token_id[..event.token_id.len().min(12)]
                                    );
                                    None
                                }
                            }
                        } else if let Some(entry) = &ledger_entry {
                            warn!(
                                "SELL: ledger shows active copy of {} from {} but we no longer \
                                 hold it — marking closed.",
                                &event.token_id[..event.token_id.len().min(12)],
                                &entry.source_wallet[..entry.source_wallet.len().min(10)],
                            );
                            let mut ledger = copy_ledger.lock().await;
                            ledger.record_close(&event.token_id, &entry.source_wallet.clone());
                            Some(
                                "SELL skipped: position already closed (ledger synced)".to_string(),
                            )
                        } else {
                            if live_target_holds {
                                Some(
                                    "SELL skipped: target closing long we never entered"
                                        .to_string(),
                                )
                            } else {
                                Some(
                                    "SELL skipped: target opening short (not supported)"
                                        .to_string(),
                                )
                            }
                        }
                    }
                };

                if let Some(reason) = skip_reason {
                    warn!("{}", reason);
                    eval.validated = false;
                    eval.reason = Some(reason);
                }
            }

            // Update TUI feed (single push, correct validated state)
            {
                let mut guard = state.write().await;
                guard.push_evaluated_trade(eval.clone());
            }

            if eval.validated {
                info!("Trade Validated: {:?}", eval.original_event);

                let is_closing = event.side == TradeSide::SELL;

                // Determine limit price: rounded to 2dp (CLOB tick), capped to [0.01, 0.99]
                let limit_price =
                    calculate_limit_price(event.price, event.side, config.max_slippage_pct);

                let order: Option<(OrderRequest, Decimal)> = if is_closing {
                    // -- SELL: close our position using our 100% held size --
                    // (Polymarket handles the CTF fee by deducting from the USDC payout,
                    // we no longer reduce the share count to avoid "dust" shares).
                    let our_held_size = {
                        let guard = state.read().await;
                        guard
                            .positions
                            .get(&event.token_id)
                            .map(|p| p.size)
                            .unwrap_or(Decimal::ZERO)
                    };

                    let truncated_size = our_held_size.trunc_with_scale(2);
                    if truncated_size <= rust_decimal::Decimal::ZERO {
                        tracing::warn!("Dust fractional balance {:.4} truncates to 0.00 — skipping limit order logic and delegating to Gasless Relayer.", our_held_size);
                        None
                    } else {
                        Some((
                            OrderRequest {
                                token_id: event.token_id.clone(),
                                price: limit_price,
                                size: truncated_size,
                                side: event.side,
                            },
                            Decimal::ZERO,
                        ))
                    }
                } else {
                    // -- BUY: size according to active SizingMode, capped and $5 floored --

                    // 6. Slippage guard: reject if spread > 2.5% or depth < $2000
                    let guard_limit_price =
                        slippage_guard::limit_price(event.price, TradeSide::BUY);
                    if let Err(reason) =
                        slippage_guard::check_spread(event.price, guard_limit_price)
                    {
                        warn!(
                            "BUY skipped (slippage): {} for token {}",
                            reason,
                            &event.token_id[..event.token_id.len().min(12)]
                        );
                        eval.validated = false;
                        eval.reason = Some(format!("Slippage guard: {}", reason));
                    } else if let Err(reason) =
                        slippage_guard::check_depth(&event.token_id, TradeSide::BUY)
                    {
                        warn!(
                            "BUY skipped (depth): {} for token {}",
                            reason,
                            &event.token_id[..event.token_id.len().min(12)]
                        );
                        eval.validated = false;
                        eval.reason = Some(format!("Depth guard: {}", reason));
                    }

                    // 7. Wash-trade filter: reject if same address ≥3 trades in 60s
                    if eval.validated && wash_filter.is_wash_trade(&event.taker_address, &event) {
                        warn!(
                            "BUY skipped (wash): wash trade detected for address {} on token {}",
                            &event.taker_address[..event.taker_address.len().min(10)],
                            &event.token_id[..event.token_id.len().min(12)]
                        );
                        wash_filter.record(&event.taker_address, &event);
                        eval.validated = false;
                        eval.reason = Some("Wash trade detected".to_string());
                    } else {
                        wash_filter.record(&event.taker_address, &event);
                    }

                    if !eval.validated {
                        // Push feed and skip sizing
                        let mut g = state.write().await;
                        g.push_evaluated_trade(eval.clone());
                        continue;
                    }

                    let current_balance = {
                        let guard = state.read().await;
                        guard.total_balance
                    };
                    // target_notional = what the target just bet in dollar terms
                    let target_notional = event.size * event.price;
                    let wallet_scalar = config
                        .target_scalars
                        .get(&event.taker_address)
                        .cloned()
                        .unwrap_or(Decimal::ONE);
                    let budget_usd = compute_order_usd(
                        current_balance,
                        &config.sizing_mode,
                        config.copy_size_pct,
                        wallet_scalar,
                        config.max_trade_size_usd,
                        target_notional,
                    );
                    // Budget exactly reflects the sizing engine's mathematical intent.
                    // (TargetUSD mirrors natively, Scalars scale natively, Fixed/SelfPct override natively)
                    let raw_size = budget_usd / event.price;
                    let buy_size = raw_size.round_dp(2); // CLOB requires 2dp lot size

                    // CLOB hard minimum: 5 shares. Orders below this always 400.
                    if buy_size < MIN_ORDER_SHARES {
                        warn!(
                            "BUY skipped: computed {:.2} shares is below CLOB minimum of {} shares \
                             (budget=${:.2} at price ${:.3}). Increase COPY_SIZE_PCT or wait for higher balance.",
                            buy_size, MIN_ORDER_SHARES, budget_usd, limit_price
                        );
                        None
                    } else {
                        // Pre-check balance taking the maximum CTF fee overhead into account
                        // fee = C * feeRate * p * (1 - p). We assume a max 200bps (0.02) feeRate to be safe.
                        let p = limit_price;
                        let max_ctf_fee =
                            buy_size * Decimal::from_str("0.02").unwrap() * p * (Decimal::ONE - p);
                        let order_cost = buy_size * limit_price;
                        let total_cost = order_cost + max_ctf_fee;

                        if current_balance < total_cost {
                            warn!(
                                "Insufficient balance (have ${:.2}, need ${:.2} including fee) -- skipping entry",
                                current_balance, total_cost
                            );
                            None
                        } else {
                            // Check whether we already have a pending GTC order for this token.
                            let already_pending = {
                                let guard = state.read().await;
                                guard.pending_orders.contains_key(&event.token_id)
                            };
                            if already_pending {
                                warn!(
                                    "BUY skipped: live GTC order already exists for token {}",
                                    event.token_id
                                );
                                None
                            } else {
                                Some((
                                    OrderRequest {
                                        token_id: event.token_id.clone(),
                                        price: limit_price,
                                        size: buy_size,
                                        side: event.side,
                                    },
                                    total_cost,
                                ))
                            }
                        }
                    }
                };

                if let Some((order, cost_to_deduct)) = order {
                    // Register token as pending BEFORE spawning so any concurrent
                    // events for the same token are blocked immediately.
                    {
                        let mut guard = state.write().await;
                        guard.pending_orders.insert(
                            order.token_id.clone(),
                            crate::models::QueuedOrder {
                                token_id: order.token_id.clone(),
                                price: order.price,
                                size: order.size,
                                side: order.side,
                                event_end_date: resolved_end_date,
                            },
                        );

                        // Eagerly secure the margin requirement from our balance!
                        // This prevents rapid-fire sequential trades from overlapping
                        // against the same stale balance and throwing CLOB 400 bounds!
                        if cost_to_deduct > Decimal::ZERO {
                            guard.total_balance -= cost_to_deduct;
                        }
                    }

                    let submitter_clone = submitter.clone();
                    let state_clone = state.clone();
                    let token_id_clone = order.token_id.clone();
                    let source_wallet_clone = event.taker_address.clone();
                    let is_closing = event.side == TradeSide::SELL;
                    let order_size = order.size;
                    let order_price = order.price;
                    let ledger_clone = copy_ledger.clone();

                    // Capture entry_price from ledger BEFORE spawning for realized PnL.
                    let entry_price = if is_closing {
                        let ledger = copy_ledger.lock().await;
                        ledger
                            .find_active_for_token(&event.token_id)
                            .map(|e| e.entry_price)
                    } else {
                        None
                    };

                    let is_sim = config.is_sim;
                    let sl_state_clone = sl_state.clone();

                    tokio::spawn(async move {
                        match submitter_clone(order).await {
                            Ok(()) => {
                                let mut ledger = ledger_clone.lock().await;
                                if is_closing {
                                    ledger.record_close(&token_id_clone, &source_wallet_clone);
                                    info!(
                                        "Ledger: closed {} from {}",
                                        &token_id_clone[..token_id_clone.len().min(12)],
                                        &source_wallet_clone[..source_wallet_clone.len().min(10)],
                                    );
                                    // Accumulate realized PnL
                                    if let Some(avg_entry) = entry_price {
                                        let pnl = (order_price - avg_entry) * order_size;
                                        let mut guard = state_clone.write().await;
                                        guard.realized_pnl += pnl;
                                        // Subtract this token's contribution from unrealized;
                                        // price refresh corrects the full sum within 20s.
                                        let old_unrealized = (order_price - avg_entry) * order_size;
                                        guard.unrealized_pnl -= old_unrealized;
                                        // Update AI wallet stats
                                        if pnl >= Decimal::ZERO {
                                            guard.record_win(&source_wallet_clone, pnl);
                                        } else {
                                            guard.record_loss(&source_wallet_clone, pnl);
                                        }
                                    }
                                } else {
                                    ledger.record_copy(
                                        token_id_clone.clone(),
                                        source_wallet_clone.clone(),
                                        order_size,
                                        order_price,
                                    );
                                    info!(
                                        "Ledger: recorded copy of {} from {}",
                                        &token_id_clone[..token_id_clone.len().min(12)],
                                        &source_wallet_clone[..source_wallet_clone.len().min(10)],
                                    );
                                    // Track entry price for stop-loss / take-profit monitor
                                    sl_state_clone
                                        .lock()
                                        .await
                                        .record_entry(token_id_clone.clone(), order_price);
                                }

                                if is_sim {
                                    let mut guard = state_clone.write().await;
                                    let fee_rate = rust_decimal::Decimal::from_str("0.02").unwrap();
                                    let max_ctf_fee = order_size
                                        * fee_rate
                                        * order_price
                                        * (rust_decimal::Decimal::ONE - order_price);

                                    if is_closing {
                                        guard.positions.remove(&token_id_clone);
                                        guard.total_balance +=
                                            (order_size * order_price) - max_ctf_fee;
                                    } else {
                                        // Auto-fill mock position
                                        guard.positions.insert(
                                            token_id_clone.clone(),
                                            crate::models::Position {
                                                token_id: token_id_clone.clone(),
                                                size: order_size,
                                                average_entry_price: order_price,
                                            },
                                        );
                                        // We purposefully do NOT deduct the cost here!
                                        // It was already eagerly deducted right before `tokio::spawn`!
                                    }
                                    // Remove from pending logic to match real world
                                    guard.pending_orders.remove(&token_id_clone);
                                }
                            }
                            Err(e) => {
                                // Remove from pending on failure so the order can be retried.
                                let mut guard = state_clone.write().await;
                                guard.pending_orders.remove(&token_id_clone);

                                // RESTORE the unused eagerly-deducted margin cap!
                                if cost_to_deduct > Decimal::ZERO {
                                    guard.total_balance += cost_to_deduct;
                                }

                                tracing::error!("Execution failed: {}", e);
                            }
                        }
                    });
                }
            } else {
                warn!("Skipped trade: {}", eval.reason.unwrap_or_default());
            }
        }
    });
}
