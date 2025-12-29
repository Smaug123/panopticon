use crate::domain::ids::{PromptId, RepoId, ReviewId};
use crate::domain::repo::CommitSha;
use crate::domain::untrusted::UntrustedString;
use std::str::FromStr;

/// How the review was triggered - discriminated union, no invalid combinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTrigger {
    /// Triggered by the daily scheduler.
    Scheduled,
    /// Manually triggered by the user.
    Manual,
}

impl ReviewTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewTrigger::Scheduled => "scheduled",
            ReviewTrigger::Manual => "manual",
        }
    }
}

impl FromStr for ReviewTrigger {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "scheduled" => Ok(ReviewTrigger::Scheduled),
            "manual" => Ok(ReviewTrigger::Manual),
            _ => Err(()),
        }
    }
}

/// Review status - state machine encoded in types.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReviewStatus {
    Pending,
    InProgress {
        started_at: chrono::DateTime<chrono::Utc>,
    },
    Completed {
        started_at: chrono::DateTime<chrono::Utc>,
        completed_at: chrono::DateTime<chrono::Utc>,
        duration_secs: u32,
    },
    Failed {
        started_at: Option<chrono::DateTime<chrono::Utc>>,
        failed_at: chrono::DateTime<chrono::Utc>,
        error: String,
    },
}

impl ReviewStatus {
    pub fn status_str(&self) -> &'static str {
        match self {
            ReviewStatus::Pending => "pending",
            ReviewStatus::InProgress { .. } => "in_progress",
            ReviewStatus::Completed { .. } => "completed",
            ReviewStatus::Failed { .. } => "failed",
        }
    }
}

/// Structured output from the LLM for a single prompt.
///
/// All string fields use `UntrustedString` because LLM output is untrusted
/// and must be sanitized before display to prevent XSS attacks.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReviewOutput {
    /// Internal reasoning (stored but not displayed by default).
    /// Uses UntrustedString because it comes from the LLM.
    pub detailed_reasoning: UntrustedString,
    /// Flag indicating whether action is required.
    pub action_required: bool,
    /// User-facing comments in markdown format.
    /// Uses UntrustedString because it comes from the LLM.
    pub user_visible_comments: UntrustedString,
}

/// A single prompt's result within a review.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PromptResult {
    pub prompt_id: PromptId,
    pub prompt_name: String,
    pub output: ReviewOutput,
}

/// A review of a repository.
#[derive(Debug, Clone)]
pub struct Review {
    pub id: ReviewId,
    pub repo_id: RepoId,
    pub commit_sha: CommitSha,
    pub trigger: ReviewTrigger,
    pub status: ReviewStatus,
    pub results: Vec<PromptResult>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Input for creating a new review.
pub struct NewReview {
    pub repo_id: RepoId,
    pub commit_sha: CommitSha,
    pub trigger: ReviewTrigger,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_trigger_roundtrips() {
        for trigger in [ReviewTrigger::Scheduled, ReviewTrigger::Manual] {
            let s = trigger.as_str();
            let parsed: ReviewTrigger = s.parse().unwrap();
            assert_eq!(parsed, trigger);
        }
    }

    #[test]
    fn review_status_serializes_correctly() {
        let pending = ReviewStatus::Pending;
        let json = serde_json::to_value(&pending).unwrap();
        assert_eq!(json["status"], "pending");

        let in_progress = ReviewStatus::InProgress {
            started_at: chrono::Utc::now(),
        };
        let json = serde_json::to_value(&in_progress).unwrap();
        assert_eq!(json["status"], "in_progress");
        assert!(json["started_at"].is_string());
    }
}
