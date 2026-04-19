//! API server — unified HTTP API on port 3000.
//!
//! Exposes:
//! - `/api/state`            — live bot state
//! - `/api/config`           — read/write config.toml
//! - `/api/env`              — read/write secrets (.env)
//! - `/api/action/restart`   — seamless hot-reboot
//! - `/api/ai/stats`         — per-wallet win/loss statistics
//! - `/api/ai/markets`       — list all known markets (from scanner)
//! - `/api/ai/markets/mute`  — toggle market mute
//! - `/ai/close`             — emergency close all positions
//! - `/ai/freeze`            — freeze all new BUY entries

use crate::config::BotConfig;
use crate::models::{EvaluatedTrade, Position, TargetPosition};
use crate::state::{BotState, PerfMetrics, PnlSnapshot, WalletStats};
use crate::stop_loss::StopLossState;
use axum::{
    extract::{Json, State},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info};

// ---------------------------------------------------------------------------
// Shared API state
// ---------------------------------------------------------------------------

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct ApiState {
    pub bot_state: Arc<RwLock<BotState>>,
    pub copy_ledger: Arc<tokio::sync::Mutex<crate::copy_ledger::CopyLedger>>,
    /// Cloned Arc of the order submitter so both strategy engine and AI
    /// endpoints share the same underlying closure.
    pub submitter: Arc<
        dyn Fn(
                crate::models::OrderRequest,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
            + Send
            + Sync,
    >,
    /// Stop-loss / take-profit tracked positions state.
    pub sl_state: Arc<Mutex<StopLossState>>,
}

// ---------------------------------------------------------------------------
// Setup router (shown when .env is missing)
// ---------------------------------------------------------------------------

pub fn create_setup_router() -> Router {
    Router::new()
        .route(
            "/api/state",
            get(|| async { axum::Json(serde_json::json!({ "status": "setup_required" })) }),
        )
        .route("/api/setup", post(handle_setup))
        .fallback_service(
            tower_http::services::ServeDir::new("web/dist").append_index_html_on_directories(true),
        )
}

#[derive(serde::Deserialize)]
pub struct SetupPayload {
    pub private_key: String,
    pub funder_address: String,
}

async fn handle_setup(Json(payload): Json<SetupPayload>) -> axum::response::Response {
    use crate::config::{BotConfig, TargetsConfig};
    use std::io::Write;

    if let Ok(mut env_file) = std::fs::File::create(".env") {
        let _ = writeln!(env_file, "PRIVATE_KEY=\"{}\"", payload.private_key);
        let _ = writeln!(env_file, "FUNDER_ADDRESS=\"{}\"", payload.funder_address);
    }

    let default_cfg = BotConfig {
        targets: TargetsConfig { wallets: vec![] },
        ..Default::default()
    };
    let _ = crate::config::write_toml(&default_cfg);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let exe = std::env::current_exe().unwrap();
        let orig_args: Vec<String> = std::env::args().collect();

        // Replace --ui with --ui-reboot (suppress browser re-open), keep all other args
        let mut new_args: Vec<String> = orig_args[1..]
            .iter()
            .filter(|a| *a != "--ui")
            .cloned()
            .collect();
        new_args.push("--ui-reboot".to_string());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            tracing::info!("Setup complete — rebooting with args: {:?}", new_args);
            let err = std::process::Command::new(&exe).args(&new_args).exec();
            tracing::error!("Seamless setup reboot failed: {}", err);
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new(&exe).args(&new_args).spawn();
            std::process::exit(0);
        }
    });

    axum::Json(serde_json::json!({ "success": true })).into_response()
}

// ---------------------------------------------------------------------------
// State response
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct StateResponse {
    pub positions: HashMap<String, Position>,
    pub live_feed: Vec<EvaluatedTrade>,
    pub total_balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub realized_pnl: Decimal,
    pub target_positions: Vec<TargetPosition>,
    pub copies_executed: u32,
    pub trades_skipped: u32,
    pub copied_count: usize,
    pub next_scan_secs: u64,
    pub pending_orders: std::collections::HashMap<String, crate::models::QueuedOrder>,
    pub active_orders: Vec<crate::models::ActiveApiOrder>,
    pub position_sources: HashMap<String, String>,
    pub wallet_stats: HashMap<String, WalletStats>,
    pub muted_markets: Vec<String>,
    pub today_copies: u32,
    pub today_wins: u32,
    pub today_losses: u32,
    pub today_pnl: Decimal,
    /// Whether AI freeze is currently active (blocks new BUY entries).
    pub is_frozen: bool,
    /// Unix timestamp (seconds) when freeze expires. Null if not frozen.
    pub freeze_until_secs: Option<u64>,
    /// Today's realized loss (for daily loss circuit-breaker).
    pub today_realized_loss: Decimal,
    /// Daily starting balance (for daily loss %).
    pub daily_start_balance: Decimal,
    /// Whether daily loss circuit-breaker was triggered.
    pub daily_loss_triggered: bool,
    /// Currently blacklisted wallet addresses.
    pub wallet_blacklist: Vec<String>,
    /// Performance metrics.
    pub perf: PerfMetrics,
    /// PnL history snapshots for equity curve.
    pub pnl_history: Vec<PnlSnapshot>,
    /// Whether running in simulation mode.
    pub is_sim: bool,
    /// Stop-loss / take-profit tracked positions status.
    pub sl_status: Vec<crate::stop_loss::TrackedPositionStatus>,
    /// Token ownership strategy currently active.
    pub token_ownership_strategy: String,
    /// Whether partial close is enabled.
    pub enable_partial_close: bool,
}

async fn get_state(State(api_state): State<ApiState>) -> Json<StateResponse> {
    let guard = api_state.bot_state.read().await;
    let ledger = api_state.copy_ledger.lock().await;

    let mut position_sources = HashMap::new();
    for token_id in guard.positions.keys() {
        if let Some(entry) = ledger.find_active_for_token(token_id) {
            position_sources.insert(token_id.clone(), entry.source_wallet.clone());
        } else if let Some(entry) = ledger
            .entries
            .iter()
            .rev()
            .find(|e| e.token_id == *token_id)
        {
            position_sources.insert(token_id.clone(), entry.source_wallet.clone());
        }
    }
    for o in &guard.active_orders {
        if let Some(entry) = ledger.find_active_for_token(&o.token_id) {
            position_sources.insert(o.token_id.clone(), entry.source_wallet.clone());
        } else if let Some(entry) = ledger
            .entries
            .iter()
            .rev()
            .find(|e| e.token_id == o.token_id)
        {
            position_sources.insert(o.token_id.clone(), entry.source_wallet.clone());
        }
    }

    Json(StateResponse {
        positions: guard.positions.clone(),
        live_feed: guard.live_feed.iter().cloned().collect(),
        total_balance: guard.total_balance,
        unrealized_pnl: guard.unrealized_pnl,
        realized_pnl: guard.realized_pnl,
        target_positions: guard.target_positions.clone(),
        copies_executed: guard.copies_executed,
        trades_skipped: guard.trades_skipped,
        copied_count: guard.copied_count,
        next_scan_secs: guard.next_scan_secs,
        pending_orders: guard.pending_orders.clone(),
        active_orders: guard.active_orders.clone(),
        position_sources,
        wallet_stats: guard.wallet_stats.clone(),
        muted_markets: guard.muted_markets.iter().cloned().collect(),
        today_copies: guard.today_copies,
        today_wins: guard.today_wins,
        today_losses: guard.today_losses,
        today_pnl: guard.today_pnl,
        is_frozen: guard.is_frozen(),
        freeze_until_secs: guard.freeze_until.map(|t| {
            let remaining = t.saturating_duration_since(Instant::now());
            std::time::UNIX_EPOCH
                .checked_add(remaining)
                .map(|st| st.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs())
                .unwrap_or(0)
        }),
        today_realized_loss: guard.today_realized_loss,
        daily_start_balance: guard.daily_start_balance,
        daily_loss_triggered: guard.daily_loss_triggered,
        wallet_blacklist: guard.wallet_blacklist.iter().cloned().collect(),
        perf: guard.perf.clone(),
        pnl_history: guard.pnl_history.clone(),
        is_sim: false, // will be overridden by caller if needed
        sl_status: {
            let sl_guard = api_state.sl_state.lock().await;
            sl_guard.get_all_status(&{
                let mut prices = HashMap::new();
                for tp in &guard.target_positions {
                    prices.insert(tp.token_id.clone(), tp.cur_price);
                }
                prices
            })
        },
        token_ownership_strategy: guard.token_ownership_strategy.clone(),
        enable_partial_close: guard.enable_partial_close,
    })
}

// ---------------------------------------------------------------------------
// AI Stats endpoint
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct AiStatsResponse {
    pub wallet_stats: HashMap<String, WalletStats>,
    pub today_copies: u32,
    pub today_wins: u32,
    pub today_losses: u32,
    pub today_pnl: Decimal,
    pub total_wallets: usize,
    pub overall_win_rate: Option<f64>,
}

async fn get_ai_stats(State(api_state): State<ApiState>) -> Json<AiStatsResponse> {
    let guard = api_state.bot_state.read().await;

    let total: u32 = guard.wallet_stats.values().map(|s| s.wins + s.losses).sum();
    let wins: u32 = guard.wallet_stats.values().map(|s| s.wins).sum();
    let overall_win_rate = if total > 0 {
        Some(wins as f64 / total as f64 * 100.0)
    } else {
        None
    };

    Json(AiStatsResponse {
        wallet_stats: guard.wallet_stats.clone(),
        today_copies: guard.today_copies,
        today_wins: guard.today_wins,
        today_losses: guard.today_losses,
        today_pnl: guard.today_pnl,
        total_wallets: guard.wallet_stats.len(),
        overall_win_rate,
    })
}

// ---------------------------------------------------------------------------
// Market mute endpoints
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct MarketsResponse {
    pub markets: Vec<MarketInfo>,
    pub muted: Vec<String>,
}

#[derive(Serialize)]
pub struct MarketInfo {
    pub token_id: String,
    pub title: String,
    pub outcome: String,
    pub muted: bool,
}

async fn get_markets(State(api_state): State<ApiState>) -> Json<MarketsResponse> {
    let guard = api_state.bot_state.read().await;
    let markets: Vec<MarketInfo> = guard
        .target_positions
        .iter()
        .map(|tp| MarketInfo {
            token_id: tp.token_id.clone(),
            title: tp.title.clone(),
            outcome: tp.outcome.clone(),
            muted: guard.muted_markets.contains(&tp.token_id),
        })
        .collect();
    Json(MarketsResponse {
        muted: guard.muted_markets.iter().cloned().collect(),
        markets,
    })
}

#[derive(Deserialize)]
pub struct MuteRequest {
    pub token_id: String,
}

#[derive(Serialize)]
pub struct MuteResponse {
    pub token_id: String,
    pub muted: bool,
}

async fn post_market_mute(
    State(api_state): State<ApiState>,
    Json(req): Json<MuteRequest>,
) -> Json<MuteResponse> {
    let muted = {
        let mut guard = api_state.bot_state.write().await;
        guard.toggle_market_mute(&req.token_id)
    };
    info!(
        "[API] Market mute toggled for {}: {}",
        &req.token_id[..req.token_id.len().min(12)],
        muted
    );
    Json(MuteResponse {
        token_id: req.token_id,
        muted,
    })
}

// ---------------------------------------------------------------------------
// AI emergency control endpoints
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AiCloseRequest {
    pub token_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AiFreezeRequest {
    pub duration_secs: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Serialize)]
pub struct AiActionResponse {
    pub success: bool,
    pub message: String,
}

async fn ai_close(
    State(api_state): State<ApiState>,
    Json(req): Json<AiCloseRequest>,
) -> impl IntoResponse {
    let reason = req.reason.as_deref().unwrap_or("AI emergency close");
    info!(
        "[AI/close] called: token_id={:?}, reason={}",
        req.token_id, reason
    );

    let positions: Vec<(String, Decimal)> = {
        let guard = api_state.bot_state.read().await;
        if let Some(ref tid) = req.token_id {
            guard.positions.get(tid).map(|p| (tid.clone(), p.size))
        } else {
            guard
                .positions
                .iter()
                .map(|(id, p)| (id.clone(), p.size))
                .collect::<Vec<_>>()
                .into_iter()
                .next()
        }
        .map(|x| vec![x])
        .unwrap_or_default()
    };

    if positions.is_empty() {
        return (
            axum::http::StatusCode::OK,
            Json(AiActionResponse {
                success: true,
                message: "No positions to close.".to_string(),
            }),
        );
    }

    let mut closed = 0u32;
    let mut failed = 0u32;

    for (token_id, size) in &positions {
        if size <= &Decimal::ZERO {
            continue;
        }
        let order = crate::models::OrderRequest {
            token_id: token_id.clone(),
            price: Decimal::new(99, 2),
            size: *size,
            side: crate::models::TradeSide::SELL,
        };
        match (api_state.submitter)(order).await {
            Ok(()) => {
                closed += 1;
                info!(
                    "[AI/close] Closed {} shares of {}",
                    size,
                    &token_id[..token_id.len().min(12)]
                );
            }
            Err(e) => {
                failed += 1;
                error!(
                    "[AI/close] Failed to close {}: {}",
                    &token_id[..token_id.len().min(12)],
                    e
                );
            }
        }
    }

    (
        axum::http::StatusCode::OK,
        Json(AiActionResponse {
            success: failed == 0,
            message: format!(
                "Closed {}/{} positions. ({failed} failed)",
                closed,
                closed + failed
            ),
        }),
    )
}

async fn ai_freeze(
    State(api_state): State<ApiState>,
    Json(req): Json<AiFreezeRequest>,
) -> impl IntoResponse {
    let reason = req.reason.as_deref().unwrap_or("AI freeze");
    let duration = req.duration_secs.unwrap_or(300);
    info!(
        "[AI/freeze] called: duration={}s, reason={}",
        duration, reason
    );

    // Activate freeze in state
    {
        let mut guard = api_state.bot_state.write().await;
        guard.freeze_for(duration);
    }

    let positions: Vec<(String, Decimal)> = {
        let guard = api_state.bot_state.read().await;
        guard
            .positions
            .iter()
            .map(|(id, p)| (id.clone(), p.size))
            .collect()
    };

    let mut closed = 0u32;
    let mut failed = 0u32;

    for (token_id, size) in &positions {
        if size <= &Decimal::ZERO {
            continue;
        }
        let order = crate::models::OrderRequest {
            token_id: token_id.clone(),
            price: Decimal::new(99, 2),
            size: *size,
            side: crate::models::TradeSide::SELL,
        };
        match (api_state.submitter)(order).await {
            Ok(()) => {
                closed += 1;
                info!(
                    "[AI/freeze] Closed {} shares of {}",
                    size,
                    &token_id[..token_id.len().min(12)]
                );
            }
            Err(e) => {
                failed += 1;
                error!(
                    "[AI/freeze] Failed to close {}: {}",
                    &token_id[..token_id.len().min(12)],
                    e
                );
            }
        }
    }

    (
        axum::http::StatusCode::OK,
        Json(AiActionResponse {
            success: true,
            message: format!(
                "Frozen for {}s. Closed {}/{} positions. ({} failed). Reason: {}",
                duration,
                closed,
                closed + failed,
                failed,
                reason
            ),
        }),
    )
}

// ---------------------------------------------------------------------------
// AI unfreeze endpoint
// ---------------------------------------------------------------------------

async fn ai_unfreeze(State(api_state): State<ApiState>) -> impl IntoResponse {
    info!("[AI/unfreeze] called — clearing freeze");
    {
        let mut guard = api_state.bot_state.write().await;
        guard.freeze_until = None;
    }
    (
        axum::http::StatusCode::OK,
        Json(AiActionResponse {
            success: true,
            message: "Trading unfrozen. New BUY entries are allowed.".to_string(),
        }),
    )
}

// ---------------------------------------------------------------------------
// Performance metrics endpoint
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct PerfResponse {
    pub perf: PerfMetrics,
    pub uptime_secs: u64,
}

async fn get_perf(State(api_state): State<ApiState>) -> Json<PerfResponse> {
    let guard = api_state.bot_state.read().await;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let uptime = now_secs.saturating_sub(guard.perf.started_at_secs);
    Json(PerfResponse {
        perf: guard.perf.clone(),
        uptime_secs: uptime,
    })
}

// ---------------------------------------------------------------------------
// Wallet blacklist management
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct WalletBlacklistRequest {
    pub wallet: String,
}

#[derive(Serialize)]
pub struct WalletBlacklistResponse {
    pub wallet: String,
    pub blacklisted: bool,
}

async fn post_wallet_blacklist(
    State(api_state): State<ApiState>,
    Json(req): Json<WalletBlacklistRequest>,
) -> Json<WalletBlacklistResponse> {
    let key = req.wallet.to_lowercase();
    let mut guard = api_state.bot_state.write().await;
    let was_in = guard.wallet_blacklist.contains(&key);
    if was_in {
        guard.wallet_blacklist.remove(&key);
    } else {
        guard.wallet_blacklist.insert(key.clone());
    }
    info!(
        "[API] Wallet blacklist toggled for {}: {}",
        &key[..key.len().min(10)],
        !was_in
    );
    Json(WalletBlacklistResponse {
        wallet: req.wallet,
        blacklisted: !was_in,
    })
}

// ---------------------------------------------------------------------------
// CSV export endpoint
// ---------------------------------------------------------------------------

async fn get_csv_export(State(api_state): State<ApiState>) -> impl IntoResponse {
    let guard = api_state.bot_state.read().await;
    let ledger = api_state.copy_ledger.lock().await;

    let mut csv = String::from("token_id,source_wallet,size,entry_price,copied_at,closed\n");
    for entry in &ledger.entries {
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            entry.token_id,
            entry.source_wallet,
            entry.size,
            entry.entry_price,
            entry.copied_at,
            if entry.closed { "CLOSED" } else { "OPEN" }
        ));
    }

    // Also add wallet stats
    csv.push_str(
        "\n\nWallet Stats\nwallet,total_copies,wins,losses,win_rate,total_pnl,blacklisted\n",
    );
    for (wallet, stats) in &guard.wallet_stats {
        let wr = if stats.wins + stats.losses > 0 {
            stats.wins as f64 / (stats.wins + stats.losses) as f64 * 100.0
        } else {
            0.0
        };
        let bl = guard.wallet_blacklist.contains(wallet);
        csv.push_str(&format!(
            "{},{},{},{},{:.1}%,{},{}\n",
            wallet, stats.total_copies, stats.wins, stats.losses, wr, stats.total_pnl, bl
        ));
    }

    (
        [
            ("content-type", "text/csv"),
            (
                "content-disposition",
                "attachment; filename=polycopier_export.csv",
            ),
        ],
        csv,
    )
}

// ---------------------------------------------------------------------------
// Stop-loss / take-profit tracked positions status
// ---------------------------------------------------------------------------

async fn get_sl_status(
    State(api_state): State<ApiState>,
) -> Json<Vec<crate::stop_loss::TrackedPositionStatus>> {
    // Get current prices from BotState (same source as the SL monitor loop)
    let current_prices: HashMap<String, Decimal> = {
        let bot = api_state.bot_state.read().await;
        let mut prices = HashMap::new();
        for token_id in bot.positions.keys() {
            if let Some(tp) = bot
                .target_positions
                .iter()
                .find(|tp| &tp.token_id == token_id)
            {
                prices.insert(token_id.clone(), tp.cur_price);
            }
        }
        // Also include prices from pending_orders for tokens not in target_positions
        for (tid, pending) in &bot.pending_orders {
            if !prices.contains_key(tid) {
                prices.insert(tid.clone(), pending.price);
            }
        }
        prices
    };

    let guard = api_state.sl_state.lock().await;
    Json(guard.get_all_status(&current_prices))
}

// ---------------------------------------------------------------------------
// PnL history endpoint
// ---------------------------------------------------------------------------

async fn get_pnl_history(State(api_state): State<ApiState>) -> Json<Vec<Value>> {
    let guard = api_state.bot_state.read().await;
    Json(guard.get_pnl_history_for_chart())
}

#[derive(Serialize, Deserialize)]
pub struct EnvData {
    pub private_key: String,
    pub funder_address: String,
}

async fn get_config() -> Json<serde_json::Value> {
    let raw = std::fs::read_to_string("config.toml").unwrap_or_default();
    let toml_val: toml::Value = raw
        .parse()
        .unwrap_or(toml::Value::Table(toml::map::Map::new()));
    Json(serde_json::to_value(toml_val).unwrap())
}

async fn post_config(Json(payload): Json<BotConfig>) -> Json<serde_json::Value> {
    if let Err(e) = crate::config::write_toml(&payload) {
        return Json(serde_json::json!({ "error": e.to_string() }));
    }
    Json(serde_json::json!({ "success": true }))
}

async fn get_env() -> Json<EnvData> {
    let _ = dotenvy::dotenv();
    let private_key = std::env::var("PRIVATE_KEY").unwrap_or_default();
    let funder_address = std::env::var("FUNDER_ADDRESS").unwrap_or_default();
    Json(EnvData {
        private_key,
        funder_address,
    })
}

async fn post_env(Json(payload): Json<EnvData>) -> Json<serde_json::Value> {
    if let Err(e) = crate::config::write_secrets_env(&payload.private_key, &payload.funder_address)
    {
        return Json(serde_json::json!({ "error": e.to_string() }));
    }
    Json(serde_json::json!({ "success": true }))
}

async fn restart() -> Json<serde_json::Value> {
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let exe = std::env::current_exe().unwrap_or_else(|_| "cargo".into());
        let args: Vec<String> = std::env::args().collect();

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            tracing::warn!("Executing seamless API reboot...");
            let err = std::process::Command::new(&exe).args(&args[1..]).exec();
            tracing::error!("Seamless API reboot failed: {}", err);
            std::process::exit(1);
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new(&exe).args(&args[1..]).spawn();
            std::process::exit(0);
        }
    });
    Json(serde_json::json!({ "success": true }))
}

// ---------------------------------------------------------------------------
// Router factory
// ---------------------------------------------------------------------------

#[allow(clippy::type_complexity)]
pub fn create_router(
    bot_state: Arc<RwLock<BotState>>,
    copy_ledger: Arc<tokio::sync::Mutex<crate::copy_ledger::CopyLedger>>,
    submitter: Arc<
        dyn Fn(
                crate::models::OrderRequest,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
            + Send
            + Sync,
    >,
    sl_state: Arc<Mutex<StopLossState>>,
) -> Router {
    use tower_http::cors::{Any, CorsLayer};
    use tower_http::services::{ServeDir, ServeFile};

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let state = ApiState {
        bot_state,
        copy_ledger,
        submitter,
        sl_state,
    };

    let root_path = std::env::current_dir().unwrap().join("web/dist");
    let serve_dir =
        ServeDir::new(&root_path).fallback(ServeFile::new(root_path.join("index.html")));

    Router::new()
        // Core state
        .route("/api/state", get(get_state))
        // Config
        .route("/api/config", get(get_config).post(post_config))
        .route("/api/env", get(get_env).post(post_env))
        .route("/api/action/restart", post(restart))
        // AI stats panel
        .route("/api/ai/stats", get(get_ai_stats))
        .route("/api/ai/markets", get(get_markets))
        .route("/api/ai/markets/mute", post(post_market_mute))
        // AI emergency control
        .route("/ai/close", post(ai_close))
        .route("/ai/freeze", post(ai_freeze))
        .route("/ai/unfreeze", post(ai_unfreeze))
        // Performance & monitoring
        .route("/api/perf", get(get_perf))
        .route("/api/pnl/history", get(get_pnl_history))
        // Wallet blacklist
        .route("/api/wallet/blacklist", post(post_wallet_blacklist))
        // Stop-loss / take-profit status
        .route("/api/sl/status", get(get_sl_status))
        // CSV export
        .route("/api/csv/export", get(get_csv_export))
        .with_state(state)
        .layer(cors)
        .fallback_service(serve_dir)
}
