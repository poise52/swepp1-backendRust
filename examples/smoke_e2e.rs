//! Локальный «проход» по основным HTTP-ручкам: без VPS.
//!
//! Цепочка: `health` → регистрация/логин/`me` → таблица рекордов → сапёр → онлайн-лобби
//! (2 пользователя, ready, старт матча, ход, `opponent-state`). WebSocket здесь не тестируется
//! (см. `stress_online`).
//!
//! ```text
//! # сервер с Postgres:
//! cargo run
//!
//! # другой терминал:
//! cargo run --example smoke_e2e
//!
//! SMOKE_API_URL=http://127.0.0.1:3000 cargo run --example smoke_e2e
//! ```
//!
//! При частых прогонах при желании: `DISABLE_RATE_LIMIT=1` на стороне сервера.

use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
struct AuthBody {
    user: UserStub,
    token: String,
}

#[derive(Debug, Deserialize)]
struct UserStub {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MinesweeperCreate {
    game_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LobbyShort {
    id: String,
    invite_code: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartMatch {
    match_id: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let base = std::env::var("SMOKE_API_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".into());

    // username ≤ 20 символов
    let user_a = format!("ha_{suffix}");
    let user_b = format!("hb_{suffix}");
    let email_a = format!("e2e_a_{suffix}@local.test");
    let email_b = format!("e2e_b_{suffix}@local.test");
    let password = "SmokeE2ePass9";

    let client = Client::new();

    macro_rules! ok {
        ($step:expr, $res:expr) => {{
            let r = $res;
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            if !status.is_success() {
                eprintln!("FAIL: {} → {} {}", $step, status, text);
                process::exit(1);
            }
            text
        }};
    }

    // --- health ---
    {
        let r = client.get(format!("{base}/health")).send().await?;
        let _ = ok!("GET /health", r);
        println!("ok: GET /health");
    }

    // --- register A, login A, me ---
    let (id_a, _) = {
        let r = client
            .post(format!("{base}/api/auth/register"))
            .json(&json!({
                "username": user_a,
                "email": email_a,
                "password": password,
            }))
            .send()
            .await?;
        let body = ok!("POST /api/auth/register (A)", r);
        let parsed: AuthBody = serde_json::from_str(&body)?;
        (parsed.user.id.clone(), parsed.token)
    };
    println!("ok: register A → {}", id_a);

    let token_a_login = {
        let r = client
            .post(format!("{base}/api/auth/login"))
            .json(&json!({ "email": email_a, "password": password }))
            .send()
            .await?;
        let body = ok!("POST /api/auth/login (A)", r);
        let parsed: AuthBody = serde_json::from_str(&body)?;
        parsed.token
    };
    println!("ok: login A");

    {
        let r = client
            .get(format!("{base}/api/auth/me"))
            .header("Authorization", format!("Bearer {token_a_login}"))
            .send()
            .await?;
        let _ = ok!("GET /api/auth/me (A)", r);
        println!("ok: GET /api/auth/me (A)");
    }

    // --- records ---
    {
        let r = client
            .post(format!("{base}/api/records"))
            .header("Authorization", format!("Bearer {token_a_login}"))
            .json(&json!({
                "difficulty": "Easy",
                "rows": 9,
                "cols": 9,
                "mines": 10,
                "time": 120,
                "seed": 42,
                "won": true
            }))
            .send()
            .await?;
        let _ = ok!("POST /api/records", r);
        println!("ok: POST /api/records");
    }

    {
        let r = client
            .get(format!("{base}/api/records?limit=5"))
            .send()
            .await?;
        let _ = ok!("GET /api/records", r);
        println!("ok: GET /api/records");
    }

    {
        let r = client
            .get(format!("{base}/api/records/user/{id_a}?limit=5"))
            .send()
            .await?;
        let _ = ok!("GET /api/records/user/:userId", r);
        println!("ok: GET /api/records/user/:userId");
    }

    // --- minesweeper ---
    let ms_game = {
        let r = client
            .post(format!("{base}/api/minesweeper/games"))
            .header("Authorization", format!("Bearer {token_a_login}"))
            .json(&json!({
                "rows": 5,
                "cols": 5,
                "mines": 5,
                "seed": 999001
            }))
            .send()
            .await?;
        let body = ok!("POST /api/minesweeper/games", r);
        let parsed: MinesweeperCreate = serde_json::from_str(&body)?;
        println!("ok: POST /api/minesweeper/games → {}", parsed.game_id);
        parsed.game_id
    };

    {
        let r = client
            .get(format!("{base}/api/minesweeper/games/{ms_game}"))
            .send()
            .await?;
        let _ = ok!("GET /api/minesweeper/games/:id", r);
        println!("ok: GET /api/minesweeper/games/:id");
    }

    {
        let r = client
            .post(format!("{base}/api/minesweeper/games/{ms_game}/reveal"))
            .header("Authorization", format!("Bearer {token_a_login}"))
            .json(&json!({ "row": 0, "col": 0 }))
            .send()
            .await?;
        let _ = ok!("POST /api/minesweeper/games/:id/reveal", r);
        println!("ok: POST reveal");
    }

    {
        let r = client
            .delete(format!("{base}/api/minesweeper/games/{ms_game}"))
            .send()
            .await?;
        let _ = ok!("DELETE /api/minesweeper/games/:id", r);
        println!("ok: DELETE minesweeper game");
    }

    // --- register B, login B ---
    let token_b = {
        let r = client
            .post(format!("{base}/api/auth/register"))
            .json(&json!({
                "username": user_b,
                "email": email_b,
                "password": password,
            }))
            .send()
            .await?;
        let _ = ok!("POST /api/auth/register (B)", r);
        let r = client
            .post(format!("{base}/api/auth/login"))
            .json(&json!({ "email": email_b, "password": password }))
            .send()
            .await?;
        let body = ok!("POST /api/auth/login (B)", r);
        let parsed: AuthBody = serde_json::from_str(&body)?;
        println!("ok: register+login B");
        parsed.token
    };

    // --- online lobby ---
    let lobby = {
        let r = client
            .post(format!("{base}/api/online/lobbies"))
            .header("Authorization", format!("Bearer {token_a_login}"))
            .json(&json!({
                "mode": "casual",
                "rows": 5,
                "cols": 5,
                "mines": 5,
                "seed": 4242
            }))
            .send()
            .await?;
        let body = ok!("POST /api/online/lobbies", r);
        let parsed: LobbyShort = serde_json::from_str(&body)?;
        println!("ok: create lobby {}", parsed.id);
        parsed
    };

    {
        let r = client
            .get(format!("{base}/api/online/lobbies/{}", lobby.id))
            .send()
            .await?;
        let _ = ok!("GET /api/online/lobbies/:id (public)", r);
        println!("ok: GET lobby");
    }

    {
        let r = client
            .post(format!("{base}/api/online/lobbies/join"))
            .header("Authorization", format!("Bearer {token_b}"))
            .json(&json!({ "inviteCode": lobby.invite_code }))
            .send()
            .await?;
        let _ = ok!("POST /api/online/lobbies/join", r);
        println!("ok: join lobby");
    }

    {
        let r = client
            .post(format!(
                "{base}/api/online/lobbies/{}/ready",
                lobby.id
            ))
            .header("Authorization", format!("Bearer {token_a_login}"))
            .json(&json!({ "ready": true }))
            .send()
            .await?;
        let _ = ok!("POST ready (A)", r);
        let r = client
            .post(format!(
                "{base}/api/online/lobbies/{}/ready",
                lobby.id
            ))
            .header("Authorization", format!("Bearer {token_b}"))
            .json(&json!({ "ready": true }))
            .send()
            .await?;
        let _ = ok!("POST ready (B)", r);
        println!("ok: both ready");
    }

    let match_id = {
        let r = client
            .post(format!(
                "{base}/api/online/lobbies/{}/start",
                lobby.id
            ))
            .header("Authorization", format!("Bearer {token_a_login}"))
            .send()
            .await?;
        let body = ok!("POST /api/online/lobbies/:id/start", r);
        let parsed: StartMatch = serde_json::from_str(&body)?;
        println!("ok: start match {}", parsed.match_id);
        parsed.match_id
    };

    {
        let r = client
            .post(format!(
                "{base}/api/online/matches/{match_id}/moves"
            ))
            .header("Authorization", format!("Bearer {token_a_login}"))
            .json(&json!({ "row": 0, "col": 0, "action": "reveal" }))
            .send()
            .await?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if status != reqwest::StatusCode::NO_CONTENT {
            eprintln!("FAIL: POST match move → {status} {text}");
            process::exit(1);
        }
        println!("ok: POST match move (204)");
    }

    {
        let r = client
            .get(format!(
                "{base}/api/online/matches/{match_id}/opponent-state"
            ))
            .header("Authorization", format!("Bearer {token_b}"))
            .send()
            .await?;
        let _ = ok!("GET opponent-state (B)", r);
        println!("ok: GET opponent-state");
    }

    {
        let r = client.post(format!("{base}/api/auth/logout")).send().await?;
        let _ = ok!("POST /api/auth/logout", r);
        println!("ok: POST /api/auth/logout");
    }

    println!("\n--- smoke_e2e: all steps passed ---");
    Ok(())
}
