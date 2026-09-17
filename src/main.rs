#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod auth;
mod config;
mod db;
mod error;
mod handlers;
mod metrics;
mod models;
mod server;
mod state;
mod values;

use std::{path::Path, process};

#[tokio::main]
async fn main() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_owned());
    let app_config = match config::AppConfig::load(Path::new(&config_path)) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!("loading configuration file {config_path} failed: {error:#}");
            process::exit(1);
        }
    };

    tracing::info!("starting HorizonDB");
    let state = match state::AppState::initialize(app_config).await {
        Ok(state) => state,
        Err(error) => {
            tracing::error!("HorizonDB initialization failed: {error:#}");
            process::exit(1);
        }
    };

    let exit_code = match server::run(state).await {
        Ok(()) => 0,
        Err(error) => {
            tracing::error!("HorizonDB stopped with error: {error:#}");
            1
        }
    };

    // Exit hard instead of falling off `main`: dropping the tokio runtime can
    // block on stuck tasks, leaving a half-dead process that still holds the
    // listening port and makes the next `pm2 restart` crash-loop on
    // EADDRINUSE. The listener is already unbound and the flush finished (or
    // timed out) by the time we get here.
    process::exit(exit_code);
}
