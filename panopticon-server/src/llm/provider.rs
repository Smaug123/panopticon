use std::pin::Pin;

use futures::Stream;

use crate::domain::review::ReviewOutput;

/// A chunk of streaming LLM output.
#[derive(Debug, Clone)]
pub struct StreamChunk {
    /// The text content of this chunk.
    pub text: String,
    /// Whether this is the final chunk.
    pub is_final: bool,
}

/// Error type for LLM operations.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("Rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u32 },

    #[error("Context length exceeded: {0}")]
    ContextLengthExceeded(String),

    #[error("Invalid response format: {0}")]
    InvalidResponse(String),

    #[error("API error: {status} - {message}")]
    ApiError { status: u16, message: String },

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Parse error: {0}")]
    ParseError(String),
}

impl From<reqwest::Error> for LlmError {
    fn from(e: reqwest::Error) -> Self {
        LlmError::NetworkError(e.to_string())
    }
}

/// Request to send to the LLM.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// System prompt (sets behavior/context).
    pub system_prompt: String,
    /// User prompt (the actual content to review).
    pub user_prompt: String,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
}

/// The LLM provider trait.
///
/// This trait abstracts over different LLM providers (OpenAI, Gemini, Claude, etc.)
/// to allow easy switching between them.
pub trait LlmProvider: Send + Sync {
    /// Get the provider name for logging.
    fn name(&self) -> &'static str;

    /// Get the maximum context length in tokens.
    fn max_context_tokens(&self) -> u32;

    /// Complete a request with streaming output.
    ///
    /// Returns a stream of text chunks. The final chunk will have is_final=true.
    fn complete_stream(
        &self,
        request: LlmRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send + '_>>;

    /// Complete a request with structured output.
    ///
    /// This method handles the parsing of the LLM's response into a ReviewOutput.
    /// The default implementation collects the stream and parses the result.
    fn complete_structured(
        &self,
        request: LlmRequest,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ReviewOutput, LlmError>> + Send + '_>>
    {
        Box::pin(async move {
            use futures::StreamExt;

            let mut full_text = String::new();
            let mut stream = self.complete_stream(request);

            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                full_text.push_str(&chunk.text);
            }

            crate::llm::schema::parse_review_output(&full_text)
                .map_err(|e| LlmError::ParseError(e.to_string()))
        })
    }
}
