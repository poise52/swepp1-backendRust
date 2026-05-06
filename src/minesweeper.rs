use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::verify_token;
use crate::errors::ApiError;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GameStatus {
    Idle,
    Playing,
    Won,
    Lost,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CellMark {
    None,
    Flag,
    Question,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellDto {
    pub row: i32,
    pub col: i32,
    #[serde(rename = "isMine")]
    pub is_mine: bool,
    #[serde(rename = "isOpen")]
    pub is_open: bool,
    pub mark: CellMark,
    #[serde(rename = "adjacentMines")]
    pub adjacent_mines: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameSettingsDto {
    #[serde(rename = "fieldGeneration")]
    pub field_generation: String,
    #[serde(rename = "showQuestionMarks")]
    pub show_question_marks: bool,
    #[serde(rename = "enableChord")]
    pub enable_chord: bool,
    #[serde(rename = "devMode")]
    pub dev_mode: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateGameRequest {
    pub rows: i32,
    pub cols: i32,
    pub mines: i32,
    pub seed: Option<i32>,
    pub settings: Option<GameSettingsDto>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CellActionRequest {
    pub row: i32,
    pub col: i32,
}

#[derive(Debug, Serialize)]
pub struct GameStateResponse {
    #[serde(rename = "gameId")]
    pub game_id: String,
    pub board: Vec<Vec<CellDto>>,
    #[serde(rename = "gameStatus")]
    pub game_status: GameStatus,
    pub rows: i32,
    pub cols: i32,
    pub mines: i32,
    pub seed: i32,
    pub time: i32,
}

#[derive(Debug, Deserialize)]
pub struct GameStateQuery {
    #[serde(rename = "devMode")]
    pub dev_mode: Option<bool>,
}

#[derive(Debug, FromRow)]
struct GameRow {
    pub id: String,
    pub rows: i32,
    pub cols: i32,
    pub mines: i32,
    pub seed: i32,
    #[sqlx(rename = "gameStatus")]
    pub game_status: String,
    pub board: serde_json::Value,
    pub settings: serde_json::Value,
    #[sqlx(rename = "startedAt")]
    pub started_at: Option<DateTime<Utc>>,
}

pub async fn create_game(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateGameRequest>,
) -> Result<(StatusCode, Json<GameStateResponse>), ApiError> {
    if payload.rows < 5 || payload.cols < 5 || payload.mines < 1 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Invalid game params"));
    }

    let max_mines = ((payload.rows * payload.cols) as f32 * 0.8) as i32;
    if payload.mines > max_mines {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Invalid game params"));
    }

    let user_id = optional_user_id(&headers, &state.jwt_secret);
    let seed = payload.seed.unwrap_or_else(|| (Utc::now().timestamp_millis() % 1_000_000) as i32);
    let settings = payload.settings.unwrap_or(GameSettingsDto {
        field_generation: "safe-start".to_string(),
        show_question_marks: true,
        enable_chord: true,
        dev_mode: false,
    });

    let mut board = create_empty_board(payload.rows, payload.cols);
    place_mines_initial(&mut board, payload.rows, payload.cols, payload.mines, seed);

    let game_id = Uuid::new_v4().to_string();
    let row = sqlx::query_as::<_, GameRow>(
        r#"
        INSERT INTO minesweeper_games
          (id, "userId", rows, cols, mines, seed, "gameStatus", board, settings, "startedAt")
        VALUES
          ($1, $2, $3, $4, $5, $6, 'idle', $7, $8, NULL)
        RETURNING id, rows, cols, mines, seed, "gameStatus", board, settings, "startedAt"
        "#,
    )
    .bind(&game_id)
    .bind(user_id)
    .bind(payload.rows)
    .bind(payload.cols)
    .bind(payload.mines)
    .bind(seed)
    .bind(serde_json::to_value(&board).map_err(internal_error)?)
    .bind(serde_json::to_value(&settings).map_err(internal_error)?)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    build_state_response(row, settings.dev_mode)
        .map(|r| (StatusCode::CREATED, Json(r)))
}

pub async fn reveal_cell(
    State(state): State<AppState>,
    Path(game_id): Path<String>,
    Json(payload): Json<CellActionRequest>,
) -> Result<Json<GameStateResponse>, ApiError> {
    let mut row = fetch_game(&state, &game_id).await?;
    let mut board: Vec<Vec<CellDto>> = serde_json::from_value(row.board.clone()).map_err(internal_error)?;
    let settings: GameSettingsDto = serde_json::from_value(row.settings.clone()).map_err(internal_error)?;
    let mut status = parse_status(&row.game_status);
    let mut started_at = row.started_at;

    if status == GameStatus::Won || status == GameStatus::Lost {
        return build_state_response(row, settings.dev_mode).map(Json);
    }
    if payload.row < 0 || payload.col < 0 || payload.row >= row.rows || payload.col >= row.cols {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Invalid cell"));
    }

    if status == GameStatus::Idle {
        if settings.field_generation != "random" {
            relocate_mines_from_zone(&mut board, row.rows, row.cols, payload.row, payload.col);
        }
        calculate_numbers(&mut board, row.rows, row.cols);
        status = GameStatus::Playing;
        started_at = Some(Utc::now());
    }

    open_cell(
        &mut board,
        row.rows,
        row.cols,
        payload.row,
        payload.col,
        &settings,
        &mut status,
    );

    if status != GameStatus::Lost && check_win(&board) {
        status = GameStatus::Won;
        flag_all_mines(&mut board);
    }

    persist_game(&state, &game_id, &board, status, started_at).await?;
    row = fetch_game(&state, &game_id).await?;
    build_state_response(row, settings.dev_mode).map(Json)
}

pub async fn mark_cell(
    State(state): State<AppState>,
    Path(game_id): Path<String>,
    Json(payload): Json<CellActionRequest>,
) -> Result<Json<GameStateResponse>, ApiError> {
    let mut row = fetch_game(&state, &game_id).await?;
    let mut board: Vec<Vec<CellDto>> = serde_json::from_value(row.board.clone()).map_err(internal_error)?;
    let settings: GameSettingsDto = serde_json::from_value(row.settings.clone()).map_err(internal_error)?;
    let status = parse_status(&row.game_status);

    if status == GameStatus::Won || status == GameStatus::Lost {
        return build_state_response(row, settings.dev_mode).map(Json);
    }
    if payload.row < 0 || payload.col < 0 || payload.row >= row.rows || payload.col >= row.cols {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Invalid cell"));
    }

    let cell = &mut board[payload.row as usize][payload.col as usize];
    if !cell.is_open {
        cell.mark = match cell.mark {
            CellMark::None => CellMark::Flag,
            CellMark::Flag => {
                if settings.show_question_marks {
                    CellMark::Question
                } else {
                    CellMark::None
                }
            }
            CellMark::Question => CellMark::None,
        };
    }

    persist_game(&state, &game_id, &board, status, row.started_at).await?;
    row = fetch_game(&state, &game_id).await?;
    build_state_response(row, settings.dev_mode).map(Json)
}

pub async fn get_game(
    State(state): State<AppState>,
    Path(game_id): Path<String>,
    Query(query): Query<GameStateQuery>,
) -> Result<Json<GameStateResponse>, ApiError> {
    let row = fetch_game(&state, &game_id).await?;
    let dev_mode = query.dev_mode.unwrap_or(false);
    build_state_response(row, dev_mode).map(Json)
}

pub async fn delete_game(
    State(state): State<AppState>,
    Path(game_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    sqlx::query(r#"DELETE FROM minesweeper_games WHERE id = $1"#)
        .bind(game_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn ensure_minesweeper_schema(state: &AppState) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS minesweeper_games (
            id TEXT PRIMARY KEY,
            "userId" TEXT NULL,
            rows INTEGER NOT NULL,
            cols INTEGER NOT NULL,
            mines INTEGER NOT NULL,
            seed INTEGER NOT NULL,
            "gameStatus" TEXT NOT NULL,
            board JSONB NOT NULL,
            settings JSONB NOT NULL,
            "startedAt" TIMESTAMPTZ NULL,
            "createdAt" TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            "updatedAt" TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&state.db)
    .await
    .map_err(internal_error)?;
    Ok(())
}

fn create_empty_board(rows: i32, cols: i32) -> Vec<Vec<CellDto>> {
    let mut board = vec![];
    for r in 0..rows {
        let mut row = vec![];
        for c in 0..cols {
            row.push(CellDto {
                row: r,
                col: c,
                is_mine: false,
                is_open: false,
                mark: CellMark::None,
                adjacent_mines: 0,
            });
        }
        board.push(row);
    }
    board
}

fn create_seeded_random(seed: i32) -> impl FnMut() -> f64 {
    let mut state = seed as u32;
    move || {
        state = state.wrapping_add(0x6D2B79F5);
        let mut t = state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        ((t ^ (t >> 14)) as f64) / 4294967296.0
    }
}

fn place_mines_initial(board: &mut [Vec<CellDto>], rows: i32, cols: i32, mines: i32, seed: i32) {
    let mut positions = vec![];
    for r in 0..rows {
        for c in 0..cols {
            positions.push((r, c));
        }
    }
    let mut rnd = create_seeded_random(seed);
    for i in (1..positions.len()).rev() {
        let j = (rnd() * ((i + 1) as f64)).floor() as usize;
        positions.swap(i, j);
    }
    for (r, c) in positions.into_iter().take(mines as usize) {
        board[r as usize][c as usize].is_mine = true;
    }
}

fn neighbors(rows: i32, cols: i32, row: i32, col: i32) -> Vec<(i32, i32)> {
    let mut list = vec![];
    for dr in -1..=1 {
        for dc in -1..=1 {
            if dr == 0 && dc == 0 {
                continue;
            }
            let nr = row + dr;
            let nc = col + dc;
            if nr >= 0 && nr < rows && nc >= 0 && nc < cols {
                list.push((nr, nc));
            }
        }
    }
    list
}

fn relocate_mines_from_zone(board: &mut [Vec<CellDto>], rows: i32, cols: i32, click_row: i32, click_col: i32) {
    let mut mines_to_move = vec![];
    for dr in -1..=1 {
        for dc in -1..=1 {
            let nr = click_row + dr;
            let nc = click_col + dc;
            if nr >= 0 && nr < rows && nc >= 0 && nc < cols && board[nr as usize][nc as usize].is_mine {
                mines_to_move.push((nr, nc));
            }
        }
    }
    if mines_to_move.is_empty() {
        return;
    }

    let mut free = vec![];
    for r in 0..rows {
        for c in 0..cols {
            let in_zone = (r - click_row).abs() <= 1 && (c - click_col).abs() <= 1;
            if !in_zone && !board[r as usize][c as usize].is_mine {
                free.push((r, c));
            }
        }
    }
    for (idx, from) in mines_to_move.iter().enumerate() {
        if idx >= free.len() {
            break;
        }
        let to = free[idx];
        board[from.0 as usize][from.1 as usize].is_mine = false;
        board[to.0 as usize][to.1 as usize].is_mine = true;
    }
}

fn calculate_numbers(board: &mut [Vec<CellDto>], rows: i32, cols: i32) {
    for r in 0..rows {
        for c in 0..cols {
            if board[r as usize][c as usize].is_mine {
                continue;
            }
            let count = neighbors(rows, cols, r, c)
                .into_iter()
                .filter(|(nr, nc)| board[*nr as usize][*nc as usize].is_mine)
                .count() as i32;
            board[r as usize][c as usize].adjacent_mines = count;
        }
    }
}

fn reveal_empty(board: &mut [Vec<CellDto>], rows: i32, cols: i32, row: i32, col: i32) {
    let cell = &board[row as usize][col as usize];
    if cell.is_open || cell.mark == CellMark::Flag {
        return;
    }
    board[row as usize][col as usize].is_open = true;
    if !board[row as usize][col as usize].is_mine && board[row as usize][col as usize].adjacent_mines == 0 {
        for (nr, nc) in neighbors(rows, cols, row, col) {
            if !board[nr as usize][nc as usize].is_open && !board[nr as usize][nc as usize].is_mine {
                reveal_empty(board, rows, cols, nr, nc);
            }
        }
    }
}

fn reveal_all_mines(board: &mut [Vec<CellDto>], rows: i32, cols: i32) {
    for r in 0..rows {
        for c in 0..cols {
            if board[r as usize][c as usize].is_mine {
                board[r as usize][c as usize].is_open = true;
            }
        }
    }
}

fn flag_all_mines(board: &mut [Vec<CellDto>]) {
    for row in board {
        for cell in row {
            if cell.is_mine {
                cell.mark = CellMark::Flag;
            }
        }
    }
}

fn check_win(board: &[Vec<CellDto>]) -> bool {
    !board
        .iter()
        .flatten()
        .any(|c| !c.is_mine && !c.is_open)
}

fn open_cell(
    board: &mut [Vec<CellDto>],
    rows: i32,
    cols: i32,
    row: i32,
    col: i32,
    settings: &GameSettingsDto,
    status: &mut GameStatus,
) {
    let cell = board[row as usize][col as usize].clone();
    if cell.is_open && cell.adjacent_mines > 0 {
        if !settings.enable_chord {
            return;
        }
        let neigh = neighbors(rows, cols, row, col);
        let flagged = neigh
            .iter()
            .filter(|(nr, nc)| board[*nr as usize][*nc as usize].mark == CellMark::Flag)
            .count() as i32;
        let closed_unflagged: Vec<(i32, i32)> = neigh
            .iter()
            .copied()
            .filter(|(nr, nc)| {
                !board[*nr as usize][*nc as usize].is_open && board[*nr as usize][*nc as usize].mark != CellMark::Flag
            })
            .collect();
        if closed_unflagged.len() as i32 == cell.adjacent_mines - flagged && !closed_unflagged.is_empty() {
            for (nr, nc) in closed_unflagged {
                board[nr as usize][nc as usize].mark = CellMark::Flag;
            }
            return;
        }
        if flagged == cell.adjacent_mines {
            for (nr, nc) in neigh {
                if !board[nr as usize][nc as usize].is_open && board[nr as usize][nc as usize].mark != CellMark::Flag {
                    open_cell(board, rows, cols, nr, nc, settings, status);
                }
            }
        }
        return;
    }

    if cell.is_open || cell.mark == CellMark::Flag {
        return;
    }
    if cell.is_mine {
        if settings.dev_mode {
            return;
        }
        board[row as usize][col as usize].is_open = true;
        reveal_all_mines(board, rows, cols);
        *status = GameStatus::Lost;
        return;
    }
    reveal_empty(board, rows, cols, row, col);
}

fn parse_status(raw: &str) -> GameStatus {
    match raw {
        "playing" => GameStatus::Playing,
        "won" => GameStatus::Won,
        "lost" => GameStatus::Lost,
        _ => GameStatus::Idle,
    }
}

fn status_to_str(status: GameStatus) -> &'static str {
    match status {
        GameStatus::Idle => "idle",
        GameStatus::Playing => "playing",
        GameStatus::Won => "won",
        GameStatus::Lost => "lost",
    }
}

fn now_elapsed(started_at: Option<DateTime<Utc>>) -> i32 {
    started_at
        .map(|s| (Utc::now() - s).num_seconds().max(0) as i32)
        .unwrap_or(0)
}

async fn fetch_game(state: &AppState, game_id: &str) -> Result<GameRow, ApiError> {
    sqlx::query_as::<_, GameRow>(
        r#"
        SELECT id, rows, cols, mines, seed, "gameStatus", board, settings, "startedAt"
        FROM minesweeper_games
        WHERE id = $1
        "#,
    )
    .bind(game_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Game not found"))
}

async fn persist_game(
    state: &AppState,
    game_id: &str,
    board: &[Vec<CellDto>],
    status: GameStatus,
    started_at: Option<DateTime<Utc>>,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        UPDATE minesweeper_games
        SET board = $2, "gameStatus" = $3, "startedAt" = $4, "updatedAt" = NOW()
        WHERE id = $1
        "#,
    )
    .bind(game_id)
    .bind(serde_json::to_value(board).map_err(internal_error)?)
    .bind(status_to_str(status))
    .bind(started_at)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;
    Ok(())
}

fn build_state_response(row: GameRow, dev_mode: bool) -> Result<GameStateResponse, ApiError> {
    let mut board: Vec<Vec<CellDto>> = serde_json::from_value(row.board).map_err(internal_error)?;
    if !dev_mode {
        for r in &mut board {
            for c in r {
                if !c.is_open {
                    c.is_mine = false;
                }
            }
        }
    }
    Ok(GameStateResponse {
        game_id: row.id,
        board,
        game_status: parse_status(&row.game_status),
        rows: row.rows,
        cols: row.cols,
        mines: row.mines,
        seed: row.seed,
        time: now_elapsed(row.started_at),
    })
}

fn optional_user_id(headers: &HeaderMap, jwt_secret: &str) -> Option<String> {
    let auth_header = headers.get("authorization").and_then(|h| h.to_str().ok())?;
    if !auth_header.starts_with("Bearer ") {
        return None;
    }
    let token = auth_header.trim_start_matches("Bearer ").trim();
    verify_token(token, jwt_secret).map(|c| c.user_id)
}

fn internal_error<E: std::fmt::Display>(error: E) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Internal server error: {error}"),
    )
}
