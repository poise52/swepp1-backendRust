use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::{DateTime, Utc};
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
    #[sqlx(rename = "ratingPts")]
    #[serde(rename = "ratingPts")]
    pub rating_pts: i32,
    #[serde(rename = "worldRank")]
    pub world_rank: i64,
    #[sqlx(rename = "createdAt")]
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct InsertedUser {
    pub id: String,
    pub username: String,
    pub email: String,
    #[sqlx(rename = "ratingPts")]
    pub rating_pts: i32,
    #[sqlx(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct UserWithPassword {
    pub id: String,
    pub username: String,
    pub email: String,
    pub password: String,
    #[sqlx(rename = "ratingPts")]
    pub rating_pts: i32,
    #[sqlx(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateRecordRequest {
    pub difficulty: String,
    #[validate(range(min = 1))]
    pub rows: i32,
    #[validate(range(min = 1))]
    pub cols: i32,
    #[validate(range(min = 1))]
    pub mines: i32,
    #[validate(range(min = 1))]
    pub time: i32,
    pub seed: i32,
    pub won: bool,
}

#[derive(Debug, Deserialize)]
pub struct RecordsQuery {
    pub difficulty: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct UserRecordsQuery {
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct GameRecord {
    pub id: String,
    #[sqlx(rename = "userId")]
    #[serde(rename = "userId")]
    pub user_id: String,
    pub difficulty: String,
    pub rows: i32,
    pub cols: i32,
    pub mines: i32,
    pub time: i32,
    pub seed: i32,
    pub won: bool,
    #[sqlx(rename = "createdAt")]
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct GameRecordWithUser {
    pub id: String,
    #[sqlx(rename = "userId")]
    #[serde(rename = "userId")]
    pub user_id: String,
    pub difficulty: String,
    pub rows: i32,
    pub cols: i32,
    pub mines: i32,
    pub time: i32,
    pub seed: i32,
    pub won: bool,
    #[sqlx(rename = "createdAt")]
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    pub user: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RecordsResponse<T> {
    pub records: Vec<T>,
    pub total: i64,
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

    let inserted = sqlx::query_as::<_, InsertedUser>(
        r#"
        INSERT INTO users (id, username, email, password, "ratingPts", "updatedAt")
        VALUES ($1, $2, $3, $4, 0, NOW())
        RETURNING id, username, email, "ratingPts", "createdAt"
        "#,
    )
    .bind(&user_id)
    .bind(&payload.username)
    .bind(&payload.email)
    .bind(&hashed_password)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    let world_rank = fetch_world_rank(&state, &inserted.id, inserted.rating_pts).await?;
    let user = UserPublic {
        id: inserted.id,
        username: inserted.username,
        email: inserted.email,
        rating_pts: inserted.rating_pts,
        world_rank,
        created_at: inserted.created_at,
    };

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
        SELECT id, username, email, password, "ratingPts", "createdAt"
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

    let world_rank = fetch_world_rank(&state, &user.id, user.rating_pts).await?;
    let user_public = UserPublic {
        id: user.id,
        username: user.username,
        email: user.email,
        rating_pts: user.rating_pts,
        world_rank,
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
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let user = sqlx::query_as::<_, UserWithPassword>(
        r#"
        SELECT id, username, email, password, "ratingPts", "createdAt"
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "User not found"))?;

    let world_rank = fetch_world_rank(&state, &user.id, user.rating_pts).await?;

    Ok(Json(UserPublic {
        id: user.id,
        username: user.username,
        email: user.email,
        rating_pts: user.rating_pts,
        world_rank,
        created_at: user.created_at,
    }))
}

pub async fn create_record(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateRecordRequest>,
) -> Result<(StatusCode, Json<GameRecord>), ApiError> {
    payload.validate().map_err(|e| {
        ApiError::with_body(
            StatusCode::BAD_REQUEST,
            json!({ "message": "Validation error", "errors": e.to_string() }),
        )
    })?;

    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let record = sqlx::query_as::<_, GameRecord>(
        r#"
        INSERT INTO game_records ("id", "userId", difficulty, rows, cols, mines, time, seed, won)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id, "userId", difficulty, rows, cols, mines, time, seed, won, "createdAt"
        "#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(payload.difficulty)
    .bind(payload.rows)
    .bind(payload.cols)
    .bind(payload.mines)
    .bind(payload.time)
    .bind(payload.seed)
    .bind(payload.won)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    Ok((StatusCode::CREATED, Json(record)))
}

pub async fn get_records(
    State(state): State<AppState>,
    Query(query): Query<RecordsQuery>,
) -> Result<Json<RecordsResponse<GameRecordWithUser>>, ApiError> {
    let difficulty_filter = query.difficulty.as_deref();
    let limit = query.limit.unwrap_or(10).min(100) as i64;

    let records = if difficulty_filter.is_some() && difficulty_filter != Some("Все") {
        sqlx::query_as::<_, GameRecordWithUser>(
            r#"
            SELECT
                gr.id,
                gr."userId",
                gr.difficulty,
                gr.rows,
                gr.cols,
                gr.mines,
                gr.time,
                gr.seed,
                gr.won,
                gr."createdAt",
                json_build_object('username', u.username) as user
            FROM game_records gr
            JOIN users u ON u.id = gr."userId"
            WHERE gr.won = true AND gr.difficulty = $1
            ORDER BY gr.time ASC
            LIMIT $2
            "#,
        )
        .bind(difficulty_filter.unwrap_or_default())
        .bind(limit)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?
    } else {
        sqlx::query_as::<_, GameRecordWithUser>(
            r#"
            SELECT
                gr.id,
                gr."userId",
                gr.difficulty,
                gr.rows,
                gr.cols,
                gr.mines,
                gr.time,
                gr.seed,
                gr.won,
                gr."createdAt",
                json_build_object('username', u.username) as user
            FROM game_records gr
            JOIN users u ON u.id = gr."userId"
            WHERE gr.won = true
            ORDER BY gr.time ASC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&state.db)
        .await
        .map_err(internal_error)?
    };

    let total = if difficulty_filter.is_some() && difficulty_filter != Some("Все") {
        sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*) FROM game_records
            WHERE won = true AND difficulty = $1
            "#,
        )
        .bind(difficulty_filter.unwrap_or_default())
        .fetch_one(&state.db)
        .await
        .map_err(internal_error)?
    } else {
        sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*) FROM game_records
            WHERE won = true
            "#,
        )
        .fetch_one(&state.db)
        .await
        .map_err(internal_error)?
    };

    Ok(Json(RecordsResponse { records, total }))
}

pub async fn get_user_records(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Query(query): Query<UserRecordsQuery>,
) -> Result<Json<RecordsResponse<GameRecord>>, ApiError> {
    let limit = query.limit.unwrap_or(10).min(100) as i64;

    let records = sqlx::query_as::<_, GameRecord>(
        r#"
        SELECT id, "userId", difficulty, rows, cols, mines, time, seed, won, "createdAt"
        FROM game_records
        WHERE "userId" = $1
        ORDER BY "createdAt" DESC
        LIMIT $2
        "#,
    )
    .bind(&user_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    let total = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM game_records
        WHERE "userId" = $1
        "#,
    )
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    Ok(Json(RecordsResponse { records, total }))
}

pub async fn ensure_schema_extensions(state: &AppState) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL,
            email TEXT NOT NULL,
            password TEXT NOT NULL,
            "ratingPts" INTEGER NOT NULL DEFAULT 0,
            "createdAt" TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            "updatedAt" TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS users_email_key ON users (email)
        "#,
    )
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS users_username_key ON users (username)
        "#,
    )
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    sqlx::query(
        r#"
        ALTER TABLE users
        ADD COLUMN IF NOT EXISTS "ratingPts" INTEGER NOT NULL DEFAULT 0
        "#,
    )
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS game_records (
            id TEXT PRIMARY KEY,
            "userId" TEXT NOT NULL,
            difficulty TEXT NOT NULL,
            rows INTEGER NOT NULL,
            cols INTEGER NOT NULL,
            mines INTEGER NOT NULL,
            time INTEGER NOT NULL,
            seed INTEGER NOT NULL,
            won BOOLEAN NOT NULL,
            "createdAt" TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    Ok(())
}

fn extract_user_id(headers: &HeaderMap, jwt_secret: &str) -> Result<String, ApiError> {
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Unauthorized"))?;

    if !auth_header.starts_with("Bearer ") {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "Unauthorized"));
    }

    let token = auth_header.trim_start_matches("Bearer ").trim();
    let claims =
        verify_token(token, jwt_secret).ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Invalid token"))?;

    Ok(claims.user_id)
}

async fn fetch_world_rank(state: &AppState, user_id: &str, rating_pts: i32) -> Result<i64, ApiError> {
    let higher_count = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*) FROM users
        WHERE "ratingPts" > $1
           OR ("ratingPts" = $1 AND id < $2)
        "#,
    )
    .bind(rating_pts)
    .bind(user_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    Ok(higher_count + 1)
}

fn internal_error<E: std::fmt::Display>(error: E) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Internal server error: {error}"),
    )
}
