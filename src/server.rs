use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio::{
    net::{TcpListener, TcpStream},
    signal,
    sync::{Mutex, Semaphore},
    time::interval,
};
use tokio_tungstenite::{
    accept_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

use crate::{
    handlers::dispatch,
    models::{PacketMessage, ResponseMessage},
    state::AppState,
};

const SESSION_PURGE_INTERVAL: Duration = Duration::from_mins(5);
const MEMORY_PURGE_INTERVAL: Duration = Duration::from_mins(1);
const STATS_INTERVAL: Duration = Duration::from_mins(1);

pub async fn run(state: Arc<AppState>) -> anyhow::Result<()> {
    let addr = format!("{}:{}", state.config.server.host, state.config.server.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    tracing::info!(%addr, "HorizonDB listening");

    let permits = Arc::new(Semaphore::new(state.config.server.max_connections));

    let session_purger = tokio::spawn(session_purge_loop(Arc::clone(&state)));
    let memory_purger = tokio::spawn(memory_purge_loop(Arc::clone(&state)));
    let stats_logger = tokio::spawn(stats_loop(Arc::clone(&state)));

    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    state.metrics.connection_opened();
                    let Ok(permit) = permits.clone().try_acquire_owned() else {
                        state.metrics.connection_closed();
                        tracing::warn!(%peer, "connection limit reached, rejecting");
                        continue;
                    };
                    let task_state = Arc::clone(&state);
                    tokio::spawn(async move {
                        let _permit = permit;
                        let connection_state = Arc::clone(&task_state);
                        if let Err(error) = serve_connection(connection_state, stream, peer).await {
                            tracing::debug!(%peer, %error, "connection ended");
                        }
                        task_state.metrics.connection_closed();
                    });
                }
                Err(error) => tracing::error!(%error, "failed to accept connection"),
            },
            () = shutdown_signal() => {
                tracing::info!("shutdown signal received");
                break;
            }
        }
    }

    session_purger.abort();
    memory_purger.abort();
    stats_logger.abort();

    let active_sessions = state.sessions.active_sessions().await;
    let (memory_hits, memory_misses) = state.repository.memory_stats();
    state
        .metrics
        .log_stats(active_sessions, memory_hits, memory_misses);
    state.shutdown().await;
    tracing::info!("HorizonDB stopped");
    Ok(())
}

async fn serve_connection(
    state: Arc<AppState>,
    stream: TcpStream,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let websocket_config = WebSocketConfig {
        max_message_size: Some(state.config.server.max_message_size),
        max_frame_size: Some(state.config.server.max_message_size),
        ..WebSocketConfig::default()
    };
    let websocket = accept_async_with_config(stream, Some(websocket_config))
        .await
        .with_context(|| format!("websocket handshake with {peer} failed"))?;

    // Requests already carry a unique `id` (the SDK correlates by it), so we
    // can handle them concurrently instead of one-at-a-time. Split the socket
    // into a shared sink + an independent stream, and spawn one task per
    // request: a slow `all` must never block unrelated `get`s on this link.
    let (sink, mut stream) = websocket.split();
    let sink = Arc::new(Mutex::new(sink));

    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Text(text)) => {
                let state = Arc::clone(&state);
                let sink = Arc::clone(&sink);
                tokio::spawn(async move {
                    let response = match serde_json::from_str::<PacketMessage>(&text) {
                        Ok(packet) => dispatch(&state, packet).await,
                        Err(error) => ResponseMessage::failure(
                            "unknown".to_owned(),
                            format!("Invalid JSON: {error}"),
                        ),
                    };
                    let payload = response.encode();
                    if let Err(error) = sink.lock().await.send(Message::Text(payload)).await {
                        tracing::debug!(%peer, %error, "failed to send response");
                    }
                });
            }
            Ok(Message::Close(frame)) => {
                tracing::debug!(%peer, ?frame, "client closed connection");
                break;
            }
            Ok(_) => {}
            Err(error) => return Err(anyhow::anyhow!("websocket error from {peer}: {error}")),
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal as unix_signal};

        match unix_signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                tracing::error!(%error, "unable to listen for SIGTERM");
                signal::ctrl_c().await.ok();
            }
        }
    }

    #[cfg(not(unix))]
    {
        signal::ctrl_c().await.ok();
    }
}

async fn session_purge_loop(state: Arc<AppState>) {
    let mut ticker = interval(SESSION_PURGE_INTERVAL);
    loop {
        ticker.tick().await;
        let purged = state.sessions.purge_expired().await;
        if purged > 0 {
            tracing::debug!(purged, "expired sessions removed");
        }
    }
}

async fn memory_purge_loop(state: Arc<AppState>) {
    let mut ticker = interval(MEMORY_PURGE_INTERVAL);
    loop {
        ticker.tick().await;
        let purged = state.repository.purge_expired().await;
        if purged > 0 {
            tracing::debug!(purged, "expired memory entries removed");
        }
    }
}

async fn stats_loop(state: Arc<AppState>) {
    let mut ticker = interval(STATS_INTERVAL);
    loop {
        ticker.tick().await;
        let active_sessions = state.sessions.active_sessions().await;
        let (memory_hits, memory_misses) = state.repository.memory_stats();
        state
            .metrics
            .log_stats(active_sessions, memory_hits, memory_misses);
    }
}
