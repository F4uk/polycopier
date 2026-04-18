//! Wash trading filter module
//!
//! Detects and filters wash trading patterns — artificial trading activity
//! designed to manipulate market prices or create false volume.
//!
//! Detection rule:
//!   If the **same wallet address** trades the **same outcome (token)**
//!   more than 3 times within 60 seconds, the trade is flagged as wash trading.

use crate::models::TradeEvent;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Maximum trades from one address on one outcome within the window.
pub const WASH_THRESHOLD: usize = 3;

/// Sliding time window in seconds.
pub const WASH_WINDOW_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Internal record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct TradeRecord {
    timestamp: Instant,
}

// ---------------------------------------------------------------------------
// Filter state
// ---------------------------------------------------------------------------

/// Thread-safe wash-trade filter. Wrap in `Arc<Mutex<>>` for shared use.
pub struct WashTradeFilter {
    /// Maps `(address_lowercase, token_id)` → list of trade timestamps.
    trades: HashMap<(String, String), Vec<TradeRecord>>,
}

impl WashTradeFilter {
    pub fn new() -> Self {
        Self {
            trades: HashMap::new(),
        }
    }

    /// Check whether the given trade from `address` would be flagged as wash trading.
    ///
    /// Returns `true` if the trade is a wash trade (i.e. ≥ WASH_THRESHOLD recent
    /// trades from the same address on the same outcome within WASH_WINDOW_SECS).
    pub fn is_wash_trade(&self, address: &str, event: &TradeEvent) -> bool {
        let key = (address.to_lowercase(), event.token_id.clone());
        let Some(records) = self.trades.get(&key) else {
            return false;
        };

        let now = Instant::now();
        let window = Duration::from_secs(WASH_WINDOW_SECS);
        let recent_count = records
            .iter()
            .filter(|r| now.duration_since(r.timestamp) <= window)
            .count();

        // If we already have ≥ WASH_THRESHOLD recent trades, the next one is wash.
        recent_count >= WASH_THRESHOLD
    }

    /// Record a trade so future calls to `is_wash_trade` can detect patterns.
    pub fn record(&mut self, address: &str, event: &TradeEvent) {
        let key = (address.to_lowercase(), event.token_id.clone());
        self.trades.entry(key).or_default().push(TradeRecord {
            timestamp: Instant::now(),
        });
    }

    /// Evict stale entries to keep memory bounded.
    pub fn cleanup(&mut self) {
        let now = Instant::now();
        let window = Duration::from_secs(WASH_WINDOW_SECS * 2);
        self.trades.retain(|_, records| {
            records.retain(|r| now.duration_since(r.timestamp) <= window);
            !records.is_empty()
        });
    }
}

impl Default for WashTradeFilter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TradeSide;
    use rust_decimal_macros::dec;

    fn make_event(token_id: &str) -> TradeEvent {
        TradeEvent {
            transaction_hash: format!(
                "0x{:016x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64
            ),
            maker_address: "0xMaker".to_string(),
            taker_address: "0xTaker".to_string(),
            token_id: token_id.to_string(),
            price: dec!(0.5),
            size: dec!(10),
            side: TradeSide::BUY,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    #[test]
    fn test_below_threshold() {
        let mut f = WashTradeFilter::new();
        let addr = "0xABCD";
        let ev = make_event("token1");
        for _ in 0..WASH_THRESHOLD - 1 {
            assert!(!f.is_wash_trade(addr, &ev));
            f.record(addr, &ev);
        }
    }

    #[test]
    fn test_at_threshold() {
        let mut f = WashTradeFilter::new();
        let addr = "0xEF01";
        let ev = make_event("token2");
        for _ in 0..WASH_THRESHOLD {
            f.record(addr, &ev);
        }
        assert!(f.is_wash_trade(addr, &ev));
    }

    #[test]
    fn test_different_tokens_no_cross() {
        let mut f = WashTradeFilter::new();
        let addr = "0x2222";
        let ev_a = make_event("aaa");
        let ev_b = make_event("bbb");
        for _ in 0..WASH_THRESHOLD {
            f.record(addr, &ev_a);
        }
        // Different token — should NOT flag
        assert!(!f.is_wash_trade(addr, &ev_b));
    }

    #[test]
    fn test_cleanup() {
        let mut f = WashTradeFilter::new();
        let addr = "0x3333";
        let ev = make_event("token3");
        for _ in 0..5 {
            f.record(addr, &ev);
        }
        f.cleanup(); // should not panic; entries are fresh so retained
        assert!(f.is_wash_trade(addr, &ev));
    }
}
