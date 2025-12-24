pub mod api;
pub mod config;
pub mod db;
pub mod domain;
pub mod github;
pub mod llm;
pub mod scheduler;
pub mod static_files;

use std::sync::Arc;
use tokio::sync::broadcast;

pub use config::AppConfig;
use domain::ids::ReviewId;

/// Broadcast channel message for streaming review updates to connected clients.
#[derive(Clone, Debug)]
pub struct ReviewUpdate {
    pub review_id: ReviewId,
    pub prompt_name: String,
    pub chunk: String,
    pub is_final: bool,
}

/// Shared application state passed to all handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub config: Arc<AppConfig>,
    pub llm: Arc<dyn llm::provider::LlmProvider>,
    pub github: Arc<github::fetch::GitHubFetcher>,
    /// Broadcast channel for streaming review updates to SSE clients.
    pub review_updates: broadcast::Sender<ReviewUpdate>,
}
