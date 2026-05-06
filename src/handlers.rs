use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;
use validator::Validate;

use crate::auth::{generate_token, verify_token};
use crate::errors::{ApiError, HealthResponse};
use crate::state::AppState;

#[derive(Debug, Deserialize, Validate)]
pub struct RegisterRequest {
    #[validate(length(min = 3, max = 20))]
    pub username: String,
    #[validate(email)]
    pub email: String,
    #[validate(length(min = 6))]
    pub password: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(email)]
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize, FromRow, Clone)]
pub struct UserPublic {
    pub id: String,
    pub username: String,
    pub email: String,
    #[sqlx(rename = "createdAt")]
    #[serde(rename = "createdAt")]
    pub created_at: NaiveDateTime,
}

#[derive(Debug, FromRow)]
struct UserWithPassword {
    pub id: String,
    pub username: String,
    pub email: String,
    pub password: String,
    #[sqlx(rename = "createdAt")]
    pub created_at: NaiveDateTime,
}

#[derive(Serialize)]
pub struct AuthResponse {
    user: UserPublic,
    token: String,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        timestamp: Utc::now().to_rfc3339(),
    })
}

pub async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<AuthResponse>), ApiError> {
    payload.validate().map_err(|e| {
        ApiError::with_body(
            StatusCode::BAD_REQUEST,
            json!({ "message": "Validation error", "errors": e.to_string() }),
        )
    })?;

    let existing = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(1) FROM users WHERE email = $1 OR username = $2"#,
    )
    .bind(&payload.email)
    .bind(&payload.username)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    if existing > 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Email or username already exists",
        ));
    }

    let hashed_password = hash(&payload.password, DEFAULT_COST).map_err(internal_error)?;
    let user_id = Uuid::new_v4().to_string();

    let user = sqlx::query_as::<_, UserPublic>(
        r#"
        INSERT INTO users (id, username, email, password, "updatedAt")
        VALUES ($1, $2, $3, $4, NOW())
        RETURNING id, username, email, "createdAt"
        "#,
    )
    .bind(&user_id)
    .bind(&payload.username)
    .bind(&payload.email)
    .bind(&hashed_password)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    let token = generate_token(&user.id.to_string(), &state.jwt_secret, state.jwt_exp_seconds)?;

    Ok((StatusCode::CREATED, Json(AuthResponse { user, token })))
}

pub async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    payload.validate().map_err(|e| {
        ApiError::with_body(
            StatusCode::BAD_REQUEST,
            json!({ "message": "Validation error", "errors": e.to_string() }),
        )
    })?;

    let user = sqlx::query_as::<_, UserWithPassword>(
        r#"
        SELECT id, username, email, password, "createdAt"
        FROM users
        WHERE email = $1
        "#,
    )
    .bind(&payload.email)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Invalid credentials"))?;

    let is_valid = verify(&payload.password, &user.password).map_err(internal_error)?;
    if !is_valid {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "Invalid credentials",
        ));
    }

    let token = generate_token(&user.id.to_string(), &state.jwt_secret, state.jwt_exp_seconds)?;

    let user_public = UserPublic {
        id: user.id,
        username: user.username,
        email: user.email,
        created_at: user.created_at,
    };

    Ok(Json(AuthResponse {
        user: user_public,
        token,
    }))
}

pub async fn logout() -> Json<serde_json::Value> {
    Json(json!({ "message": "Logged out successfully" }))
}

pub async fn get_current_user(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<UserPublic>, ApiError> {
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Unauthorized"))?;

    if !auth_header.starts_with("Bearer ") {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "Unauthorized"));
    }

    let token = auth_header.trim_start_matches("Bearer ").trim();
    let claims = verify_token(token, &state.jwt_secret)
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Invalid token"))?;

    let user = sqlx::query_as::<_, UserPublic>(
        r#"
        SELECT id, username, email, "createdAt"
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(&claims.user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "User not found"))?;

    Ok(Json(user))
}

fn internal_error<E: std::fmt::Display>(error: E) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Internal server error: {error}"),
    )
}
