use sqlx::SqlitePool;

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
    created_at: String,
}

impl TryFrom<RepoRow> for Repo {
    type Error = &'static str;

    fn try_from(row: RepoRow) -> Result<Self, Self::Error> {
        let url = GitHubRepoUrl::parse(&row.url).ok_or("invalid URL in database")?;
        let last_commit_sha = match &row.last_commit_sha {
            Some(sha) => Some(CommitSha::parse(sha).ok_or("invalid commit SHA in database")?),
            None => None,
        };
        let created_at =
            chrono::DateTime::parse_from_rfc3339(&row.created_at).map_err(|_| "invalid date")?;

        Ok(Repo {
            id: RepoId::new(row.id),
            url,
            last_commit_sha,
            created_at: created_at.with_timezone(&chrono::Utc),
        })
    }
}

/// Create a new repository.
pub async fn create(pool: &SqlitePool, new_repo: NewRepo) -> Result<Repo, sqlx::Error> {
    let url = new_repo.url.as_str();
    let owner = new_repo.url.owner();
    let name = new_repo.url.name();

    let row = sqlx::query_as::<_, RepoRow>(
        r#"
        INSERT INTO repos (url, owner, name)
        VALUES (?, ?, ?)
        RETURNING id, url, owner, name, last_commit_sha, created_at
        "#,
    )
    .bind(url)
    .bind(owner)
    .bind(name)
    .fetch_one(pool)
    .await?;

    row.try_into()
        .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))
}

/// Get a repository by ID.
pub async fn get_by_id(pool: &SqlitePool, id: RepoId) -> Result<Option<Repo>, sqlx::Error> {
    let row = sqlx::query_as::<_, RepoRow>(
        r#"
        SELECT id, url, owner, name, last_commit_sha, created_at
        FROM repos
        WHERE id = ?
        "#,
    )
    .bind(id.into_inner())
    .fetch_optional(pool)
    .await?;

    match row {
        Some(r) => Ok(Some(
            r.try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))?,
        )),
        None => Ok(None),
    }
}

/// Get all repositories.
pub async fn list_all(pool: &SqlitePool) -> Result<Vec<Repo>, sqlx::Error> {
    let rows = sqlx::query_as::<_, RepoRow>(
        r#"
        SELECT id, url, owner, name, last_commit_sha, created_at
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
