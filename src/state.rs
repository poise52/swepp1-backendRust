use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use sqlx::PgPool;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub jwt_secret: String,
    pub jwt_exp_seconds: u64,
    pub rate_limit: Arc<Mutex<RateLimitState>>,
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
