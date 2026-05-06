use std::env;

#[derive(Clone)]
pub struct Config {
    pub port: u16,
    pub database_url: String,
    pub jwt_secret: String,
    pub jwt_expires_in_seconds: u64,
    pub frontend_url: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(3000);

        let database_url =
            env::var("DATABASE_URL").expect("DATABASE_URL is required to start backend");
        let jwt_secret = env::var("JWT_SECRET").unwrap_or_else(|_| "your-secret-key".to_string());
        let jwt_expires_in_raw = env::var("JWT_EXPIRES_IN").unwrap_or_else(|_| "7d".to_string());
        let jwt_expires_in_seconds = parse_duration_seconds(&jwt_expires_in_raw).unwrap_or(7 * 24 * 60 * 60);
        let frontend_url = env::var("FRONTEND_URL").ok();

        Self {
            port,
            database_url,
            jwt_secret,
            jwt_expires_in_seconds,
            frontend_url,
        }
    }
}

fn parse_duration_seconds(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (num_part, unit) = trimmed.split_at(trimmed.len().saturating_sub(1));
    let value = num_part.parse::<u64>().ok()?;

    match unit {
        "s" => Some(value),
        "m" => Some(value * 60),
        "h" => Some(value * 60 * 60),
        "d" => Some(value * 24 * 60 * 60),
        _ => trimmed.parse::<u64>().ok(),
    }
}
