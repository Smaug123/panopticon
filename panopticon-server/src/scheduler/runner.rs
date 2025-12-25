use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use tokio::time::interval;

use crate::db::{jobs, prompts, repos, reviews};
use crate::domain::ids::RepoId;
use crate::domain::job::{Job, JobPayload, NewJob};
use crate::domain::review::{NewReview, ReviewTrigger};
use crate::github::concat::{chunk_contents, ChunkingStrategy};
use crate::github::filter::FileFilter;
use crate::llm::provider::LlmRequest;
use crate::llm::schema::build_system_prompt;
use crate::AppState;
use crate::ReviewUpdate;
use crate::ReviewUpdateKind;

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
                if let Err(e) = jobs::fail(&self.state.db, job.id, &error_msg, should_retry).await {
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

        let start_time = std::time::Instant::now();

        // Get repo contents
        let filter = FileFilter::default();
        let contents = self.state.github.get_contents(&repo.url, &filter).await?;

        tracing::info!(
            "Loaded {} files ({} bytes)",
            contents.files.len(),
            contents.total_size
        );

        // Chunk contents if needed
        let strategy = ChunkingStrategy::default();
        let chunks = chunk_contents(&contents.files, &strategy);

        tracing::info!("Split into {} chunk(s)", chunks.len());

        // For each prompt, run review
        let mut had_error = false;
        for prompt in &prompts_list {
            tracing::info!("Running prompt: {}", prompt.name);

            match self.run_prompt_review(review.id, &prompt, &chunks).await {
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
        } else {
            reviews::mark_completed(&self.state.db, review.id, duration.as_secs() as u32).await?;
            // Update repo's last commit SHA
            repos::update_last_commit(&self.state.db, repo_id, &current_sha).await?;
        }

        // Send review complete event to close SSE streams
        let _ = self.state.review_updates.send(ReviewUpdate {
            review_id: review.id,
            prompt_name: String::new(),
            kind: ReviewUpdateKind::ReviewComplete,
        });

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
        if chunks.len() == 1 {
            let request = LlmRequest {
                system_prompt,
                user_prompt: format!(
                    "Please review the following codebase:\n\n{}",
                    chunks[0].content
                ),
                max_tokens: 16_000,
            };

            return self
                .run_llm_streaming(review_id, &prompt.name, request)
                .await;
        }

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

            let output = self
                .run_llm_streaming(review_id, &prompt.name, request)
                .await?;

            // Use as_raw() to get the content for aggregation
            all_reasoning.push(format!(
                "## Part {}\n{}",
                i + 1,
                output.detailed_reasoning.as_raw()
            ));
            any_action_required |= output.action_required;
            all_comments.push(format!(
                "## Part {}\n{}",
                i + 1,
                output.user_visible_comments.as_raw()
            ));
        }

        // Combine results - wrap in UntrustedString since content is from LLM
        use crate::domain::untrusted::UntrustedString;
        Ok(ReviewOutput {
            detailed_reasoning: UntrustedString::from(all_reasoning.join("\n\n")),
            action_required: any_action_required,
            user_visible_comments: UntrustedString::from(all_comments.join("\n\n")),
        })
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

        // Send prompt complete event (the entire review may have more prompts)
        let _ = self.state.review_updates.send(ReviewUpdate {
            review_id,
            prompt_name: prompt_name.to_string(),
            kind: ReviewUpdateKind::PromptComplete,
        });

        crate::llm::schema::parse_review_output(&full_text)
            .map_err(|e| anyhow::anyhow!("Failed to parse LLM response: {}", e))
    }

    async fn schedule_daily_reviews(&self) -> anyhow::Result<()> {
        let review_interval_hours = self.state.config.scheduler.review_interval_hours as i64;

        // Get all repos
        let all_repos = repos::list_all(&self.state.db).await?;

        for repo in all_repos {
            // Check if there's already a pending job
            if jobs::has_pending_review(&self.state.db, repo.id).await? {
                continue;
            }

            // Check last review time
            let last_review = reviews::get_latest_for_repo(&self.state.db, repo.id).await?;

            let should_schedule = match last_review {
                None => true, // Never reviewed
                Some(review) => {
                    let hours_since = (Utc::now() - review.created_at).num_hours();
                    hours_since >= review_interval_hours
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
