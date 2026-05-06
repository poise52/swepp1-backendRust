//! Искусственная нагрузка: N независимых лобби (по 2 игрока), старт матча, несколько ходов.
//!
//! ## Подготовка
//! 1. Запустите Postgres и сервер бэкенда.
//! 2. Для больших N (сотни регистраций + тысячи запросов) отключите rate limit:
//!    `DISABLE_RATE_LIMIT=1`
//!
//! ## Запуск
//! ```text
//! DISABLE_RATE_LIMIT=1 STRESS_LOBBIES=500 STRESS_CONCURRENCY=40 \
//!   cargo run --example stress_online --release
//! ```
//!
//! Переменные окружения:
//! - `STRESS_BASE_URL` — по умолчанию `http://127.0.0.1:3000`
//! - `STRESS_LOBBIES` — число параллельных лобби (по 2 пользователя на лобби), по умолчанию `20`
//! - `STRESS_CONCURRENCY` — сколько лобби готовится одновременно, по умолчанию `10`
//! - `STRESS_MOVES` — сколько последовательных открытий клетки у хоста после старта, по умолчанию `6`
//!
//! Память/CPU смотрите на VPS: `htop`, `RSS` процесса, `postgresql` в топе.

use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

#[derive(Debug, Deserialize)]
struct AuthBody {
    token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LobbyBody {
    id: String,
    invite_code: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartBody {
    match_id: String,
    my_game_id: String,
}

fn env_usize(key: &'static str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

async fn fetch_token(client: &Client, base: &str, slot: u64) -> Result<String, String> {
    let username = format!("st{slot}");
    let email = format!("stress{slot}@local.test");
    let password = "StressTestPass1";

    let reg = client
        .post(format!("{base}/api/auth/register"))
        .json(&json!({
            "username": username,
            "email": email,
            "password": password,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if reg.status() == StatusCode::CREATED {
        let body: AuthBody = reg.json().await.map_err(|e| e.to_string())?;
        return Ok(body.token);
    }

    let login = client
        .post(format!("{base}/api/auth/login"))
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !login.status().is_success() {
        return Err(format!(
            "auth slot {slot}: register {:?}, login {:?}",
            reg.status(),
            login.status()
        ));
    }

    let body: AuthBody = login.json().await.map_err(|e| e.to_string())?;
    Ok(body.token)
}

async fn simulate_lobby(
    client: &Client,
    base: &str,
    lobby_index: u64,
    moves: usize,
) -> Result<(), String> {
    let u_a = lobby_index * 2;
    let u_b = lobby_index * 2 + 1;

    let token_a = fetch_token(client, base, u_a).await?;
    let token_b = fetch_token(client, base, u_b).await?;

    let create = client
        .post(format!("{base}/api/online/lobbies"))
        .header("Authorization", format!("Bearer {token_a}"))
        .json(&json!({
            "mode": "casual",
            "rows": 9,
            "cols": 9,
            "mines": 10,
            "settings": {
                "fieldGeneration": "safe-start",
                "showQuestionMarks": true,
                "enableChord": false,
                "devMode": false
            }
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !create.status().is_success() {
        return Err(format!(
            "lobby {lobby_index} create {:?}",
            create.status()
        ));
    }

    let lobby: LobbyBody = create.json().await.map_err(|e| e.to_string())?;

    let join = client
        .post(format!("{base}/api/online/lobbies/join"))
        .header("Authorization", format!("Bearer {token_b}"))
        .json(&json!({ "inviteCode": lobby.invite_code }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !join.status().is_success() {
        return Err(format!("lobby {lobby_index} join {:?}", join.status()));
    }

    for (tok, _) in [(token_a.clone(), "a"), (token_b.clone(), "b")] {
        let ready = client
            .post(format!(
                "{}/api/online/lobbies/{}/ready",
                base, lobby.id
            ))
            .header("Authorization", format!("Bearer {tok}"))
            .json(&json!({ "ready": true }))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !ready.status().is_success() {
            return Err(format!(
                "lobby {lobby_index} ready {:?}",
                ready.status()
            ));
        }
    }

    let start = client
        .post(format!(
            "{}/api/online/lobbies/{}/start",
            base, lobby.id
        ))
        .header("Authorization", format!("Bearer {token_a}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !start.status().is_success() {
        return Err(format!("lobby {lobby_index} start {:?}", start.status()));
    }

    let started: StartBody = start.json().await.map_err(|e| e.to_string())?;

    let coords: [(i32, i32); 9] = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (2, 0),
        (2, 1),
        (3, 0),
        (4, 0),
    ];

    for k in 0..moves.min(coords.len()) {
        let (row, col) = coords[k];
        let reveal = client
            .post(format!(
                "{}/api/minesweeper/games/{}/reveal",
                base, started.my_game_id
            ))
            .header("Authorization", format!("Bearer {token_a}"))
            .json(&json!({ "row": row, "col": col }))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !reveal.status().is_success() {
            return Err(format!(
                "lobby {lobby_index} reveal {row},{col} {:?}",
                reveal.status()
            ));
        }

        let mv = client
            .post(format!(
                "{}/api/online/matches/{}/moves",
                base, started.match_id
            ))
            .header("Authorization", format!("Bearer {token_a}"))
            .json(&json!({
                "row": row,
                "col": col,
                "action": "reveal",
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if mv.status() != StatusCode::NO_CONTENT {
            return Err(format!(
                "lobby {lobby_index} match move {:?}",
                mv.status()
            ));
        }
    }

    let opp = client
        .get(format!(
            "{}/api/online/matches/{}/opponent-state",
            base, started.match_id
        ))
        .header("Authorization", format!("Bearer {token_a}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !opp.status().is_success() {
        return Err(format!(
            "lobby {lobby_index} opponent-state {:?}",
            opp.status()
        ));
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let base = std::env::var("STRESS_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
    let lobbies = env_usize("STRESS_LOBBIES", 20);
    let concurrency = env_usize("STRESS_CONCURRENCY", 10).max(1);
    let moves = env_usize("STRESS_MOVES", 6);

    let client = Client::builder()
        .pool_max_idle_per_host(concurrency + 8)
        .timeout(Duration::from_secs(120))
        .build()?;

    let sem = Arc::new(Semaphore::new(concurrency));
    let ok = Arc::new(Mutex::new(0u64));
    let fail = Arc::new(Mutex::new(0u64));

    println!(
        "stress_online: base={base} lobbies={lobbies} concurrency={concurrency} moves={moves}\n\
         hint: for large lobbies set DISABLE_RATE_LIMIT=1 on the server"
    );

    let t0 = Instant::now();
    let mut next = 0u64;
    let mut join_set = JoinSet::new();

    loop {
        while join_set.len() < concurrency && next < lobbies as u64 {
            let idx = next;
            next += 1;
            let client = client.clone();
            let base = base.clone();
            let sem = sem.clone();
            let ok = ok.clone();
            let fail = fail.clone();

            join_set.spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                match simulate_lobby(&client, &base, idx, moves).await {
                    Ok(()) => {
                        *ok.lock().await += 1;
                    }
                    Err(e) => {
                        eprintln!("ERR lobby {idx}: {e}");
                        *fail.lock().await += 1;
                    }
                }
            });
        }

        if join_set.join_next().await.is_none() {
            break;
        }
    }

    let elapsed = t0.elapsed();
    let ok_n = *ok.lock().await;
    let fail_n = *fail.lock().await;

    println!(
        "done in {:?}: ok={ok_n} fail={fail_n} ({} users touched)",
        elapsed,
        lobbies * 2
    );

    Ok(())
}
