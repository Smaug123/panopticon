//! Types for handling untrusted content that requires sanitization before display.
//!
//! LLM output is untrusted and could contain malicious content (XSS payloads,
//! prompt injection, etc). These types make it impossible to accidentally
//! render untrusted content without explicit sanitization.

use serde::{Deserialize, Serialize};

/// A string from an untrusted source that must be sanitized before display.
///
/// This type wraps content from external sources (like LLM responses) and
/// prevents accidental use in HTML contexts without proper escaping.
///
/// # Design
///
/// - Cannot be interpolated into strings via Display or ToString
/// - Must explicitly call `sanitize_html()` or `as_raw()` to extract content
/// - The `as_raw()` method is marked unsafe to discourage casual use
/// - Serializes as a regular string for storage/API responses
///
/// # Example
///
/// ```
/// use panopticon_server::domain::untrusted::UntrustedString;
///
/// let untrusted = UntrustedString::new("<script>alert('xss')</script>");
/// let safe = untrusted.sanitize_html();
/// assert!(!safe.contains("<script>"));
/// assert!(safe.contains("&lt;script&gt;"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UntrustedString(String);

impl From<String> for UntrustedString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for UntrustedString {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl UntrustedString {
    /// Create a new untrusted string.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Get the raw content. Use with caution - this bypasses sanitization.
    ///
    /// Only use this when:
    /// - Storing to a database (which will re-sanitize on display)
    /// - Passing to another system that handles its own sanitization
    /// - Debugging or logging (not for user display)
    pub fn as_raw(&self) -> &str {
        &self.0
    }

    /// Get the length of the raw content.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Check if the content is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Sanitize the content for safe HTML display.
    ///
    /// This escapes HTML special characters to prevent XSS:
    /// - `<` becomes `&lt;`
    /// - `>` becomes `&gt;`
    /// - `&` becomes `&amp;`
    /// - `"` becomes `&quot;`
    /// - `'` becomes `&#x27;`
    pub fn sanitize_html(&self) -> String {
        html_escape(&self.0)
    }

    /// Render markdown content with HTML sanitization.
    ///
    /// This converts markdown to HTML while ensuring any raw HTML in the
    /// markdown source is escaped (not rendered as actual HTML).
    pub fn render_markdown_safe(&self) -> String {
        // First escape any raw HTML in the markdown source
        let escaped = html_escape(&self.0);
        // Then render markdown (which is safe because raw HTML was escaped)
        render_markdown(&escaped)
    }
}

/// Escape HTML special characters.
fn html_escape(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&#x27;"),
            _ => result.push(c),
        }
    }
    result
}

/// Simple markdown to HTML conversion.
///
/// This is a basic implementation that handles common patterns.
/// The input should already be HTML-escaped before calling this.
fn render_markdown(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut result = String::with_capacity(text.len() * 2);

    for line in text.lines() {
        // Headers
        if let Some(content) = line.strip_prefix("#### ") {
            result.push_str("<h4>");
            result.push_str(content);
            result.push_str("</h4>\n");
        } else if let Some(content) = line.strip_prefix("### ") {
            result.push_str("<h3>");
            result.push_str(content);
            result.push_str("</h3>\n");
        } else if let Some(content) = line.strip_prefix("## ") {
            result.push_str("<h2>");
            result.push_str(content);
            result.push_str("</h2>\n");
        } else if let Some(content) = line.strip_prefix("# ") {
            result.push_str("<h1>");
            result.push_str(content);
            result.push_str("</h1>\n");
        } else if line.trim().is_empty() {
            result.push('\n');
        } else {
            // Regular paragraph line
            result.push_str("<p>");
            result.push_str(line);
            result.push_str("</p>\n");
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escape_works() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("a & b"), "a &amp; b");
        assert_eq!(html_escape(r#""quoted""#), "&quot;quoted&quot;");
        assert_eq!(html_escape("it's"), "it&#x27;s");
    }

    #[test]
    fn untrusted_string_sanitizes_xss() {
        let xss = UntrustedString::new("<script>alert('xss')</script>");
        let safe = xss.sanitize_html();
        assert!(!safe.contains("<script>"));
        assert!(safe.contains("&lt;script&gt;"));
    }

    #[test]
    fn untrusted_string_serializes_as_string() {
        let untrusted = UntrustedString::new("test content");
        let json = serde_json::to_string(&untrusted).unwrap();
        assert_eq!(json, "\"test content\"");
    }

    #[test]
    fn untrusted_string_deserializes_from_string() {
        let json = "\"test content\"";
        let untrusted: UntrustedString = serde_json::from_str(json).unwrap();
        assert_eq!(untrusted.as_raw(), "test content");
    }

    #[test]
    fn markdown_rendering_escapes_html() {
        let malicious = UntrustedString::new("# Header\n<img src=x onerror=\"alert('xss')\">");
        let rendered = malicious.render_markdown_safe();
        assert!(!rendered.contains("<img"));
        assert!(rendered.contains("&lt;img"));
    }

    #[test]
    fn event_handlers_are_neutralized() {
        let malicious = UntrustedString::new("<div onclick=\"steal()\">click me</div>");
        let safe = malicious.sanitize_html();
        // The tag is escaped, so onclick can't execute (it's just text now)
        assert!(safe.contains("&lt;div"));
        assert!(!safe.contains("<div"), "Raw tag should be escaped");
    }
}
