use axum::middleware;
use axum::routing::{delete, get, post, put};
use axum::Router;

use crate::api::{auth, handlers, sse};
use crate::static_files::serve_static;
use crate::AppState;

pub fn create_router(state: AppState) -> Router {
    // API routes that require authentication
    let api_routes = Router::new()
        // Repos
        .route("/repos", get(handlers::list_repos))
        .route("/repos", post(handlers::create_repo))
        .route("/repos/{id}", get(handlers::get_repo))
        .route("/repos/{id}", delete(handlers::delete_repo))
        // Prompts
        .route("/repos/{id}/prompts", get(handlers::list_prompts))
        .route("/repos/{id}/prompts", post(handlers::create_prompt))
        .route("/repos/{id}/prompts/{pid}", put(handlers::update_prompt))
        .route("/repos/{id}/prompts/{pid}", delete(handlers::delete_prompt))
        // Reviews
        .route("/repos/{id}/reviews", get(handlers::list_reviews))
        .route("/repos/{id}/reviews", post(handlers::trigger_review))
        .route("/reviews/{id}", get(handlers::get_review))
        .route("/reviews/{id}/stream", get(sse::review_stream))
        // Apply auth middleware
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));

    // Public routes
    let public_routes = Router::new().route("/api/health", get(handlers::health_check));

    Router::new()
        .nest("/api", api_routes)
        .merge(public_routes)
        .fallback(serve_static)
        .with_state(state)
}
