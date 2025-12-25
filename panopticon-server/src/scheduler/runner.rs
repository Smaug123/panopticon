use std::time::Duration;

use futures::StreamExt;
use sqlx::SqlitePool;
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
struct ReviewCleanupGuard {
    db: SqlitePool,
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
        review_id: ReviewId,
        review_updates: tokio::sync::broadcast::Sender<ReviewUpdate>,
    ) -> Self {
        Self {
            db,
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

    /// Helper to broadcast ReviewComplete event.
    fn broadcast_complete(&self) {
        let _ = self.review_updates.send(ReviewUpdate {
            review_id: self.review_id,
            prompt_name: String::new(),
            kind: ReviewUpdateKind::ReviewComplete,
        });
    }
}

impl Drop for ReviewCleanupGuard {
    fn drop(&mut self) {
        if self.should_cleanup {
            // We're being dropped without completing successfully.
            // Mark the review as failed to prevent it being stuck in_progress.
            //
            // Since Drop is sync but our DB operations are async, we spawn a
            // blocking task to handle the cleanup. This is acceptable because:
            // 1. We're in an error path anyway
            // 2. The alternative (stuck review) is worse
            let db = self.db.clone();
            let review_id = self.review_id;
            let review_updates = self.review_updates.clone();
            // Use the actual error if set, otherwise fall back to generic message
            let error_msg = self
                .error_message
                .take()
                .unwrap_or_else(|| "Review interrupted: process error or early return".to_string());

            // Use tokio::spawn to run cleanup asynchronously
            tokio::spawn(async move {
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
                let _ = review_updates.send(ReviewUpdate {
                    review_id,
                    prompt_name: String::new(),
                    kind: ReviewUpdateKind::ReviewComplete,
                });
            });
        }
    }
}

/// Background job runner that processes the job queue.
pub struct JobRunner {
    state: AppState,
}

impl JobRunner {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Run the job runner until shutdown signal is received.
    pub async fn run(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let poll_interval = Duration::from_secs(self.state.config.scheduler.poll_interval_secs);
        let mut ticker = interval(poll_interval);

        tracing::info!(
            "Job runner started, polling every {}s",
            poll_interval.as_secs()
        );

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.tick().await;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("Job runner shutting down");
                        break;
                    }
                }
            }
        }
    }

    async fn tick(&self) {
        // Reclaim jobs stuck in 'running' state (e.g., from process crashes)
        let timeout_minutes = self.state.config.scheduler.job_timeout_minutes as i64;
        match jobs::reclaim_stuck(&self.state.db, timeout_minutes).await {
            Ok(count) if count > 0 => {
                tracing::warn!("Reclaimed {} stuck job(s)", count);
            }
            Err(e) => {
                tracing::error!("Failed to reclaim stuck jobs: {}", e);
            }
            _ => {}
        }

        // Process pending jobs
        self.process_pending_jobs().await;

        // Schedule daily reviews for repos that need them
        if let Err(e) = self.schedule_daily_reviews().await {
            tracing::error!("Failed to schedule daily reviews: {}", e);
        }
    }

    async fn process_pending_jobs(&self) {
        // Process jobs one at a time
        loop {
            match jobs::claim_next(&self.state.db).await {
                Ok(Some(job)) => {
                    tracing::info!("Processing job {}: {:?}", job.id, job.payload);
                    self.execute_job(job).await;
                }
                Ok(None) => break, // No more pending jobs
                Err(e) => {
                    tracing::error!("Failed to claim job: {}", e);
                    break;
                }
            }
        }
    }

    async fn execute_job(&self, job: Job) {
        let result = match &job.payload {
            JobPayload::ReviewRepo { repo_id, force } => self.run_review(*repo_id, *force).await,
        };

        match result {
            Ok(()) => {
                if let Err(e) = jobs::complete(&self.state.db, job.id).await {
                    tracing::error!("Failed to mark job {} as complete: {}", job.id, e);
                }
            }
            Err(e) => {
                let error_msg = e.to_string();
                tracing::error!("Job {} failed: {}", job.id, error_msg);

                let should_retry = job.attempts < job.max_attempts;

                // Extract retry_after from the error if it's a rate limit
                // The error chain may contain LlmError::RateLimited
                let retry_after_secs = extract_retry_after(&e);

                if let Err(e) = jobs::fail(
                    &self.state.db,
                    job.id,
                    &error_msg,
                    should_retry,
                    retry_after_secs,
                )
                .await
                {
                    tracing::error!("Failed to mark job {} as failed: {}", job.id, e);
                }
            }
        }
    }

    async fn run_review(&self, repo_id: RepoId, force: bool) -> anyhow::Result<()> {
        // Get repo
        let repo = repos::get_by_id(&self.state.db, repo_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Repo not found"))?;

        tracing::info!(
            "Running review for {}/{}",
            repo.url.owner(),
            repo.url.name()
        );

        // Fetch/update repo and get current SHA
        let current_sha = self.state.github.fetch(&repo.url).await?;

        // Check if we should skip (no changes since last review)
        if !force {
            if let Some(last_sha) = &repo.last_commit_sha {
                if last_sha == &current_sha {
                    tracing::info!("No changes since last review, skipping");
                    // Record that we checked, even though we're not doing a full review.
                    // This prevents the scheduler from re-queueing this repo every poll.
                    repos::update_last_checked(&self.state.db, repo_id).await?;
                    return Ok(());
                }
            }
        }

        // Get enabled prompts
        let prompts_list = prompts::list_enabled_by_repo(&self.state.db, repo_id).await?;
        if prompts_list.is_empty() {
            tracing::warn!("No enabled prompts for repo, skipping review");
            return Ok(());
        }

        // Create review record
        let review = reviews::create(
            &self.state.db,
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
        reviews::mark_in_progress(&self.state.db, review.id).await?;

        // Create cleanup guard - this ensures the review is marked as failed
        // if we early-return due to any error after this point.
        // Use `cleanup_guard.check()` for fallible operations to preserve error messages.
        let mut cleanup_guard = ReviewCleanupGuard::new(
            self.state.db.clone(),
            review.id,
            self.state.review_updates.clone(),
        );

        let start_time = std::time::Instant::now();

        // Get repo contents
        let filter = FileFilter::default();
        let contents =
            cleanup_guard.check(self.state.github.get_contents(&repo.url, &filter).await)?;

        tracing::info!(
            "Loaded {} files ({} bytes)",
            contents.files.len(),
            contents.total_size
        );

        // Chunk contents based on the model's context limit.
        // This prevents context_length_exceeded errors with smaller-context models.
        // We use 80% of the model's limit to leave room for the prompt and response.
        let model_context = self.state.llm.max_context_tokens();
        let chunk_limit = (model_context as f64 * 0.8) as u32;
        let strategy = ChunkingStrategy::with_max_tokens(chunk_limit);
        let chunks = chunk_contents(&contents.files, &strategy);

        tracing::info!("Split into {} chunk(s)", chunks.len());

        // For each prompt, run review
        let mut had_error = false;
        for prompt in &prompts_list {
            tracing::info!("Running prompt: {}", prompt.name);

            match self.run_prompt_review(review.id, prompt, &chunks).await {
                Ok(output) => {
                    // Store result
                    if let Err(e) = reviews::add_result(
                        &self.state.db,
                        review.id,
                        prompt.id,
                        &prompt.name,
                        &output,
                    )
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
            reviews::mark_failed(&self.state.db, review.id, "Some prompts failed").await?;
            // Broadcast AFTER DB update to prevent race condition where UI refreshes
            // and sees stale in_progress status
            cleanup_guard.broadcast_complete();
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
        cleanup_guard
            .check(repos::update_last_commit(&self.state.db, repo_id, &current_sha).await)?;
        // Also update last_checked_at so the scheduler knows when we last checked this repo
        cleanup_guard.check(repos::update_last_checked(&self.state.db, repo_id).await)?;
        cleanup_guard.check(
            reviews::mark_completed(&self.state.db, review.id, duration.as_secs() as u32).await,
        )?;

        // Broadcast AFTER DB update to prevent race condition
        cleanup_guard.broadcast_complete();
        // Disarm the guard - we completed successfully
        cleanup_guard.disarm();

        tracing::info!("Review completed in {:.1}s", duration.as_secs_f64());

        Ok(())
    }

    async fn run_prompt_review(
        &self,
        review_id: crate::domain::ids::ReviewId,
        prompt: &crate::domain::prompt::Prompt,
        chunks: &[crate::github::concat::Chunk],
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
                max_tokens: 16_000,
            };

            self.run_llm_streaming(review_id, &prompt.name, request)
                .await?
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
                    max_tokens: 8_000,
                };

                let chunk_output = self
                    .run_llm_streaming(review_id, &prompt.name, request)
                    .await?;

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
        let _ = self.state.review_updates.send(ReviewUpdate {
            review_id,
            prompt_name: prompt.name.clone(),
            kind: ReviewUpdateKind::PromptComplete,
        });

        Ok(output)
    }

    async fn run_llm_streaming(
        &self,
        review_id: crate::domain::ids::ReviewId,
        prompt_name: &str,
        request: LlmRequest,
    ) -> anyhow::Result<crate::domain::review::ReviewOutput> {
        let mut stream = self.state.llm.complete_stream(request);
        let mut full_text = String::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            full_text.push_str(&chunk.text);

            // Broadcast chunk updates for SSE clients (only if there's text)
            if !chunk.text.is_empty() {
                let _ = self.state.review_updates.send(ReviewUpdate {
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
