use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;

/// Extract API key from a request.
///
/// Checks in order:
/// 1. Authorization header: `Authorization: Bearer <token>`
/// 2. Query parameter: `?token=<token>` (needed for EventSource/SSE which can't send headers)
fn extract_api_key(request: &Request<Body>) -> Option<String> {
    // Try Authorization header first
    if let Some(auth_header) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
    {
        if let Some(token) = auth_header.strip_prefix("Bearer ") {
            return Some(token.to_string());
        }
    }

    // Fall back to query parameter for SSE/EventSource clients
    request.uri().query().and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.into_owned())
    })
}

/// Middleware that requires a valid API key.
///
/// Supports two authentication methods:
/// 1. Authorization header: `Authorization: Bearer <token>`
/// 2. Query parameter: `?token=<token>` (needed for EventSource/SSE which can't send headers)
pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let provided_key = extract_api_key(&request).ok_or(StatusCode::UNAUTHORIZED)?;

    if provided_key != state.config.auth.api_key {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
}
