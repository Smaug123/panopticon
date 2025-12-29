use std::pin::Pin;
use std::time::Duration;

use async_stream::stream;
use futures::Stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::llm::provider::{LlmError, LlmProvider, LlmRequest, StreamChunk};
use crate::llm::schema::REVIEW_OUTPUT_SCHEMA;

/// OpenAI API provider using the Responses API.
pub struct OpenAiProvider {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
    reasoning_effort: String,
    context_limit: u32,
}

impl OpenAiProvider {
    pub fn new(
        api_key: String,
        model: String,
        base_url: Option<String>,
        reasoning_effort: String,
        context_limit: Option<u32>,
        request_timeout_secs: Option<u64>,
    ) -> Self {
        // Use ClientBuilder with no_proxy to avoid system proxy lookup
        // which can fail in sandboxed test environments on macOS.
        // Add timeouts to prevent hanging requests:
        // - connect_timeout: 30s for initial connection
        // - timeout: configurable, defaults to 10 minutes (reasoning models can be slow)
        let timeout_secs = request_timeout_secs.unwrap_or(600);
        let client = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("Failed to create HTTP client");
        Self {
            client,
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            reasoning_effort,
            context_limit: context_limit.unwrap_or(1_000_000),
        }
    }
}

/// Reasoning configuration for the Responses API.
#[derive(Serialize)]
struct ReasoningConfig {
    effort: String,
}

/// Text format configuration for structured output.
#[derive(Serialize)]
struct TextFormat {
    format: TextFormatType,
}

/// Text format type for JSON schema output.
#[derive(Serialize)]
struct TextFormatType {
    #[serde(rename = "type")]
    format_type: String,
    schema: serde_json::Value,
    name: String,
    strict: bool,
}

/// Request body for the Responses API.
#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    instructions: String,
    /// The user's input. For the Responses API, this can be a simple string
    /// when there's only one user message, which is our use case.
    input: String,
    reasoning: ReasoningConfig,
    text: TextFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    stream: bool,
}

/// Streaming event from the Responses API.
#[derive(Deserialize, Debug)]
struct StreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    delta: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: ApiError,
}

#[derive(Deserialize)]
struct ApiError {
    message: String,
    #[serde(rename = "type")]
    error_type: Option<String>,
}

impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn max_context_tokens(&self) -> u32 {
        self.context_limit
    }

    fn complete_stream(
        &self,
        request: LlmRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send + '_>> {
        Box::pin(stream! {
            // Parse the schema for structured output
            let schema: serde_json::Value = serde_json::from_str(REVIEW_OUTPUT_SCHEMA)
                .expect("Invalid review output schema");

            // Use the user prompt directly as a string input.
            // The Responses API accepts either a string or an array of input items.
            // Since we have a single user message, a string is simpler and correct.
            let body = ResponsesRequest {
                model: self.model.clone(),
                instructions: request.system_prompt,
                input: request.user_prompt,
                reasoning: ReasoningConfig {
                    effort: self.reasoning_effort.clone(),
                },
                text: TextFormat {
                    format: TextFormatType {
                        format_type: "json_schema".to_string(),
                        schema,
                        name: "review_output".to_string(),
                        strict: true,
                    },
                },
                // Include max_output_tokens to control output length and prevent budget overruns
                max_output_tokens: Some(request.max_tokens),
                stream: true,
            };

            let response = match self.client
                .post(format!("{}/responses", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    yield Err(LlmError::NetworkError(e.to_string()));
                    return;
                }
            };

            let status = response.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(60);
                yield Err(LlmError::RateLimited { retry_after_secs: retry_after });
                return;
            }

            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let message = match serde_json::from_str::<ErrorResponse>(&text) {
                    Ok(err) => {
                        if err.error.error_type.as_deref() == Some("context_length_exceeded") {
                            yield Err(LlmError::ContextLengthExceeded(err.error.message));
                            return;
                        }
                        err.error.message
                    }
                    Err(_) => text,
                };
                yield Err(LlmError::ApiError { status: status.as_u16(), message });
                return;
            }

            // Process SSE stream from Responses API
            let mut buffer = String::new();
            let mut byte_stream = response.bytes_stream();

            use futures::StreamExt;
            while let Some(chunk_result) = byte_stream.next().await {
                let bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(LlmError::NetworkError(e.to_string()));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&bytes));

                // Process complete lines
                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim().to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    // Handle Responses API SSE events
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            yield Ok(StreamChunk { text: String::new(), is_final: true });
                            return;
                        }

                        match serde_json::from_str::<StreamEvent>(data) {
                            Ok(event) => {
                                // Handle text delta events
                                if event.event_type == "response.output_text.delta" {
                                    if let Some(delta) = event.delta {
                                        yield Ok(StreamChunk { text: delta, is_final: false });
                                    }
                                } else if event.event_type == "response.completed"
                                    || event.event_type == "response.done"
                                {
                                    yield Ok(StreamChunk { text: String::new(), is_final: true });
                                    return;
                                }
                                // Ignore other event types (reasoning, metadata, etc.)
                            }
                            Err(e) => {
                                tracing::debug!("Failed to parse SSE data: {} - {}", e, data);
                            }
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_has_correct_name() {
        let provider = OpenAiProvider::new(
            "test-key".to_string(),
            "gpt-5.2-2025-12-11".to_string(),
            None,
            "high".to_string(),
            None,
            None,
        );
        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn provider_uses_default_context_limit() {
        let provider = OpenAiProvider::new(
            "test-key".to_string(),
            "gpt-5.2-2025-12-11".to_string(),
            None,
            "high".to_string(),
            None,
            None,
        );
        assert_eq!(provider.max_context_tokens(), 1_000_000);
    }

    #[test]
    fn provider_uses_custom_context_limit() {
        let provider = OpenAiProvider::new(
            "test-key".to_string(),
            "gpt-4o".to_string(),
            None,
            "high".to_string(),
            Some(128_000),
            None,
        );
        assert_eq!(provider.max_context_tokens(), 128_000);
    }

    #[test]
    fn responses_api_request_uses_string_input() {
        // Verify the request format is correct for the Responses API.
        // The input field should be a simple string, not an array of messages.
        let schema: serde_json::Value =
            serde_json::from_str(REVIEW_OUTPUT_SCHEMA).expect("Valid schema");

        let request = ResponsesRequest {
            model: "gpt-5.2".to_string(),
            instructions: "You are a helpful assistant".to_string(),
            input: "Hello, how are you?".to_string(),
            reasoning: ReasoningConfig {
                effort: "high".to_string(),
            },
            text: TextFormat {
                format: TextFormatType {
                    format_type: "json_schema".to_string(),
                    schema,
                    name: "review_output".to_string(),
                    strict: true,
                },
            },
            max_output_tokens: Some(1000),
            stream: true,
        };

        let json = serde_json::to_value(&request).expect("Should serialize");

        // Verify input is a string, not an array
        assert!(
            json["input"].is_string(),
            "input should be a string, got: {}",
            json["input"]
        );
        assert_eq!(json["input"], "Hello, how are you?");

        // Verify other required fields are present
        assert_eq!(json["model"], "gpt-5.2");
        assert_eq!(json["instructions"], "You are a helpful assistant");
        assert_eq!(json["reasoning"]["effort"], "high");
        assert_eq!(json["stream"], true);
    }
}
