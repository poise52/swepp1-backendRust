//! Публичный URL фронта для invite-ссылок: env, заголовки запроса или dev-fallback.

use axum::http::HeaderMap;

/// Берётся из `FRONTEND_URL`, если задан.
/// Иначе — из `X-Forwarded-Host` / `Host` (удобно для IP или домена за Caddy без env).
/// Если запрос идёт напрямую на API (`localhost:3000`) — возвращаем [invite_fallback] (как в dev).
pub fn resolve_invite_base_url(
    headers: &HeaderMap,
    configured_public_url: &Option<String>,
    invite_fallback: &str,
) -> String {
    if let Some(u) = configured_public_url {
        return u.trim_end_matches('/').to_string();
    }
    public_site_from_request_headers(headers).unwrap_or_else(|| invite_fallback.to_string())
}

pub fn public_site_from_request_headers(headers: &HeaderMap) -> Option<String> {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|h| h.to_str().ok())?
        .split(',')
        .next()?
        .trim();

    let host_lc = host.to_lowercase();
    // Прямой вызов API в dev (не через Vite proxy) — считаем непубличным для инвайта.
    if host_lc.starts_with("localhost:") || host_lc.starts_with("127.0.0.1:") {
        return None;
    }

    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .and_then(|p| p.split(',').next().map(str::trim))
        .unwrap_or_else(|| {
            if host_looks_like_ip(host) {
                "http"
            } else {
                "https"
            }
        });

    Some(format!("{proto}://{host}"))
}

fn host_looks_like_ip(host: &str) -> bool {
    let inner = if host.starts_with('[') {
        host.strip_prefix('[')
            .and_then(|s| s.split(']').next())
            .unwrap_or(host)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    inner.parse::<std::net::IpAddr>().is_ok()
}
