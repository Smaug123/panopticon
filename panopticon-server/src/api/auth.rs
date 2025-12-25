use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;

/// Extract API key from the Authorization header.
fn extract_api_key_from_header(request: &Request<Body>) -> Option<String> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|auth_header| auth_header.strip_prefix("Bearer "))
        .map(|token| token.to_string())
}

/// Extract token from query parameter.
fn extract_token_from_query(request: &Request<Body>) -> Option<String> {
    request.uri().query().and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.into_owned())
    })
}

/// Middleware that requires a valid API key.
///
/// Supports only Authorization header: `Authorization: Bearer <token>`
/// For SSE endpoints, use `require_stream_token` instead to avoid
/// exposing the API key in query strings.
pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let provided_key = extract_api_key_from_header(&request).ok_or(StatusCode::UNAUTHORIZED)?;

    if provided_key != state.config.auth.api_key {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
}

/// Middleware that requires a valid stream token (for SSE endpoints).
///
/// Stream tokens are short-lived (30s) and single-use. They are obtained
/// by calling POST /api/stream-token with a valid API key, then used
/// in the query string for SSE connections.
///
/// This prevents the long-lived API key from appearing in URLs which
/// can leak via browser history, referrer headers, and server logs.
pub async fn require_stream_token(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = extract_token_from_query(&request).ok_or(StatusCode::UNAUTHORIZED)?;

    if !state.stream_tokens.validate_and_consume(&token).await {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
}
