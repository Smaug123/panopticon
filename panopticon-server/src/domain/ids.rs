use std::fmt;

/// Macro to define strongly-typed ID wrappers.
/// Prevents mixing repo/review/prompt/job IDs at compile time.
macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(i64);

        impl $name {
            pub fn new(id: i64) -> Self {
                Self(id)
            }

            pub fn into_inner(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<i64> for $name {
            fn from(id: i64) -> Self {
                Self(id)
            }
        }
    };
}

define_id!(RepoId);
define_id!(ReviewId);
define_id!(PromptId);
define_id!(JobId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct_types() {
        let repo_id = RepoId::new(1);
        let review_id = ReviewId::new(1);

        // This would fail to compile if we tried to compare them:
        // assert_eq!(repo_id, review_id);

        // But we can compare same types:
        assert_eq!(repo_id, RepoId::new(1));
        assert_eq!(review_id, ReviewId::new(1));
    }

    #[test]
    fn ids_serialize_as_numbers() {
        let repo_id = RepoId::new(42);
        let json = serde_json::to_string(&repo_id).unwrap();
        assert_eq!(json, "42");

        let parsed: RepoId = serde_json::from_str("42").unwrap();
        assert_eq!(parsed, repo_id);
    }
}
