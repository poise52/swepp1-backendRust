//! Локальная проверка входа и `/me`: register → login → GET /api/auth/me.
//!
//! Username в API — максимум 20 символов (`sa_<ms>`). Email `smoke_auth_*` отличает прогон от `smoke_register`.
//!
//! ```text
//! cargo run --example smoke_auth
//! SMOKE_API_URL=http://127.0.0.1:3000 cargo run --example smoke_auth
//! ```

use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
struct LoginBody {
    token: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let base = std::env::var("SMOKE_API_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".into());

    // До 20 символов: не `smoke_auth_<ms>`. Отличаем от `smoke_register` (`smoke_<ms>`) при том же suffix.
    let username = format!("sa_{suffix}");
    let email = format!("smoke_auth_{suffix}@local.test");
    let password = "smokeAuthPass123";

    let client = Client::new();

    let reg_url = format!("{base}/api/auth/register");
    println!("POST {reg_url}");
    let reg = client
        .post(&reg_url)
        .json(&json!({
            "username": username,
            "email": email,
            "password": password,
        }))
        .send()
        .await?;
    println!("register status: {}", reg.status());
    if !reg.status().is_success() {
        eprintln!("{}", reg.text().await?);
        std::process::exit(1);
    }

    let login_url = format!("{base}/api/auth/login");
    println!("POST {login_url}");
    let login = client
        .post(&login_url)
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await?;

    let status = login.status();
    let body_text = login.text().await.unwrap_or_default();
    println!("login status: {status}");
    println!("login body:\n{body_text}");

    if !status.is_success() {
        std::process::exit(1);
    }

    let parsed: LoginBody = serde_json::from_str(&body_text)?;
    let me_url = format!("{base}/api/auth/me");
    println!("GET {me_url}");
    let me = client
        .get(&me_url)
        .header("Authorization", format!("Bearer {}", parsed.token))
        .send()
        .await?;

    let me_status = me.status();
    let me_body = me.text().await.unwrap_or_default();
    println!("me status: {me_status}");
    println!("me body:\n{me_body}");

    if !me_status.is_success() {
        std::process::exit(1);
    }

    Ok(())
}
