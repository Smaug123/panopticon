use axum::http::{header, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "static/"]
struct StaticAssets;

/// Serve static files or index.html for SPA routing.
pub async fn serve_static(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try to serve the exact path
    if let Some(content) = StaticAssets::get(path) {
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();

        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime)],
            content.data.to_vec(),
        )
            .into_response();
    }

    // For paths starting with "static/", return 404 if not found
    if path.starts_with("static/") {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Otherwise, serve index.html (SPA routing)
    match StaticAssets::get("index.html") {
        Some(content) => Html(content.data.to_vec()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
