use crate::github::fetch::FileContent;

/// Format repository contents for LLM consumption.
///
/// Each file is formatted with a header showing its path, followed by its content.
pub fn format_for_llm(contents: &[FileContent]) -> String {
    let mut output = String::new();

    for file in contents {
        output.push_str(&format!("=== {} ===\n", file.path.display()));
        output.push_str(&file.content);
        if !file.content.ends_with('\n') {
            output.push('\n');
        }
        output.push('\n');
    }

    output
}

/// Estimate token count using a rough heuristic.
///
/// This uses the common approximation of ~4 characters per token for English text.
/// For code, this tends to slightly overestimate, which is safer for context limits.
pub fn estimate_tokens(text: &str) -> u32 {
    (text.len() / 4) as u32
}

/// Split contents into chunks that fit within a token limit.
#[derive(Debug)]
pub struct Chunk {
    pub content: String,
    pub file_count: usize,
    pub estimated_tokens: u32,
}

/// Chunking strategy for large repositories.
#[derive(Debug, Clone)]
pub enum ChunkingStrategy {
    /// Send everything in one request (fail if too large).
    NoChunking,
    /// Split by files, grouping until token limit is reached.
    ByFile { max_tokens_per_chunk: u32 },
}

impl Default for ChunkingStrategy {
    fn default() -> Self {
        Self::ByFile {
            max_tokens_per_chunk: 100_000,
        }
    }
}

/// Split files into chunks based on strategy.
pub fn chunk_contents(files: &[FileContent], strategy: &ChunkingStrategy) -> Vec<Chunk> {
    match strategy {
        ChunkingStrategy::NoChunking => {
            let content = format_for_llm(files);
            vec![Chunk {
                estimated_tokens: estimate_tokens(&content),
                file_count: files.len(),
                content,
            }]
        }
        ChunkingStrategy::ByFile { max_tokens_per_chunk } => {
            chunk_by_file(files, *max_tokens_per_chunk)
        }
    }
}

fn format_single_file(file: &FileContent) -> String {
    let mut output = format!("=== {} ===\n", file.path.display());
    output.push_str(&file.content);
    if !file.content.ends_with('\n') {
        output.push('\n');
    }
    output.push('\n');
    output
}

fn chunk_by_file(files: &[FileContent], max_tokens: u32) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut current_content = String::new();
    let mut current_files = 0usize;
    let mut current_tokens = 0u32;

    for file in files {
        let file_content = format_single_file(file);
        let file_tokens = estimate_tokens(&file_content);

        // If single file exceeds limit, it gets its own chunk
        if file_tokens > max_tokens {
            // Flush current chunk first
            if current_files > 0 {
                chunks.push(Chunk {
                    content: std::mem::take(&mut current_content),
                    file_count: current_files,
                    estimated_tokens: current_tokens,
                });
                current_files = 0;
                current_tokens = 0;
            }

            // Add oversized file as solo chunk
            chunks.push(Chunk {
                content: file_content,
                file_count: 1,
                estimated_tokens: file_tokens,
            });
            continue;
        }

        // Would adding this file exceed limit?
        if current_tokens + file_tokens > max_tokens && current_files > 0 {
            chunks.push(Chunk {
                content: std::mem::take(&mut current_content),
                file_count: current_files,
                estimated_tokens: current_tokens,
            });
            current_files = 0;
            current_tokens = 0;
        }

        current_content.push_str(&file_content);
        current_files += 1;
        current_tokens += file_tokens;
    }

    // Don't forget the last chunk
    if current_files > 0 {
        chunks.push(Chunk {
            content: current_content,
            file_count: current_files,
            estimated_tokens: current_tokens,
        });
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_file(name: &str, content: &str) -> FileContent {
        FileContent {
            path: PathBuf::from(name),
            content: content.to_string(),
        }
    }

    #[test]
    fn format_includes_file_headers() {
        let files = vec![
            make_file("main.rs", "fn main() {}"),
            make_file("lib.rs", "pub mod foo;"),
        ];

        let output = format_for_llm(&files);

        assert!(output.contains("=== main.rs ==="));
        assert!(output.contains("fn main() {}"));
        assert!(output.contains("=== lib.rs ==="));
        assert!(output.contains("pub mod foo;"));
    }

    #[test]
    fn estimate_tokens_approximates() {
        // ~4 chars per token
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn chunking_respects_token_limit() {
        // Create files that are ~25 tokens each (100 chars)
        let files: Vec<FileContent> = (0..10)
            .map(|i| make_file(&format!("file{}.rs", i), &"x".repeat(100)))
            .collect();

        let strategy = ChunkingStrategy::ByFile {
            max_tokens_per_chunk: 100, // ~400 chars, fits ~3 files with headers
        };

        let chunks = chunk_contents(&files, &strategy);

        // Should have multiple chunks
        assert!(chunks.len() > 1);

        // Each chunk should be under the limit (with some tolerance for headers)
        for chunk in &chunks {
            assert!(
                chunk.estimated_tokens <= 150, // Allow some overhead
                "Chunk has {} tokens",
                chunk.estimated_tokens
            );
        }
    }

    #[test]
    fn chunking_preserves_all_files() {
        let files: Vec<FileContent> = (0..5)
            .map(|i| make_file(&format!("file{}.rs", i), "content"))
            .collect();

        let strategy = ChunkingStrategy::ByFile {
            max_tokens_per_chunk: 50,
        };

        let chunks = chunk_contents(&files, &strategy);
        let total_files: usize = chunks.iter().map(|c| c.file_count).sum();

        assert_eq!(total_files, 5);
    }

    #[test]
    fn no_chunking_returns_single_chunk() {
        let files = vec![make_file("a.rs", "a"), make_file("b.rs", "b")];

        let chunks = chunk_contents(&files, &ChunkingStrategy::NoChunking);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].file_count, 2);
    }
}
