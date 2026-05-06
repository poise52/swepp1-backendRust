mod auth;
mod config;
mod errors;
mod handlers;
mod middleware;
mod minesweeper;
mod online;
mod public_url;
mod state;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::middleware::from_fn_with_state;
use axum::routing::{get, patch, post};
use axum::Router;
use axum::http::request::Parts;
use dotenvy::dotenv;
use http::{header, HeaderValue, Method};
use sqlx::postgres::PgPoolOptions;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::config::Config;
use crate::handlers::{
    create_record, ensure_schema_extensions, get_current_user, get_records, get_user_records, health, login, logout,
    register,
};
use crate::middleware::rate_limit;
use crate::minesweeper::{
    create_game, delete_game, ensure_minesweeper_schema, get_game, mark_cell, reveal_cell,
};
use crate::online::{
    create_lobby, ensure_online_schema, finish_match, get_active_match, get_lobby, get_opponent_state,
    join_lobby, patch_lobby_settings, prepare_lobby_next_round, set_ready, start_lobby_match,
    submit_match_move, ws_lobby,
};
use crate::state::{AppState, RateLimitState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    let config = Config::from_env();
    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await?;

    let configured_public_url = config
        .frontend_url
        .as_ref()
        .map(|s| s.trim_end_matches('/').to_string());
    let invite_fallback = "http://localhost:5173".to_string();

    let static_origins: [&str; 2] = ["http://localhost:5173", "https://poise52.github.io"];
    let configured_for_cors = configured_public_url.clone();
    let cors = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
        .allow_credentials(true)
        .allow_origin(AllowOrigin::predicate(
            move |origin: &HeaderValue, _parts: &Parts| {
                let Ok(s) = origin.to_str() else {
                    return false;
                };
                for a in &static_origins {
                    if *a == s {
                        return true;
                    }
                }
                if let Some(ref c) = configured_for_cors {
                    return c == s;
                }
                s.starts_with("http://") || s.starts_with("https://")
            },
        ));

    let state = AppState {
        db,
        jwt_secret: config.jwt_secret,
        jwt_exp_seconds: config.jwt_expires_in_seconds,
        configured_public_url,
        invite_fallback,
        rate_limit: Arc::new(tokio::sync::Mutex::new(RateLimitState::new())),
        online_hubs: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
    };
    if let Err(err) = ensure_schema_extensions(&state).await {
        return Err(std::io::Error::other(err.body.to_string()).into());
    }
    if let Err(err) = ensure_minesweeper_schema(&state).await {
        return Err(std::io::Error::other(err.body.to_string()).into());
    }
    if let Err(err) = ensure_online_schema(&state).await {
        return Err(std::io::Error::other(err.body.to_string()).into());
    }

    let auth_routes = Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", get(get_current_user));
    let records_routes = Router::new()
        .route("/", post(create_record).get(get_records))
        .route("/user/:userId", get(get_user_records));
    let minesweeper_routes = Router::new()
        .route("/games", post(create_game))
        .route("/games/:gameId", get(get_game).delete(delete_game))
        .route("/games/:gameId/reveal", post(reveal_cell))
        .route("/games/:gameId/mark", post(mark_cell));
    let online_routes = Router::new()
        .route("/lobbies", post(create_lobby))
        .route("/lobbies/join", post(join_lobby))
        .route("/lobbies/:lobbyId", get(get_lobby))
        .route("/lobbies/:lobbyId/active-match", get(get_active_match))
        .route("/lobbies/:lobbyId/settings", patch(patch_lobby_settings))
        .route("/lobbies/:lobbyId/next-round", post(prepare_lobby_next_round))
        .route("/lobbies/:lobbyId/ready", post(set_ready))
        .route("/lobbies/:lobbyId/start", post(start_lobby_match))
        .route("/lobbies/:lobbyId/ws", get(ws_lobby))
        .route("/matches/:matchId/moves", post(submit_match_move))
        .route("/matches/:matchId/finish", post(finish_match))
        .route("/matches/:matchId/opponent-state", get(get_opponent_state));

    let app = Router::new()
        .route("/health", get(health))
        .nest("/api/auth", auth_routes)
        .nest("/api/records", records_routes)
        .nest("/api/minesweeper", minesweeper_routes)
        .nest("/api/online", online_routes)
        .layer(from_fn_with_state(state.clone(), rate_limit))
        .layer(cors)
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("Server running on port {}", config.port);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
