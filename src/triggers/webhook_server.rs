//! axum webhook server
//!
//! 路由：
//! - `GET /health` → 200 `{"status":"ok"}`（无认证）
//! - `POST /webhook/:kind` → handler（需 `X-Webhook-Token` 认证）

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use crossbeam_channel::Sender;
use serde_json::json;
use tracing::{debug, warn};

use crate::domain::{ExternalInput, SignalSource};
use crate::triggers::config::WebhookConfig;

/// 启动 webhook server。
///
/// 阻塞当前 tokio task，直到 server 退出。
pub async fn run_webhook_server(
    input_tx: Sender<ExternalInput>,
    config: WebhookConfig,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/webhook/:kind", post(webhook_handler))
        .with_state(AppState {
            input_tx,
            auth_token: config.auth_token,
        });

    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    tracing::info!(
        event = "WebhookServerStarted",
        listen_addr = %config.listen_addr,
        "webhook server listening"
    );
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Clone)]
struct AppState {
    input_tx: Sender<ExternalInput>,
    auth_token: String,
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn webhook_handler(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    // 认证
    let token = headers
        .get("X-Webhook-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if token != state.auth_token {
        warn!(event = "WebhookAuthFailed", kind = %kind, "auth failed");
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 解析 body
    let json_body: serde_json::Value = serde_json::from_slice(&body).map_err(|_| {
        debug!(event = "WebhookBadJson", kind = %kind, "invalid json body");
        StatusCode::BAD_REQUEST
    })?;

    debug!(
        event = "WebhookRequestReceived",
        kind = %kind,
        body_len = body.len(),
        "received webhook"
    );

    let external_input = ExternalInput::Webhook {
        source: SignalSource("webhook".to_string()),
        kind,
        body: json_body,
    };
    state
        .input_tx
        .send(external_input)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((StatusCode::ACCEPTED, Json(json!({"status": "accepted"}))))
}
