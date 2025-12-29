use sqlx::{Executor, Sqlite, SqlitePool};

use crate::domain::ids::RepoId;
use crate::domain::repo::{CommitSha, GitHubRepoUrl, NewRepo, Repo};

/// Database row for repos table.
#[derive(sqlx::FromRow)]
struct RepoRow {
    id: i64,
    url: String,
    #[allow(dead_code)]
    owner: String,
    #[allow(dead_code)]
    name: String,
    last_commit_sha: Option<String>,
    last_checked_at: Option<String>,
    created_at: String,
}

fn parse_datetime(s: &str) -> Result<chrono::DateTime<chrono::Utc>, &'static str> {
    // Try RFC3339 first (preferred format)
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&chrono::Utc));
    }

    // Fall back to SQLite's datetime('now') format: "YYYY-MM-DD HH:MM:SS"
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|naive| naive.and_utc())
        .map_err(|_| "invalid date")
}

impl TryFrom<RepoRow> for Repo {
    type Error = &'static str;

    fn try_from(row: RepoRow) -> Result<Self, Self::Error> {
        let url = GitHubRepoUrl::parse(&row.url).ok_or("invalid URL in database")?;
        let last_commit_sha = match &row.last_commit_sha {
            Some(sha) => Some(CommitSha::parse(sha).ok_or("invalid commit SHA in database")?),
            None => None,
        };
        let last_checked_at = row
            .last_checked_at
            .as_deref()
            .map(parse_datetime)
            .transpose()?;
        let created_at = parse_datetime(&row.created_at)?;

        Ok(Repo {
            id: RepoId::new(row.id),
            url,
            last_commit_sha,
            last_checked_at,
            created_at,
        })
    }
}

/// Create a new repository.
///
/// Accepts any executor (pool or transaction) for transactional safety.
pub async fn create<'e, E>(executor: E, new_repo: NewRepo) -> Result<Repo, sqlx::Error>
where
    E: Executor<'e, Database = Sqlite>,
{
    let url = new_repo.url.as_str();
    let owner = new_repo.url.owner();
    let name = new_repo.url.name();

    let row = sqlx::query_as::<_, RepoRow>(
        r#"
        INSERT INTO repos (url, owner, name)
        VALUES (?, ?, ?)
        RETURNING id, url, owner, name, last_commit_sha, last_checked_at, created_at
        "#,
    )
    .bind(url)
    .bind(owner)
    .bind(name)
    .fetch_one(executor)
    .await?;

    row.try_into()
        .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))
}

/// Get a repository by ID.
pub async fn get_by_id(pool: &SqlitePool, id: RepoId) -> Result<Option<Repo>, sqlx::Error> {
    let row = sqlx::query_as::<_, RepoRow>(
        r#"
        SELECT id, url, owner, name, last_commit_sha, last_checked_at, created_at
        FROM repos
        WHERE id = ?
        "#,
    )
    .bind(id.into_inner())
    .fetch_optional(pool)
    .await?;

    match row {
        Some(r) => Ok(Some(r.try_into().map_err(|e| {
            sqlx::Error::Decode(Box::new(std::io::Error::other(e)))
        })?)),
        None => Ok(None),
    }
}

/// Get all repositories.
pub async fn list_all(pool: &SqlitePool) -> Result<Vec<Repo>, sqlx::Error> {
    let rows = sqlx::query_as::<_, RepoRow>(
        r#"
        SELECT id, url, owner, name, last_commit_sha, last_checked_at, created_at
        FROM repos
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|r| {
            r.try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))
        })
        .collect()
}

/// Delete a repository by ID.
pub async fn delete(pool: &SqlitePool, id: RepoId) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM repos WHERE id = ?")
        .bind(id.into_inner())
        .execute(pool)
        .await?;

    Ok(result.rows_affected() > 0)
}

/// Update the last commit SHA for a repository.
pub async fn update_last_commit(
    pool: &SqlitePool,
    id: RepoId,
    sha: &CommitSha,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE repos SET last_commit_sha = ? WHERE id = ?")
        .bind(sha.as_str())
        .bind(id.into_inner())
        .execute(pool)
        .await?;

    Ok(())
}

/// Check if a repository exists by URL.
pub async fn exists_by_url(pool: &SqlitePool, url: &GitHubRepoUrl) -> Result<bool, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM repos WHERE url = ?")
        .bind(url.as_str())
        .fetch_one(pool)
        .await?;

    Ok(count > 0)
}

/// Update the last_checked_at timestamp to now.
/// Call this after checking a repo for changes, even if no changes were found.
pub async fn update_last_checked(pool: &SqlitePool, id: RepoId) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE repos SET last_checked_at = ? WHERE id = ?")
        .bind(&now)
        .bind(id.into_inner())
        .execute(pool)
        .await?;

    Ok(())
}

/// Check if a repository needs to be checked for updates.
///
/// Returns true if:
/// - The repo has never been checked (last_checked_at is NULL), OR
/// - More than `interval_hours` have passed since the last check
///
/// This is used by the scheduler to determine if a new review job should be created.
pub async fn needs_check(
    pool: &SqlitePool,
    id: RepoId,
    interval_hours: i64,
) -> Result<bool, sqlx::Error> {
    // Check if last_checked_at is null or older than interval_hours
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM repos
        WHERE id = ?
          AND (
            last_checked_at IS NULL
            OR datetime(last_checked_at) < datetime('now', ? || ' hours')
          )
        "#,
    )
    .bind(id.into_inner())
    .bind(-interval_hours) // negative because we want "X hours ago"
    .fetch_one(pool)
    .await?;

    Ok(count > 0)
}
