use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::error::ApiError;
use crate::db::{jobs, prompts, repos, reviews};
use crate::domain::ids::{PromptId, RepoId, ReviewId};
use crate::domain::job::NewJob;
use crate::domain::prompt::{NewPrompt, PromptText, DEFAULT_REVIEW_PROMPT};
use crate::domain::repo::{GitHubRepoUrl, NewRepo};
use crate::domain::review::ReviewStatus;
use crate::AppState;

// --- Health Check ---

pub async fn health_check() -> StatusCode {
    StatusCode::OK
}

// --- Repos ---

#[derive(Serialize)]
pub struct RepoResponse {
    pub id: i64,
    pub url: String,
    pub owner: String,
    pub name: String,
    pub last_commit_sha: Option<String>,
    pub prompt_count: i64,
    pub last_review: Option<ReviewSummary>,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct ReviewSummary {
    pub id: i64,
    pub status: String,
    pub trigger: String,
    pub action_required: Option<bool>,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct CreateRepoRequest {
    pub url: String,
}

pub async fn list_repos(
    State(state): State<AppState>,
) -> Result<Json<Vec<RepoResponse>>, ApiError> {
    let repos_list = repos::list_all(&state.db).await?;
    let mut responses = Vec::new();

    for repo in repos_list {
        let prompt_count = prompts::count_by_repo(&state.db, repo.id).await?;
        let last_review = reviews::get_latest_for_repo(&state.db, repo.id).await?;

        responses.push(RepoResponse {
            id: repo.id.into_inner(),
            url: repo.url.as_str().to_string(),
            owner: repo.url.owner().to_string(),
            name: repo.url.name().to_string(),
            last_commit_sha: repo.last_commit_sha.map(|s| s.as_str().to_string()),
            prompt_count,
            last_review: last_review.map(|r| ReviewSummary {
                id: r.id.into_inner(),
                status: r.status.status_str().to_string(),
                trigger: r.trigger.as_str().to_string(),
                action_required: match &r.status {
                    ReviewStatus::Completed { .. } => {
                        // Would need to check results, simplified for now
                        None
                    }
                    _ => None,
                },
                created_at: r.created_at.to_rfc3339(),
            }),
            created_at: repo.created_at.to_rfc3339(),
        });
    }

    Ok(Json(responses))
}

pub async fn create_repo(
    State(state): State<AppState>,
    Json(req): Json<CreateRepoRequest>,
) -> Result<(StatusCode, Json<RepoResponse>), ApiError> {
    let url = GitHubRepoUrl::parse(&req.url)
        .ok_or_else(|| ApiError::BadRequest("Invalid GitHub URL".to_string()))?;

    // Check if already exists
    if repos::exists_by_url(&state.db, &url).await? {
        return Err(ApiError::Conflict(
            "Repository already registered".to_string(),
        ));
    }

    let repo = repos::create(&state.db, NewRepo { url }).await?;

    // Create default prompt
    let default_prompt_text =
        PromptText::new(DEFAULT_REVIEW_PROMPT.to_string()).expect("Default prompt should be valid");

    prompts::create(
        &state.db,
        NewPrompt {
            repo_id: repo.id,
            name: "Comprehensive Review".to_string(),
            text: default_prompt_text,
            is_default: true,
        },
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(RepoResponse {
            id: repo.id.into_inner(),
            url: repo.url.as_str().to_string(),
            owner: repo.url.owner().to_string(),
            name: repo.url.name().to_string(),
            last_commit_sha: None,
            prompt_count: 1,
            last_review: None,
            created_at: repo.created_at.to_rfc3339(),
        }),
    ))
}

pub async fn get_repo(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<RepoResponse>, ApiError> {
    let repo_id = RepoId::new(id);
    let repo = repos::get_by_id(&state.db, repo_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Repository not found".to_string()))?;

    let prompt_count = prompts::count_by_repo(&state.db, repo.id).await?;
    let last_review = reviews::get_latest_for_repo(&state.db, repo.id).await?;

    Ok(Json(RepoResponse {
        id: repo.id.into_inner(),
        url: repo.url.as_str().to_string(),
        owner: repo.url.owner().to_string(),
        name: repo.url.name().to_string(),
        last_commit_sha: repo.last_commit_sha.map(|s| s.as_str().to_string()),
        prompt_count,
        last_review: last_review.map(|r| ReviewSummary {
            id: r.id.into_inner(),
            status: r.status.status_str().to_string(),
            trigger: r.trigger.as_str().to_string(),
            action_required: None,
            created_at: r.created_at.to_rfc3339(),
        }),
        created_at: repo.created_at.to_rfc3339(),
    }))
}

pub async fn delete_repo(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let repo_id = RepoId::new(id);

    // Get repo to delete local clone
    if let Some(repo) = repos::get_by_id(&state.db, repo_id).await? {
        // Delete local clone (ignore errors)
        let _ = state.github.delete(&repo.url).await;
    }

    let deleted = repos::delete(&state.db, repo_id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("Repository not found".to_string()))
    }
}

// --- Prompts ---

#[derive(Serialize)]
pub struct PromptResponse {
    pub id: i64,
    pub name: String,
    pub text: String,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct CreatePromptRequest {
    pub name: String,
    pub text: String,
}

#[derive(Deserialize)]
pub struct UpdatePromptRequest {
    pub name: String,
    pub text: String,
    pub enabled: bool,
}

pub async fn list_prompts(
    State(state): State<AppState>,
    Path(repo_id): Path<i64>,
) -> Result<Json<Vec<PromptResponse>>, ApiError> {
    let repo_id = RepoId::new(repo_id);

    // Verify repo exists
    repos::get_by_id(&state.db, repo_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Repository not found".to_string()))?;

    let prompts_list = prompts::list_by_repo(&state.db, repo_id).await?;

    Ok(Json(
        prompts_list
            .into_iter()
            .map(|p| PromptResponse {
                id: p.id.into_inner(),
                name: p.name,
                text: p.text.as_str().to_string(),
                enabled: p.enabled,
                is_default: p.is_default,
                created_at: p.created_at.to_rfc3339(),
            })
            .collect(),
    ))
}

pub async fn create_prompt(
    State(state): State<AppState>,
    Path(repo_id): Path<i64>,
    Json(req): Json<CreatePromptRequest>,
) -> Result<(StatusCode, Json<PromptResponse>), ApiError> {
    let repo_id = RepoId::new(repo_id);

    // Verify repo exists
    repos::get_by_id(&state.db, repo_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Repository not found".to_string()))?;

    let text = PromptText::new(req.text)
        .ok_or_else(|| ApiError::BadRequest("Prompt text cannot be empty".to_string()))?;

    let prompt = prompts::create(
        &state.db,
        NewPrompt {
            repo_id,
            name: req.name,
            text,
            is_default: false,
        },
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(PromptResponse {
            id: prompt.id.into_inner(),
            name: prompt.name,
            text: prompt.text.as_str().to_string(),
            enabled: prompt.enabled,
            is_default: prompt.is_default,
            created_at: prompt.created_at.to_rfc3339(),
        }),
    ))
}

pub async fn update_prompt(
    State(state): State<AppState>,
    Path((repo_id, prompt_id)): Path<(i64, i64)>,
    Json(req): Json<UpdatePromptRequest>,
) -> Result<Json<PromptResponse>, ApiError> {
    let repo_id = RepoId::new(repo_id);
    let prompt_id = PromptId::new(prompt_id);

    // Verify repo exists
    repos::get_by_id(&state.db, repo_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Repository not found".to_string()))?;

    // Verify prompt exists AND belongs to this repo (ownership check)
    prompts::get_by_id_and_repo(&state.db, prompt_id, repo_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Prompt not found".to_string()))?;

    let text = PromptText::new(req.text)
        .ok_or_else(|| ApiError::BadRequest("Prompt text cannot be empty".to_string()))?;

    let updated = prompts::update(&state.db, prompt_id, &req.name, &text, req.enabled).await?;
    if !updated {
        return Err(ApiError::NotFound("Prompt not found".to_string()));
    }

    let prompt = prompts::get_by_id(&state.db, prompt_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Prompt not found".to_string()))?;

    Ok(Json(PromptResponse {
        id: prompt.id.into_inner(),
        name: prompt.name,
        text: prompt.text.as_str().to_string(),
        enabled: prompt.enabled,
        is_default: prompt.is_default,
        created_at: prompt.created_at.to_rfc3339(),
    }))
}

pub async fn delete_prompt(
    State(state): State<AppState>,
    Path((repo_id, prompt_id)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    let repo_id = RepoId::new(repo_id);
    let prompt_id = PromptId::new(prompt_id);

    // Verify repo exists
    repos::get_by_id(&state.db, repo_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Repository not found".to_string()))?;

    // Verify prompt exists AND belongs to this repo (ownership check)
    // Also check if it's a default prompt
    let prompt = prompts::get_by_id_and_repo(&state.db, prompt_id, repo_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Prompt not found".to_string()))?;

    if prompt.is_default {
        return Err(ApiError::BadRequest(
            "Cannot delete default prompt".to_string(),
        ));
    }

    let deleted = prompts::delete(&state.db, prompt_id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("Prompt not found".to_string()))
    }
}

// --- Reviews ---

#[derive(Serialize)]
pub struct ReviewResponse {
    pub id: i64,
    pub repo_id: i64,
    pub commit_sha: String,
    pub trigger: String,
    pub status: serde_json::Value,
    pub results: Vec<ReviewResultResponse>,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct ReviewResultResponse {
    pub prompt_id: i64,
    pub prompt_name: String,
    pub action_required: bool,
    pub user_visible_comments: String,
    pub detailed_reasoning: String,
}

pub async fn list_reviews(
    State(state): State<AppState>,
    Path(repo_id): Path<i64>,
) -> Result<Json<Vec<ReviewResponse>>, ApiError> {
    let repo_id = RepoId::new(repo_id);

    // Verify repo exists
    repos::get_by_id(&state.db, repo_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Repository not found".to_string()))?;

    let reviews_list = reviews::list_by_repo(&state.db, repo_id).await?;

    Ok(Json(
        reviews_list
            .into_iter()
            .map(|r| ReviewResponse {
                id: r.id.into_inner(),
                repo_id: r.repo_id.into_inner(),
                commit_sha: r.commit_sha.as_str().to_string(),
                trigger: r.trigger.as_str().to_string(),
                status: serde_json::to_value(&r.status).unwrap_or_default(),
                results: vec![], // List doesn't include results for efficiency
                created_at: r.created_at.to_rfc3339(),
            })
            .collect(),
    ))
}

pub async fn trigger_review(
    State(state): State<AppState>,
    Path(repo_id): Path<i64>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let repo_id = RepoId::new(repo_id);

    // Verify repo exists
    repos::get_by_id(&state.db, repo_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Repository not found".to_string()))?;

    // Check if there's already a pending review
    if jobs::has_pending_review(&state.db, repo_id).await? {
        return Err(ApiError::Conflict(
            "Review already in progress or scheduled".to_string(),
        ));
    }

    // Create job with force=true (manual trigger)
    let job = jobs::create(&state.db, NewJob::review_repo(repo_id, true)).await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "message": "Review scheduled",
            "job_id": job.id.into_inner()
        })),
    ))
}

pub async fn get_review(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ReviewResponse>, ApiError> {
    let review_id = ReviewId::new(id);

    let review = reviews::get_by_id(&state.db, review_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Review not found".to_string()))?;

    Ok(Json(ReviewResponse {
        id: review.id.into_inner(),
        repo_id: review.repo_id.into_inner(),
        commit_sha: review.commit_sha.as_str().to_string(),
        trigger: review.trigger.as_str().to_string(),
        status: serde_json::to_value(&review.status).unwrap_or_default(),
        results: review
            .results
            .into_iter()
            .map(|r| ReviewResultResponse {
                prompt_id: r.prompt_id.into_inner(),
                prompt_name: r.prompt_name,
                action_required: r.output.action_required,
                // Use as_raw() - content is sent as JSON (safe), frontend must escape for HTML
                user_visible_comments: r.output.user_visible_comments.as_raw().to_string(),
                detailed_reasoning: r.output.detailed_reasoning.as_raw().to_string(),
            })
            .collect(),
        created_at: review.created_at.to_rfc3339(),
    }))
}

// --- Stream Tokens ---

#[derive(Serialize)]
pub struct StreamTokenResponse {
    pub token: String,
}

/// Generate a short-lived stream token for SSE authentication.
///
/// This endpoint exchanges the API key (sent in the Authorization header)
/// for a short-lived token that can be safely used in query strings for
/// SSE connections. The token is valid for 30 seconds and can only be used once.
///
/// This prevents the long-lived API key from appearing in:
/// - Browser history
/// - Referrer headers
/// - Server logs
pub async fn create_stream_token(State(state): State<AppState>) -> Json<StreamTokenResponse> {
    let token = state.stream_tokens.generate().await;
    Json(StreamTokenResponse { token })
}
