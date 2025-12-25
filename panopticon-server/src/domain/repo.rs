use crate::domain::ids::RepoId;
use url::Url;

/// A validated GitHub repository URL.
/// Only constructible from valid github.com URLs - parse, don't validate.
///
/// Owner and name are normalized to lowercase to match GitHub's case-insensitive
/// behavior and prevent filesystem collisions on case-insensitive systems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepoUrl {
    /// Owner (normalized to lowercase)
    owner: String,
    /// Repo name (normalized to lowercase)
    name: String,
    url: Url,
}

impl GitHubRepoUrl {
    /// Parse a URL string into a validated GitHub repo URL.
    /// Returns None if not a valid GitHub repository URL.
    ///
    /// Note: Owner and name are normalized to lowercase. GitHub treats
    /// `Owner/Repo` and `owner/repo` as the same repository, and on
    /// case-insensitive filesystems (macOS, Windows), having both
    /// could corrupt clones.
    pub fn parse(s: &str) -> Option<Self> {
        // Reject URLs with double slashes in the path portion before parsing.
        // The url crate normalizes paths, so we check the raw string.
        // We look for "//" after "github.com" to avoid matching the scheme "https://"
        if let Some(host_pos) = s.find("github.com") {
            let after_host = &s[host_pos + "github.com".len()..];
            if after_host.contains("//") {
                return None;
            }
        }

        let url = Url::parse(s).ok()?;

        // Must be HTTPS
        if url.scheme() != "https" {
            return None;
        }

        // Must be github.com
        if url.host_str() != Some("github.com") {
            return None;
        }

        // Reject query strings and fragments - we only accept clean repo URLs
        if url.query().is_some() || url.fragment().is_some() {
            return None;
        }

        // Extract owner/name from path
        let path = url.path().trim_start_matches('/');
        let parts: Vec<&str> = path.split('/').collect();

        if parts.len() < 2 {
            return None;
        }

        // Normalize to lowercase for consistency with GitHub's case-insensitive behavior
        let owner = parts[0].to_lowercase();
        let name = parts[1].trim_end_matches(".git").to_lowercase();

        if owner.is_empty() || name.is_empty() {
            return None;
        }

        // Reject paths that look like they're pointing to a specific file/tree
        // e.g., github.com/owner/repo/blob/main/file.rs
        if parts.len() > 2 && !parts[2].is_empty() {
            return None;
        }

        Some(Self { owner, name, url })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the HTTPS clone URL for git operations.
    pub fn clone_url(&self) -> String {
        format!("https://github.com/{}/{}.git", self.owner, self.name)
    }

    /// Returns the original URL string.
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }
}

impl serde::Serialize for GitHubRepoUrl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.url.as_str().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for GitHubRepoUrl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        GitHubRepoUrl::parse(&s).ok_or_else(|| serde::de::Error::custom("invalid GitHub repo URL"))
    }
}

/// A commit SHA - exactly 40 hex characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSha(String);

impl CommitSha {
    /// Parse a string into a validated commit SHA.
    /// Returns None if not exactly 40 hex characters.
    pub fn parse(s: &str) -> Option<Self> {
        if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(Self(s.to_lowercase()))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl serde::Serialize for CommitSha {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for CommitSha {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        CommitSha::parse(&s).ok_or_else(|| serde::de::Error::custom("invalid commit SHA"))
    }
}

/// A registered repository with its metadata.
#[derive(Debug, Clone)]
pub struct Repo {
    pub id: RepoId,
    pub url: GitHubRepoUrl,
    pub last_commit_sha: Option<CommitSha>,
    /// When we last checked this repo for changes.
    /// This prevents rescheduling jobs every poll interval for repos with no changes.
    pub last_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Input for creating a new repo - only valid states constructible.
pub struct NewRepo {
    pub url: GitHubRepoUrl,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_valid_github_urls() {
        // Note: owner and name are normalized to lowercase
        let cases = [
            ("https://github.com/owner/repo", "owner", "repo"),
            ("https://github.com/owner/repo.git", "owner", "repo"),
            ("https://github.com/Rust-Lang/rust", "rust-lang", "rust"), // normalized
            ("https://github.com/a/b", "a", "b"),
            ("https://github.com/OWNER/REPO", "owner", "repo"), // normalized
        ];

        for (url, expected_owner, expected_name) in cases {
            let parsed = GitHubRepoUrl::parse(url);
            assert!(parsed.is_some(), "Failed to parse: {}", url);
            let parsed = parsed.unwrap();
            assert_eq!(parsed.owner(), expected_owner);
            assert_eq!(parsed.name(), expected_name);
        }
    }

    #[test]
    fn case_insensitive_urls_produce_same_owner_name() {
        let url1 = GitHubRepoUrl::parse("https://github.com/Owner/Repo").unwrap();
        let url2 = GitHubRepoUrl::parse("https://github.com/owner/repo").unwrap();
        let url3 = GitHubRepoUrl::parse("https://github.com/OWNER/REPO").unwrap();

        // All should normalize to the same values
        assert_eq!(url1.owner(), url2.owner());
        assert_eq!(url2.owner(), url3.owner());
        assert_eq!(url1.name(), url2.name());
        assert_eq!(url2.name(), url3.name());
    }

    #[test]
    fn rejects_invalid_github_urls() {
        let invalid = [
            "not a url",
            "https://gitlab.com/owner/repo",
            "https://github.com/",
            "https://github.com/owner",
            "https://github.com/owner/",
            "https://github.com/owner/repo/blob/main/file.rs",
            "ftp://github.com/owner/repo",
            // Query strings and fragments should be rejected
            "https://github.com/owner/repo?utm=1",
            "https://github.com/owner/repo#readme",
            "https://github.com/owner/repo?foo=bar#section",
            // Extra slashes in path should be rejected
            "https://github.com/owner/repo//blob",
            "https://github.com/owner//repo",
            "https://github.com//owner/repo",
        ];

        for url in invalid {
            assert!(
                GitHubRepoUrl::parse(url).is_none(),
                "Should have rejected: {}",
                url
            );
        }
    }

    #[test]
    fn commit_sha_validates_correctly() {
        // Valid SHA
        let valid = "a".repeat(40);
        assert!(CommitSha::parse(&valid).is_some());

        let valid_mixed = "abc123def456abc123def456abc123def456abc1";
        assert!(CommitSha::parse(valid_mixed).is_some());

        // Invalid: wrong length
        assert!(CommitSha::parse("abc123").is_none());
        assert!(CommitSha::parse(&"a".repeat(39)).is_none());
        assert!(CommitSha::parse(&"a".repeat(41)).is_none());

        // Invalid: non-hex characters
        let invalid = "g".repeat(40);
        assert!(CommitSha::parse(&invalid).is_none());
    }

    proptest! {
        #[test]
        fn github_url_roundtrips(
            owner in "[a-zA-Z][a-zA-Z0-9-]{0,38}",
            name in "[a-zA-Z][a-zA-Z0-9._-]{0,99}"
        ) {
            let url_str = format!("https://github.com/{}/{}", owner, name);
            let parsed = GitHubRepoUrl::parse(&url_str);

            prop_assert!(parsed.is_some());
            let parsed = parsed.unwrap();
            // Owner and name are normalized to lowercase
            prop_assert_eq!(parsed.owner(), owner.to_lowercase());
            prop_assert_eq!(parsed.name(), name.trim_end_matches(".git").to_lowercase());
        }

        #[test]
        fn commit_sha_rejects_invalid_length(s in ".*") {
            // Valid SHAs are exactly 40 hex chars
            let is_valid_sha = s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit());
            prop_assert_eq!(CommitSha::parse(&s).is_some(), is_valid_sha);
        }
    }
}
