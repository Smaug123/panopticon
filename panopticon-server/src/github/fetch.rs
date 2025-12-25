use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::domain::repo::{CommitSha, GitHubRepoUrl};
use crate::github::filter::FileFilter;

/// Error type for GitHub fetching operations.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("Git error: {0}")]
    GitError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Invalid commit SHA: {0}")]
    InvalidSha(String),

    #[error("Repository not found")]
    NotFound,
}

/// A file's content from a repository.
#[derive(Debug, Clone)]
pub struct FileContent {
    pub path: PathBuf,
    pub content: String,
}

/// Contents of a repository.
#[derive(Debug)]
pub struct RepoContents {
    pub files: Vec<FileContent>,
    pub total_size: usize,
}

/// Fetches and manages local clones of GitHub repositories.
pub struct GitHubFetcher {
    repos_dir: PathBuf,
}

impl GitHubFetcher {
    pub fn new(repos_dir: PathBuf) -> Self {
        Self { repos_dir }
    }

    /// Clone or update a repo, returning the current HEAD SHA.
    pub async fn fetch(&self, url: &GitHubRepoUrl) -> Result<CommitSha, FetchError> {
        let repo_path = self.repo_path(url);

        if repo_path.exists() {
            self.pull(&repo_path).await?;
        } else {
            self.clone(url, &repo_path).await?;
        }

        self.get_head_sha(&repo_path).await
    }

    /// Get contents of repo as a list of files.
    pub async fn get_contents(
        &self,
        url: &GitHubRepoUrl,
        filter: &FileFilter,
    ) -> Result<RepoContents, FetchError> {
        let repo_path = self.repo_path(url);

        if !repo_path.exists() {
            return Err(FetchError::NotFound);
        }

        let files = self.list_files(&repo_path, filter).await?;
        let mut contents = Vec::new();
        let mut total_size = 0;

        for file_path in files {
            match tokio::fs::read_to_string(&file_path).await {
                Ok(content) => {
                    total_size += content.len();
                    let relative_path = file_path.strip_prefix(&repo_path).unwrap().to_path_buf();
                    contents.push(FileContent {
                        path: relative_path,
                        content,
                    });
                }
                Err(e) => {
                    // Skip files we can't read (binary, permissions, etc.)
                    tracing::debug!("Skipping file {}: {}", file_path.display(), e);
                }
            }
        }

        Ok(RepoContents {
            files: contents,
            total_size,
        })
    }

    /// Get the local path for a repo.
    fn repo_path(&self, url: &GitHubRepoUrl) -> PathBuf {
        self.repos_dir.join(url.owner()).join(url.name())
    }

    async fn clone(&self, url: &GitHubRepoUrl, path: &Path) -> Result<(), FetchError> {
        // Create parent directories
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tracing::info!("Cloning {} to {}", url.clone_url(), path.display());

        let output = Command::new("git")
            .args(["clone", "--depth", "1", &url.clone_url()])
            .arg(path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(FetchError::GitError(stderr.to_string()));
        }

        Ok(())
    }

    async fn pull(&self, path: &Path) -> Result<(), FetchError> {
        tracing::debug!("Updating repo at {}", path.display());

        // Fetch latest
        let output = Command::new("git")
            .args(["fetch", "--depth", "1", "origin"])
            .current_dir(path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(FetchError::GitError(format!("fetch failed: {}", stderr)));
        }

        // Reset to origin/HEAD
        let output = Command::new("git")
            .args(["reset", "--hard", "origin/HEAD"])
            .current_dir(path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(FetchError::GitError(format!("reset failed: {}", stderr)));
        }

        Ok(())
    }

    async fn get_head_sha(&self, path: &Path) -> Result<CommitSha, FetchError> {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(FetchError::GitError(format!(
                "rev-parse failed: {}",
                stderr
            )));
        }

        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        CommitSha::parse(&sha).ok_or(FetchError::InvalidSha(sha))
    }

    async fn list_files(
        &self,
        repo_path: &Path,
        filter: &FileFilter,
    ) -> Result<Vec<PathBuf>, FetchError> {
        let mut files = Vec::new();
        let mut stack = vec![repo_path.to_path_buf()];

        while let Some(dir) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&dir).await?;

            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let metadata = entry.metadata().await?;

                if metadata.is_dir() {
                    // Check if directory should be traversed
                    let relative = path.strip_prefix(repo_path).unwrap_or(&path);
                    if filter.should_include(relative, 0) {
                        stack.push(path);
                    }
                } else if metadata.is_file() {
                    let relative = path.strip_prefix(repo_path).unwrap_or(&path);
                    if filter.should_include(relative, metadata.len()) {
                        files.push(path);
                    }
                }
            }
        }

        // Sort for consistent ordering
        files.sort();
        Ok(files)
    }

    /// Delete the local clone of a repository.
    pub async fn delete(&self, url: &GitHubRepoUrl) -> Result<(), FetchError> {
        let repo_path = self.repo_path(url);
        if repo_path.exists() {
            tokio::fs::remove_dir_all(&repo_path).await?;
        }
        Ok(())
    }
}
