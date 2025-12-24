use sqlx::SqlitePool;

use crate::domain::ids::{PromptId, RepoId};
use crate::domain::prompt::{NewPrompt, Prompt, PromptText};

/// Database row for prompts table.
#[derive(sqlx::FromRow)]
struct PromptRow {
    id: i64,
    repo_id: i64,
    name: String,
    text: String,
    enabled: bool,
    is_default: bool,
    created_at: String,
}

impl TryFrom<PromptRow> for Prompt {
    type Error = &'static str;

    fn try_from(row: PromptRow) -> Result<Self, Self::Error> {
        let text = PromptText::new(row.text).ok_or("empty prompt text in database")?;
        let created_at =
            chrono::DateTime::parse_from_rfc3339(&row.created_at).map_err(|_| "invalid date")?;

        Ok(Prompt {
            id: PromptId::new(row.id),
            repo_id: RepoId::new(row.repo_id),
            name: row.name,
            text,
            enabled: row.enabled,
            is_default: row.is_default,
            created_at: created_at.with_timezone(&chrono::Utc),
        })
    }
}

/// Create a new prompt.
pub async fn create(pool: &SqlitePool, new_prompt: NewPrompt) -> Result<Prompt, sqlx::Error> {
    let row = sqlx::query_as::<_, PromptRow>(
        r#"
        INSERT INTO prompts (repo_id, name, text, is_default)
        VALUES (?, ?, ?, ?)
        RETURNING id, repo_id, name, text, enabled, is_default, created_at
        "#,
    )
    .bind(new_prompt.repo_id.into_inner())
    .bind(&new_prompt.name)
    .bind(new_prompt.text.as_str())
    .bind(new_prompt.is_default)
    .fetch_one(pool)
    .await?;

    row.try_into()
        .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))
}

/// Get a prompt by ID.
pub async fn get_by_id(pool: &SqlitePool, id: PromptId) -> Result<Option<Prompt>, sqlx::Error> {
    let row = sqlx::query_as::<_, PromptRow>(
        r#"
        SELECT id, repo_id, name, text, enabled, is_default, created_at
        FROM prompts
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

/// List all prompts for a repository.
pub async fn list_by_repo(pool: &SqlitePool, repo_id: RepoId) -> Result<Vec<Prompt>, sqlx::Error> {
    let rows = sqlx::query_as::<_, PromptRow>(
        r#"
        SELECT id, repo_id, name, text, enabled, is_default, created_at
        FROM prompts
        WHERE repo_id = ?
        ORDER BY is_default DESC, created_at ASC
        "#,
    )
    .bind(repo_id.into_inner())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|r| {
            r.try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))
        })
        .collect()
}

/// List all enabled prompts for a repository.
pub async fn list_enabled_by_repo(
    pool: &SqlitePool,
    repo_id: RepoId,
) -> Result<Vec<Prompt>, sqlx::Error> {
    let rows = sqlx::query_as::<_, PromptRow>(
        r#"
        SELECT id, repo_id, name, text, enabled, is_default, created_at
        FROM prompts
        WHERE repo_id = ? AND enabled = 1
        ORDER BY is_default DESC, created_at ASC
        "#,
    )
    .bind(repo_id.into_inner())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|r| {
            r.try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(std::io::Error::other(e))))
        })
        .collect()
}

/// Update a prompt's text and enabled status.
pub async fn update(
    pool: &SqlitePool,
    id: PromptId,
    name: &str,
    text: &PromptText,
    enabled: bool,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        r#"
        UPDATE prompts
        SET name = ?, text = ?, enabled = ?
        WHERE id = ?
        "#,
    )
    .bind(name)
    .bind(text.as_str())
    .bind(enabled)
    .bind(id.into_inner())
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Delete a prompt by ID.
pub async fn delete(pool: &SqlitePool, id: PromptId) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM prompts WHERE id = ?")
        .bind(id.into_inner())
        .execute(pool)
        .await?;

    Ok(result.rows_affected() > 0)
}

/// Count prompts for a repository.
pub async fn count_by_repo(pool: &SqlitePool, repo_id: RepoId) -> Result<i64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prompts WHERE repo_id = ?")
        .bind(repo_id.into_inner())
        .fetch_one(pool)
        .await?;

    Ok(count)
}
