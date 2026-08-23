use std::sync::Arc;

use anyhow::Context;
use tracing::info;

use crate::auth::{Authenticator, SessionStore};
use crate::config::AppConfig;
use crate::db::Repository;
use crate::metrics::Metrics;

pub struct AppState {
    pub config: Arc<AppConfig>,
    pub repository: Repository,
    pub sessions: SessionStore,
    pub authenticator: Authenticator,
    pub metrics: Metrics,
}

impl AppState {
    pub async fn initialize(config: AppConfig) -> anyhow::Result<Arc<Self>> {
        info!("initializing horizondb");

        let repository = Repository::connect(&config.database)
            .await
            .context("database connection failed")?;

        let authenticator =
            Authenticator::new(&config.auth).context("invalid auth configuration")?;

        let sessions = SessionStore::new(config.auth.session_timeout_secs);

        Ok(Arc::new(Self {
            config: Arc::new(config),
            repository,
            sessions,
            authenticator,
            metrics: Metrics::default(),
        }))
    }

    pub async fn shutdown(&self) {
        self.repository.shutdown().await;
    }
}
