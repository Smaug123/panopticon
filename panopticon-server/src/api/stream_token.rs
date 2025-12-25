//! Short-lived stream tokens for SSE authentication.
//!
//! This module provides a way to exchange long-lived API keys for short-lived
//! stream tokens that can be safely used in query strings for SSE connections.
//!
//! The problem: EventSource (SSE) cannot send custom headers, so the API key
//! would need to be passed in the query string. But query strings can leak via:
//! - Browser history
//! - Referrer headers
//! - Server logs
//!
//! The solution: Exchange the API key for a short-lived token that:
//! - Is only valid for 30 seconds
//! - Can only be used once
//! - Is stored in memory (not persisted)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::sync::RwLock;

/// Token validity duration (30 seconds is enough to establish SSE connection)
const TOKEN_TTL: Duration = Duration::from_secs(30);

/// A short-lived stream token.
#[derive(Clone)]
pub struct StreamToken {
    /// When the token expires
    pub expires_at: Instant,
    /// Whether the token has been used
    pub used: bool,
}

/// In-memory store for stream tokens.
#[derive(Clone)]
pub struct StreamTokenStore {
    tokens: Arc<RwLock<HashMap<String, StreamToken>>>,
}

impl Default for StreamTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamTokenStore {
    pub fn new() -> Self {
        Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Generate a new short-lived stream token.
    pub async fn generate(&self) -> String {
        let token: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();

        let mut tokens = self.tokens.write().await;

        // Clean up expired tokens while we have the lock
        let now = Instant::now();
        tokens.retain(|_, t| t.expires_at > now);

        tokens.insert(
            token.clone(),
            StreamToken {
                expires_at: now + TOKEN_TTL,
                used: false,
            },
        );

        token
    }

    /// Validate and consume a stream token.
    /// Returns true if the token was valid, false otherwise.
    /// A token can only be used once.
    pub async fn validate_and_consume(&self, token: &str) -> bool {
        let mut tokens = self.tokens.write().await;

        // Clean up expired tokens
        let now = Instant::now();
        tokens.retain(|_, t| t.expires_at > now);

        // Check if token exists, is valid, and hasn't been used
        if let Some(stream_token) = tokens.get_mut(token) {
            if !stream_token.used && stream_token.expires_at > now {
                stream_token.used = true;
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generated_token_should_be_valid() {
        let store = StreamTokenStore::new();
        let token = store.generate().await;

        assert!(
            store.validate_and_consume(&token).await,
            "Freshly generated token should be valid"
        );
    }

    #[tokio::test]
    async fn token_can_only_be_used_once() {
        let store = StreamTokenStore::new();
        let token = store.generate().await;

        assert!(
            store.validate_and_consume(&token).await,
            "First use should succeed"
        );
        assert!(
            !store.validate_and_consume(&token).await,
            "Second use should fail"
        );
    }

    #[tokio::test]
    async fn invalid_token_should_fail() {
        let store = StreamTokenStore::new();
        assert!(
            !store.validate_and_consume("invalid-token").await,
            "Invalid token should fail"
        );
    }
}
