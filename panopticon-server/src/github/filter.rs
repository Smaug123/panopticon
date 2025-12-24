use std::path::Path;

/// File filter configuration for excluding files from repo content.
#[derive(Debug, Clone)]
pub struct FileFilter {
    /// Glob patterns to exclude (applied first).
    exclude_patterns: Vec<glob::Pattern>,
    /// Maximum file size in bytes.
    max_file_size: u64,
}

impl Default for FileFilter {
    fn default() -> Self {
        let exclude_patterns = [
            // Lock files
            "**/package-lock.json",
            "**/yarn.lock",
            "**/Cargo.lock",
            "**/Gemfile.lock",
            "**/poetry.lock",
            "**/pnpm-lock.yaml",
            "**/flake.lock",
            "**/composer.lock",
            "**/Pipfile.lock",
            // Generated directories
            "**/node_modules/**",
            "**/target/**",
            "**/dist/**",
            "**/build/**",
            "**/.git/**",
            "**/__pycache__/**",
            "**/.venv/**",
            "**/venv/**",
            "**/.tox/**",
            "**/.mypy_cache/**",
            "**/.pytest_cache/**",
            "**/coverage/**",
            "**/.next/**",
            "**/.nuxt/**",
            "**/vendor/**",
            "**/deps/**",
            "**/_build/**",
            // Binary and media files
            "**/*.png",
            "**/*.jpg",
            "**/*.jpeg",
            "**/*.gif",
            "**/*.ico",
            "**/*.webp",
            "**/*.svg",
            "**/*.bmp",
            "**/*.woff",
            "**/*.woff2",
            "**/*.ttf",
            "**/*.eot",
            "**/*.otf",
            "**/*.pdf",
            "**/*.zip",
            "**/*.tar",
            "**/*.tar.gz",
            "**/*.tgz",
            "**/*.rar",
            "**/*.7z",
            "**/*.exe",
            "**/*.dll",
            "**/*.so",
            "**/*.dylib",
            "**/*.a",
            "**/*.o",
            "**/*.obj",
            "**/*.pyc",
            "**/*.pyo",
            "**/*.class",
            "**/*.jar",
            "**/*.war",
            "**/*.ear",
            "**/*.wasm",
            "**/*.mp3",
            "**/*.mp4",
            "**/*.avi",
            "**/*.mov",
            "**/*.wav",
            "**/*.ogg",
            "**/*.webm",
            // Minified files
            "**/*.min.js",
            "**/*.min.css",
            "**/*.bundle.js",
            "**/*.bundle.css",
            // Source maps
            "**/*.map",
            // Database files
            "**/*.db",
            "**/*.sqlite",
            "**/*.sqlite3",
            // IDE and editor files
            "**/.idea/**",
            "**/.vscode/**",
            "**/.vs/**",
            "**/*.swp",
            "**/*.swo",
            "**/*~",
            // OS files
            "**/.DS_Store",
            "**/Thumbs.db",
        ]
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

        Self {
            exclude_patterns,
            max_file_size: 100_000, // 100KB
        }
    }
}

impl FileFilter {
    /// Check if a file should be included in the repo content.
    pub fn should_include(&self, path: &Path, size: u64) -> bool {
        // Check size first (cheap)
        if size > self.max_file_size {
            return false;
        }

        // Check exclude patterns
        let path_str = path.to_string_lossy();
        for pattern in &self.exclude_patterns {
            if pattern.matches(&path_str) {
                return false;
            }
        }

        // Exclude hidden files and directories (except .github, .gitignore, etc.)
        for component in path.components() {
            if let std::path::Component::Normal(name) = component {
                let name = name.to_string_lossy();
                if name.starts_with('.')
                    && name != ".github"
                    && name != ".gitignore"
                    && name != ".gitattributes"
                    && name != ".editorconfig"
                    && name != ".env.example"
                    && name != ".dockerignore"
                {
                    return false;
                }
            }
        }

        true
    }

    /// Create a filter with custom max file size.
    pub fn with_max_file_size(mut self, size: u64) -> Self {
        self.max_file_size = size;
        self
    }

    /// Add additional exclude patterns.
    pub fn with_exclude_patterns(mut self, patterns: &[&str]) -> Self {
        for p in patterns {
            if let Ok(pattern) = glob::Pattern::new(p) {
                self.exclude_patterns.push(pattern);
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn excludes_lock_files() {
        let filter = FileFilter::default();

        assert!(!filter.should_include(&PathBuf::from("package-lock.json"), 100));
        assert!(!filter.should_include(&PathBuf::from("yarn.lock"), 100));
        assert!(!filter.should_include(&PathBuf::from("Cargo.lock"), 100));
        assert!(!filter.should_include(&PathBuf::from("sub/dir/package-lock.json"), 100));
    }

    #[test]
    fn excludes_generated_directories() {
        let filter = FileFilter::default();

        assert!(!filter.should_include(&PathBuf::from("node_modules/foo/index.js"), 100));
        assert!(!filter.should_include(&PathBuf::from("target/debug/main"), 100));
        assert!(!filter.should_include(&PathBuf::from(".git/objects/abc"), 100));
    }

    #[test]
    fn excludes_binary_files() {
        let filter = FileFilter::default();

        assert!(!filter.should_include(&PathBuf::from("image.png"), 100));
        assert!(!filter.should_include(&PathBuf::from("font.woff2"), 100));
        assert!(!filter.should_include(&PathBuf::from("archive.zip"), 100));
    }

    #[test]
    fn excludes_large_files() {
        let filter = FileFilter::default();

        assert!(filter.should_include(&PathBuf::from("small.rs"), 1000));
        assert!(!filter.should_include(&PathBuf::from("big.rs"), 200_000));
    }

    #[test]
    fn includes_source_files() {
        let filter = FileFilter::default();

        assert!(filter.should_include(&PathBuf::from("main.rs"), 100));
        assert!(filter.should_include(&PathBuf::from("src/lib.rs"), 100));
        assert!(filter.should_include(&PathBuf::from("index.js"), 100));
        assert!(filter.should_include(&PathBuf::from("style.css"), 100));
        assert!(filter.should_include(&PathBuf::from("README.md"), 100));
    }

    #[test]
    fn includes_special_dotfiles() {
        let filter = FileFilter::default();

        assert!(filter.should_include(&PathBuf::from(".gitignore"), 100));
        assert!(filter.should_include(&PathBuf::from(".editorconfig"), 100));
        assert!(filter.should_include(&PathBuf::from(".github/workflows/ci.yml"), 100));
    }

    #[test]
    fn excludes_other_hidden_files() {
        let filter = FileFilter::default();

        assert!(!filter.should_include(&PathBuf::from(".secret"), 100));
        assert!(!filter.should_include(&PathBuf::from(".hidden/file"), 100));
    }
}
