//! WebSocket side of the proxy: handshake, then a bidirectional byte pump
//! between the WebSocket and the raw TCP connection to the VNC server.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::StatusCode;
use hyper::body::Incoming;
use hyper::header::{
    CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL, UPGRADE,
};
use hyper::upgrade::OnUpgrade;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use log::{debug, info, warn};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{interval, timeout};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

use crate::now_millis;
use crate::record::Recorder;
use crate::server::State;
use crate::web::{Resp, text};

/// VNC updates can be very large (a raw 4K framebuffer is ~33 MiB), so the
/// tungstenite defaults (16 MiB frame / 64 MiB message) are too small.
const MAX_WS_PAYLOAD: usize = 256 * 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn ws_config() -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_WS_PAYLOAD);
    config.max_frame_size = Some(MAX_WS_PAYLOAD);
    config
}

pub fn is_websocket(req: &Request<Incoming>) -> bool {
    req.method() == hyper::Method::GET
        && req
            .headers()
            .get(UPGRADE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        && req.headers().contains_key(SEC_WEBSOCKET_KEY)
}

/// Answer a WebSocket upgrade request.
///
/// The TCP connection to the VNC server is established *before* the 101 is
/// sent, so an unreachable target produces a normal HTTP error that noVNC
/// displays instead of a half-open WebSocket.
pub async fn upgrade(mut req: Request<Incoming>, state: Arc<State>) -> Resp {
    let Some(key) = req.headers().get(SEC_WEBSOCKET_KEY) else {
        return text(StatusCode::BAD_REQUEST, "Missing Sec-WebSocket-Key");
    };
    let key = key.as_bytes().to_vec();

    // noVNC asks for the "binary" sub-protocol when it is configured; echo it
    // when present so the browser sees the protocol it requested.
    let protocol = req
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .split(',')
                .map(str::trim)
                .find(|proto| proto.eq_ignore_ascii_case("binary"))
                .map(str::to_string)
        });

    let on_upgrade = hyper::upgrade::on(&mut req);

    let target = state.cfg.target.clone();
    let tcp = match timeout(CONNECT_TIMEOUT, TcpStream::connect(&target)).await {
        Ok(Ok(tcp)) => tcp,
        Ok(Err(err)) => {
            warn!("cannot connect to VNC server {target}: {err}");
            return text(
                StatusCode::BAD_GATEWAY,
                &format!("Cannot connect to VNC server at {target}: {err}"),
            );
        }
        Err(_) => {
            warn!("timed out connecting to VNC server {target}");
            return text(
                StatusCode::GATEWAY_TIMEOUT,
                &format!("Timed out connecting to VNC server at {target}"),
            );
        }
    };
    let _ = tcp.set_nodelay(true);

    let mut builder = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(UPGRADE, "websocket")
        .header(CONNECTION, "Upgrade")
        .header(SEC_WEBSOCKET_ACCEPT, derive_accept_key(&key));
    if let Some(protocol) = protocol {
        builder = builder.header(SEC_WEBSOCKET_PROTOCOL, protocol);
    }
    let response = builder
        .body(Full::new(Bytes::new()))
        .expect("upgrade response is always valid");

    tokio::spawn(async move {
        bridge(state, on_upgrade, tcp).await;
    });

    response
}

async fn bridge(state: Arc<State>, on_upgrade: OnUpgrade, tcp: TcpStream) {
    let upgraded = match on_upgrade.await {
        Ok(upgraded) => upgraded,
        Err(err) => {
            debug!("websocket upgrade failed: {err}");
            return;
        }
    };

    let ws =
        WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(ws_config()))
            .await;

    let _guard = ActiveGuard::new(state.clone());
    let recorder = state.open_recorder();
    let started = Instant::now();
    info!("VNC session opened (target {})", state.cfg.target);

    let (mut ws_tx, mut ws_rx) = ws.split();
    let (mut tcp_rx, mut tcp_tx) = tcp.into_split();

    let idle = state.cfg.idle;
    let heartbeat = state.cfg.heartbeat;
    let last_activity = Arc::new(AtomicU64::new(now_millis()));

    // Browser -> VNC server.
    let recorder_a = recorder.clone();
    let activity_a = last_activity.clone();
    let mut client_to_server = tokio::spawn(async move {
        while let Some(message) = ws_rx.next().await {
            match message {
                Ok(Message::Binary(data)) => {
                    activity_a.store(now_millis(), Ordering::Relaxed);
                    record(&recorder_a, true, &data);
                    if tcp_tx.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Text(data)) => {
                    activity_a.store(now_millis(), Ordering::Relaxed);
                    record(&recorder_a, true, data.as_bytes());
                    if tcp_tx.write_all(data.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) => break,
                // Ping/Pong are handled by tungstenite itself.
                Ok(_) => {}
                Err(err) => {
                    debug!("websocket receive error: {err}");
                    break;
                }
            }
        }
        let _ = tcp_tx.shutdown().await;
    });

    // VNC server -> browser, plus heartbeat and idle handling.
    let recorder_b = recorder.clone();
    let activity_b = last_activity;
    let mut tick = if heartbeat.is_some() || idle.is_some() {
        Some(interval(Duration::from_millis(500)))
    } else {
        None
    };
    let mut server_to_client = tokio::spawn(async move {
        let mut buffer = vec![0u8; 64 * 1024];
        let mut last_ping = Instant::now();
        loop {
            tokio::select! {
                read = tcp_rx.read(&mut buffer) => match read {
                    Ok(0) => {
                        debug!("VNC server closed the connection");
                        let _ = ws_tx.send(Message::Close(None)).await;
                        break;
                    }
                    Ok(n) => {
                        activity_b.store(now_millis(), Ordering::Relaxed);
                        record(&recorder_b, false, &buffer[..n]);
                        if ws_tx
                            .send(Message::Binary(buffer[..n].to_vec().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(err) => {
                        debug!("tcp read error: {err}");
                        break;
                    }
                },
                _ = tick_opt(&mut tick) => {
                    if let Some(heartbeat) = heartbeat
                        && last_ping.elapsed() >= heartbeat
                    {
                        if ws_tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                            break;
                        }
                        last_ping = Instant::now();
                    }
                    if let Some(idle) = idle {
                        let elapsed = now_millis()
                            .saturating_sub(activity_b.load(Ordering::Relaxed));
                        if elapsed >= idle.as_millis() as u64 {
                            info!("closing idle VNC session");
                            let _ = ws_tx.send(Message::Close(None)).await;
                            break;
                        }
                    }
                }
            }
        }
        let _ = ws_tx.close().await;
    });

    tokio::select! {
        _ = &mut client_to_server => server_to_client.abort(),
        _ = &mut server_to_client => client_to_server.abort(),
    }

    if let Some(recorder) = recorder
        && let Ok(mut recorder) = recorder.lock()
    {
        recorder.finish();
    }
    debug!("VNC session closed after {:?}", started.elapsed());
}

async fn tick_opt(tick: &mut Option<tokio::time::Interval>) {
    match tick {
        Some(tick) => {
            tick.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

fn record(recorder: &Option<Arc<Mutex<Recorder>>>, from_client: bool, data: &[u8]) {
    if let Some(recorder) = recorder
        && let Ok(mut recorder) = recorder.lock()
    {
        recorder.frame(from_client, data);
    }
}

/// Tracks how many sessions are open, used by `--timeout`.
struct ActiveGuard {
    state: Arc<State>,
}

impl ActiveGuard {
    fn new(state: Arc<State>) -> Self {
        state.active.fetch_add(1, Ordering::SeqCst);
        Self { state }
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if self.state.active.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.state
                .last_active
                .store(crate::now_secs(), Ordering::SeqCst);
        }
    }
}
