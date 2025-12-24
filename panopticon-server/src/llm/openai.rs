use std::pin::Pin;

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
}

impl OpenAiProvider {
    pub fn new(
        api_key: String,
        model: String,
        base_url: Option<String>,
        reasoning_effort: String,
    ) -> Self {
        // Use ClientBuilder with no_proxy to avoid system proxy lookup
        // which can fail in sandboxed test environments on macOS
        let client = Client::builder()
            .no_proxy()
            .build()
            .expect("Failed to create HTTP client");
        Self {
            client,
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            reasoning_effort,
        }
    }
}

/// Input message for the Responses API.
#[derive(Serialize)]
struct ResponsesInputMessage {
    role: String,
    content: String,
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
    input: Vec<ResponsesInputMessage>,
    reasoning: ReasoningConfig,
    text: TextFormat,
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
        // gpt-5.2 supports 1M context
        1_000_000
    }

    fn complete_stream(
        &self,
        request: LlmRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send + '_>> {
        Box::pin(stream! {
            let input = vec![ResponsesInputMessage {
                role: "user".to_string(),
                content: request.user_prompt,
            }];

            // Parse the schema for structured output
            let schema: serde_json::Value = serde_json::from_str(REVIEW_OUTPUT_SCHEMA)
                .expect("Invalid review output schema");

            let body = ResponsesRequest {
                model: self.model.clone(),
                instructions: request.system_prompt,
                input,
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
        );
        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn provider_has_reasonable_context_limit() {
        let provider = OpenAiProvider::new(
            "test-key".to_string(),
            "gpt-5.2-2025-12-11".to_string(),
            None,
            "high".to_string(),
        );
        assert!(provider.max_context_tokens() >= 100_000);
    }
}
