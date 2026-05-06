use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{HeaderMap, StatusCode};
use http::header;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::errors::ApiError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    #[serde(rename = "userId")]
    pub user_id: String,
    /// Omit in legacy tokens → deserialize as empty string → callers treat as `user`.
    #[serde(default)]
    pub role: String,
    pub exp: usize,
}

pub fn generate_token(user_id: &str, role: &str, secret: &str, expires_in_seconds: u64) -> Result<String, ApiError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Internal server error: {e}")))?;
    let exp = now.as_secs().saturating_add(expires_in_seconds) as usize;

    let claims = Claims {
        user_id: user_id.to_string(),
        role: role.to_string(),
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

pub fn role_allows_client_dev(role: &str) -> bool {
    role == "admin" || role == "superuser"
}

pub fn effective_role(role: &str) -> &str {
    if role.is_empty() {
        "user"
    } else {
        role
    }
}

pub fn bearer_claims(headers: &HeaderMap, secret: &str) -> Option<Claims> {
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = auth.strip_prefix("Bearer ")?.trim();
    verify_token(token, secret)
}
