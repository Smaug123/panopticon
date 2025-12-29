use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::interval;

use crate::db::{jobs, prompts, repos, reviews};
use crate::domain::ids::{RepoId, ReviewId};
use crate::domain::job::{Job, JobPayload, NewJob};
use crate::domain::review::{NewReview, ReviewTrigger};
use crate::github::concat::{chunk_contents, ChunkingStrategy};
use crate::github::filter::FileFilter;
use crate::llm::provider::{LlmError, LlmRequest};
use crate::llm::schema::build_system_prompt;
use crate::AppState;
use crate::ReviewUpdate;
use crate::ReviewUpdateKind;

/// Extract retry_after_secs from an error if it's a rate limit error.
/// Looks through the error chain for LlmError::RateLimited.
fn extract_retry_after(err: &anyhow::Error) -> Option<u32> {
    // Check if the error chain contains a rate limit error
    for cause in err.chain() {
        if let Some(LlmError::RateLimited { retry_after_secs }) = cause.downcast_ref::<LlmError>() {
            return Some(*retry_after_secs);
        }
    }
    None
}

/// Guard that ensures a review is marked as failed if dropped without being disarmed.
///
/// This prevents reviews from getting stuck in "in_progress" state when errors
/// occur after `mark_in_progress` is called. The guard should be created after
/// marking a review as in_progress, and disarmed only after successfully
/// completing all operations.
///
/// On cleanup, also updates `last_checked_at` for the repo to prevent immediate
/// rescheduling after max retries are exhausted.
struct ReviewCleanupGuard {
    db: SqlitePool,
    repo_id: RepoId,
    review_id: ReviewId,
    review_updates: tokio::sync::broadcast::Sender<ReviewUpdate>,
    /// If true, the guard will mark the review as failed on drop.
    /// Set to false when the review completes successfully.
    should_cleanup: bool,
    /// The error message to use if cleanup is needed.
    /// This preserves the actual error instead of using a generic message.
    error_message: Option<String>,
}

impl ReviewCleanupGuard {
    fn new(
        db: SqlitePool,
        repo_id: RepoId,
        review_id: ReviewId,
        review_updates: tokio::sync::broadcast::Sender<ReviewUpdate>,
    ) -> Self {
        Self {
            db,
            repo_id,
            review_id,
            review_updates,
            should_cleanup: true,
            error_message: None,
        }
    }

    /// Set the error message to use if cleanup is triggered.
    /// This allows preserving the actual error instead of a generic message.
    fn set_error(&mut self, error: impl Into<String>) {
        self.error_message = Some(error.into());
    }

    /// Check a result and capture any error before propagating.
    /// Use this instead of `?` to preserve the actual error message in the review.
    ///
    /// Example: `let value = guard.check(fallible_operation().await)?;`
    fn check<T, E: std::fmt::Display>(&mut self, result: Result<T, E>) -> Result<T, E> {
        if let Err(ref e) = result {
            self.set_error(e.to_string());
        }
        result
    }

    /// Disarm the guard - the review completed successfully, no cleanup needed.
    fn disarm(mut self) {
        self.should_cleanup = false;
    }

    /// Helper to broadcast ReviewComplete event with results.
    fn broadcast_complete(&self, results: serde_json::Value) {
        let _ = self.review_updates.send(ReviewUpdate {
            review_id: self.review_id,
            prompt_name: String::new(),
            kind: ReviewUpdateKind::ReviewComplete { results },
        });
    }
}

impl Drop for ReviewCleanupGuard {
    fn drop(&mut self) {
        if self.should_cleanup {
            // We're being dropped without completing successfully.
            // Mark the review as failed to prevent it being stuck in_progress.
            // Also update last_checked_at to prevent immediate rescheduling.
            //
            // Since Drop is sync but our DB operations are async, we spawn a
            // blocking task to handle the cleanup. This is acceptable because:
            // 1. We're in an error path anyway
            // 2. The alternative (stuck review) is worse
            let db = self.db.clone();
            let repo_id = self.repo_id;
            let review_id = self.review_id;
            let review_updates = self.review_updates.clone();
            // Use the actual error if set, otherwise fall back to generic message
            let error_msg = self
                .error_message
                .take()
                .unwrap_or_else(|| "Review interrupted: process error or early return".to_string());

            // Use tokio::spawn to run cleanup asynchronously
            tokio::spawn(async move {
                // Update last_checked_at to prevent immediate rescheduling after max retries.
                // This covers all failure paths (git fetch, get_contents, prompt failures, etc.)
                if let Err(e) = repos::update_last_checked(&db, repo_id).await {
                    tracing::error!(
                        "Failed to update last_checked_at for repo {} during cleanup: {}",
                        repo_id.into_inner(),
                        e
                    );
                }

                if let Err(e) = reviews::mark_failed(&db, review_id, &error_msg).await {
                    tracing::error!(
                        "Failed to mark review {} as failed during cleanup: {}",
                        review_id.into_inner(),
                        e
                    );
                } else {
                    tracing::warn!(
                        "Review {} marked as failed: {}",
                        review_id.into_inner(),
                        error_msg
                    );
                }

                // Broadcast ReviewComplete after DB update
                // Fetch results to include in the broadcast for consistent SSE payload
                let results = match reviews::get_by_id(&db, review_id).await {
                    Ok(Some(review)) => serde_json::to_value(&review.results).unwrap_or_default(),
                    _ => serde_json::Value::Array(vec![]),
                };
                let _ = review_updates.send(ReviewUpdate {
                    review_id,
                    prompt_name: String::new(),
                    kind: ReviewUpdateKind::ReviewComplete { results },
                });
            });
        }
    }
}

/// Background job runner that processes the job queue.
///
/// Jobs are spawned as background tasks, allowing the runner to remain responsive
/// and process multiple jobs concurrently (up to max_concurrent_jobs).
pub struct JobRunner {
    state: AppState,
    /// Semaphore to limit concurrent jobs
    semaphore: Arc<Semaphore>,
    /// Maximum concurrent jobs
    max_concurrent: usize,
}

impl JobRunner {
    pub fn new(state: AppState) -> Self {
        let max_concurrent = state.config.scheduler.max_concurrent_jobs;
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            state,
        }
    }

    /// Run the job runner until shutdown signal is received.
    ///
    /// Jobs are spawned as background tasks and tracked in a JoinSet.
    /// On shutdown, waits up to 60s for in-flight tasks to complete before aborting.
    pub async fn run(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let poll_interval = Duration::from_secs(self.state.config.scheduler.poll_interval_secs);
        let mut ticker = interval(poll_interval);
        let mut tasks: JoinSet<()> = JoinSet::new();

        tracing::info!(
            "Job runner started, polling every {}s, max {} concurrent jobs",
            poll_interval.as_secs(),
            self.max_concurrent
        );

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.tick(&mut tasks).await;
                }
                // Reap completed tasks without blocking
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    if let Err(e) = result {
                        tracing::error!("Job task panicked: {:?}", e);
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!(
                            "Job runner shutting down, waiting for {} in-flight task(s)",
                            tasks.len()
                        );
                        break;
                    }
                }
            }
        }

        // Graceful shutdown: wait for all in-flight tasks with timeout
        let shutdown_timeout = Duration::from_secs(60);
        let deadline = tokio::time::Instant::now() + shutdown_timeout;

        while !tasks.is_empty() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::warn!(
                    "Shutdown timeout: {} task(s) still running, aborting",
                    tasks.len()
                );
                tasks.abort_all();
                break;
            }

            match tokio::time::timeout(remaining, tasks.join_next()).await {
                Ok(Some(Ok(()))) => {
                    tracing::debug!("Task completed during shutdown, {} remaining", tasks.len());
                }
                Ok(Some(Err(e))) => {
                    tracing::error!("Task panicked during shutdown: {:?}", e);
                }
                Ok(None) => break, // All tasks done
                Err(_) => {
                    tracing::warn!("Shutdown timeout reached");
                    tasks.abort_all();
                    break;
                }
            }
        }

        tracing::info!("Job runner shutdown complete");
    }

    async fn tick(&self, tasks: &mut JoinSet<()>) {
        // Reclaim jobs with stale heartbeats (e.g., from process crashes).
        // This only affects jobs whose heartbeat hasn't been updated - long-running
        // jobs that are still updating their heartbeat will not be reclaimed.
        let heartbeat_timeout = self.state.config.scheduler.heartbeat_timeout_minutes as i64;
        match jobs::reclaim_stuck(&self.state.db, heartbeat_timeout).await {
            Ok(count) if count > 0 => {
                tracing::warn!("Reclaimed {} stuck job(s) with stale heartbeats", count);
            }
            Err(e) => {
                tracing::error!("Failed to reclaim stuck jobs: {}", e);
            }
            _ => {}
        }

        // Process pending jobs (spawns tasks up to concurrency limit)
        self.process_pending_jobs(tasks).await;

        // Schedule daily reviews for repos that need them
        if let Err(e) = self.schedule_daily_reviews().await {
            tracing::error!("Failed to schedule daily reviews: {}", e);
        }
    }

    async fn process_pending_jobs(&self, tasks: &mut JoinSet<()>) {
        // Spawn jobs up to concurrency limit
        loop {
            // Try to acquire permit (non-blocking check)
            let permit = match self.semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    // At capacity, stop claiming jobs
                    tracing::debug!("At concurrency limit, not claiming more jobs");
                    break;
                }
            };

            match jobs::claim_next(&self.state.db).await {
                Ok(Some(job)) => {
                    let job_id = job.id;
                    tracing::info!("Claimed job {}: {:?}", job_id, job.payload);

                    // Clone what we need for the spawned task
                    let state = self.state.clone();
                    let git_timeout =
                        Duration::from_secs(self.state.config.scheduler.git_timeout_secs);
                    let heartbeat_interval =
                        Duration::from_secs(self.state.config.scheduler.heartbeat_interval_secs);

                    tasks.spawn(async move {
                        // Permit is held for duration of task, released on drop
                        let _permit = permit;

                        // Spawn a background task to periodically update the heartbeat.
                        // This keeps the job from being reclaimed while it's still running.
                        let db = state.db.clone();
                        let heartbeat_handle = tokio::spawn(async move {
                            let mut ticker = interval(heartbeat_interval);
                            loop {
                                ticker.tick().await;
                                if let Err(e) = jobs::update_heartbeat(&db, job_id).await {
                                    tracing::warn!(
                                        "Failed to update heartbeat for job {}: {}",
                                        job_id,
                                        e
                                    );
                                }
                            }
                        });

                        let result = Self::execute_job_static(&state, job, git_timeout).await;

                        // Stop the heartbeat task now that the job is done
                        heartbeat_handle.abort();

                        match &result {
                            Ok(()) => {
                                tracing::info!("Job {} completed successfully", job_id)
                            }
                            Err(e) => tracing::error!("Job {} failed: {}", job_id, e),
                        }
                    });
                }
                Ok(None) => {
                    // No more pending jobs, release permit and stop
                    drop(permit);
                    break;
                }
                Err(e) => {
                    tracing::error!("Failed to claim job: {}", e);
                    drop(permit);
                    break;
                }
            }
        }
    }

    /// Execute a job - static version for spawning in background tasks.
    async fn execute_job_static(
        state: &AppState,
        job: Job,
        git_timeout: Duration,
    ) -> anyhow::Result<()> {
        let result = match &job.payload {
            JobPayload::ReviewRepo { repo_id, force } => {
                Self::run_review_static(state, *repo_id, *force, git_timeout).await
            }
        };

        match result {
            Ok(()) => {
                if let Err(e) = jobs::complete(&state.db, job.id).await {
                    tracing::error!("Failed to mark job {} as complete: {}", job.id, e);
                }
                Ok(())
            }
            Err(e) => {
                let error_msg = e.to_string();

                let should_retry = job.attempts < job.max_attempts;

                // Extract retry_after from the error if it's a rate limit
                // The error chain may contain LlmError::RateLimited
                let retry_after_secs = extract_retry_after(&e);

                if let Err(db_err) = jobs::fail(
                    &state.db,
                    job.id,
                    &error_msg,
                    should_retry,
                    retry_after_secs,
                )
                .await
                {
                    tracing::error!("Failed to mark job {} as failed: {}", job.id, db_err);
                }
                Err(e)
            }
        }
    }

    /// Run a review - static version for spawning in background tasks.
    async fn run_review_static(
        state: &AppState,
        repo_id: RepoId,
        force: bool,
        git_timeout: Duration,
    ) -> anyhow::Result<()> {
        // Get repo
        let repo = repos::get_by_id(&state.db, repo_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Repo not found"))?;

        tracing::info!(
            "Running review for {}/{}",
            repo.url.owner(),
            repo.url.name()
        );

        // Fetch/update repo and get current SHA (with timeout)
        let current_sha = tokio::time::timeout(git_timeout, state.github.fetch(&repo.url))
            .await
            .map_err(|_| anyhow::anyhow!("Git fetch timed out after {:?}", git_timeout))??;

        // Check if we should skip (no changes since last review)
        if !force {
            if let Some(last_sha) = &repo.last_commit_sha {
                if last_sha == &current_sha {
                    tracing::info!("No changes since last review, skipping");
                    // Record that we checked, even though we're not doing a full review.
                    // This prevents the scheduler from re-queueing this repo every poll.
                    repos::update_last_checked(&state.db, repo_id).await?;
                    return Ok(());
                }
            }
        }

        // Get enabled prompts
        let prompts_list = prompts::list_enabled_by_repo(&state.db, repo_id).await?;
        if prompts_list.is_empty() {
            tracing::warn!("No enabled prompts for repo, skipping review");
            return Ok(());
        }

        // Create review record
        let review = reviews::create(
            &state.db,
            NewReview {
                repo_id,
                commit_sha: current_sha.clone(),
                trigger: if force {
                    ReviewTrigger::Manual
                } else {
                    ReviewTrigger::Scheduled
                },
            },
        )
        .await?;

        // Mark review as in progress
        reviews::mark_in_progress(&state.db, review.id).await?;

        // Create cleanup guard - this ensures the review is marked as failed
        // if we early-return due to any error after this point.
        // Use `cleanup_guard.check()` for fallible operations to preserve error messages.
        let mut cleanup_guard = ReviewCleanupGuard::new(
            state.db.clone(),
            repo_id,
            review.id,
            state.review_updates.clone(),
        );

        let start_time = std::time::Instant::now();

        // Get repo contents
        let filter = FileFilter::default();
        let contents = cleanup_guard.check(state.github.get_contents(&repo.url, &filter).await)?;

        tracing::info!(
            "Loaded {} files ({} bytes)",
            contents.files.len(),
            contents.total_size
        );

        // Calculate context budget, accounting for both input and output tokens.
        // The model's context window must fit: system_prompt + user_prompt + output.
        let model_context = state.llm.max_context_tokens();

        // Reserve space for output tokens. Cap at reasonable values that work across models.
        // Single-chunk reviews can have longer outputs; multi-chunk need aggregation.
        // For very small context models, we may need to reduce these further.
        let single_chunk_output = 16_000u32.min(model_context / 4);
        let multi_chunk_output = 8_000u32.min(model_context / 8);

        // Reserve ~10k tokens for system prompt and formatting overhead
        let prompt_overhead = 10_000u32.min(model_context / 10);

        // Calculate max input tokens, accounting for output and prompt overhead.
        // Use the larger output budget (single_chunk) for conservative chunk sizing.
        let input_budget = model_context
            .saturating_sub(single_chunk_output)
            .saturating_sub(prompt_overhead);

        let strategy = ChunkingStrategy::with_max_tokens(input_budget);
        let chunks = chunk_contents(&contents.files, &strategy);

        tracing::info!(
            "Split into {} chunk(s) (input budget: {} tokens, output: {}/{} tokens)",
            chunks.len(),
            input_budget,
            single_chunk_output,
            multi_chunk_output
        );

        // For each prompt, run review
        let mut had_error = false;
        for prompt in &prompts_list {
            tracing::info!("Running prompt: {}", prompt.name);

            match Self::run_prompt_review_static(
                state,
                review.id,
                prompt,
                &chunks,
                single_chunk_output,
                multi_chunk_output,
            )
            .await
            {
                Ok(output) => {
                    // Store result
                    if let Err(e) =
                        reviews::add_result(&state.db, review.id, prompt.id, &prompt.name, &output)
                            .await
                    {
                        tracing::error!("Failed to store review result: {}", e);
                        had_error = true;
                    }
                }
                Err(e) => {
                    tracing::error!("Prompt {} failed: {}", prompt.name, e);
                    had_error = true;
                }
            }
        }

        let duration = start_time.elapsed();

        if had_error {
            // Update last_checked_at even on failure to prevent immediate rescheduling.
            // Without this, failed reviews would create an infinite loop:
            // 1. Review fails, job exhausts max retries
            // 2. Scheduler sees needs_check() = true (last_checked_at unchanged)
            // 3. Scheduler creates new job immediately
            // 4. Goto 1
            repos::update_last_checked(&state.db, repo_id).await.ok();
            reviews::mark_failed(&state.db, review.id, "Some prompts failed").await?;
            // Broadcast AFTER DB update to prevent race condition where UI refreshes
            // and sees stale in_progress status. Fetch results for consistent SSE payload.
            let results = reviews::get_by_id(&state.db, review.id)
                .await
                .ok()
                .flatten()
                .map(|r| serde_json::to_value(&r.results).unwrap_or_default())
                .unwrap_or_else(|| serde_json::Value::Array(vec![]));
            cleanup_guard.broadcast_complete(results);
            // Disarm the guard since we've handled the error ourselves
            cleanup_guard.disarm();
            // Return an error so the job is marked as failed and can be retried.
            // Previously we returned Ok(()), which marked the job as completed and
            // prevented retries. The scheduler would then see this failed review as
            // "recent" and not schedule a retry for review_interval_hours.
            return Err(anyhow::anyhow!(
                "Review failed: some prompts failed (see review {} for details)",
                review.id.into_inner()
            ));
        }

        // Update repo's last commit SHA and last_checked_at BEFORE marking complete.
        // This ensures that if update_last_commit fails, the review is marked failed
        // (by the cleanup guard) and on retry we don't create a duplicate review
        // for the same commit.
        cleanup_guard.check(repos::update_last_commit(&state.db, repo_id, &current_sha).await)?;
        // Also update last_checked_at so the scheduler knows when we last checked this repo
        cleanup_guard.check(repos::update_last_checked(&state.db, repo_id).await)?;
        cleanup_guard.check(
            reviews::mark_completed(&state.db, review.id, duration.as_secs() as u32).await,
        )?;

        // Broadcast AFTER DB update to prevent race condition.
        // Fetch results for consistent SSE payload.
        let results = reviews::get_by_id(&state.db, review.id)
            .await
            .ok()
            .flatten()
            .map(|r| serde_json::to_value(&r.results).unwrap_or_default())
            .unwrap_or_else(|| serde_json::Value::Array(vec![]));
        cleanup_guard.broadcast_complete(results);
        // Disarm the guard - we completed successfully
        cleanup_guard.disarm();

        tracing::info!("Review completed in {:.1}s", duration.as_secs_f64());

        Ok(())
    }

    async fn run_prompt_review_static(
        state: &AppState,
        review_id: crate::domain::ids::ReviewId,
        prompt: &crate::domain::prompt::Prompt,
        chunks: &[crate::github::concat::Chunk],
        single_chunk_max_output: u32,
        multi_chunk_max_output: u32,
    ) -> anyhow::Result<crate::domain::review::ReviewOutput> {
        use crate::domain::review::ReviewOutput;

        let system_prompt = build_system_prompt(prompt.text.as_str());

        // If single chunk, just run it
        let output = if chunks.len() == 1 {
            let request = LlmRequest {
                system_prompt,
                user_prompt: format!(
                    "Please review the following codebase:\n\n{}",
                    chunks[0].content
                ),
                max_tokens: single_chunk_max_output,
            };

            Self::run_llm_streaming_static(state, review_id, &prompt.name, request).await?
        } else {
            // Multiple chunks: review each and aggregate
            let mut all_reasoning = Vec::new();
            let mut any_action_required = false;
            let mut all_comments = Vec::new();

            for (i, chunk) in chunks.iter().enumerate() {
                let chunk_prompt = format!(
                    "Please review part {} of {} of this codebase:\n\n{}",
                    i + 1,
                    chunks.len(),
                    chunk.content
                );

                let request = LlmRequest {
                    system_prompt: system_prompt.clone(),
                    user_prompt: chunk_prompt,
                    max_tokens: multi_chunk_max_output,
                };

                let chunk_output =
                    Self::run_llm_streaming_static(state, review_id, &prompt.name, request).await?;

                // Use as_raw() to get the content for aggregation
                all_reasoning.push(format!(
                    "## Part {}\n{}",
                    i + 1,
                    chunk_output.detailed_reasoning.as_raw()
                ));
                any_action_required |= chunk_output.action_required;
                all_comments.push(format!(
                    "## Part {}\n{}",
                    i + 1,
                    chunk_output.user_visible_comments.as_raw()
                ));
            }

            // Combine results - wrap in UntrustedString since content is from LLM
            use crate::domain::untrusted::UntrustedString;
            ReviewOutput {
                detailed_reasoning: UntrustedString::from(all_reasoning.join("\n\n")),
                action_required: any_action_required,
                user_visible_comments: UntrustedString::from(all_comments.join("\n\n")),
            }
        };

        // Send prompt complete event once per prompt (not per chunk)
        let _ = state.review_updates.send(ReviewUpdate {
            review_id,
            prompt_name: prompt.name.clone(),
            kind: ReviewUpdateKind::PromptComplete,
        });

        Ok(output)
    }

    async fn run_llm_streaming_static(
        state: &AppState,
        review_id: crate::domain::ids::ReviewId,
        prompt_name: &str,
        request: LlmRequest,
    ) -> anyhow::Result<crate::domain::review::ReviewOutput> {
        let mut stream = state.llm.complete_stream(request);
        let mut full_text = String::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            full_text.push_str(&chunk.text);

            // Broadcast chunk updates for SSE clients (only if there's text)
            if !chunk.text.is_empty() {
                let _ = state.review_updates.send(ReviewUpdate {
                    review_id,
                    prompt_name: prompt_name.to_string(),
                    kind: ReviewUpdateKind::Chunk { text: chunk.text },
                });
            }
        }

        // Note: PromptComplete is emitted by run_prompt_review after all chunks
        // for a prompt are processed. This ensures multi-chunk prompts only emit
        // PromptComplete once, not once per chunk.

        crate::llm::schema::parse_review_output(&full_text)
            .map_err(|e| anyhow::anyhow!("Failed to parse LLM response: {}", e))
    }

    async fn schedule_daily_reviews(&self) -> anyhow::Result<()> {
        use crate::domain::review::ReviewStatus;

        let review_interval_hours = self.state.config.scheduler.review_interval_hours as i64;

        // Get all repos
        let all_repos = repos::list_all(&self.state.db).await?;

        for repo in all_repos {
            // Check if there's already a pending job
            if jobs::has_pending_review(&self.state.db, repo.id).await? {
                continue;
            }

            // Check if repo has any enabled prompts - skip if not.
            // This prevents a tight scheduling loop for repos where prompt creation
            // failed or all prompts have been disabled.
            if !prompts::has_enabled_prompts(&self.state.db, repo.id).await? {
                continue;
            }

            // Check if we've checked this repo recently (via last_checked_at).
            // This covers both successful reviews AND "no changes" early-returns.
            // Without this check, repos with no changes would be re-scheduled every poll.
            if !repos::needs_check(&self.state.db, repo.id, review_interval_hours).await? {
                continue;
            }

            // Also check the last review status - don't schedule if one is still running.
            let last_review = reviews::get_latest_for_repo(&self.state.db, repo.id).await?;

            let should_schedule = match last_review {
                None => true, // Never reviewed
                Some(review) => {
                    match review.status {
                        ReviewStatus::Completed { .. } => {
                            // Last review completed - needs_check already verified interval
                            true
                        }
                        ReviewStatus::Failed { .. } => {
                            // Failed review - allow retry scheduling
                            true
                        }
                        ReviewStatus::Pending | ReviewStatus::InProgress { .. } => {
                            // Still running, don't schedule another
                            false
                        }
                    }
                }
            };

            if should_schedule {
                tracing::info!(
                    "Scheduling review for {}/{}",
                    repo.url.owner(),
                    repo.url.name()
                );

                let new_job = NewJob::review_repo(repo.id, false);
                jobs::create(&self.state.db, new_job).await?;
            }
        }

        Ok(())
    }
}
