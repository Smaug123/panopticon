use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::domain::ids::{JobId, RepoId};
use crate::domain::job::{Job, JobPayload, JobStatus, NewJob};

/// Database row for jobs table.
#[derive(sqlx::FromRow)]
struct JobRow {
    id: i64,
    payload: String,
    status: String,
    attempts: i64,
    max_attempts: i64,
    scheduled_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    last_error: Option<String>,
    created_at: String,
}

fn parse_datetime(s: &str) -> Result<DateTime<Utc>, &'static str> {
    // Try RFC3339 first (preferred format)
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Fall back to SQLite's datetime('now') format: "YYYY-MM-DD HH:MM:SS"
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|naive| naive.and_utc())
        .map_err(|_| "invalid date")
}

impl TryFrom<JobRow> for Job {
    type Error = &'static str;

    fn try_from(row: JobRow) -> Result<Self, Self::Error> {
        let payload: JobPayload =
            serde_json::from_str(&row.payload).map_err(|_| "invalid job payload")?;
        let status = JobStatus::from_str(&row.status).ok_or("invalid job status")?;
        let scheduled_at = parse_datetime(&row.scheduled_at)?;
        let started_at = row.started_at.as_deref().map(parse_datetime).transpose()?;
        let completed_at = row
            .completed_at
            .as_deref()
            .map(parse_datetime)
            .transpose()?;
        let created_at = parse_datetime(&row.created_at)?;

        Ok(Job {
            id: JobId::new(row.id),
            payload,
            status,
            attempts: row.attempts as u32,
            max_attempts: row.max_attempts as u32,
            scheduled_at,
            started_at,
            completed_at,
            last_error: row.last_error,
            created_at,
        })
    }
}

/// Create a new job.
pub async fn create(pool: &SqlitePool, new_job: NewJob) -> Result<Job, sqlx::Error> {
    let payload =
        serde_json::to_string(&new_job.payload).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let scheduled_at = new_job.scheduled_at.to_rfc3339();

    let row = sqlx::query_as::<_, JobRow>(
        r#"
        INSERT INTO jobs (payload, max_attempts, scheduled_at)
        VALUES (?, ?, ?)
        RETURNING id, payload, status, attempts, max_attempts, scheduled_at, started_at, completed_at, last_error, created_at
        "#,
    )
    .bind(&payload)
    .bind(new_job.max_attempts as i64)
    .bind(&scheduled_at)
    .fetch_one(pool)
    .await?;

    row.try_into()
        .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))
}

/// Atomically claim the next available job.
/// Uses UPDATE...RETURNING to prevent race conditions.
pub async fn claim_next(pool: &SqlitePool) -> Result<Option<Job>, sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    let row = sqlx::query_as::<_, JobRow>(
        r#"
        UPDATE jobs
        SET status = 'running',
            started_at = ?,
            attempts = attempts + 1
        WHERE id = (
            SELECT id FROM jobs
            WHERE status = 'pending'
              AND scheduled_at <= ?
            ORDER BY scheduled_at ASC
            LIMIT 1
        )
        RETURNING id, payload, status, attempts, max_attempts, scheduled_at, started_at, completed_at, last_error, created_at
        "#,
    )
    .bind(&now)
    .bind(&now)
    .fetch_optional(pool)
    .await?;

    match row {
        Some(r) => Ok(Some(r.try_into().map_err(|e| {
            sqlx::Error::Decode(Box::new(std::io::Error::other(e)))
        })?)),
        None => Ok(None),
    }
}

/// Mark a job as completed.
pub async fn complete(pool: &SqlitePool, id: JobId) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        r#"
        UPDATE jobs
        SET status = 'completed', completed_at = ?
        WHERE id = ?
        "#,
    )
    .bind(&now)
    .bind(id.into_inner())
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark a job as failed. If should_retry is true and attempts < max_attempts,
/// set status back to pending for retry.
pub async fn fail(
    pool: &SqlitePool,
    id: JobId,
    error: &str,
    should_retry: bool,
) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    if should_retry {
        // Set back to pending for retry
        sqlx::query(
            r#"
            UPDATE jobs
            SET status = CASE
                    WHEN attempts < max_attempts THEN 'pending'
                    ELSE 'failed'
                END,
                last_error = ?,
                completed_at = CASE
                    WHEN attempts >= max_attempts THEN ?
                    ELSE NULL
                END
            WHERE id = ?
            "#,
        )
        .bind(error)
        .bind(&now)
        .bind(id.into_inner())
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            r#"
            UPDATE jobs
            SET status = 'failed', completed_at = ?, last_error = ?
            WHERE id = ?
            "#,
        )
        .bind(&now)
        .bind(error)
        .bind(id.into_inner())
        .execute(pool)
        .await?;
    }

    Ok(())
}

/// Check if there's already a pending review job for a repo.
pub async fn has_pending_review(pool: &SqlitePool, repo_id: RepoId) -> Result<bool, sqlx::Error> {
    // Use json_extract for exact matching instead of LIKE pattern matching.
    // LIKE '%"repo_id":1%' would incorrectly match repo_id 10, 100, etc.
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM jobs
        WHERE status IN ('pending', 'running')
          AND json_extract(payload, '$.type') = 'review_repo'
          AND json_extract(payload, '$.repo_id') = ?
        "#,
    )
    .bind(repo_id.into_inner())
    .fetch_one(pool)
    .await?;

    Ok(count > 0)
}

/// Get pending jobs count.
pub async fn count_pending(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE status = 'pending'")
        .fetch_one(pool)
        .await?;

    Ok(count)
}

/// Clean up old completed/failed jobs (keep last N days).
pub async fn cleanup_old(pool: &SqlitePool, days: i64) -> Result<u64, sqlx::Error> {
    // Calculate the cutoff date in RFC3339 format
    let cutoff = (Utc::now() - chrono::Duration::days(days)).to_rfc3339();
    let result = sqlx::query(
        r#"
        DELETE FROM jobs
        WHERE status IN ('completed', 'failed')
          AND completed_at < ?
        "#,
    )
    .bind(&cutoff)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Reclaim jobs that have been stuck in 'running' state for too long.
///
/// This handles the case where a process crashes while a job is running.
/// Jobs stuck for longer than `stuck_minutes` are either:
/// - Reset to 'pending' if attempts < max_attempts (for retry)
/// - Marked as 'failed' if attempts >= max_attempts
///
/// Returns the number of jobs reclaimed.
pub async fn reclaim_stuck(pool: &SqlitePool, stuck_minutes: i64) -> Result<u64, sqlx::Error> {
    let cutoff = (Utc::now() - chrono::Duration::minutes(stuck_minutes)).to_rfc3339();
    let now = Utc::now().to_rfc3339();

    // Update stuck running jobs:
    // - If attempts < max_attempts: set back to pending for retry
    // - If attempts >= max_attempts: mark as failed
    let result = sqlx::query(
        r#"
        UPDATE jobs
        SET status = CASE
                WHEN attempts < max_attempts THEN 'pending'
                ELSE 'failed'
            END,
            last_error = CASE
                WHEN attempts < max_attempts THEN 'Job reclaimed after stuck in running state'
                ELSE 'Job failed after exceeding max attempts while stuck'
            END,
            completed_at = CASE
                WHEN attempts >= max_attempts THEN ?
                ELSE NULL
            END
        WHERE status = 'running'
          AND started_at < ?
        "#,
    )
    .bind(&now)
    .bind(&cutoff)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}
