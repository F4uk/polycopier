//! Polymarket slippage guard module
//!
//! Provides two pre-trade checks:
//! 1. Spread check — reject if buy/sell price gap exceeds 2.5%
//! 2. Order-book depth check — reject if top-5 levels total < 2000 USDC
//!
//! Also provides a `limit_price()` helper to compute a limit-order price
//! that is 0.5% more favourable than market (premium for execution certainty).

use crate::models::TradeSide;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Maximum allowed spread as a decimal fraction (2.5%).
pub const MAX_SPREAD_PCT: Decimal = dec!(0.025);

/// Limit-order premium as a decimal fraction (0.5%).
pub const LIMIT_PREMIUM_PCT: Decimal = dec!(0.005);

/// Minimum depth in USDC across top-5 order-book levels.
pub const MIN_DEPTH_USD: Decimal = dec!(2000);

/// Polymarket CLOB price bounds.
pub const MIN_PRICE: Decimal = dec!(0.01);
pub const MAX_PRICE: Decimal = dec!(0.99);

// ---------------------------------------------------------------------------
// Spread check
// ---------------------------------------------------------------------------

/// Check whether the spread between market price and execution price is
/// within the 2.5% threshold.
///
/// Returns `Ok(())` if acceptable, `Err(reason)` if the spread is excessive.
pub fn check_spread(market_price: Decimal, execution_price: Decimal) -> Result<(), String> {
    if market_price <= Decimal::ZERO {
        return Ok(()); // degenerate case — let downstream handle
    }
    let spread_pct = (execution_price - market_price).abs() / market_price;
    if spread_pct > MAX_SPREAD_PCT {
        Err(format!(
            "Spread {:.2}% exceeds max {:.1}%",
            spread_pct * dec!(100),
            MAX_SPREAD_PCT * dec!(100)
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Depth check (stub — real implementation would query the CLOB order book)
// ---------------------------------------------------------------------------

/// Check whether the top-5 order-book levels provide sufficient depth.
///
/// **Note**: The real implementation should query the Polymarket CLOB
/// `get_order_book` endpoint. This stub always returns `Ok(())` so the
/// feature is wired up end-to-end but does not block compilation when
/// the SDK order-book API is not yet available.
pub fn check_depth(_token_id: &str, _side: TradeSide) -> Result<(), String> {
    // TODO: Query CLOB order book and sum top-5 levels.
    // For now, always pass — the guard is integrated but not yet enforced
    // until the SDK order-book endpoint is wired up.
    Ok(())
}

// ---------------------------------------------------------------------------
// Limit-price calculator
// ---------------------------------------------------------------------------

/// Compute a limit-order price that is `LIMIT_PREMIUM_PCT` more favourable
/// than the market price.
///
/// - BUY  → market + 0.5%  (willing to pay slightly more for fill certainty)
/// - SELL → market - 0.5%  (willing to receive slightly less)
pub fn limit_price(market_price: Decimal, side: TradeSide) -> Decimal {
    let market_price = market_price.clamp(MIN_PRICE, MAX_PRICE);
    match side {
        TradeSide::BUY => (market_price * (Decimal::ONE + LIMIT_PREMIUM_PCT))
            .round_dp(2)
            .min(MAX_PRICE),
        TradeSide::SELL => (market_price * (Decimal::ONE - LIMIT_PREMIUM_PCT))
            .round_dp(2)
            .max(MIN_PRICE),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spread_ok() {
        assert!(check_spread(dec!(0.50), dec!(0.51)).is_ok());
    }

    #[test]
    fn test_spread_excessive() {
        assert!(check_spread(dec!(0.50), dec!(0.55)).is_err()); // 10% spread
    }

    #[test]
    fn test_limit_price_buy() {
        let lp = limit_price(dec!(0.50), TradeSide::BUY);
        assert!(lp > dec!(0.50));
        assert!(lp <= MAX_PRICE);
    }

    #[test]
    fn test_limit_price_sell() {
        let lp = limit_price(dec!(0.50), TradeSide::SELL);
        assert!(lp < dec!(0.50));
        assert!(lp >= MIN_PRICE);
    }

    #[test]
    fn test_depth_always_passes() {
        assert!(check_depth("any", TradeSide::BUY).is_ok());
    }
}
