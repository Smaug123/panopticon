use crate::domain::review::ReviewOutput;

/// JSON Schema for structured LLM output.
///
/// This schema is used to enforce structured output from the LLM,
/// ensuring we always get the three fields we need.
pub const REVIEW_OUTPUT_SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "detailed_reasoning": {
            "type": "string",
            "description": "Your detailed internal reasoning and analysis process. This will be stored but not shown to users by default."
        },
        "action_required": {
            "type": "boolean",
            "description": "Whether this review identifies issues that require attention or action from the developer."
        },
        "user_visible_comments": {
            "type": "string",
            "description": "The user-facing review comments in Markdown format. Be clear, specific, and actionable. Reference files and line numbers where applicable."
        }
    },
    "required": ["detailed_reasoning", "action_required", "user_visible_comments"],
    "additionalProperties": false
}"#;

/// Parse the LLM's JSON response into a ReviewOutput.
pub fn parse_review_output(json: &str) -> Result<ReviewOutput, ParseError> {
    // First try direct parse
    if let Ok(output) = serde_json::from_str::<ReviewOutput>(json) {
        return Ok(output);
    }

    // Try to extract JSON from markdown code blocks
    let cleaned = extract_json_from_markdown(json);
    serde_json::from_str::<ReviewOutput>(&cleaned).map_err(|e| ParseError::InvalidJson {
        message: e.to_string(),
        raw: json.to_string(),
    })
}

/// Extract JSON from markdown code blocks if present.
fn extract_json_from_markdown(text: &str) -> String {
    // Look for ```json ... ``` blocks
    if let Some(start) = text.find("```json") {
        let content_start = start + 7;
        if let Some(end) = text[content_start..].find("```") {
            return text[content_start..content_start + end].trim().to_string();
        }
    }

    // Look for ``` ... ``` blocks
    if let Some(start) = text.find("```") {
        let content_start = start + 3;
        // Skip language identifier if present
        let content_start = text[content_start..]
            .find('\n')
            .map(|i| content_start + i + 1)
            .unwrap_or(content_start);
        if let Some(end) = text[content_start..].find("```") {
            return text[content_start..content_start + end].trim().to_string();
        }
    }

    text.to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Invalid JSON response: {message}")]
    InvalidJson { message: String, raw: String },
}

/// Build the system prompt for a review, incorporating the user's custom prompt.
pub fn build_system_prompt(custom_prompt: &str) -> String {
    format!(
        r#"You are a code review assistant. Your task is to review the provided codebase and provide feedback.

{custom_prompt}

IMPORTANT: You must respond with a JSON object matching this exact schema:
{}

Guidelines for your response:
- detailed_reasoning: Write out your complete analysis process here. Consider architecture, code quality, potential bugs, security, performance, and testing.
- action_required: Set to true if you found issues that should be addressed. Set to false if the code looks good or only has minor suggestions.
- user_visible_comments: Write clear, actionable feedback in Markdown. Use headings, bullet points, and code blocks for clarity. Reference specific files and line numbers.

Do not include any text outside the JSON object."#,
        REVIEW_OUTPUT_SCHEMA
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_json() {
        let json = r#"{
            "detailed_reasoning": "The code looks good overall.",
            "action_required": false,
            "user_visible_comments": "No issues found."
        }"#;

        let output = parse_review_output(json).unwrap();
        assert_eq!(output.detailed_reasoning, "The code looks good overall.");
        assert!(!output.action_required);
        assert_eq!(output.user_visible_comments, "No issues found.");
    }

    #[test]
    fn extracts_json_from_markdown() {
        let markdown = r#"Here's my analysis:

```json
{
    "detailed_reasoning": "Found issues.",
    "action_required": true,
    "user_visible_comments": "Fix the bug."
}
```

Hope this helps!"#;

        let output = parse_review_output(markdown).unwrap();
        assert!(output.action_required);
    }

    #[test]
    fn build_system_prompt_includes_custom() {
        let prompt = build_system_prompt("Focus on security.");
        assert!(prompt.contains("Focus on security."));
        assert!(prompt.contains("detailed_reasoning"));
    }
}
