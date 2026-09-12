use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use tokio::{
    net::{TcpListener, TcpStream},
    signal,
    sync::{Mutex, Semaphore},
    time::interval,
};
use tokio_tungstenite::{
    WebSocketStream, accept_async_with_config,
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
/// Requests a single connection may have in flight. Beyond that the socket
/// is simply not read until responses drain, which turns into TCP
/// backpressure for the client instead of unbounded task growth here.
const MAX_IN_FLIGHT_PER_CONNECTION: usize = 1_024;
/// Deadline applied to requests that do not carry their own `timeoutMs`:
/// work the client has certainly abandoned is not worth doing.
const DEFAULT_REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// A Ping is sent when nothing was received for this long...
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// ...and the connection is dropped when it stays silent for this long, so a
/// half-open link releases its connection permit instead of holding it forever.
const IDLE_TIMEOUT: Duration = Duration::from_mins(2);

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
                            // A dropped link takes every in-flight request of
                            // that client with it: make it visible in production.
                            tracing::warn!(%peer, %error, "connection ended with error");
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
    let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT_PER_CONNECTION));

    let outcome = read_loop(&state, &mut stream, &sink, &in_flight, peer).await;

    // The read half is done (client Close frame, oversized message, broken
    // link). Spawned responses may still hold the sink for a while; without
    // an explicit close the client would keep the socket open and time out
    // every request it sends meanwhile instead of reconnecting right away.
    if let Err(error) = sink.lock().await.close().await {
        tracing::debug!(%peer, %error, "close frame not sent");
    }
    outcome
}

async fn read_loop(
    state: &Arc<AppState>,
    stream: &mut SplitStream<WebSocketStream<TcpStream>>,
    sink: &Arc<Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>>,
    in_flight: &Arc<Semaphore>,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let mut last_received = Instant::now();
    loop {
        let message = match tokio::time::timeout(PING_INTERVAL, stream.next()).await {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(_elapsed) => {
                if last_received.elapsed() >= IDLE_TIMEOUT {
                    return Err(anyhow::anyhow!(
                        "idle timeout: nothing received from {peer} for {:?}",
                        last_received.elapsed()
                    ));
                }
                if let Err(error) = sink.lock().await.send(Message::Ping(Vec::new())).await {
                    return Err(anyhow::anyhow!("ping to {peer} failed: {error}"));
                }
                continue;
            }
        };
        last_received = Instant::now();
        match message {
            Ok(Message::Text(text)) => {
                let received_at = Instant::now();
                let Ok(slot) = Arc::clone(in_flight).acquire_owned().await else {
                    break;
                };
                let state = Arc::clone(state);
                let sink = Arc::clone(sink);
                tokio::spawn(async move {
                    let _slot = slot;
                    let response = match serde_json::from_str::<PacketMessage>(&text) {
                        Ok(packet) => handle_packet(&state, packet, received_at).await,
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
            Err(error) => {
                // Typical causes: a frame/message above server.max_message_size
                // (tungstenite "Capacity" error) or a broken TCP link.
                return Err(anyhow::anyhow!("websocket error from {peer}: {error}"));
            }
        }
    }
    Ok(())
}

/// Skips requests that already waited longer than their client-side timeout
/// (queued behind the in-flight cap): the answer would be discarded anyway,
/// and executing it would only delay requests the client still waits for.
async fn handle_packet(
    state: &AppState,
    packet: PacketMessage,
    received_at: Instant,
) -> ResponseMessage {
    let deadline = packet
        .timeout_ms
        .filter(|ms| *ms > 0)
        .map_or(DEFAULT_REQUEST_DEADLINE, Duration::from_millis);
    let waited = received_at.elapsed();
    if waited >= deadline {
        state.metrics.request_started();
        state.metrics.request_failed();
        tracing::warn!(id = %packet.id, operation = %packet.operation, ?waited, "request dropped: deadline exceeded before dispatch");
        return ResponseMessage::failure(packet.id, "deadline exceeded");
    }
    dispatch(state, packet).await
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
