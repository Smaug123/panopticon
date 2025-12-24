use sqlx::SqlitePool;

use crate::domain::ids::{PromptId, RepoId, ReviewId};
use crate::domain::repo::CommitSha;
use crate::domain::review::{
    NewReview, PromptResult, Review, ReviewOutput, ReviewStatus, ReviewTrigger,
};

/// Database row for reviews table.
#[derive(sqlx::FromRow)]
struct ReviewRow {
    id: i64,
    repo_id: i64,
    commit_sha: String,
    trigger: String,
    status: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    duration_secs: Option<i64>,
    error: Option<String>,
    created_at: String,
}

/// Database row for review_results table.
#[derive(sqlx::FromRow)]
struct ReviewResultRow {
    #[allow(dead_code)]
    id: i64,
    #[allow(dead_code)]
    review_id: i64,
    prompt_id: i64,
    prompt_name: String,
    detailed_reasoning: String,
    action_required: bool,
    user_visible_comments: String,
}

fn parse_datetime(s: &str) -> Result<chrono::DateTime<chrono::Utc>, &'static str> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|_| "invalid date")
}

fn parse_status(row: &ReviewRow) -> Result<ReviewStatus, &'static str> {
    match row.status.as_str() {
        "pending" => Ok(ReviewStatus::Pending),
        "in_progress" => {
            let started_at = row
                .started_at
                .as_deref()
                .ok_or("in_progress status without started_at")?;
            Ok(ReviewStatus::InProgress {
                started_at: parse_datetime(started_at)?,
            })
        }
        "completed" => {
            let started_at = row
                .started_at
                .as_deref()
                .ok_or("completed status without started_at")?;
            let completed_at = row
                .completed_at
                .as_deref()
                .ok_or("completed status without completed_at")?;
            Ok(ReviewStatus::Completed {
                started_at: parse_datetime(started_at)?,
                completed_at: parse_datetime(completed_at)?,
                duration_secs: row.duration_secs.unwrap_or(0) as u32,
            })
        }
        "failed" => {
            let failed_at = row
                .completed_at
                .as_deref()
                .ok_or("failed status without completed_at")?;
            Ok(ReviewStatus::Failed {
                started_at: row.started_at.as_deref().map(parse_datetime).transpose()?,
                failed_at: parse_datetime(failed_at)?,
                error: row.error.clone().unwrap_or_default(),
            })
        }
        _ => Err("unknown status"),
    }
}

impl TryFrom<(ReviewRow, Vec<ReviewResultRow>)> for Review {
    type Error = &'static str;

    fn try_from((row, results): (ReviewRow, Vec<ReviewResultRow>)) -> Result<Self, Self::Error> {
        let commit_sha = CommitSha::parse(&row.commit_sha).ok_or("invalid commit SHA")?;
        let trigger = ReviewTrigger::from_str(&row.trigger).ok_or("invalid trigger")?;
        let status = parse_status(&row)?;
        let created_at = parse_datetime(&row.created_at)?;

        let results = results
            .into_iter()
            .map(|r| PromptResult {
                prompt_id: PromptId::new(r.prompt_id),
                prompt_name: r.prompt_name,
                output: ReviewOutput {
                    detailed_reasoning: r.detailed_reasoning,
                    action_required: r.action_required,
                    user_visible_comments: r.user_visible_comments,
                },
            })
            .collect();

        Ok(Review {
            id: ReviewId::new(row.id),
            repo_id: RepoId::new(row.repo_id),
            commit_sha,
            trigger,
            status,
            results,
            created_at,
        })
    }
}

/// Create a new review.
pub async fn create(pool: &SqlitePool, new_review: NewReview) -> Result<Review, sqlx::Error> {
    let row = sqlx::query_as::<_, ReviewRow>(
        r#"
        INSERT INTO reviews (repo_id, commit_sha, trigger)
        VALUES (?, ?, ?)
        RETURNING id, repo_id, commit_sha, trigger, status, started_at, completed_at, duration_secs, error, created_at
        "#,
    )
    .bind(new_review.repo_id.into_inner())
    .bind(new_review.commit_sha.as_str())
    .bind(new_review.trigger.as_str())
    .fetch_one(pool)
    .await?;

    (row, vec![])
        .try_into()
        .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))
}

/// Get a review by ID with its results.
pub async fn get_by_id(pool: &SqlitePool, id: ReviewId) -> Result<Option<Review>, sqlx::Error> {
    let row = sqlx::query_as::<_, ReviewRow>(
        r#"
        SELECT id, repo_id, commit_sha, trigger, status, started_at, completed_at, duration_secs, error, created_at
        FROM reviews
        WHERE id = ?
        "#,
    )
    .bind(id.into_inner())
    .fetch_optional(pool)
    .await?;

    let row = match row {
        Some(r) => r,
        None => return Ok(None),
    };

    let results = sqlx::query_as::<_, ReviewResultRow>(
        r#"
        SELECT id, review_id, prompt_id, prompt_name, detailed_reasoning, action_required, user_visible_comments
        FROM review_results
        WHERE review_id = ?
        "#,
    )
    .bind(id.into_inner())
    .fetch_all(pool)
    .await?;

    Ok(Some(
        (row, results)
            .try_into()
            .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))?,
    ))
}

/// List reviews for a repository (without results for efficiency).
pub async fn list_by_repo(pool: &SqlitePool, repo_id: RepoId) -> Result<Vec<Review>, sqlx::Error> {
    let rows = sqlx::query_as::<_, ReviewRow>(
        r#"
        SELECT id, repo_id, commit_sha, trigger, status, started_at, completed_at, duration_secs, error, created_at
        FROM reviews
        WHERE repo_id = ?
        ORDER BY created_at DESC
        "#,
    )
    .bind(repo_id.into_inner())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|r| {
            (r, vec![])
                .try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))
        })
        .collect()
}

/// Get the most recent review for a repository.
pub async fn get_latest_for_repo(
    pool: &SqlitePool,
    repo_id: RepoId,
) -> Result<Option<Review>, sqlx::Error> {
    let row = sqlx::query_as::<_, ReviewRow>(
        r#"
        SELECT id, repo_id, commit_sha, trigger, status, started_at, completed_at, duration_secs, error, created_at
        FROM reviews
        WHERE repo_id = ?
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(repo_id.into_inner())
    .fetch_optional(pool)
    .await?;

    match row {
        Some(r) => Ok(Some(
            (r, vec![])
                .try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))?,
        )),
        None => Ok(None),
    }
}

/// Mark a review as in progress.
pub async fn mark_in_progress(pool: &SqlitePool, id: ReviewId) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE reviews
        SET status = 'in_progress', started_at = datetime('now')
        WHERE id = ?
        "#,
    )
    .bind(id.into_inner())
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark a review as completed.
pub async fn mark_completed(
    pool: &SqlitePool,
    id: ReviewId,
    duration_secs: u32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE reviews
        SET status = 'completed', completed_at = datetime('now'), duration_secs = ?
        WHERE id = ?
        "#,
    )
    .bind(duration_secs as i64)
    .bind(id.into_inner())
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark a review as failed.
pub async fn mark_failed(
    pool: &SqlitePool,
    id: ReviewId,
    error: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE reviews
        SET status = 'failed', completed_at = datetime('now'), error = ?
        WHERE id = ?
        "#,
    )
    .bind(error)
    .bind(id.into_inner())
    .execute(pool)
    .await?;

    Ok(())
}

/// Add a result for a prompt.
pub async fn add_result(
    pool: &SqlitePool,
    review_id: ReviewId,
    prompt_id: PromptId,
    prompt_name: &str,
    output: &ReviewOutput,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO review_results (review_id, prompt_id, prompt_name, detailed_reasoning, action_required, user_visible_comments)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(review_id.into_inner())
    .bind(prompt_id.into_inner())
    .bind(prompt_name)
    .bind(&output.detailed_reasoning)
    .bind(output.action_required)
    .bind(&output.user_visible_comments)
    .execute(pool)
    .await?;

    Ok(())
}
