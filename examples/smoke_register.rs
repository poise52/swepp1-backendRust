//! Локальная проверка регистрации без деплоя.
//!
//! 1) Подними Postgres и бэк: `cargo run` (или release) с `.env` и `DATABASE_URL`.
//! 2) В другом терминале:
//!    `cargo run --example smoke_register`
//!
//! Опционально: `SMOKE_API_URL=http://127.0.0.1:3000` (по умолчанию так).

use reqwest::Client;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let base = std::env::var("SMOKE_API_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".into());

    let username = format!("smoke_{suffix}");
    let email = format!("smoke_{suffix}@local.test");
    let password = "smokePass123";

    let client = Client::new();
    let url = format!("{base}/api/auth/register");
    println!("POST {url}");
    println!("body: username={username} email={email}");

    let resp = client
        .post(&url)
        .json(&json!({
            "username": username,
            "email": email,
            "password": password,
        }))
        .send()
        .await?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!("status: {status}");
    println!("body:\n{body}");

    if !status.is_success() {
        std::process::exit(1);
    }

    Ok(())
}
