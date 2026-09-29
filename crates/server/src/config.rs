//! Server configuration: `clankjob.toml` (design §18.2).
//!
//! Secrets are never written in the file itself; they are references resolved at load
//! time from Docker secrets (`{ secret = "name" }`) or environment variables
//! (`{ env = "NAME" }`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clankjob_core::case::Budgets;
use secrecy::SecretString;
use serde::Deserialize;

/// Default location of Docker secrets.
const DEFAULT_SECRETS_DIR: &str = "/run/secrets";

/// Errors loading the configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The file is not valid TOML or has unknown or missing fields.
    #[error("invalid configuration: {0}")]
    Parse(#[from] toml::de::Error),
    /// A secret reference could not be resolved.
    #[error("secret {reference}: {message}")]
    Secret {
        /// The reference, e.g. `secret "api_token"`.
        reference: String,
        /// What went wrong.
        message: String,
    },
    /// The values are inconsistent.
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Where a secret comes from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
// `untagged` picks the variant by shape: a bare string, `{ secret = … }` or `{ env = … }`.
#[serde(untagged)]
pub enum SecretRef {
    /// A file in the secrets directory (Docker secrets).
    File {
        /// File name inside the secrets directory.
        secret: String,
    },
    /// An environment variable.
    Env {
        /// Variable name.
        env: String,
    },
    /// The value itself. Accepted, but logged as a warning.
    Literal(String),
}

impl SecretRef {
    /// Resolve the secret's value.
    ///
    /// # Arguments
    ///
    /// * `secrets_dir` - Directory holding Docker secrets
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Secret`] if the file or variable is missing or empty, or the
    /// secret name tries to escape the secrets directory.
    pub fn resolve<P>(&self, secrets_dir: P) -> Result<SecretString, ConfigError>
    where
        P: AsRef<Path>,
    {
        let (reference, value) = match self {
            Self::File { secret } => {
                let reference = format!("secret \"{secret}\"");
                if secret.is_empty() || secret.contains(['/', '\\']) || secret.starts_with('.') {
                    return Err(ConfigError::Secret {
                        reference,
                        message: "invalid secret name".to_owned(),
                    });
                }
                let path = secrets_dir.as_ref().join(secret);
                let value = std::fs::read_to_string(&path).map_err(|error| ConfigError::Secret {
                    reference: reference.clone(),
                    message: format!("cannot read {}: {error}", path.display()),
                })?;
                (reference, value)
            }
            Self::Env { env } => {
                let reference = format!("env \"{env}\"");
                let value = std::env::var(env).map_err(|error| ConfigError::Secret {
                    reference: reference.clone(),
                    message: error.to_string(),
                })?;
                (reference, value)
            }
            Self::Literal(value) => {
                tracing::warn!("a secret is written literally in the configuration; prefer a secret or env reference");
                ("literal".to_owned(), value.clone())
            }
        };
        // Secret files usually end with a newline that is not part of the value.
        let value = value.trim();
        if value.is_empty() {
            return Err(ConfigError::Secret {
                reference,
                message: "value is empty".to_owned(),
            });
        }
        Ok(SecretString::from(value.to_owned()))
    }
}

/// Supported LLM provider kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ProviderKind {
    /// OpenAI-compatible Chat Completions.
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
}

/// One configured LLM.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    /// Provider kind.
    pub provider: ProviderKind,
    /// API base URL.
    pub base_url: String,
    /// API key; omit for local servers that need none.
    #[serde(default)]
    pub api_key: Option<SecretRef>,
    /// Default model.
    pub model: String,
    /// Other models offered when starting a case (any model id is still accepted).
    #[serde(default)]
    pub models: Vec<String>,
    /// Also offer the models the provider lists at `{base_url}/models`, refreshed hourly.
    #[serde(default = "default_true")]
    pub discover_models: bool,
    /// The model can see images, so cases can show it image files (design §7.5).
    #[serde(default)]
    pub vision: bool,
    /// Maximum time for one completion.
    #[serde(default = "default_llm_timeout", with = "humantime_serde")]
    pub timeout: Duration,
}

fn default_true() -> bool {
    true
}

fn default_llm_timeout() -> Duration {
    Duration::from_mins(5)
}

/// API settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// Accepted bearer tokens; at least one is required.
    pub tokens: Vec<SecretRef>,
}

/// The whole configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Address to listen on.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Address users reach the server at, e.g. `https://clank.acme.com` behind a reverse
    /// proxy. Browser pages on this origin may call the API.
    #[serde(default)]
    pub public_url: Option<String>,
    /// Other origins allowed to call the API from a browser (CORS), e.g. a separately
    /// hosted UI or a development server.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Directory for the database and files.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Directory with prompt overrides and profiles.
    #[serde(default)]
    pub prompts_dir: Option<PathBuf>,
    /// Directory holding Docker secrets.
    #[serde(default = "default_secrets_dir")]
    pub secrets_dir: PathBuf,
    /// Worker threads running activations.
    #[serde(default = "default_workers")]
    pub workers: usize,
    /// How long to wait for running activations on shutdown.
    #[serde(default = "default_shutdown_grace", with = "humantime_serde")]
    pub shutdown_grace: Duration,
    /// Profile used by cases that do not name one.
    #[serde(default)]
    pub default_profile: Option<String>,
    /// LLM used by cases that do not name one.
    #[serde(default = "default_llm_name")]
    pub default_llm: String,
    /// API settings.
    pub api: ApiConfig,
    /// Configured LLMs by name.
    pub llm: BTreeMap<String, LlmConfig>,
    /// Budgets for cases that do not set their own.
    #[serde(default)]
    pub budgets: Budgets,
}

fn default_listen() -> String {
    "0.0.0.0:8080".to_owned()
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("/data")
}

fn default_secrets_dir() -> PathBuf {
    PathBuf::from(DEFAULT_SECRETS_DIR)
}

fn default_workers() -> usize {
    4
}

fn default_shutdown_grace() -> Duration {
    Duration::from_secs(30)
}

fn default_llm_name() -> String {
    "default".to_owned()
}

impl Config {
    /// Parse a configuration from TOML text and check it is consistent.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Parse`] for invalid TOML and [`ConfigError::Invalid`] if no
    /// API token or LLM is configured, `default_llm` is unknown, or `workers` is zero.
    pub fn parse<S>(text: S) -> Result<Self, ConfigError>
    where
        S: AsRef<str>,
    {
        let config: Self = toml::from_str(text.as_ref())?;
        if config.api.tokens.is_empty() {
            return Err(ConfigError::Invalid("`api.tokens` needs at least one token".to_owned()));
        }
        if !config.llm.contains_key(&config.default_llm) {
            return Err(ConfigError::Invalid(format!(
                "`default_llm` \"{}\" is not a configured [llm]",
                config.default_llm
            )));
        }
        if config.workers == 0 {
            return Err(ConfigError::Invalid("`workers` must be at least 1".to_owned()));
        }
        let urls = config.public_url.iter().chain(&config.allowed_origins);
        if let Some(bad) = urls.into_iter().find(|url| crate::cors::origin_of(url).is_none()) {
            return Err(ConfigError::Invalid(format!(
                "\"{bad}\" is not a URL like https://clank.acme.com (in `public_url` or `allowed_origins`)"
            )));
        }
        Ok(config)
    }

    /// Origins allowed to call the API from a browser: `public_url` plus `allowed_origins`,
    /// normalized to `scheme://host[:port]`.
    #[must_use]
    pub fn cors_origins(&self) -> Vec<String> {
        let mut origins: Vec<String> = Vec::with_capacity(self.allowed_origins.len().saturating_add(1));
        for origin in self
            .public_url
            .iter()
            .chain(&self.allowed_origins)
            .filter_map(|url| crate::cors::origin_of(url))
        {
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
        origins
    }

    /// Read and parse a configuration file.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Read`] if the file cannot be read, or any error of
    /// [`Config::parse`].
    pub fn load<P>(path: P) -> Result<Self, ConfigError>
    where
        P: AsRef<Path>,
    {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    const MINIMAL: &str = r#"
        [api]
        tokens = [{ secret = "api_token" }]

        [llm.default]
        provider = "openai-compatible"
        base_url = "http://localhost:11434/v1"
        model = "llama3"
    "#;

    #[test]
    fn minimal_config_gets_defaults() {
        let config = Config::parse(MINIMAL).unwrap();

        assert_eq!(config.listen, "0.0.0.0:8080");
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(config.workers, 4);
        assert_eq!(config.budgets, Budgets::default());
        assert_eq!(config.llm["default"].timeout, Duration::from_mins(5));
        assert_eq!(config.llm["default"].api_key, None);
        assert!(config.llm["default"].models.is_empty());
        assert!(config.llm["default"].discover_models);
        assert!(config.cors_origins().is_empty());
    }

    #[test]
    fn public_url_and_allowed_origins_become_cors_origins() {
        let text = format!(
            "public_url = \"https://Clank.acme.com/\"\n\
             allowed_origins = [\"http://localhost:5173\", \"https://clank.acme.com\"]\n{MINIMAL}"
        );

        let config = Config::parse(text).unwrap();

        assert_eq!(
            config.cors_origins(),
            ["https://clank.acme.com", "http://localhost:5173"]
        );
    }

    #[test]
    fn a_public_url_without_scheme_is_rejected() {
        let text = format!("public_url = \"clank.acme.com\"\n{MINIMAL}");

        assert!(matches!(Config::parse(text), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn full_config_parses() {
        let text = r#"
            listen = "127.0.0.1:9000"
            data_dir = "/tmp/clankjob"
            prompts_dir = "/prompts"
            workers = 2
            shutdown_grace = "1m"
            default_profile = "general"
            default_llm = "fast"

            [api]
            tokens = ["literal-token", { env = "API_TOKEN" }]

            [llm.fast]
            provider = "openai-compatible"
            base_url = "https://api.openai.com/v1"
            api_key = { secret = "llm_api_key" }
            model = "gpt-4.1-mini"
            timeout = "90s"

            [budgets]
            max_activations = 5
        "#;

        let config = Config::parse(text).unwrap();

        assert_eq!(config.default_llm, "fast");
        assert_eq!(config.shutdown_grace, Duration::from_mins(1));
        assert_eq!(config.api.tokens[0], SecretRef::Literal("literal-token".to_owned()));
        assert_eq!(
            config.api.tokens[1],
            SecretRef::Env {
                env: "API_TOKEN".to_owned()
            }
        );
        assert_eq!(config.budgets.max_activations, 5);
        assert_eq!(
            config.budgets.max_turns_per_activation,
            Budgets::default().max_turns_per_activation
        );
    }

    #[test]
    fn example_config_in_the_repository_is_valid() {
        let config = Config::parse(include_str!("../../../clankjob.example.toml")).unwrap();

        assert_eq!(config.llm["default"].base_url, "https://openrouter.ai/api/v1");
        assert_eq!(
            config.api.tokens[0],
            SecretRef::Env {
                env: "CLANKJOB_TOKEN".to_owned()
            }
        );
    }

    #[test]
    fn inconsistent_configs_are_rejected() {
        let no_tokens = MINIMAL.replace(r#"[{ secret = "api_token" }]"#, "[]");
        let bad_default = format!("default_llm = \"missing\"\n{MINIMAL}");
        let typo = format!("workerz = 3\n{MINIMAL}");

        assert!(matches!(Config::parse(no_tokens), Err(ConfigError::Invalid(_))));
        assert!(matches!(Config::parse(bad_default), Err(ConfigError::Invalid(_))));
        assert!(matches!(Config::parse(typo), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn secrets_resolve_from_files_and_are_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("api_token"), "s3cret\n").unwrap();

        let value = SecretRef::File {
            secret: "api_token".to_owned(),
        }
        .resolve(dir.path())
        .unwrap();

        assert_eq!(value.expose_secret(), "s3cret");
    }

    #[test]
    fn secret_names_cannot_escape_the_secrets_directory() {
        let dir = tempfile::tempdir().unwrap();

        for name in ["../etc/passwd", "a/b", ".hidden", ""] {
            let result = SecretRef::File {
                secret: name.to_owned(),
            }
            .resolve(dir.path());
            assert!(matches!(result, Err(ConfigError::Secret { .. })), "{name}");
        }
    }

    #[test]
    fn missing_or_empty_secrets_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty"), "\n").unwrap();

        assert!(
            SecretRef::File {
                secret: "absent".to_owned()
            }
            .resolve(dir.path())
            .is_err()
        );
        assert!(
            SecretRef::File {
                secret: "empty".to_owned()
            }
            .resolve(dir.path())
            .is_err()
        );
        assert!(
            SecretRef::Env {
                env: "CLANKJOB_TEST_UNSET_VARIABLE".to_owned()
            }
            .resolve(dir.path())
            .is_err()
        );
        assert_eq!(
            SecretRef::Literal("x".to_owned()).resolve(dir.path()).unwrap().expose_secret(),
            "x"
        );
    }
}
