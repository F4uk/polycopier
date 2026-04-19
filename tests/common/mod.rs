//! Shared helpers for all integration test files.
//! Each test binary includes this with `mod common;`.
#![allow(dead_code)]

use polycopier::config::Config;
use polycopier::models::{
    EvaluatedTrade, Position, ScanStatus, SizingMode, TargetPosition, TradeEvent, TradeSide,
};

use polycopier::state::BotState;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashSet;

/// Default test configuration. Uses `SizingMode::Fixed` with $10 cap so sizing tests
/// are deterministic without a live balance. Override fields as needed per test.
pub fn test_config() -> Config {
    Config {
        private_key: "0xdeadbeef".to_string(),
        funder_address: "0x1111111111111111111111111111111111111111".to_string(),
        chain_id: 137,
        target_wallets: vec!["0xabc".to_string()],
        target_scalars: std::collections::HashMap::new(),
        max_slippage_pct: dec!(0.02),
        max_trade_size_usd: dec!(10.00),
        max_delay_seconds: 2,
        max_copy_loss_pct: dec!(0.40),
        max_copy_gain_pct: dec!(0.05),
        min_entry_price: dec!(0.02),
        max_entry_price: dec!(0.999),
        sizing_mode: SizingMode::Fixed,
        copy_size_pct: None,
        scan_max_entries_per_cycle: 1,
        sell_fee_buffer: dec!(0.97),
        ledger_retention_days: 90,
        ignore_closing_in_mins: None,
        max_daily_volume_usd: dec!(0),
        max_consecutive_losses: 0,
        loss_cooldown_secs: 300,
        stop_loss_enabled: false,
        force_stop_price: dec!(0),
        force_close_price: dec!(0),
        stop_loss_check_interval_secs: 60,
        max_daily_loss_pct: dec!(0),
        max_single_loss_usd: dec!(0),
        wallet_blacklist_consecutive_losses: 0,
        wallet_blacklist_min_win_rate: dec!(0),
        category_blacklist: vec![],
        min_hours_to_expiry: dec!(0),
        min_volume_1h_usd: dec!(0),
        max_spread_pct: dec!(0.025),
        min_holders: 0,
        telegram_bot_token: String::new(),
        telegram_chat_id: String::new(),
        telegram_min_pnl_usd: dec!(0),
        token_ownership_strategy: "first_come".to_string(),
        enable_partial_close: true,
        local_cache_ttl_secs: 3,
        api_timeout_degrade_secs: 5,
        is_sim: false,
        sim_balance: None,
    }
}

pub fn make_trade(taker: &str, price: Decimal, size: Decimal, side: TradeSide) -> TradeEvent {
    make_trade_for_token(taker, "99999", price, size, side)
}

pub fn make_trade_for_token(
    taker: &str,
    token_id: &str,
    price: Decimal,
    size: Decimal,
    side: TradeSide,
) -> TradeEvent {
    TradeEvent {
        transaction_hash: "0xtest".to_string(),
        maker_address: taker.to_string(),
        taker_address: taker.to_string(),
        token_id: token_id.to_string(),
        price,
        size,
        side,
        timestamp: chrono::Utc::now().timestamp(),
    }
}

pub fn make_eval(validated: bool) -> EvaluatedTrade {
    EvaluatedTrade {
        original_event: make_trade("0xabc", dec!(0.50), dec!(10), TradeSide::BUY),
        validated,
        reason: if validated {
            None
        } else {
            Some("test skip".to_string())
        },
    }
}

pub fn empty_set() -> HashSet<String> {
    HashSet::new()
}

pub fn token_set(tokens: &[&str]) -> HashSet<String> {
    tokens.iter().map(|s| s.to_string()).collect()
}

pub fn target_pos(token_id: &str) -> TargetPosition {
    TargetPosition {
        title: "Test Market".to_string(),
        outcome: "YES".to_string(),
        token_id: token_id.to_string(),
        cur_price: dec!(0.50),
        avg_price: dec!(0.45),
        percent_pnl: dec!(0.10),
        size: dec!(20),
        status: ScanStatus::Monitoring,
        source_wallet: "0xtest..wall".to_string(),
    }
}

pub fn make_position(token_id: &str, size: Decimal) -> (String, Position) {
    (
        token_id.to_string(),
        Position {
            token_id: token_id.to_string(),
            size,
            average_entry_price: dec!(0.50),
        },
    )
}

/// Seeded BotState with total_balance set, for tests that require balance pre-check.
pub fn state_with_balance(balance: Decimal) -> BotState {
    let mut s = BotState::new(false, None);
    s.total_balance = balance;
    s
}
