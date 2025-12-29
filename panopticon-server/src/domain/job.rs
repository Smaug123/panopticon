use crate::domain::ids::{JobId, RepoId};
use chrono::{DateTime, Utc};
use std::str::FromStr;

/// Job payload types - extensible but type-safe.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobPayload {
    /// Review a repository.
    ReviewRepo {
        repo_id: RepoId,
        /// If true (manual trigger), always run even if no changes.
        force: bool,
    },
}

/// Job status in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
        }
    }
}

impl FromStr for JobStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(JobStatus::Pending),
            "running" => Ok(JobStatus::Running),
            "completed" => Ok(JobStatus::Completed),
            "failed" => Ok(JobStatus::Failed),
            _ => Err(()),
        }
    }
}

/// A job in the persistent queue.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: JobId,
    pub payload: JobPayload,
    pub status: JobStatus,
    pub attempts: u32,
    pub max_attempts: u32,
    pub scheduled_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// Last heartbeat timestamp. Updated periodically while running.
    /// Used to detect truly stuck jobs (worker crash) vs long-running jobs.
    pub last_heartbeat: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Input for creating a new job.
pub struct NewJob {
    pub payload: JobPayload,
    pub scheduled_at: DateTime<Utc>,
    pub max_attempts: u32,
}

impl NewJob {
    /// Create a new job to review a repo.
    pub fn review_repo(repo_id: RepoId, force: bool) -> Self {
        Self {
            payload: JobPayload::ReviewRepo { repo_id, force },
            scheduled_at: Utc::now(),
            max_attempts: 3,
        }
    }

    /// Create a job scheduled for a specific time.
    pub fn scheduled_at(mut self, at: DateTime<Utc>) -> Self {
        self.scheduled_at = at;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_payload_serializes_with_tag() {
        let payload = JobPayload::ReviewRepo {
            repo_id: RepoId::new(1),
            force: true,
        };
        let json = serde_json::to_value(&payload).unwrap();

        assert_eq!(json["type"], "review_repo");
        assert_eq!(json["repo_id"], 1);
        assert_eq!(json["force"], true);
    }

    #[test]
    fn job_payload_roundtrips() {
        let payload = JobPayload::ReviewRepo {
            repo_id: RepoId::new(42),
            force: false,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: JobPayload = serde_json::from_str(&json).unwrap();

        match parsed {
            JobPayload::ReviewRepo { repo_id, force } => {
                assert_eq!(repo_id, RepoId::new(42));
                assert!(!force);
            }
        }
    }

    #[test]
    fn job_status_roundtrips() {
        for status in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
        ] {
            let s = status.as_str();
            let parsed: JobStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }
}
