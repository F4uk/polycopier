//! Configuration loading — two-file design:
//!
//! | File            | Contains                          | Version-controlled? |
//! |-----------------|-----------------------------------|---------------------|
//! | `.env`          | Secrets: `PRIVATE_KEY`, `FUNDER_ADDRESS` only | **No** |
//! | `config.toml`   | All tunables + `[targets]` (wallet addresses) | **Yes** |
//!
//! ## Loading order
//!
//! 1. `.env` is read via `dotenvy` for secrets and any legacy tunable keys.
//! 2. `config.toml` is read for tunables; it takes precedence over `.env` for
//!    shared keys so the split is a non-breaking migration.
//! 3. If `config.toml` does not exist, it is generated from defaults (and from
//!    any legacy tunable values already in `.env`) so existing setups continue
//!    to work without manual intervention.
//! 4. Interactive prompts are only shown for secrets that are still missing or
//!    look like placeholders.

use inquire::{Password, Text};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;

// Re-export SizingMode so callers can import it via `polycopier::config::SizingMode`
pub use crate::models::SizingMode;

// ---------------------------------------------------------------------------
// TOML-serialisable tunables structure
// ---------------------------------------------------------------------------

/// All non-secret tunables + target wallet list.
/// Written to / read from `config.toml`.
/// Secrets (PRIVATE_KEY, FUNDER_ADDRESS) stay in `.env`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    pub targets: TargetsConfig,
    pub execution: ExecutionConfig,
    pub sizing: SizingConfig,
    pub scanner: ScannerConfig,
    pub risk: RiskConfig,
    pub ledger: LedgerConfig,
    pub stop_loss: StopLossConfig,
    pub risk_guard: RiskGuardConfig,
    pub market_filter: MarketFilterConfig,
    pub telegram: TelegramConfig,
    pub trading: TradingConfig,
    pub risk_by_category: RiskByCategoryConfig,
}

/// Copy-trade target wallets — public on-chain addresses, safe in config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetsConfig {
    /// Polymarket proxy wallet addresses to copy-trade.
    pub wallets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Slippage buffer applied to copied trade price for limit orders (0.02 = 2%).
    pub max_slippage_pct: Decimal,
    /// Hard ceiling per copied trade regardless of sizing mode.
    pub max_trade_size_usd: Decimal,
    /// Discard listener events older than this many seconds (staleness filter).
    pub max_delay_seconds: i64,
    /// SELL size = held_size × sell_fee_buffer. Absorbs CLOB fee. Default 0.97.
    pub sell_fee_buffer: Decimal,
    /// Skip entries and cancel pending orders for markets that close within this many minutes.
    pub ignore_closing_in_mins: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizingConfig {
    /// Sizing algorithm: "self_pct" | "target_usd" | "fixed".
    pub mode: String,
    /// Fraction of our balance per trade for self_pct mode (e.g. 0.15 = 15%).
    pub copy_size_pct: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerConfig {
    /// Skip catch-up if target is already this % underwater (0.40 = 40%).
    pub max_copy_loss_pct: Decimal,
    /// Skip catch-up if target is already this % in profit (0.05 = 5%).
    pub max_copy_gain_pct: Decimal,
    /// Minimum token price for catch-up entries (filters near-zero dust).
    pub min_entry_price: Decimal,
    /// Maximum token price for catch-up entries.
    pub max_entry_price: Decimal,
    /// Max positions queued per scan cycle (default 1 = conservative).
    pub max_entries_per_cycle: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Max USD traded per UTC day (BUY + SELL). 0 = disabled.
    pub max_daily_volume_usd: Decimal,
    /// Consecutive losses before cooldown pause. 0 = disabled.
    pub max_consecutive_losses: u32,
    /// Seconds to pause after hitting max_consecutive_losses. Default 300.
    pub loss_cooldown_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerConfig {
    /// Days to keep closed ledger entries. 0 = never prune.
    pub retention_days: u32,
}

/// Dynamic tiered stop-loss / take-profit monitoring config.
/// Parameters are auto-selected by entry price tier (Polymarket binary options).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopLossConfig {
    /// Enable local stop-loss / take-profit monitoring.
    pub enabled: bool,
    /// Force stop-loss: if price drops below this, exit immediately (e.g. 0.15).
    pub force_stop_price: Decimal,
    /// Force take-profit: if price rises above this, exit immediately (e.g. 0.95).
    pub force_close_price: Decimal,
    /// How often (seconds) to check price levels. Default 3.
    pub check_interval_secs: u64,
}

/// Enhanced risk guard config: daily loss circuit-breaker + per-trade cap + wallet blacklist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskGuardConfig {
    /// Max daily loss as fraction of starting balance (0.15 = 15%). Triggers auto-close + freeze.
    pub max_daily_loss_pct: Decimal,
    /// Max absolute USD loss per single trade before force-sell. 0 = disabled.
    pub max_single_loss_usd: Decimal,
    /// Consecutive losses from one wallet before auto-blacklisting. 0 = disabled.
    pub wallet_blacklist_consecutive_losses: u32,
    /// Win rate below this fraction triggers wallet blacklisting. 0 = disabled.
    pub wallet_blacklist_min_win_rate: Decimal,
}

/// Market quality filter config: auto-skip low-quality / irrelevant markets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketFilterConfig {
    /// Market categories to always skip (e.g. ["tennis", "sports", "esports", "gaming", "ball"]).
    pub category_blacklist: Vec<String>,
    /// Skip markets ending within this many hours. 0 = disabled.
    pub min_hours_to_expiry: Decimal,
    /// Min 1h volume in USD. Markets below are skipped. 0 = disabled.
    pub min_volume_1h_usd: Decimal,
    /// Max bid-ask spread (0.02 = 2%). Markets above are skipped. 0 = disabled.
    pub max_spread_pct: Decimal,
    /// Min number of holders. Markets below are skipped. 0 = disabled.
    pub min_holders: u32,
}

/// Telegram notification config (optional).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Bot token from @BotFather. Empty = disabled.
    pub bot_token: String,
    /// Chat ID to send messages to. Empty = disabled.
    pub chat_id: String,
    /// Minimum PnL event to trigger a notification (in USD). 0 = all events.
    pub min_pnl_usd: Decimal,
}

/// Per-category position limit config.
/// Stored as: HashMap<category_name, max_position_usd>
/// Category names match Polymarket API (e.g. "politics.us-election").
/// 0.0 = completely disabled (no entries allowed).
/// Missing categories fall back to the default_limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskByCategoryConfig {
    /// Enable per-category position limits.
    pub enabled: bool,
    /// Per-category max position sizes (USDC). HashMap key = category slug.
    pub limits: std::collections::HashMap<String, Decimal>,
    /// Default max position size for categories not in `limits`.
    pub default_limit: Decimal,
}

/// Trading strategy config: token ownership, partial close, and API latency tuning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    /// Token ownership strategy: "first_come" | "win_rate_priority" | "multi_wallet_average" | "whitelist_only".
    /// - first_come: first wallet to BUY owns the token (original behavior).
    /// - win_rate_priority: highest win-rate wallet owns the token.
    /// - multi_wallet_average: average across all wallets holding the token.
    /// - whitelist_only: only copy from whitelisted wallets (target_scalars > 0).
    pub token_ownership_strategy: String,
    /// Enable partial close support: when target partially reduces, reduce our position proportionally.
    pub enable_partial_close: bool,
    /// Local state cache TTL in seconds for our and target wallet positions.
    pub local_cache_ttl_secs: u64,
    /// API timeout threshold in seconds for smart degradation.
    /// When exceeded, prioritize local ledger decisions over live API calls.
    pub api_timeout_degrade_secs: u64,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            targets: TargetsConfig { wallets: vec![] },
            execution: ExecutionConfig {
                max_slippage_pct: Decimal::from_str("0.02").unwrap(),
                max_trade_size_usd: Decimal::from_str("10.00").unwrap(),
                max_delay_seconds: 10,
                sell_fee_buffer: Decimal::from_str("0.97").unwrap(),
                ignore_closing_in_mins: Some(15),
            },
            sizing: SizingConfig {
                mode: "self_pct".to_string(),
                copy_size_pct: Some(Decimal::from_str("0.15").unwrap()),
            },
            scanner: ScannerConfig {
                max_copy_loss_pct: Decimal::from_str("0.40").unwrap(),
                max_copy_gain_pct: Decimal::from_str("0.05").unwrap(),
                min_entry_price: Decimal::from_str("0.02").unwrap(),
                max_entry_price: Decimal::from_str("0.999").unwrap(),
                max_entries_per_cycle: 1,
            },
            risk: RiskConfig {
                max_daily_volume_usd: Decimal::ZERO,
                max_consecutive_losses: 0,
                loss_cooldown_secs: 300,
            },
            ledger: LedgerConfig { retention_days: 90 },
            stop_loss: StopLossConfig {
                enabled: true,
                force_stop_price: Decimal::from_str("0.15").unwrap(),
                force_close_price: Decimal::from_str("0.95").unwrap(),
                check_interval_secs: 3,
            },
            risk_guard: RiskGuardConfig {
                max_daily_loss_pct: Decimal::from_str("0.15").unwrap(),
                max_single_loss_usd: Decimal::from_str("5.0").unwrap(),
                wallet_blacklist_consecutive_losses: 3,
                wallet_blacklist_min_win_rate: Decimal::from_str("0.40").unwrap(),
            },
            market_filter: MarketFilterConfig {
                category_blacklist: vec![
                    "tennis".into(),
                    "sports".into(),
                    "esports".into(),
                    "gaming".into(),
                    "ball".into(),
                ],
                min_hours_to_expiry: Decimal::from_str("24").unwrap(),
                min_volume_1h_usd: Decimal::from_str("1000").unwrap(),
                max_spread_pct: Decimal::from_str("0.02").unwrap(),
                min_holders: 50,
            },
            telegram: TelegramConfig {
                bot_token: String::new(),
                chat_id: String::new(),
                min_pnl_usd: Decimal::ZERO,
            },
            trading: TradingConfig {
                token_ownership_strategy: "first_come".to_string(),
                enable_partial_close: true,
                local_cache_ttl_secs: 3,
                api_timeout_degrade_secs: 3,
            },
            risk_by_category: RiskByCategoryConfig {
                enabled: false,
                limits: std::collections::HashMap::new(),
                default_limit: Decimal::from(20),
            },
        }
    }
}

const CONFIG_TOML_PATH: &str = "config.toml";

// ---------------------------------------------------------------------------
// Flat runnable Config (what the rest of the code sees)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Config {
    // Secrets (from .env — never logged or committed)
    pub private_key: String,
    pub funder_address: String,
    pub chain_id: u64,

    // Target wallets (from config.toml [targets].wallets)
    pub target_wallets: Vec<String>,
    pub target_scalars: std::collections::HashMap<String, Decimal>,

    // Tunables (from config.toml)
    pub max_slippage_pct: Decimal,
    pub max_trade_size_usd: Decimal,
    pub max_delay_seconds: i64,
    pub ignore_closing_in_mins: Option<u64>,
    pub max_copy_loss_pct: Decimal,
    pub max_copy_gain_pct: Decimal,
    pub min_entry_price: Decimal,
    pub max_entry_price: Decimal,
    pub sizing_mode: SizingMode,
    pub copy_size_pct: Option<Decimal>,
    pub scan_max_entries_per_cycle: usize,
    pub sell_fee_buffer: Decimal,
    pub ledger_retention_days: u32,
    pub max_daily_volume_usd: Decimal,
    pub max_consecutive_losses: u32,
    pub loss_cooldown_secs: u64,
    // Stop-loss / take-profit tunables (dynamic tiered)
    pub stop_loss_enabled: bool,
    pub force_stop_price: Decimal,
    pub force_close_price: Decimal,
    pub stop_loss_check_interval_secs: u64,
    // Risk guard tunables
    pub max_daily_loss_pct: Decimal,
    pub max_single_loss_usd: Decimal,
    pub wallet_blacklist_consecutive_losses: u32,
    pub wallet_blacklist_min_win_rate: Decimal,
    // Market filter tunables
    pub category_blacklist: Vec<String>,
    pub min_hours_to_expiry: Decimal,
    pub min_volume_1h_usd: Decimal,
    pub max_spread_pct: Decimal,
    pub min_holders: u32,
    // Telegram
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
    pub telegram_min_pnl_usd: Decimal,
    // Trading strategy
    pub token_ownership_strategy: String,
    pub enable_partial_close: bool,
    pub local_cache_ttl_secs: u64,
    pub api_timeout_degrade_secs: u64,
    // Risk by category
    pub risk_by_category_enabled: bool,
    pub risk_by_category_limits: std::collections::HashMap<String, Decimal>,
    pub risk_by_category_default: Decimal,
    pub is_sim: bool,
    pub sim_balance: Option<rust_decimal::Decimal>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns true if a config value looks like a placeholder that hasn't been filled in.
pub fn is_placeholder(val: &str) -> bool {
    let v = val.trim().trim_matches('"');
    v.is_empty()
        || v == "."
        || v.starts_with("your-")
        || v.starts_with("0xYour")
        || v.starts_with("0xTarget")
        || v.contains("here")
}

/// Returns true if the value looks like a valid 32-byte EVM private key (64 hex chars,
/// optionally prefixed with "0x"). Used to catch test/placeholder keys before the
/// alloy SDK gives an opaque "invalid string length" error.
pub fn is_valid_private_key_format(val: &str) -> bool {
    let v = val.trim().trim_matches('"');
    let hex = v.strip_prefix("0x").unwrap_or(v);
    hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// Load `config.toml` if it exists. Returns `None` on missing or parse error.
fn load_toml() -> Option<BotConfig> {
    let raw = fs::read_to_string(CONFIG_TOML_PATH).ok()?;
    match toml::from_str::<BotConfig>(&raw) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("config.toml parse error — using defaults: {e}");
            None
        }
    }
}

/// Format a `Vec<String>` as a TOML inline array string literal.
fn format_toml_list(items: &[String]) -> String {
    if items.is_empty() {
        "[]".to_string()
    } else {
        let quoted: Vec<String> = items.iter().map(|s| format!("\"{s}\"")).collect();
        format!("[{}]", quoted.join(", "))
    }
}

/// Write `BotConfig` to `config.toml` with inline comments.
pub fn write_toml(cfg: &BotConfig) -> anyhow::Result<()> {
    // Format the wallets as a TOML inline array.
    let wallets_toml = if cfg.targets.wallets.is_empty() {
        "[]".to_string()
    } else {
        let quoted: Vec<String> = cfg
            .targets
            .wallets
            .iter()
            .map(|w| format!("\"{w}\""))
            .collect();
        format!("[{}]", quoted.join(", "))
    };

    let content = format!(
        r#"# polycopier config -- safe to version control (no secrets here)
# Secrets (PRIVATE_KEY, FUNDER_ADDRESS) stay in .env

[targets]
# Polymarket proxy wallet addresses to copy-trade
wallets = {wallets}

[execution]
# Slippage buffer applied to copied trade price for limit orders (2% = 0.02)
max_slippage_pct = {slippage}
# Hard ceiling per copied trade in USD
max_trade_size_usd = {max_trade}
# Drop listener events older than N seconds (staleness filter)
max_delay_seconds = {delay}
# SELL size = held_size x sell_fee_buffer (absorbs CLOB fee, default 0.97)
sell_fee_buffer = {fee_buf}
# Skip markets closing in less than X minutes (e.g. 15)
{ignore_closing}

[sizing]
# Sizing algorithm: "self_pct" | "target_usd" | "fixed"
mode = "{mode}"
# Fraction of our balance per trade for self_pct mode (0.15 = 15%)
{copy_size_line}

[scanner]
# Skip catch-up if target already this % underwater (0.40 = 40%)
max_copy_loss_pct = {loss_pct}
# Skip catch-up if target already this % in profit (0.05 = 5%)
max_copy_gain_pct = {gain_pct}
# Minimum token price for catch-up entries (filters near-zero dust)
min_entry_price = {min_price}
# Maximum token price for catch-up entries
max_entry_price = {max_price}
# Max positions queued per scan cycle (1 = conservative, raise to 2-3 for bulk)
max_entries_per_cycle = {max_entries}

[risk]
# Max USD traded per UTC day (BUY + SELL combined). 0 = disabled.
max_daily_volume_usd = {daily_vol}
# Consecutive losses before triggering a cooldown pause. 0 = disabled.
max_consecutive_losses = {consec_loss}
# Seconds to pause after hitting max_consecutive_losses
loss_cooldown_secs = {cooldown}

[ledger]
# Days to keep closed ledger entries before pruning on startup. 0 = never prune.
retention_days = {retention}

[stop_loss]
# Enable local stop-loss / take-profit monitoring
enabled = {sl_enabled}
# Force stop-loss: exit immediately if price drops below this (0.15 = $0.15)
force_stop_price = {force_stop}
# Force take-profit: exit immediately if price rises above this (0.95 = $0.95)
force_close_price = {force_close}
# How often (seconds) to check price levels
check_interval_secs = {sl_interval}

[risk_guard]
# Max daily loss as fraction of starting balance (0.15 = 15%). Triggers auto-close + freeze.
max_daily_loss_pct = {rg_daily_loss}
# Max absolute USD loss per single trade. 0 = disabled.
max_single_loss_usd = {rg_single_loss}
# Consecutive losses from one wallet before auto-blacklisting. 0 = disabled.
wallet_blacklist_consecutive_losses = {rg_consec}
# Win rate below this fraction triggers wallet blacklisting. 0 = disabled.
wallet_blacklist_min_win_rate = {rg_wr}

[market_filter]
# Market categories to always skip (e.g. tennis, sports, esports, gaming, ball)
category_blacklist = {cat_bl}
# Skip markets ending within this many hours. 0 = disabled.
min_hours_to_expiry = {mf_expiry}
# Min 1h volume in USD. Markets below are skipped. 0 = disabled.
min_volume_1h_usd = {mf_vol}
# Max bid-ask spread (0.02 = 2%). Markets above are skipped. 0 = disabled.
max_spread_pct = {mf_spread}
# Min number of holders. Markets below are skipped. 0 = disabled.
min_holders = {mf_holders}

[telegram]
# Bot token from @BotFather. Empty = disabled.
bot_token = "{tg_token}"
# Chat ID to send messages to. Empty = disabled.
chat_id = "{tg_chat}"
# Minimum PnL event to trigger notification (USD). 0 = all events.
min_pnl_usd = {tg_pnl}

[trading]
# Token ownership strategy: first_come | win_rate_priority | multi_wallet_average | whitelist_only
#   first_come         -- first wallet to BUY owns the token (original behavior)
#   win_rate_priority  -- highest win-rate wallet owns the token
#   multi_wallet_average -- average across all wallets holding the token
#   whitelist_only     -- only copy from whitelisted wallets (target_scalars > 0)
token_ownership_strategy = "{tos}"
# Enable partial close: when target partially reduces, reduce our position proportionally
enable_partial_close = {epc}
# Local state cache TTL in seconds for our and target wallet positions
local_cache_ttl_secs = {cache_ttl}
# API timeout threshold in seconds for smart degradation (fallback to local ledger)
api_timeout_degrade_secs = {api_degrade}

[risk_by_category]
# Enable per-category position limits. When enabled, each market category
# (e.g. "politics.us-election", "economics.fed") has its own max position cap.
enabled = {rbc_enabled}
# Default max position per category (USDC). Unlisted categories use this.
# Set to 0 to completely disable a category.
default_limit = {rbc_default}
# Example per-category limits (uncomment to use):
# "politics.us-election" = 20.0
# "politics.congress" = 15.0
# "economics.fed" = 10.0
# "sports.tennis" = 0.0  # 0 = fully disabled"#,
        wallets = wallets_toml,
        slippage = cfg.execution.max_slippage_pct,
        max_trade = cfg.execution.max_trade_size_usd,
        delay = cfg.execution.max_delay_seconds,
        fee_buf = cfg.execution.sell_fee_buffer,
        ignore_closing = match cfg.execution.ignore_closing_in_mins {
            Some(m) => format!("ignore_closing_in_mins = {}", m),
            None => "# ignore_closing_in_mins = 15".to_string(),
        },
        mode = cfg.sizing.mode,
        copy_size_line = match cfg.sizing.copy_size_pct {
            Some(p) => format!("copy_size_pct = {p}"),
            None => "# copy_size_pct = 0.15  # only used for self_pct mode".to_string(),
        },
        loss_pct = cfg.scanner.max_copy_loss_pct,
        gain_pct = cfg.scanner.max_copy_gain_pct,
        min_price = cfg.scanner.min_entry_price,
        max_price = cfg.scanner.max_entry_price,
        max_entries = cfg.scanner.max_entries_per_cycle,
        daily_vol = cfg.risk.max_daily_volume_usd,
        consec_loss = cfg.risk.max_consecutive_losses,
        cooldown = cfg.risk.loss_cooldown_secs,
        retention = cfg.ledger.retention_days,
        sl_enabled = cfg.stop_loss.enabled,
        force_stop = cfg.stop_loss.force_stop_price,
        force_close = cfg.stop_loss.force_close_price,
        sl_interval = cfg.stop_loss.check_interval_secs,
        rg_daily_loss = cfg.risk_guard.max_daily_loss_pct,
        rg_single_loss = cfg.risk_guard.max_single_loss_usd,
        rg_consec = cfg.risk_guard.wallet_blacklist_consecutive_losses,
        rg_wr = cfg.risk_guard.wallet_blacklist_min_win_rate,
        cat_bl = format_toml_list(&cfg.market_filter.category_blacklist),
        mf_expiry = cfg.market_filter.min_hours_to_expiry,
        mf_vol = cfg.market_filter.min_volume_1h_usd,
        mf_spread = cfg.market_filter.max_spread_pct,
        mf_holders = cfg.market_filter.min_holders,
        tg_token = cfg.telegram.bot_token,
        tg_chat = cfg.telegram.chat_id,
        tg_pnl = cfg.telegram.min_pnl_usd,
        tos = cfg.trading.token_ownership_strategy,
        epc = cfg.trading.enable_partial_close,
        cache_ttl = cfg.trading.local_cache_ttl_secs,
        api_degrade = cfg.trading.api_timeout_degrade_secs,
        rbc_enabled = cfg.risk_by_category.enabled,
        rbc_default = cfg.risk_by_category.default_limit,
    );
    fs::write(CONFIG_TOML_PATH, content)?;
    Ok(())
}

/// Write `.env` with secrets only (PRIVATE_KEY and FUNDER_ADDRESS).
/// TARGET_WALLETS is now in config.toml [targets].wallets.
pub fn write_secrets_env(private_key: &str, funder_address: &str) -> anyhow::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(".env")?;
    writeln!(f, "# polycopier secrets -- DO NOT version control")?;
    writeln!(f, "PRIVATE_KEY=\"{private_key}\"")?;
    writeln!(f, "FUNDER_ADDRESS=\"{funder_address}\"")?;
    Ok(())
}

/// Migrate legacy tunable values from `.env` into a `BotConfig`.
/// When a key is present in `.env`, it overrides the default.
/// This is called when `config.toml` doesn't exist so existing setups migrate seamlessly.
/// TARGET_WALLETS is also migrated to the targets section.
fn migrate_from_env(defaults: BotConfig) -> BotConfig {
    let e = |k: &str| env::var(k).unwrap_or_default();
    let dec = |k: &str, fallback: Decimal| -> Decimal {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    let u32v = |k: &str, fallback: u32| -> u32 {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    let u64v = |k: &str, fallback: u64| -> u64 {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    let i64v = |k: &str, fallback: i64| -> i64 {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    let usizev = |k: &str, fallback: usize| -> usize {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
            .max(1)
    };

    let sizing_mode = e("SIZING_MODE");
    let sizing_mode = if sizing_mode.is_empty() {
        defaults.sizing.mode.clone()
    } else {
        sizing_mode
    };

    let copy_size_pct = env::var("COPY_SIZE_PCT")
        .ok()
        .and_then(|v| v.parse::<Decimal>().ok())
        .filter(|&p| p > Decimal::ZERO && p <= Decimal::ONE)
        .or(defaults.sizing.copy_size_pct);

    // Migrate TARGET_WALLETS from old .env format (comma-separated string) to Vec<String>
    let legacy_wallets: Vec<String> = env::var("TARGET_WALLETS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && !is_placeholder(s))
        .collect();
    let wallets = if legacy_wallets.is_empty() {
        defaults.targets.wallets
    } else {
        legacy_wallets
    };

    BotConfig {
        targets: TargetsConfig { wallets },
        execution: ExecutionConfig {
            max_slippage_pct: dec("MAX_SLIPPAGE_PCT", defaults.execution.max_slippage_pct),
            max_trade_size_usd: dec("MAX_TRADE_SIZE_USD", defaults.execution.max_trade_size_usd),
            max_delay_seconds: i64v("MAX_DELAY_SECONDS", defaults.execution.max_delay_seconds),
            sell_fee_buffer: dec("SELL_FEE_BUFFER", defaults.execution.sell_fee_buffer),
            ignore_closing_in_mins: env::var("IGNORE_CLOSING_IN_MINS")
                .ok()
                .and_then(|v| v.parse().ok())
                .or(defaults.execution.ignore_closing_in_mins),
        },
        sizing: SizingConfig {
            mode: sizing_mode,
            copy_size_pct,
        },
        scanner: ScannerConfig {
            max_copy_loss_pct: dec("MAX_COPY_LOSS_PCT", defaults.scanner.max_copy_loss_pct),
            max_copy_gain_pct: dec("MAX_COPY_GAIN_PCT", defaults.scanner.max_copy_gain_pct),
            min_entry_price: dec("MIN_ENTRY_PRICE", defaults.scanner.min_entry_price),
            max_entry_price: dec("MAX_ENTRY_PRICE", defaults.scanner.max_entry_price),
            max_entries_per_cycle: usizev(
                "SCAN_MAX_ENTRIES_PER_CYCLE",
                defaults.scanner.max_entries_per_cycle,
            ),
        },
        risk: RiskConfig {
            max_daily_volume_usd: dec("MAX_DAILY_VOLUME_USD", defaults.risk.max_daily_volume_usd),
            max_consecutive_losses: u32v(
                "MAX_CONSECUTIVE_LOSSES",
                defaults.risk.max_consecutive_losses,
            ),
            loss_cooldown_secs: u64v("LOSS_COOLDOWN_SECS", defaults.risk.loss_cooldown_secs),
        },
        ledger: LedgerConfig {
            retention_days: u32v("LEDGER_RETENTION_DAYS", defaults.ledger.retention_days),
        },
        stop_loss: StopLossConfig {
            enabled: env::var("STOP_LOSS_ENABLED")
                .ok()
                .and_then(|v| v.parse::<bool>().ok())
                .unwrap_or(defaults.stop_loss.enabled),
            force_stop_price: dec("FORCE_STOP_PRICE", defaults.stop_loss.force_stop_price),
            force_close_price: dec("FORCE_CLOSE_PRICE", defaults.stop_loss.force_close_price),
            check_interval_secs: u64v(
                "STOP_LOSS_CHECK_INTERVAL_SECS",
                defaults.stop_loss.check_interval_secs,
            ),
        },
        risk_guard: RiskGuardConfig {
            max_daily_loss_pct: dec("MAX_DAILY_LOSS_PCT", defaults.risk_guard.max_daily_loss_pct),
            max_single_loss_usd: dec(
                "MAX_SINGLE_LOSS_USD",
                defaults.risk_guard.max_single_loss_usd,
            ),
            wallet_blacklist_consecutive_losses: u32v(
                "WALLET_BLACKLIST_CONSECUTIVE_LOSSES",
                defaults.risk_guard.wallet_blacklist_consecutive_losses,
            ),
            wallet_blacklist_min_win_rate: dec(
                "WALLET_BLACKLIST_MIN_WIN_RATE",
                defaults.risk_guard.wallet_blacklist_min_win_rate,
            ),
        },
        market_filter: MarketFilterConfig {
            category_blacklist: defaults.market_filter.category_blacklist.clone(),
            min_hours_to_expiry: dec(
                "MIN_HOURS_TO_EXPIRY",
                defaults.market_filter.min_hours_to_expiry,
            ),
            min_volume_1h_usd: dec(
                "MIN_VOLUME_1H_USD",
                defaults.market_filter.min_volume_1h_usd,
            ),
            max_spread_pct: dec("MAX_SPREAD_PCT", defaults.market_filter.max_spread_pct),
            min_holders: u32v("MIN_HOLDERS", defaults.market_filter.min_holders),
        },
        telegram: TelegramConfig {
            bot_token: e("TELEGRAM_BOT_TOKEN"),
            chat_id: e("TELEGRAM_CHAT_ID"),
            min_pnl_usd: dec("TELEGRAM_MIN_PNL_USD", defaults.telegram.min_pnl_usd),
        },
        trading: TradingConfig {
            token_ownership_strategy: e("TOKEN_OWNERSHIP_STRATEGY")
                .parse::<String>()
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or(defaults.trading.token_ownership_strategy),
            enable_partial_close: env::var("ENABLE_PARTIAL_CLOSE")
                .ok()
                .and_then(|v| v.parse::<bool>().ok())
                .unwrap_or(defaults.trading.enable_partial_close),
            local_cache_ttl_secs: u64v(
                "LOCAL_CACHE_TTL_SECS",
                defaults.trading.local_cache_ttl_secs,
            ),
            api_timeout_degrade_secs: u64v(
                "API_TIMEOUT_DEGRADE_SECS",
                defaults.trading.api_timeout_degrade_secs,
            ),
        },
        risk_by_category: RiskByCategoryConfig {
            enabled: defaults.risk_by_category.enabled,
            limits: defaults.risk_by_category.limits.clone(),
            default_limit: defaults.risk_by_category.default_limit,
        },
    }
}

// ---------------------------------------------------------------------------
// Config::load_or_prompt — primary entry point
// ---------------------------------------------------------------------------

impl Config {
    pub async fn load_or_prompt(is_ui: bool) -> anyhow::Result<Self> {
        // Load .env (secrets + any legacy tunable keys)
        let _ = dotenvy::dotenv();

        let mut write_new_env = false;

        // -- Secrets: prompt only if missing, placeholder, or invalid format ---
        //
        // A valid EVM private key is exactly 32 bytes = 64 hex chars (+ optional "0x").
        // We re-prompt if the key fails this check so the alloy SDK never sees a
        // short/invalid key and crashes with the opaque "invalid string length" error.

        let funder_address = match env::var("FUNDER_ADDRESS")
            .ok()
            .filter(|v| !is_placeholder(v) && !v.is_empty())
        {
            Some(v) => v,
            None => {
                if is_ui {
                    tracing::info!("Web UI Setup Mode active! Please complete your configuration in the browser at http://localhost:3000");
                    std::future::pending::<()>().await;
                }

                write_new_env = true;
                Text::new("Enter your Polymarket Funder Address (Gnosis Safe / Proxy):")
                    .prompt()
                    .unwrap_or_default()
            }
        };

        let private_key = match env::var("PRIVATE_KEY")
            .ok()
            .filter(|v| !is_placeholder(v) && is_valid_private_key_format(v))
        {
            Some(v) => v,
            None => {
                if is_ui {
                    tracing::info!("Web UI Setup Mode active! Please complete your configuration in the browser at http://localhost:3000");
                    std::future::pending::<()>().await;
                }

                write_new_env = true;
                println!(
                    "PRIVATE_KEY is missing or invalid. A valid key is 64 hex chars (32 bytes)."
                );
                Password::new("Enter your Polymarket Signer Private Key (Hidden):")
                    .without_confirmation()
                    .prompt()
                    .unwrap_or_default()
            }
        };

        // -- Tunables + targets: load from config.toml, or migrate from .env ---

        let (toml_cfg, write_new_toml) = if Path::new(CONFIG_TOML_PATH).exists() {
            // config.toml already exists — use it.
            // But if the targets list is empty, we still need to prompt.
            let cfg = load_toml().unwrap_or_default();
            (cfg, false)
        } else {
            // First run or legacy setup: migrate any .env tunable + TARGET_WALLETS keys.
            let migrated = migrate_from_env(BotConfig::default());

            // If using self_pct and COPY_SIZE_PCT wasn't in env, prompt for it
            let migrated = if migrated.sizing.mode.starts_with("self_pct")
                && migrated.sizing.copy_size_pct.is_none()
            {
                let pct_str =
                    Text::new("Fraction of MY balance to use per trade (e.g. 0.15 = 15%):")
                        .with_default("0.15")
                        .prompt()
                        .unwrap_or_else(|_| "0.15".to_string());
                let copy_size_pct = pct_str
                    .parse::<Decimal>()
                    .ok()
                    .filter(|&p| p > Decimal::ZERO && p <= Decimal::ONE);
                BotConfig {
                    sizing: SizingConfig {
                        copy_size_pct,
                        ..migrated.sizing
                    },
                    ..migrated
                }
            } else {
                migrated
            };

            (migrated, true)
        };

        // -- Prompt for target wallets if the list is empty -------------------
        // (Either first run with no legacy TARGET_WALLETS, or config.toml
        //  was manually created without a [targets] section.)
        let mut prompted_targets = false;
        let toml_cfg = if toml_cfg.targets.wallets.is_empty() && !is_ui {
            prompted_targets = true;
            let raw = Text::new("Enter Target Wallets to copy-trade (comma separated):")
                .prompt()
                .unwrap_or_default();
            let wallets: Vec<String> = raw
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty() && !is_placeholder(s))
                .collect();
            BotConfig {
                targets: TargetsConfig { wallets },
                ..toml_cfg
            }
        } else {
            toml_cfg
        };

        // Write config.toml if it was newly generated / targets were just prompted.
        if write_new_toml || prompted_targets {
            if let Err(e) = write_toml(&toml_cfg) {
                tracing::warn!("Failed to write config.toml: {e}");
            } else if write_new_toml {
                println!("Generated config.toml from current settings.");
            }
        }

        // Write .env (secrets only: PRIVATE_KEY + FUNDER_ADDRESS) if prompted.
        if write_new_env {
            if let Err(e) = write_secrets_env(&private_key, &funder_address) {
                tracing::warn!("Failed to write .env: {e}");
            }
        }

        // -- Validate required fields before handing off ----------------------
        if private_key.trim().is_empty() {
            anyhow::bail!(
                "PRIVATE_KEY is missing.\n\
                 Set it in .env or re-run to be prompted."
            );
        }
        if !is_valid_private_key_format(&private_key) {
            anyhow::bail!(
                "PRIVATE_KEY looks invalid (expected 64 hex chars, got {}).\n\
                 Check .env and re-run.",
                private_key.trim_matches('"').trim_start_matches("0x").len()
            );
        }
        if funder_address.trim().is_empty() {
            anyhow::bail!(
                "FUNDER_ADDRESS is missing.\n\
                 Set it in .env or re-run to be prompted."
            );
        }
        if toml_cfg.targets.wallets.is_empty() && !is_ui {
            anyhow::bail!(
                "No target wallets configured.\n\
                 Add addresses to [targets].wallets in config.toml or re-run to be prompted."
            );
        }

        Ok(Self::from_parts(private_key, funder_address, toml_cfg))
    }

    /// Build a flat [`Config`] from secrets + a [`BotConfig`].
    fn from_parts(private_key: String, funder_address: String, cfg: BotConfig) -> Self {
        let mut target_wallets = Vec::new();
        let mut target_scalars = std::collections::HashMap::new();

        for entry in cfg.targets.wallets.iter() {
            let s = entry.trim().to_lowercase();
            if s.is_empty() {
                continue;
            }
            if let Some((addr, scalar_str)) = s.split_once(':') {
                let clean_addr = addr.trim().to_string();
                target_wallets.push(clean_addr.clone());
                let scalar = rust_decimal::Decimal::from_str(scalar_str.trim())
                    .unwrap_or(rust_decimal::Decimal::ONE);
                target_scalars.insert(clean_addr, scalar);
            } else {
                target_wallets.push(s.clone());
                target_scalars.insert(s, rust_decimal::Decimal::ONE);
            }
        }

        let sizing_mode = SizingMode::from_mode_str(&cfg.sizing.mode);

        Self {
            private_key,
            funder_address,
            chain_id: 137,
            target_wallets,
            target_scalars,
            max_slippage_pct: cfg.execution.max_slippage_pct,
            max_trade_size_usd: cfg.execution.max_trade_size_usd,
            max_delay_seconds: cfg.execution.max_delay_seconds,
            ignore_closing_in_mins: cfg.execution.ignore_closing_in_mins,
            max_copy_loss_pct: cfg.scanner.max_copy_loss_pct,
            max_copy_gain_pct: cfg.scanner.max_copy_gain_pct,
            min_entry_price: cfg.scanner.min_entry_price,
            max_entry_price: cfg.scanner.max_entry_price,
            sizing_mode,
            copy_size_pct: cfg.sizing.copy_size_pct,
            scan_max_entries_per_cycle: cfg.scanner.max_entries_per_cycle,
            sell_fee_buffer: cfg.execution.sell_fee_buffer,
            ledger_retention_days: cfg.ledger.retention_days,
            max_daily_volume_usd: cfg.risk.max_daily_volume_usd,
            max_consecutive_losses: cfg.risk.max_consecutive_losses,
            loss_cooldown_secs: cfg.risk.loss_cooldown_secs,
            stop_loss_enabled: cfg.stop_loss.enabled,
            force_stop_price: cfg.stop_loss.force_stop_price,
            force_close_price: cfg.stop_loss.force_close_price,
            stop_loss_check_interval_secs: cfg.stop_loss.check_interval_secs,
            max_daily_loss_pct: cfg.risk_guard.max_daily_loss_pct,
            max_single_loss_usd: cfg.risk_guard.max_single_loss_usd,
            wallet_blacklist_consecutive_losses: cfg.risk_guard.wallet_blacklist_consecutive_losses,
            wallet_blacklist_min_win_rate: cfg.risk_guard.wallet_blacklist_min_win_rate,
            category_blacklist: cfg.market_filter.category_blacklist,
            min_hours_to_expiry: cfg.market_filter.min_hours_to_expiry,
            min_volume_1h_usd: cfg.market_filter.min_volume_1h_usd,
            max_spread_pct: cfg.market_filter.max_spread_pct,
            min_holders: cfg.market_filter.min_holders,
            telegram_bot_token: cfg.telegram.bot_token,
            telegram_chat_id: cfg.telegram.chat_id,
            telegram_min_pnl_usd: cfg.telegram.min_pnl_usd,
            token_ownership_strategy: cfg.trading.token_ownership_strategy,
            enable_partial_close: cfg.trading.enable_partial_close,
            local_cache_ttl_secs: cfg.trading.local_cache_ttl_secs,
            api_timeout_degrade_secs: cfg.trading.api_timeout_degrade_secs,
            risk_by_category_enabled: cfg.risk_by_category.enabled,
            risk_by_category_limits: cfg.risk_by_category.limits.clone(),
            risk_by_category_default: cfg.risk_by_category.default_limit,
            is_sim: false, // Injected dynamically in Config::load_or_prompt wrapper
            sim_balance: None,
        }
    }

    /// Reload config from disk (called after in-TUI settings save).
    /// Reads secrets from `.env`; target wallets + tunables from `config.toml`.
    pub fn reload() -> anyhow::Result<Self> {
        let _ = dotenvy::dotenv();
        let private_key = env::var("PRIVATE_KEY").unwrap_or_default();
        let funder_address = env::var("FUNDER_ADDRESS").unwrap_or_default();
        let cfg = load_toml().unwrap_or_default();
        Ok(Self::from_parts(private_key, funder_address, cfg))
    }
}

// ────────────────────────────────────────────────────────────────────────
// CLI Parsing Logic (Extracted for Testing)
// ────────────────────────────────────────────────────────────────────────

pub struct CliArgs {
    pub is_daemon: bool,
    pub is_ui: bool,
    pub skip_open: bool,
    pub headless: bool,
    pub is_sim: bool,
    pub sim_balance: Option<Decimal>,
}

pub fn parse_cli_args(args: &[String]) -> CliArgs {
    let is_daemon = args.iter().any(|a| a == "--daemon" || a == "--headless");
    let is_ui = args.iter().any(|a| a == "--ui" || a == "--ui-reboot");
    let skip_open = args.iter().any(|a| a == "--ui-reboot");
    let headless = is_daemon || is_ui;
    let is_sim = args.iter().any(|a| a == "--sim");
    let mut sim_balance = None;
    if let Some(idx) = args.iter().position(|a| a == "--sim-balance") {
        if let Some(val) = args.get(idx + 1) {
            if let Ok(d) = rust_decimal::Decimal::from_str(val) {
                sim_balance = Some(d);
            }
        }
    }

    CliArgs {
        is_daemon,
        is_ui,
        skip_open,
        headless,
        is_sim,
        sim_balance,
    }
}
