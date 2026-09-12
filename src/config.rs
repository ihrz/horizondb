use std::{fmt, fs, path::Path};

use serde::Deserialize;

use crate::error::AppError;

const DEFAULT_SERVER_MAX_CONNECTIONS: usize = 1024;
/// A message above this size drops the whole client connection (and every
/// request in flight on it), so the default leaves room for large documents.
const DEFAULT_MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;
const MAX_MESSAGE_SIZE_LIMIT: usize = 256 * 1024 * 1024;
const DEFAULT_POOL_MAX_CONNECTIONS: u32 = 32;
const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 5;
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_LIFETIME_SECS: u64 = 1800;
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 3600;
/// How often buffered writes are batch-flushed to `PostgreSQL`.
const DEFAULT_FLUSH_INTERVAL_SECS: u64 = 300;

pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self(String::deserialize(deserializer)?))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    #[serde(default = "default_server_max_connections")]
    pub max_connections: usize,
    #[serde(default = "default_max_message_size")]
    pub max_message_size: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: Secret,
    #[serde(default = "default_pool_max_connections")]
    pub max_connections: u32,
    #[serde(default)]
    pub min_connections: u32,
    #[serde(default = "default_acquire_timeout_secs")]
    pub acquire_timeout_secs: u64,
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_max_lifetime_secs")]
    pub max_lifetime_secs: u64,
    #[serde(default = "default_flush_interval_secs")]
    pub flush_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    pub username: String,
    #[serde(default)]
    pub password: Option<Secret>,
    #[serde(default)]
    pub password_hash: Option<String>,
    #[serde(default = "default_session_timeout_secs")]
    pub session_timeout_secs: u64,
}

fn default_server_max_connections() -> usize {
    DEFAULT_SERVER_MAX_CONNECTIONS
}

fn default_max_message_size() -> usize {
    DEFAULT_MAX_MESSAGE_SIZE
}

fn default_pool_max_connections() -> u32 {
    DEFAULT_POOL_MAX_CONNECTIONS
}

fn default_acquire_timeout_secs() -> u64 {
    DEFAULT_ACQUIRE_TIMEOUT_SECS
}

fn default_idle_timeout_secs() -> u64 {
    DEFAULT_IDLE_TIMEOUT_SECS
}

fn default_max_lifetime_secs() -> u64 {
    DEFAULT_MAX_LIFETIME_SECS
}

fn default_session_timeout_secs() -> u64 {
    DEFAULT_SESSION_TIMEOUT_SECS
}

fn default_flush_interval_secs() -> u64 {
    DEFAULT_FLUSH_INTERVAL_SECS
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, AppError> {
        let raw = fs::read_to_string(path).map_err(|error| {
            AppError::Config(format!("unable to read {}: {error}", path.display()))
        })?;
        let config = parse(&raw)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), AppError> {
        if self.server.host.trim().is_empty() {
            return Err(AppError::Config("server.host must not be empty".to_owned()));
        }
        if self.server.max_connections == 0 {
            return Err(AppError::Config(
                "server.max_connections must be greater than zero".to_owned(),
            ));
        }
        if !(1024..=MAX_MESSAGE_SIZE_LIMIT).contains(&self.server.max_message_size) {
            return Err(AppError::Config(
                "server.max_message_size must be between 1 KiB and 256 MiB".to_owned(),
            ));
        }
        if self.database.min_connections > self.database.max_connections {
            return Err(AppError::Config(
                "database.min_connections cannot exceed max_connections".to_owned(),
            ));
        }
        if self.database.acquire_timeout_secs == 0
            || self.database.idle_timeout_secs == 0
            || self.database.max_lifetime_secs == 0
        {
            return Err(AppError::Config(
                "database timeouts must be greater than zero".to_owned(),
            ));
        }
        if self.database.flush_interval_secs == 0 {
            return Err(AppError::Config(
                "database.flush_interval_secs must be greater than zero".to_owned(),
            ));
        }
        if self.auth.username.trim().is_empty() {
            return Err(AppError::Config(
                "auth.username must not be empty".to_owned(),
            ));
        }
        if self.auth.session_timeout_secs == 0 {
            return Err(AppError::Config(
                "auth.session_timeout_secs must be greater than zero".to_owned(),
            ));
        }
        let has_password = self
            .auth
            .password
            .as_ref()
            .is_some_and(|secret| !secret.expose().trim().is_empty());
        let has_hash = self
            .auth
            .password_hash
            .as_ref()
            .is_some_and(|hash| !hash.trim().is_empty());
        if has_password == has_hash {
            return Err(AppError::Config(
                "auth requires exactly one of password or password_hash".to_owned(),
            ));
        }
        Ok(())
    }
}

fn parse(raw: &str) -> Result<AppConfig, AppError> {
    toml::from_str(raw).map_err(|error| AppError::Config(format!("invalid TOML: {error}")))
}

#[allow(clippy::too_many_lines)]
#[cfg(test)]
mod tests {
    use super::*;

    const VALID_MINIMAL: &str = r#"
[server]
host = "127.0.0.1"
port = 8080

[database]
host = "127.0.0.1"
port = 5432
database = "horizon"
username = "svc"
password = "pw"

[auth]
username = "admin"
password = "secret"
"#;

    #[test]
    fn minimal_config_gets_sane_defaults() {
        let config = parse(VALID_MINIMAL).unwrap();
        config.validate().unwrap();

        assert_eq!(
            config.server.max_connections,
            DEFAULT_SERVER_MAX_CONNECTIONS
        );
        assert_eq!(config.server.max_message_size, DEFAULT_MAX_MESSAGE_SIZE);
        assert_eq!(
            config.database.max_connections,
            DEFAULT_POOL_MAX_CONNECTIONS
        );
        assert_eq!(config.database.min_connections, 0);
        assert_eq!(
            config.database.flush_interval_secs,
            DEFAULT_FLUSH_INTERVAL_SECS
        );
        assert_eq!(
            config.auth.session_timeout_secs,
            DEFAULT_SESSION_TIMEOUT_SECS
        );
        assert_eq!(config.database.password.expose(), "pw");
    }

    #[test]
    fn rejects_pool_min_above_max() {
        let raw = VALID_MINIMAL.replace(
            "[database]",
            "[database]\nmin_connections = 99\nmax_connections = 10",
        );
        let config = parse(&raw).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_zero_flush_interval() {
        let raw = VALID_MINIMAL.replace("[database]", "[database]\nflush_interval_secs = 0");
        let config = parse(&raw).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_missing_credentials() {
        let raw = VALID_MINIMAL.replace("password = \"secret\"", "");
        let config = parse(&raw).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_argon2_hash_instead_of_plaintext_password() {
        let raw = VALID_MINIMAL.replace(
            "password = \"secret\"",
            "password_hash = \"$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$hash\"",
        );
        let config = parse(&raw).unwrap();
        config.validate().unwrap();
        assert!(config.auth.password.is_none());
    }
}
