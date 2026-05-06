use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use serde_json::json;

use crate::state::AppState;

const RATE_LIMIT_MAX: usize = 100;
const RATE_LIMIT_WINDOW_SECONDS: u64 = 15 * 60;

pub async fn rate_limit(
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let ip = connect_info
        .map(|ci| ci.0.ip().to_string())
        .or_else(|| {
            request
                .headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(',').next().unwrap_or(v).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    let now = Instant::now();
    let window = Duration::from_secs(RATE_LIMIT_WINDOW_SECONDS);

    {
        let mut lock = state.rate_limit.lock().await;
        let entry = lock.hits.entry(ip).or_default();
        entry.retain(|timestamp| now.duration_since(*timestamp) < window);
        if entry.len() >= RATE_LIMIT_MAX {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({ "message": "Too many requests from this IP" })),
            ));
        }
        entry.push(now);
    }

    Ok(next.run(request).await)
}
