use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::auth::verify_token;
use crate::errors::ApiError;
use crate::public_url::resolve_invite_base_url;
use crate::minesweeper::{create_empty_board, place_mines_initial, CellDto, GameSettingsDto};
use crate::state::{AppState, OnlineEvent};

const ALGORITHM_VERSION: i32 = 1;

#[derive(Debug, Deserialize)]
pub struct CreateLobbyRequest {
    pub mode: String, // casual | ranked
    pub rows: i32,
    pub cols: i32,
    pub mines: i32,
    pub seed: Option<i32>,
    pub settings: Option<GameSettingsDto>,
}

#[derive(Debug, Deserialize)]
pub struct JoinLobbyRequest {
    #[serde(rename = "inviteCode")]
    pub invite_code: Option<String>,
    #[serde(rename = "inviteLink")]
    pub invite_link: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReadyRequest {
    pub ready: bool,
}

#[derive(Debug, Deserialize)]
pub struct MatchMoveRequest {
    pub row: i32,
    pub col: i32,
    pub action: String, // reveal | mark
}

#[derive(Debug, Deserialize)]
pub struct FinishMatchRequest {
    #[serde(rename = "winnerId")]
    pub winner_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchLobbyRequest {
    pub rows: Option<i32>,
    pub cols: Option<i32>,
    pub mines: Option<i32>,
    pub seed: Option<i32>,
}

#[derive(Debug, Serialize, FromRow, Clone)]
struct LobbyRow {
    pub id: String,
    #[sqlx(rename = "ownerId")]
    pub owner_id: String,
    #[sqlx(rename = "inviteCode")]
    pub invite_code: String,
    #[sqlx(rename = "inviteLink")]
    pub invite_link: String,
    pub mode: String,
    pub rows: i32,
    pub cols: i32,
    pub mines: i32,
    pub seed: i32,
    #[sqlx(rename = "algorithmVersion")]
    pub algorithm_version: i32,
    pub settings: serde_json::Value,
    pub status: String,
}

#[derive(Debug, Serialize, FromRow, Clone)]
struct LobbyPlayerRow {
    #[sqlx(rename = "lobbyId")]
    pub lobby_id: String,
    #[sqlx(rename = "userId")]
    pub user_id: String,
    pub username: String,
    pub ready: bool,
}

#[derive(Debug, Serialize, FromRow)]
struct MatchRow {
    pub id: String,
    #[sqlx(rename = "lobbyId")]
    pub lobby_id: String,
    #[sqlx(rename = "player1Id")]
    pub player1_id: String,
    #[sqlx(rename = "player2Id")]
    pub player2_id: String,
    #[sqlx(rename = "player1GameId")]
    pub player1_game_id: String,
    #[sqlx(rename = "player2GameId")]
    pub player2_game_id: String,
    pub seed: i32,
    pub mode: String,
    #[sqlx(rename = "algorithmVersion")]
    pub algorithm_version: i32,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct LobbyResponse {
    pub id: String,
    #[serde(rename = "ownerId")]
    pub owner_id: String,
    #[serde(rename = "inviteCode")]
    pub invite_code: String,
    #[serde(rename = "inviteLink")]
    pub invite_link: String,
    pub mode: String,
    pub rows: i32,
    pub cols: i32,
    pub mines: i32,
    pub seed: i32,
    #[serde(rename = "algorithmVersion")]
    pub algorithm_version: i32,
    pub status: String,
    pub players: Vec<LobbyPlayerView>,
}

#[derive(Debug, Serialize)]
pub struct LobbyPlayerView {
    #[serde(rename = "userId")]
    pub user_id: String,
    pub username: String,
    pub ready: bool,
}

#[derive(Debug, Serialize)]
pub struct StartMatchResponse {
    #[serde(rename = "matchId")]
    pub match_id: String,
    #[serde(rename = "myGameId")]
    pub my_game_id: String,
    #[serde(rename = "opponentGameId")]
    pub opponent_game_id: String,
    pub seed: i32,
    pub rows: i32,
    pub cols: i32,
    pub mines: i32,
    pub mode: String,
    #[serde(rename = "algorithmVersion")]
    pub algorithm_version: i32,
}

#[derive(Debug, Serialize, FromRow)]
struct OpponentStateRow {
    pub board: serde_json::Value,
    #[sqlx(rename = "gameStatus")]
    pub game_status: String,
}

pub async fn create_lobby(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateLobbyRequest>,
) -> Result<(StatusCode, Json<LobbyResponse>), ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;
    validate_params(payload.rows, payload.cols, payload.mines)?;
    if payload.mode != "casual" && payload.mode != "ranked" {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Invalid mode"));
    }

    let lobby_id = Uuid::new_v4().to_string();
    let invite_code = generate_invite_code();
    let public_base = resolve_invite_base_url(
        &headers,
        &state.configured_public_url,
        &state.invite_fallback,
    );
    let invite_link = format!("{}/?invite={invite_code}", public_base.trim_end_matches('/'));
    let seed = if payload.mode == "ranked" {
        derive_ranked_seed(&lobby_id)
    } else {
        payload.seed.unwrap_or_else(|| (Utc::now().timestamp_millis() % 1_000_000) as i32)
    };
    let settings = payload.settings.unwrap_or(GameSettingsDto {
        field_generation: if payload.mode == "ranked" {
            "safe-start".to_string()
        } else {
            "random".to_string()
        },
        show_question_marks: true,
        enable_chord: true,
        dev_mode: false,
    });

    sqlx::query(
        r#"
        INSERT INTO online_lobbies
          (id, "ownerId", "inviteCode", "inviteLink", mode, rows, cols, mines, seed, "algorithmVersion", settings, status)
        VALUES
          ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'lobby')
        "#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .bind(&invite_code)
    .bind(&invite_link)
    .bind(&payload.mode)
    .bind(payload.rows)
    .bind(payload.cols)
    .bind(payload.mines)
    .bind(seed)
    .bind(ALGORITHM_VERSION)
    .bind(serde_json::to_value(&settings).map_err(internal_error)?)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    sqlx::query(
        r#"
        INSERT INTO online_lobby_players ("lobbyId", "userId", ready)
        VALUES ($1, $2, false)
        ON CONFLICT ("lobbyId", "userId") DO NOTHING
        "#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    let lobby = get_lobby_response(&state, &lobby_id).await?;
    broadcast_lobby_event(&state, &lobby_id, "lobby_updated", &lobby).await?;
    Ok((StatusCode::CREATED, Json(lobby)))
}

pub async fn join_lobby(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<JoinLobbyRequest>,
) -> Result<Json<LobbyResponse>, ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;
    let invite_code = if let Some(code) = payload.invite_code {
        code
    } else if let Some(link) = payload.invite_link {
        extract_invite_code_from_link(&link)
            .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "Invalid invite link"))?
    } else {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Invite is required"));
    };

    let lobby_id: String = sqlx::query_scalar(
        r#"SELECT id FROM online_lobbies WHERE "inviteCode" = $1 AND status = 'lobby'"#,
    )
    .bind(&invite_code)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Lobby not found"))?;

    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM online_lobby_players WHERE "lobbyId" = $1"#,
    )
    .bind(&lobby_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;
    if count >= 2 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Lobby is full"));
    }

    sqlx::query(
        r#"
        INSERT INTO online_lobby_players ("lobbyId", "userId", ready)
        VALUES ($1, $2, false)
        ON CONFLICT ("lobbyId", "userId") DO UPDATE SET ready = false
        "#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    let lobby = get_lobby_response(&state, &lobby_id).await?;
    broadcast_lobby_event(&state, &lobby_id, "lobby_updated", &lobby).await?;
    Ok(Json(lobby))
}

pub async fn get_lobby(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
) -> Result<Json<LobbyResponse>, ApiError> {
    Ok(Json(get_lobby_response(&state, &lobby_id).await?))
}

pub async fn get_active_match(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(lobby_id): Path<String>,
) -> Result<Json<StartMatchResponse>, ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;
    let in_lobby: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM online_lobby_players WHERE "lobbyId" = $1 AND "userId" = $2"#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;
    if in_lobby == 0 {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Not in this lobby"));
    }

    let m = sqlx::query_as::<_, MatchRow>(
        r#"
        SELECT id, "lobbyId", "player1Id", "player2Id", "player1GameId", "player2GameId", seed, mode, "algorithmVersion", status
        FROM online_matches
        WHERE "lobbyId" = $1 AND status = 'active'
        LIMIT 1
        "#,
    )
    .bind(&lobby_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?;

    let Some(m) = m else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "No active match"));
    };

    if m.player1_id != user_id && m.player2_id != user_id {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Not in this match"));
    }

    let lobby = get_lobby_row(&state, &lobby_id).await?;

    let response = if m.player1_id == user_id {
        StartMatchResponse {
            match_id: m.id.clone(),
            my_game_id: m.player1_game_id.clone(),
            opponent_game_id: m.player2_game_id.clone(),
            seed: m.seed,
            rows: lobby.rows,
            cols: lobby.cols,
            mines: lobby.mines,
            mode: m.mode.clone(),
            algorithm_version: m.algorithm_version,
        }
    } else {
        StartMatchResponse {
            match_id: m.id.clone(),
            my_game_id: m.player2_game_id.clone(),
            opponent_game_id: m.player1_game_id.clone(),
            seed: m.seed,
            rows: lobby.rows,
            cols: lobby.cols,
            mines: lobby.mines,
            mode: m.mode.clone(),
            algorithm_version: m.algorithm_version,
        }
    };

    Ok(Json(response))
}

pub async fn set_ready(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(lobby_id): Path<String>,
    Json(payload): Json<ReadyRequest>,
) -> Result<Json<LobbyResponse>, ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;

    sqlx::query(
        r#"
        UPDATE online_lobby_players
        SET ready = $3
        WHERE "lobbyId" = $1 AND "userId" = $2
        "#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .bind(payload.ready)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    let lobby = get_lobby_response(&state, &lobby_id).await?;
    broadcast_lobby_event(&state, &lobby_id, "lobby_updated", &lobby).await?;
    Ok(Json(lobby))
}

pub async fn start_lobby_match(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(lobby_id): Path<String>,
) -> Result<Json<StartMatchResponse>, ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;
    let lobby = get_lobby_row(&state, &lobby_id).await?;
    if lobby.owner_id != user_id {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Only owner can start"));
    }

    let players = get_lobby_players(&state, &lobby_id).await?;
    if players.len() != 2 || players.iter().any(|p| !p.ready) {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Both players must be ready"));
    }

    let settings: GameSettingsDto = serde_json::from_value(lobby.settings.clone()).map_err(internal_error)?;
    let player1 = &players[0];
    let player2 = &players[1];
    let player1_game_id = create_game_row_for_player(&state, player1.user_id.clone(), &lobby, &settings).await?;
    let player2_game_id = create_game_row_for_player(&state, player2.user_id.clone(), &lobby, &settings).await?;
    let match_id = Uuid::new_v4().to_string();

    sqlx::query(
        r#"
        INSERT INTO online_matches
          (id, "lobbyId", "player1Id", "player2Id", "player1GameId", "player2GameId", seed, mode, "algorithmVersion", status)
        VALUES
          ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'active')
        "#,
    )
    .bind(&match_id)
    .bind(&lobby_id)
    .bind(&player1.user_id)
    .bind(&player2.user_id)
    .bind(&player1_game_id)
    .bind(&player2_game_id)
    .bind(lobby.seed)
    .bind(&lobby.mode)
    .bind(lobby.algorithm_version)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    sqlx::query(r#"UPDATE online_lobbies SET status = 'active' WHERE id = $1"#)
        .bind(&lobby_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    let response = if player1.user_id == user_id {
        StartMatchResponse {
            match_id: match_id.clone(),
            my_game_id: player1_game_id.clone(),
            opponent_game_id: player2_game_id.clone(),
            seed: lobby.seed,
            rows: lobby.rows,
            cols: lobby.cols,
            mines: lobby.mines,
            mode: lobby.mode.clone(),
            algorithm_version: lobby.algorithm_version,
        }
    } else {
        StartMatchResponse {
            match_id: match_id.clone(),
            my_game_id: player2_game_id.clone(),
            opponent_game_id: player1_game_id.clone(),
            seed: lobby.seed,
            rows: lobby.rows,
            cols: lobby.cols,
            mines: lobby.mines,
            mode: lobby.mode.clone(),
            algorithm_version: lobby.algorithm_version,
        }
    };

    broadcast_lobby_event(&state, &lobby_id, "match_started", &response).await?;
    Ok(Json(response))
}

pub async fn submit_match_move(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(match_id): Path<String>,
    Json(payload): Json<MatchMoveRequest>,
) -> Result<StatusCode, ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;
    let match_row = get_match_row(&state, &match_id).await?;

    if match_row.player1_id != user_id && match_row.player2_id != user_id {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Not in match"));
    }

    sqlx::query(
        r#"
        INSERT INTO online_match_moves (id, "matchId", "userId", row, col, action)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&match_id)
    .bind(&user_id)
    .bind(payload.row)
    .bind(payload.col)
    .bind(&payload.action)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    broadcast_lobby_event(
        &state,
        &match_row.lobby_id,
        "player_move",
        &json!({
            "matchId": match_id,
            "userId": user_id,
            "row": payload.row,
            "col": payload.col,
            "action": payload.action
        }),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn finish_match(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(match_id): Path<String>,
    Json(payload): Json<FinishMatchRequest>,
) -> Result<StatusCode, ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;
    let match_row = get_match_row(&state, &match_id).await?;
    if match_row.player1_id != user_id && match_row.player2_id != user_id {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Not in match"));
    }
    if payload.winner_id != match_row.player1_id && payload.winner_id != match_row.player2_id {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "winnerId must be a match player"));
    }

    let rows_updated = sqlx::query(r#"UPDATE online_matches SET status = 'finished' WHERE id = $1 AND status = 'active'"#)
        .bind(&match_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?
        .rows_affected();

    if rows_updated == 0 {
        return Ok(StatusCode::NO_CONTENT);
    }

    if match_row.mode == "ranked" {
        let loser_id = if payload.winner_id == match_row.player1_id {
            match_row.player2_id.clone()
        } else {
            match_row.player1_id.clone()
        };
        sqlx::query(r#"UPDATE users SET "ratingPts" = "ratingPts" + 25 WHERE id = $1"#)
            .bind(&payload.winner_id)
            .execute(&state.db)
            .await
            .map_err(internal_error)?;
        sqlx::query(r#"UPDATE users SET "ratingPts" = GREATEST(0, "ratingPts" - 15) WHERE id = $1"#)
            .bind(&loser_id)
            .execute(&state.db)
            .await
            .map_err(internal_error)?;
    }

    broadcast_lobby_event(
        &state,
        &match_row.lobby_id,
        "match_finished",
        &json!({ "matchId": match_id, "winnerId": payload.winner_id }),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn patch_lobby_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(lobby_id): Path<String>,
    Json(patch): Json<PatchLobbyRequest>,
) -> Result<Json<LobbyResponse>, ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;
    let lobby = get_lobby_row(&state, &lobby_id).await?;
    if lobby.owner_id != user_id {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Only host can edit lobby settings"));
    }
    if lobby.status != "lobby" {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Edit settings between matches while the lobby is idle",
        ));
    }

    let rows = patch.rows.unwrap_or(lobby.rows);
    let cols = patch.cols.unwrap_or(lobby.cols);
    let mines = patch.mines.unwrap_or(lobby.mines);
    validate_params(rows, cols, mines)?;

    let seed = if lobby.mode == "ranked" {
        derive_ranked_seed(&lobby_id)
    } else {
        patch.seed.unwrap_or(lobby.seed)
    };

    sqlx::query(
        r#"UPDATE online_lobbies SET rows = $2, cols = $3, mines = $4, seed = $5, "updatedAt" = NOW() WHERE id = $1"#,
    )
    .bind(&lobby_id)
    .bind(rows)
    .bind(cols)
    .bind(mines)
    .bind(seed)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    let lobby_updated = get_lobby_response(&state, &lobby_id).await?;
    broadcast_lobby_event(&state, &lobby_id, "lobby_updated", &lobby_updated).await?;
    Ok(Json(lobby_updated))
}

pub async fn prepare_lobby_next_round(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(lobby_id): Path<String>,
) -> Result<Json<LobbyResponse>, ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;
    let in_lobby: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM online_lobby_players WHERE "lobbyId" = $1 AND "userId" = $2"#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;
    if in_lobby == 0 {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Not in this lobby"));
    }

    let active: Option<String> = sqlx::query_scalar(
        r#"SELECT id FROM online_matches WHERE "lobbyId" = $1 AND status = 'active' LIMIT 1"#,
    )
    .bind(&lobby_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?;
    if active.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Match still in progress",
        ));
    }

    sqlx::query(r#"UPDATE online_lobbies SET status = 'lobby', "updatedAt" = NOW() WHERE id = $1"#)
        .bind(&lobby_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    sqlx::query(r#"UPDATE online_lobby_players SET ready = false WHERE "lobbyId" = $1"#)
        .bind(&lobby_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    let lobby = get_lobby_response(&state, &lobby_id).await?;
    broadcast_lobby_event(&state, &lobby_id, "lobby_updated", &lobby).await?;
    Ok(Json(lobby))
}

pub async fn get_opponent_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(match_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = required_user_id(&headers, &state.jwt_secret)?;
    let match_row = get_match_row(&state, &match_id).await?;
    let opponent_game_id = if match_row.player1_id == user_id {
        match_row.player2_game_id
    } else if match_row.player2_id == user_id {
        match_row.player1_game_id
    } else {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Not in match"));
    };

    let row = sqlx::query_as::<_, OpponentStateRow>(
        r#"SELECT board, "gameStatus" FROM minesweeper_games WHERE id = $1"#,
    )
    .bind(&opponent_game_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Game not found"))?;

    Ok(Json(json!({
        "opponentGameId": opponent_game_id,
        "board": row.board,
        "gameStatus": row.game_status
    })))
}

pub async fn ws_lobby(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let token = q.get("token").cloned().unwrap_or_default();
    let Some(claims) = verify_token(&token, &state.jwt_secret) else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    };
    ws.on_upgrade(move |socket| ws_client(socket, state, lobby_id, claims.user_id))
}

pub async fn ensure_online_schema(state: &AppState) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS online_lobbies (
            id TEXT PRIMARY KEY,
            "ownerId" TEXT NOT NULL,
            "inviteCode" TEXT NOT NULL UNIQUE,
            "inviteLink" TEXT NOT NULL,
            mode TEXT NOT NULL,
            rows INTEGER NOT NULL,
            cols INTEGER NOT NULL,
            mines INTEGER NOT NULL,
            seed INTEGER NOT NULL,
            "algorithmVersion" INTEGER NOT NULL,
            settings JSONB NOT NULL,
            status TEXT NOT NULL,
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
        CREATE TABLE IF NOT EXISTS online_lobby_players (
            "lobbyId" TEXT NOT NULL,
            "userId" TEXT NOT NULL,
            ready BOOLEAN NOT NULL DEFAULT false,
            "joinedAt" TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            PRIMARY KEY ("lobbyId", "userId")
        )
        "#,
    )
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS online_matches (
            id TEXT PRIMARY KEY,
            "lobbyId" TEXT NOT NULL,
            "player1Id" TEXT NOT NULL,
            "player2Id" TEXT NOT NULL,
            "player1GameId" TEXT NOT NULL,
            "player2GameId" TEXT NOT NULL,
            seed INTEGER NOT NULL,
            mode TEXT NOT NULL,
            "algorithmVersion" INTEGER NOT NULL,
            status TEXT NOT NULL,
            "createdAt" TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS online_match_moves (
            id TEXT PRIMARY KEY,
            "matchId" TEXT NOT NULL,
            "userId" TEXT NOT NULL,
            row INTEGER NOT NULL,
            col INTEGER NOT NULL,
            action TEXT NOT NULL,
            "createdAt" TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    Ok(())
}

async fn ws_client(socket: WebSocket, state: AppState, lobby_id: String, user_id: String) {
    let mut rx = subscribe_lobby_channel(&state, &lobby_id).await;
    let (mut sender, mut receiver) = socket.split();
    let state_for_dc = state.clone();
    let lobby_dc = lobby_id.clone();
    let uid_dc = user_id.clone();

    let send_task = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let text = match serde_json::to_string(&event) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if sender.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });

    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
        let _ =
            broadcast_lobby_event(&state_for_dc, &lobby_dc, "player_disconnected", &json!({ "userId": uid_dc }))
                .await;
    });

    let _ = tokio::join!(send_task, recv_task);
}

async fn broadcast_lobby_event<T: Serialize>(
    state: &AppState,
    lobby_id: &str,
    event: &str,
    payload: &T,
) -> Result<(), ApiError> {
    let sender = get_or_create_lobby_channel(state, lobby_id).await;
    let evt = OnlineEvent {
        event: event.to_string(),
        payload: serde_json::to_value(payload).map_err(internal_error)?,
    };
    let _ = sender.send(evt);
    Ok(())
}

async fn subscribe_lobby_channel(state: &AppState, lobby_id: &str) -> broadcast::Receiver<OnlineEvent> {
    let sender = get_or_create_lobby_channel(state, lobby_id).await;
    sender.subscribe()
}

async fn get_or_create_lobby_channel(state: &AppState, lobby_id: &str) -> broadcast::Sender<OnlineEvent> {
    let mut hubs = state.online_hubs.lock().await;
    if let Some(sender) = hubs.get(lobby_id) {
        return sender.clone();
    }
    let (sender, _) = broadcast::channel(256);
    hubs.insert(lobby_id.to_string(), sender.clone());
    sender
}

async fn get_lobby_response(state: &AppState, lobby_id: &str) -> Result<LobbyResponse, ApiError> {
    let lobby = get_lobby_row(state, lobby_id).await?;
    let players = get_lobby_players(state, lobby_id).await?;
    Ok(LobbyResponse {
        id: lobby.id,
        owner_id: lobby.owner_id,
        invite_code: lobby.invite_code,
        invite_link: lobby.invite_link,
        mode: lobby.mode,
        rows: lobby.rows,
        cols: lobby.cols,
        mines: lobby.mines,
        seed: lobby.seed,
        algorithm_version: lobby.algorithm_version,
        status: lobby.status,
        players: players
            .into_iter()
            .map(|p| LobbyPlayerView {
                user_id: p.user_id,
                username: p.username,
                ready: p.ready,
            })
            .collect(),
    })
}

async fn get_lobby_row(state: &AppState, lobby_id: &str) -> Result<LobbyRow, ApiError> {
    sqlx::query_as::<_, LobbyRow>(
        r#"
        SELECT id, "ownerId", "inviteCode", "inviteLink", mode, rows, cols, mines, seed, "algorithmVersion", settings, status
        FROM online_lobbies
        WHERE id = $1
        "#,
    )
    .bind(lobby_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Lobby not found"))
}

async fn get_lobby_players(state: &AppState, lobby_id: &str) -> Result<Vec<LobbyPlayerRow>, ApiError> {
    sqlx::query_as::<_, LobbyPlayerRow>(
        r#"
        SELECT lp."lobbyId", lp."userId", u.username, lp.ready
        FROM online_lobby_players lp
        JOIN users u ON u.id = lp."userId"
        WHERE lp."lobbyId" = $1
        ORDER BY lp."joinedAt" ASC
        "#,
    )
    .bind(lobby_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)
}

async fn get_match_row(state: &AppState, match_id: &str) -> Result<MatchRow, ApiError> {
    sqlx::query_as::<_, MatchRow>(
        r#"
        SELECT id, "lobbyId", "player1Id", "player2Id", "player1GameId", "player2GameId", seed, mode, "algorithmVersion", status
        FROM online_matches WHERE id = $1
        "#,
    )
    .bind(match_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Match not found"))
}

async fn create_game_row_for_player(
    state: &AppState,
    user_id: String,
    lobby: &LobbyRow,
    settings: &GameSettingsDto,
) -> Result<String, ApiError> {
    let mut board: Vec<Vec<CellDto>> = create_empty_board(lobby.rows, lobby.cols);
    place_mines_initial(&mut board, lobby.rows, lobby.cols, lobby.mines, lobby.seed);
    let game_id = Uuid::new_v4().to_string();

    sqlx::query(
        r#"
        INSERT INTO minesweeper_games
          (id, "userId", rows, cols, mines, seed, "gameStatus", board, settings, "startedAt")
        VALUES
          ($1, $2, $3, $4, $5, $6, 'idle', $7, $8, NOW())
        "#,
    )
    .bind(&game_id)
    .bind(&user_id)
    .bind(lobby.rows)
    .bind(lobby.cols)
    .bind(lobby.mines)
    .bind(lobby.seed)
    .bind(serde_json::to_value(&board).map_err(internal_error)?)
    .bind(serde_json::to_value(settings).map_err(internal_error)?)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    Ok(game_id)
}

fn required_user_id(headers: &HeaderMap, jwt_secret: &str) -> Result<String, ApiError> {
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Unauthorized"))?;
    if !auth_header.starts_with("Bearer ") {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "Unauthorized"));
    }
    let token = auth_header.trim_start_matches("Bearer ").trim();
    let claims = verify_token(token, jwt_secret).ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Invalid token"))?;
    Ok(claims.user_id)
}

fn derive_ranked_seed(lobby_id: &str) -> i32 {
    let bytes = lobby_id.as_bytes();
    let mut hash: i64 = 0;
    for b in bytes {
        hash = ((hash << 5) - hash) + (*b as i64);
    }
    (hash.unsigned_abs() % 1_000_000) as i32
}

fn generate_invite_code() -> String {
    let raw = Uuid::new_v4().simple().to_string();
    raw.chars().take(6).collect::<String>().to_uppercase()
}

fn extract_invite_code_from_link(link: &str) -> Option<String> {
    let marker = "invite=";
    let idx = link.find(marker)?;
    let code = &link[idx + marker.len()..];
    let code = code.split('&').next().unwrap_or(code).trim();
    if code.is_empty() {
        None
    } else {
        Some(code.to_uppercase())
    }
}

fn validate_params(rows: i32, cols: i32, mines: i32) -> Result<(), ApiError> {
    if rows < 5 || cols < 5 || mines < 1 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Invalid game params"));
    }
    let max_mines = ((rows * cols) as f32 * 0.8) as i32;
    if mines > max_mines {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Invalid game params"));
    }
    Ok(())
}

fn internal_error<E: std::fmt::Display>(error: E) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Internal server error: {error}"),
    )
}

use futures_util::{SinkExt, StreamExt};
