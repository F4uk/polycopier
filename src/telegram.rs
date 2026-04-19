//! Telegram notification sender (optional).
//!
//! Sends messages to a Telegram chat via the Bot API.
//! Configured via `[telegram]` in config.toml. Disabled if `bot_token` or `chat_id` is empty.

use crate::config::Config;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

pub struct TelegramNotifier {
    bot_token: String,
    chat_id: String,
    min_pnl_usd: Decimal,
    enabled: bool,
    client: reqwest::Client,
}

impl TelegramNotifier {
    pub fn new(config: &Config) -> Self {
        let enabled = !config.telegram_bot_token.is_empty() && !config.telegram_chat_id.is_empty();
        Self {
            bot_token: config.telegram_bot_token.clone(),
            chat_id: config.telegram_chat_id.clone(),
            min_pnl_usd: config.telegram_min_pnl_usd,
            enabled,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Telegram HTTP client"),
        }
    }

    /// Send a text message to the configured Telegram chat.
    pub async fn send(&self, message: &str) {
        if !self.enabled {
            debug!(
                "[Telegram] Disabled — skipping message: {}",
                &message[..message.len().min(60)]
            );
            return;
        }
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": message,
            "parse_mode": "HTML"
        });

        match self.client.post(&url).json(&body).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    debug!("[Telegram] Message sent successfully");
                } else {
                    error!("[Telegram] API returned status: {}", resp.status());
                }
            }
            Err(e) => {
                error!("[Telegram] Failed to send: {}", e);
            }
        }
    }

    /// Notify about a trade execution.
    pub async fn notify_trade(&self, side: &str, token_id: &str, size: Decimal, price: Decimal) {
        let short_id = &token_id[..token_id.len().min(12)];
        let msg = format!(
            "📊 <b>Trade Executed</b>\n\
             Side: <b>{}</b>\n\
             Token: {}...\n\
             Size: {:.2}\n\
             Price: ${:.3}",
            side, short_id, size, price
        );
        self.send(&msg).await;
    }

    /// Notify about a PnL event (win/loss).
    pub async fn notify_pnl(&self, wallet: &str, pnl: Decimal, is_win: bool) {
        if self.min_pnl_usd > Decimal::ZERO && pnl.abs() < self.min_pnl_usd {
            return;
        }
        let emoji = if is_win { "🟢" } else { "🔴" };
        let label = if is_win { "WIN" } else { "LOSS" };
        let short_w = &wallet[..wallet.len().min(10)];
        let msg = format!(
            "{} <b>{}</b>\n\
             Wallet: {}...\n\
             PnL: {}${:.2}",
            emoji,
            label,
            short_w,
            if pnl >= Decimal::ZERO { "+" } else { "" },
            pnl
        );
        self.send(&msg).await;
    }

    /// Notify about a freeze/circuit-breaker event.
    pub async fn notify_alert(&self, alert_type: &str, details: &str) {
        let msg = format!(
            "⚠️ <b>{}</b>\n\
             {}",
            alert_type, details
        );
        self.send(&msg).await;
    }
}

/// Start a background task that periodically samples PnL snapshots and checks circuit-breakers.
pub fn start_pnl_sampler(state: Arc<RwLock<crate::state::BotState>>, config: Config) {
    let tg = Arc::new(TelegramNotifier::new(&config));

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;

            let mut guard = state.write().await;

            // Record PnL snapshot
            guard.record_pnl_snapshot();

            // Check daily loss circuit-breaker
            if guard.check_daily_loss_circuit_breaker(config.max_daily_loss_pct) {
                // Auto-freeze and close all positions
                guard.freeze_for(86400); // freeze for 24h
                let msg = format!(
                    "Daily loss circuit-breaker triggered! Loss: ${:.2} / ${:.2} ({:.0}%). Trading frozen.",
                    guard.today_realized_loss,
                    guard.daily_start_balance * config.max_daily_loss_pct,
                    config.max_daily_loss_pct * Decimal::from(100)
                );
                info!("[RiskGuard] {}", msg);
                drop(guard);
                tg.notify_alert("DAILY LOSS CIRCUIT-BREAKER", &msg).await;
            }
        }
    });
}
