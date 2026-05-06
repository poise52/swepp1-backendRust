mod auth;
mod config;
mod errors;
mod handlers;
mod middleware;
mod minesweeper;
mod state;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use axum::Router;
use dotenvy::dotenv;
use http::{header, HeaderValue, Method};
use sqlx::postgres::PgPoolOptions;
use tower_http::cors::CorsLayer;

use crate::config::Config;
use crate::handlers::{
    create_record, ensure_schema_extensions, get_current_user, get_records, get_user_records, health, login, logout,
    register,
};
use crate::middleware::rate_limit;
use crate::minesweeper::{
    create_game, delete_game, ensure_minesweeper_schema, get_game, mark_cell, reveal_cell,
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

    let state = AppState {
        db,
        jwt_secret: config.jwt_secret,
        jwt_exp_seconds: config.jwt_expires_in_seconds,
        rate_limit: Arc::new(tokio::sync::Mutex::new(RateLimitState::new())),
    };
    if let Err(err) = ensure_schema_extensions(&state).await {
        return Err(std::io::Error::other(err.body.to_string()).into());
    }
    if let Err(err) = ensure_minesweeper_schema(&state).await {
        return Err(std::io::Error::other(err.body.to_string()).into());
    }

    let mut origins = vec![
        HeaderValue::from_static("http://localhost:5173"),
        HeaderValue::from_static("https://poise52.github.io"),
    ];
    if let Some(frontend_url) = &config.frontend_url {
        if let Ok(value) = HeaderValue::from_str(frontend_url) {
            origins.push(value);
        }
    }

    let cors = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
        .allow_credentials(true)
        .allow_origin(origins);

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

    let app = Router::new()
        .route("/health", get(health))
        .nest("/api/auth", auth_routes)
        .nest("/api/records", records_routes)
        .nest("/api/minesweeper", minesweeper_routes)
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
