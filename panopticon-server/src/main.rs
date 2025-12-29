use std::sync::Arc;

use anyhow::Result;
use tokio::sync::broadcast;
use tracing_subscriber::EnvFilter;

use panopticon_server::api::stream_token::StreamTokenStore;
use panopticon_server::config::{AppConfig, LlmProviderConfig};
use panopticon_server::github::fetch::GitHubFetcher;
use panopticon_server::llm::openai::OpenAiProvider;
use panopticon_server::llm::provider::LlmProvider;
use panopticon_server::scheduler::runner::JobRunner;
use panopticon_server::{api, AppState};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("panopticon_server=debug,info")),
        )
        .init();

    // Load configuration
    let config = AppConfig::load().map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;
    tracing::info!("Configuration loaded");

    let config = Arc::new(config);

    // Initialize database
    let db_url = format!("sqlite:{}?mode=rwc", config.database.path.display());
    tracing::info!("Connecting to database: {}", db_url);

    let db = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;

    // Configure SQLite for better concurrency:
    // - WAL mode allows concurrent reads during writes
    // - busy_timeout waits instead of immediately failing on lock contention
    // This prevents "database is locked" errors under concurrent API + job runner load.
    sqlx::query("PRAGMA journal_mode=WAL").execute(&db).await?;
    sqlx::query("PRAGMA busy_timeout=5000").execute(&db).await?;
    tracing::debug!("SQLite WAL mode and busy timeout configured");

    // Run migrations
    tracing::info!("Running database migrations");
    sqlx::migrate!("./migrations").run(&db).await?;

    // Verify SQLite JSON1 extension is available (required for job queue queries)
    tracing::debug!("Verifying SQLite JSON1 extension availability");
    sqlx::query_scalar::<_, i64>("SELECT json_extract('{\"test\": 1}', '$.test')")
        .fetch_one(&db)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "SQLite JSON1 extension is required but not available. \
                 The JSON1 extension is needed for job queue queries. \
                 Please ensure your SQLite installation includes JSON1 support. \
                 Error: {}",
                e
            )
        })?;
    tracing::debug!("SQLite JSON1 extension verified");

    // Initialize LLM provider
    let llm_timeout = config.scheduler.llm_timeout_secs;
    let llm: Arc<dyn LlmProvider> = match &config.llm.provider {
        LlmProviderConfig::OpenAi {
            api_key,
            model,
            base_url,
            reasoning_effort,
            context_limit_tokens,
        } => Arc::new(OpenAiProvider::new(
            api_key.clone(),
            model.clone(),
            base_url.clone(),
            reasoning_effort.clone(),
            *context_limit_tokens,
            Some(llm_timeout),
        )),
    };
    tracing::info!(
        "LLM provider initialized: {} (context limit: {} tokens, timeout: {}s)",
        llm.name(),
        llm.max_context_tokens(),
        llm_timeout
    );

    // Initialize GitHub fetcher
    let github = Arc::new(GitHubFetcher::new(config.github.repos_dir.clone()));

    // Create broadcast channel for review updates (buffer 100 messages)
    let (review_updates, _) = broadcast::channel(100);

    // Create stream token store for SSE authentication
    let stream_tokens = StreamTokenStore::new();

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        llm,
        github,
        review_updates,
        stream_tokens,
    };

    // Spawn background job runner
    let runner_state = state.clone();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let runner_handle = tokio::spawn(async move {
        let runner = JobRunner::new(runner_state);
        runner.run(shutdown_rx).await;
    });

    // Create router
    let app = api::routes::create_router(state);

    // Start server
    let addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Shutdown signal received, stopping...");
            shutdown_tx.send(true).ok();
        })
        .await?;

    // Wait for job runner to complete (it handles its own graceful shutdown)
    tracing::info!("Waiting for job runner to complete...");
    if let Err(e) = runner_handle.await {
        tracing::error!("Job runner panicked: {:?}", e);
    }

    tracing::info!("Server stopped");
    Ok(())
}
