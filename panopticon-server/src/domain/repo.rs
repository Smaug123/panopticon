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

        // SECURITY: Reject path traversal components.
        // These would allow escaping the repos directory when joined to a path.
        if owner == "." || owner == ".." || name == "." || name == ".." {
            return None;
        }

        // SECURITY: Reject path separators in owner/name.
        // While URL parsing should handle '/', backslash could sneak through
        // and be interpreted as a path separator on Windows.
        if owner.contains(['/', '\\']) || name.contains(['/', '\\']) {
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
    fn path_traversal_urls_produce_safe_values_or_are_rejected() {
        // SECURITY: These URLs attempt path traversal via owner or name.
        // The URL crate normalizes .. and . path components before we see them,
        // so most of these result in either rejection (wrong number of segments)
        // or safe values (the traversal is already resolved).
        //
        // Our explicit . and .. checks are defense-in-depth in case URL crate
        // behavior changes or edge cases exist.
        let traversal_attempts = [
            // These get normalized by URL crate - .. collapses parent paths
            ("https://github.com/../repo", None), // becomes /repo (only 1 segment)
            (
                "https://github.com/../../../etc/passwd",
                Some(("etc", "passwd")),
            ), // traversal resolved
            ("https://github.com/owner/..", None), // becomes / (empty)
            ("https://github.com/./repo", None),  // becomes /repo (only 1 segment)
            ("https://github.com/owner/.", None), // becomes /owner/ (empty name)
            ("https://github.com/../..", None),   // becomes / (empty)
            // Percent-encoded traversal is also normalized
            ("https://github.com/%2E%2E/repo", None), // normalized
            ("https://github.com/%2E/%2E%2E", None),  // normalized
            // Backslash is treated as forward slash by URL parser
            ("https://github.com/foo\\bar/repo", None), // becomes /foo/bar/repo (3 segments, rejected)
        ];

        for (url, expected) in traversal_attempts {
            let result = GitHubRepoUrl::parse(url);
            match expected {
                None => {
                    assert!(
                        result.is_none(),
                        "URL {} should have been rejected but got {:?}",
                        url,
                        result.map(|r| (r.owner().to_string(), r.name().to_string()))
                    );
                }
                Some((exp_owner, exp_name)) => {
                    let parsed = result.expect(&format!("URL {} should have parsed", url));
                    assert_eq!(parsed.owner(), exp_owner, "URL {} owner mismatch", url);
                    assert_eq!(parsed.name(), exp_name, "URL {} name mismatch", url);
                    // Verify the resulting values don't contain traversal
                    assert_ne!(parsed.owner(), "..", "owner should not be ..");
                    assert_ne!(parsed.owner(), ".", "owner should not be .");
                    assert_ne!(parsed.name(), "..", "name should not be ..");
                    assert_ne!(parsed.name(), ".", "name should not be .");
                }
            }
        }
    }

    #[test]
    fn rejects_dotdot_owner_or_name_directly() {
        // SECURITY: Defense-in-depth test. While the URL crate normalizes path
        // traversal, we explicitly reject . and .. in case of edge cases or
        // future URL crate behavior changes.
        //
        // This tests the validation logic itself, not URL parsing behavior.
        // The validation ensures that even if somehow a . or .. got through
        // URL parsing, we'd still reject it.

        // Verify our validation catches these patterns
        // (We can't easily construct these through URL parsing, but the code handles them)

        // The validation is at lines 72-74 in the parse function:
        // if owner == "." || owner == ".." || name == "." || name == ".." {
        //     return None;
        // }

        // Since we can't bypass URL normalization, we just document that the check exists
        // and verify that legitimate repos with dots work fine
        assert!(GitHubRepoUrl::parse("https://github.com/owner/repo.name").is_some());
        assert!(GitHubRepoUrl::parse("https://github.com/owner/...").is_some()); // three dots is fine
        assert!(GitHubRepoUrl::parse("https://github.com/.../repo").is_some()); // three dots is fine
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

        /// SECURITY: Prove that no parsed GitHub URL can produce path traversal values.
        /// This property test generates arbitrary strings that could contain path traversal
        /// attempts and verifies that if parsing succeeds, the resulting owner/name values
        /// are safe for filesystem operations.
        #[test]
        fn parsed_urls_never_contain_path_traversal(
            // Generate arbitrary strings that might contain traversal attempts
            owner in ".*",
            name in ".*"
        ) {
            let url_str = format!("https://github.com/{}/{}", owner, name);
            if let Some(parsed) = GitHubRepoUrl::parse(&url_str) {
                // If parsing succeeds, owner and name must be safe
                prop_assert_ne!(parsed.owner(), ".", "owner must not be .");
                prop_assert_ne!(parsed.owner(), "..", "owner must not be ..");
                prop_assert_ne!(parsed.name(), ".", "name must not be .");
                prop_assert_ne!(parsed.name(), "..", "name must not be ..");

                // Must not contain path separators
                prop_assert!(!parsed.owner().contains('/'), "owner must not contain /");
                prop_assert!(!parsed.owner().contains('\\'), "owner must not contain \\");
                prop_assert!(!parsed.name().contains('/'), "name must not contain /");
                prop_assert!(!parsed.name().contains('\\'), "name must not contain \\");

                // Must not be empty
                prop_assert!(!parsed.owner().is_empty(), "owner must not be empty");
                prop_assert!(!parsed.name().is_empty(), "name must not be empty");
            }
            // If parsing fails, that's fine - we only care about successful parses being safe
        }
    }
}
