use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub jwt_secret: String,
    pub jwt_exp_seconds: u64,
    /// Если задан (env `FRONTEND_URL`) — используется для invite и строгого CORS.
    pub configured_public_url: Option<String>,
    /// Когда `FRONTEND_URL` нет и из запроса база не выведена — как на dev (Vite).
    pub invite_fallback: String,
    pub rate_limit: Arc<Mutex<RateLimitState>>,
    pub online_hubs: Arc<Mutex<HashMap<String, broadcast::Sender<OnlineEvent>>>>,
}

pub struct RateLimitState {
    pub hits: HashMap<String, Vec<Instant>>,
}

impl RateLimitState {
    pub fn new() -> Self {
        Self {
            hits: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineEvent {
    pub event: String,
    pub payload: serde_json::Value,
}
