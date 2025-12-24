use crate::domain::ids::{PromptId, RepoId};

/// A non-empty prompt text.
/// Only constructible from non-empty strings after trimming.
#[derive(Debug, Clone)]
pub struct PromptText(String);

impl PromptText {
    /// Create a new prompt text from a string.
    /// Returns None if the string is empty after trimming.
    pub fn new(s: String) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(Self(trimmed.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl serde::Serialize for PromptText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for PromptText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        PromptText::new(s).ok_or_else(|| serde::de::Error::custom("prompt text cannot be empty"))
    }
}

/// A prompt associated with a repository.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub id: PromptId,
    pub repo_id: RepoId,
    pub name: String,
    pub text: PromptText,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Input for creating a new prompt.
pub struct NewPrompt {
    pub repo_id: RepoId,
    pub name: String,
    pub text: PromptText,
    pub is_default: bool,
}

/// The default comprehensive code review prompt.
pub const DEFAULT_REVIEW_PROMPT: &str = r#"You are a senior software engineer conducting a comprehensive code review.

Analyze this codebase thoroughly, focusing on:

1. **Architecture & Design**: Overall structure, separation of concerns, design patterns used
2. **Code Quality**: Readability, maintainability, naming conventions, code organization
3. **Potential Bugs**: Logic errors, edge cases, race conditions, null/undefined handling
4. **Security**: Input validation, authentication/authorization issues, data exposure
5. **Performance**: Inefficient algorithms, unnecessary allocations, N+1 queries
6. **Testing**: Test coverage gaps, untested edge cases
7. **Documentation**: Missing or outdated documentation, unclear code sections

Be specific and actionable in your feedback. Reference specific files and line numbers where applicable.
Prioritize issues by severity: critical > major > minor > suggestion."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_text_rejects_empty() {
        assert!(PromptText::new(String::new()).is_none());
        assert!(PromptText::new("   ".to_string()).is_none());
        assert!(PromptText::new("\t\n".to_string()).is_none());
    }

    #[test]
    fn prompt_text_accepts_non_empty() {
        assert!(PromptText::new("hello".to_string()).is_some());
        assert!(PromptText::new("  hello  ".to_string()).is_some());
    }

    #[test]
    fn prompt_text_trims_whitespace() {
        let text = PromptText::new("  hello  ".to_string()).unwrap();
        assert_eq!(text.as_str(), "hello");
    }

    #[test]
    fn default_prompt_is_valid() {
        assert!(PromptText::new(DEFAULT_REVIEW_PROMPT.to_string()).is_some());
    }
}
