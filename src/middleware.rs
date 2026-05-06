use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::RETRY_AFTER;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::state::AppState;

const RATE_LIMIT_MAX: usize = 100;
const RATE_LIMIT_WINDOW_SECONDS: u64 = 15 * 60;
const AUTH_RATE_LIMIT_MAX: usize = 300;
const RECORDS_RATE_LIMIT_MAX: usize = 1000;
const MINESWEEPER_REVEAL_RATE_LIMIT_MAX: usize = 12000;
const MINESWEEPER_MARK_RATE_LIMIT_MAX: usize = 8000;
const MINESWEEPER_OTHER_RATE_LIMIT_MAX: usize = 4000;

pub async fn rate_limit(
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    if std::env::var("DISABLE_RATE_LIMIT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return Ok(next.run(request).await);
    }

    let path = request.uri().path().to_string();
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
    let (bucket, max_requests) = if path.starts_with("/api/auth/") {
        ("auth", AUTH_RATE_LIMIT_MAX)
    } else if path.starts_with("/api/records/") || path == "/api/records" {
        ("records", RECORDS_RATE_LIMIT_MAX)
    } else if path.contains("/reveal") {
        ("minesweeper_reveal", MINESWEEPER_REVEAL_RATE_LIMIT_MAX)
    } else if path.contains("/mark") {
        ("minesweeper_mark", MINESWEEPER_MARK_RATE_LIMIT_MAX)
    } else if path.starts_with("/api/minesweeper/") {
        ("minesweeper_other", MINESWEEPER_OTHER_RATE_LIMIT_MAX)
    } else if path.starts_with("/api/online/") {
        // Online create/join/ready/start/move would hit the default bucket very fast.
        ("online", 250_000)
    } else {
        ("default", RATE_LIMIT_MAX)
    };
    let key = format!("{ip}:{bucket}");

    {
        let mut lock = state.rate_limit.lock().await;
        let entry = lock.hits.entry(key).or_default();
        entry.retain(|timestamp| now.duration_since(*timestamp) < window);
        if entry.len() >= max_requests {
            let retry_after = RATE_LIMIT_WINDOW_SECONDS.to_string();
            let response = (
                StatusCode::TOO_MANY_REQUESTS,
                [(RETRY_AFTER, retry_after)],
                Json(json!({ "message": "Too many requests from this IP" })),
            )
                .into_response();
            return Ok(response);
        }
        entry.push(now);
    }

    Ok(next.run(request).await)
}
