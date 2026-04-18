//! AI-driven emergency control API server
//!
//! Provides two endpoints for emergency intervention:
//! - `POST /ai/close` - Close a specific token position
//! - `POST /ai/freeze` - Emergency freeze: close all positions and suspend new opens
//!
//! Server binds to 127.0.0.1:8989

use crate::clients::OrderSubmitter;
use crate::models::{OrderRequest, TradeSide};
use crate::state::BotState;
use axum::extract::State as AxumState;
use axum::{routing::post, Json, Router};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Global freeze flag shared across API handlers and strategy engine.
pub struct FreezeGuard {
    /// When true, all new BUY opens are rejected.
    frozen: bool,
}

impl FreezeGuard {
    pub fn new() -> Self {
        Self { frozen: false }
    }
    pub fn is_frozen(&self) -> bool {
        self.frozen
    }
    pub fn freeze(&mut self) {
        if !self.frozen {
            info!("[AI CONTROL] Trading FROZEN — all new opens blocked.");
            self.frozen = true;
        }
    }
    pub fn unfreeze(&mut self) {
        if self.frozen {
            info!("[AI CONTROL] Trading UNFROZEN — opens resumed.");
            self.frozen = false;
        }
    }
}

impl Default for FreezeGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Application state injected into all route handlers.
#[derive(Clone)]
pub struct ApiControlState {
    pub bot_state: Arc<RwLock<BotState>>,
    pub submitter: OrderSubmitter,
    pub freeze_guard: Arc<RwLock<FreezeGuard>>,
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CloseRequest {
    pub token_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FreezeRequest {
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ActionResponse {
    pub success: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /ai/close — close a specific token position
pub async fn handle_close(
    axum_state: AxumState<ApiControlState>,
    Json(req): Json<CloseRequest>,
) -> (axum::http::StatusCode, Json<ActionResponse>) {
    let token_id = &req.token_id;
    let reason = req.reason.as_deref().unwrap_or("AI close request");

    info!(
        "[AI CONTROL] /ai/close called for token {} (reason: {})",
        &token_id[..token_id.len().min(12)],
        reason
    );

    // Look up held size
    let held = {
        let guard = axum_state.bot_state.read().await;
        guard.positions.get(token_id).map(|p| p.size)
    };

    let Some(size) = held else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(ActionResponse {
                success: false,
                message: format!(
                    "No position found for token {}",
                    &token_id[..token_id.len().min(12)]
                ),
            }),
        );
    };

    if size <= Decimal::ZERO {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(ActionResponse {
                success: false,
                message: "Position size is zero or dust".to_string(),
            }),
        );
    }

    // Submit a SELL limit order at a conservative price
    let sell_price = Decimal::from_str("0.99").unwrap_or(Decimal::new(99, 2));
    let order = OrderRequest {
        token_id: token_id.clone(),
        price: sell_price,
        size,
        side: TradeSide::SELL,
    };

    match (axum_state.submitter)(order).await {
        Ok(()) => {
            info!(
                "[AI CONTROL] Successfully closed token {}",
                &token_id[..token_id.len().min(12)]
            );
            (
                axum::http::StatusCode::OK,
                Json(ActionResponse {
                    success: true,
                    message: format!(
                        "Closed {} shares of {}",
                        size,
                        &token_id[..token_id.len().min(12)]
                    ),
                }),
            )
        }
        Err(e) => {
            error!(
                "[AI CONTROL] Failed to close {}: {}",
                &token_id[..token_id.len().min(12)],
                e
            );
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(ActionResponse {
                    success: false,
                    message: format!("Close failed: {}", e),
                }),
            )
        }
    }
}

/// POST /ai/freeze — close ALL positions and freeze new opens
pub async fn handle_freeze(
    axum_state: AxumState<ApiControlState>,
    Json(req): Json<FreezeRequest>,
) -> (axum::http::StatusCode, Json<ActionResponse>) {
    let reason = req.reason.as_deref().unwrap_or("AI freeze request");

    info!("[AI CONTROL] /ai/freeze called (reason: {})", reason);

    // Activate freeze
    {
        let mut guard = axum_state.freeze_guard.write().await;
        guard.freeze();
    }

    // Collect all positions
    let positions: Vec<(String, Decimal)> = {
        let guard = axum_state.bot_state.read().await;
        guard
            .positions
            .iter()
            .map(|(id, pos)| (id.clone(), pos.size))
            .collect()
    };

    if positions.is_empty() {
        return (
            axum::http::StatusCode::OK,
            Json(ActionResponse {
                success: true,
                message: "Frozen. No open positions to close.".to_string(),
            }),
        );
    }

    let mut closed = 0u32;
    let mut failed = 0u32;

    for (token_id, size) in positions {
        let order = OrderRequest {
            token_id: token_id.clone(),
            price: Decimal::from_str("0.99").unwrap_or(Decimal::new(99, 2)),
            size,
            side: TradeSide::SELL,
        };
        match (axum_state.submitter)(order).await {
            Ok(()) => {
                closed += 1;
                info!(
                    "[AI FREEZE] Closed {} shares of {}",
                    size,
                    &token_id[..token_id.len().min(12)]
                );
            }
            Err(e) => {
                failed += 1;
                error!(
                    "[AI FREEZE] Failed to close {}: {}",
                    &token_id[..token_id.len().min(12)],
                    e
                );
            }
        }
    }

    (
        axum::http::StatusCode::OK,
        Json(ActionResponse {
            success: true,
            message: format!(
                "Frozen. Closed {}/{} positions ({} failed). Reason: {}",
                closed,
                closed + failed,
                failed,
                reason
            ),
        }),
    )
}

// ---------------------------------------------------------------------------
// Server bootstrap
// ---------------------------------------------------------------------------

/// Start the AI control API server on 127.0.0.1:8989
pub fn start_api_server(state: ApiControlState) {
    tokio::spawn(async move {
        let app = Router::new()
            .route("/ai/close", post(handle_close))
            .route("/ai/freeze", post(handle_freeze))
            .with_state(state);

        let addr = "127.0.0.1:8989";
        info!("[AI CONTROL] API server starting on http://{}", addr);

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("[AI CONTROL] Failed to bind to {}: {}", addr, e);
                return;
            }
        };

        if let Err(e) = axum::serve(listener, app).await {
            error!("[AI CONTROL] Server error: {}", e);
        }
    });
}
