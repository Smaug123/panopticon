use std::path::PathBuf;

use config::{Config, Environment, File};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    pub llm: LlmConfig,
    #[serde(default)]
    pub github: GitHubConfig,
    pub auth: AuthConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    3000
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_path")]
    pub path: PathBuf,
}

fn default_db_path() -> PathBuf {
    PathBuf::from("panopticon.db")
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LlmConfig {
    pub provider: LlmProviderConfig,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LlmProviderConfig {
    OpenAi {
        api_key: String,
        #[serde(default = "default_openai_model")]
        model: String,
        #[serde(default)]
        base_url: Option<String>,
        #[serde(default = "default_reasoning_effort")]
        reasoning_effort: String,
    },
    // Future providers can be added here:
    // Gemini { api_key: String, model: String },
    // Claude { api_key: String, model: String },
}

fn default_openai_model() -> String {
    "gpt-5.2-2025-12-11".to_string()
}

fn default_reasoning_effort() -> String {
    "high".to_string()
}

#[derive(Debug, Deserialize)]
pub struct GitHubConfig {
    #[serde(default = "default_repos_dir")]
    pub repos_dir: PathBuf,
}

fn default_repos_dir() -> PathBuf {
    PathBuf::from("./repos")
}

impl Default for GitHubConfig {
    fn default() -> Self {
        Self {
            repos_dir: default_repos_dir(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AuthConfig {
    pub api_key: String,
}

#[derive(Debug, Deserialize)]
pub struct SchedulerConfig {
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_review_interval")]
    pub review_interval_hours: u64,
    /// Jobs running longer than this are considered stuck and will be reclaimed.
    /// This handles process crashes leaving jobs in 'running' state.
    #[serde(default = "default_job_timeout")]
    pub job_timeout_minutes: u64,
}

fn default_poll_interval() -> u64 {
    60
}

fn default_review_interval() -> u64 {
    24
}

fn default_job_timeout() -> u64 {
    30 // 30 minutes default
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval(),
            review_interval_hours: default_review_interval(),
            job_timeout_minutes: default_job_timeout(),
        }
    }
}

impl AppConfig {
    /// Load configuration from files and environment variables.
    ///
    /// Priority (highest to lowest):
    /// 1. Environment variables (PANOPTICON_*)
    /// 2. config.toml in current directory
    /// 3. /etc/panopticon/config.toml
    /// 4. Default values
    ///
    /// Environment variable format uses double underscore as separator:
    /// - PANOPTICON__SERVER__PORT=8080 -> server.port = 8080
    /// - PANOPTICON__LLM__PROVIDER__API_KEY=sk-... -> llm.provider.api_key
    /// - PANOPTICON__LLM__PROVIDER__TYPE=open_ai -> llm.provider.type = "open_ai"
    pub fn load() -> Result<Self, config::ConfigError> {
        let config = Config::builder()
            // Load from /etc/panopticon/config.toml if exists
            .add_source(File::with_name("/etc/panopticon/config").required(false))
            // Load from config.toml in current directory if exists
            .add_source(File::with_name("config").required(false))
            // Override with environment variables
            // Use double underscore as separator to allow single underscores in keys
            .add_source(
                Environment::with_prefix("PANOPTICON")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        config.try_deserialize()
    }

    /// Load configuration from a specific file path.
    pub fn load_from(path: &str) -> Result<Self, config::ConfigError> {
        let config = Config::builder()
            .add_source(File::with_name(path))
            .add_source(
                Environment::with_prefix("PANOPTICON")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        config.try_deserialize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_are_sane() {
        assert_eq!(default_host(), "127.0.0.1");
        assert_eq!(default_port(), 3000);
        assert_eq!(default_poll_interval(), 60);
        assert_eq!(default_review_interval(), 24);
    }
}
