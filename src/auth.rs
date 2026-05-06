use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::errors::ApiError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    #[serde(rename = "userId")]
    pub user_id: String,
    pub exp: usize,
}

pub fn generate_token(user_id: &str, secret: &str, expires_in_seconds: u64) -> Result<String, ApiError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Internal server error: {e}")))?;
    let exp = now.as_secs().saturating_add(expires_in_seconds) as usize;

    let claims = Claims {
        user_id: user_id.to_string(),
        exp,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Internal server error: {e}")))
}

pub fn verify_token(token: &str, secret: &str) -> Option<Claims> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .ok()
    .map(|data| data.claims)
}
