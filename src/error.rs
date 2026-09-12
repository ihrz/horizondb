use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Validation(String),
    #[error("invalid credentials")]
    Auth,
    #[error("invalid or expired session")]
    SessionExpired,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("{0}")]
    Internal(String),
}

impl AppError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    pub fn client_message(&self) -> String {
        match self {
            Self::Validation(message) => message.clone(),
            Self::Auth => "invalid credentials".to_owned(),
            Self::SessionExpired => "invalid or expired session".to_owned(),
            Self::Database(_) | Self::Serialization(_) | Self::Config(_) | Self::Internal(_) => {
                "internal server error".to_owned()
            }
        }
    }

    pub fn is_internal(&self) -> bool {
        matches!(
            self,
            Self::Database(_) | Self::Serialization(_) | Self::Config(_) | Self::Internal(_)
        )
    }
}
