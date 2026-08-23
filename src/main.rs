#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod auth;
mod cache;
mod config;
mod db;
mod error;
mod handlers;
mod metrics;
mod models;
mod server;
mod state;
mod values;

use std::path::Path;

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_owned());
    let app_config = config::AppConfig::load(Path::new(&config_path))
        .with_context(|| format!("loading configuration file {config_path}"))?;

    tracing::info!("starting HorizonDB");
    let state = state::AppState::initialize(app_config).await?;
    server::run(state).await
}
