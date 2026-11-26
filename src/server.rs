//! The HTTP/WebSocket server itself: accept loop, optional TLS sniffing, and
//! request dispatch.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use log::{debug, info, warn};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;

use crate::config::Config;
use crate::record::{self, Recorder};
use crate::web::{self, Resp};
use crate::ws;

pub struct State {
    pub cfg: Arc<Config>,
    pub active: Arc<AtomicUsize>,
    pub last_active: Arc<AtomicU64>,
    tls: Option<Arc<ServerConfig>>,
    record_seq: AtomicUsize,
}

impl State {
    pub fn open_recorder(&self) -> Option<Arc<Mutex<Recorder>>> {
        let base = self.cfg.record.as_ref()?;
        let session = self.record_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let path = record::path_for(base, session);
        match Recorder::create(&path) {
            Ok(recorder) => {
                info!("recording session to {}", path.display());
                Some(Arc::new(Mutex::new(recorder)))
            }
            Err(err) => {
                warn!("cannot create recording {}: {err}", path.display());
                None
            }
        }
    }
}

pub async fn run(cfg: Config) -> Result<()> {
    let listener = TcpListener::bind(cfg.listen)
        .await
        .with_context(|| format!("cannot listen on {}", cfg.listen))?;
    run_on(cfg, listener).await
}

/// Same as [`run`], but on an already bound listener (useful for tests and
/// for socket activation).
pub async fn run_on(cfg: Config, listener: TcpListener) -> Result<()> {
    let local = listener.local_addr()?;

    let tls = cfg.tls.as_ref().map(|tls| tls.server.clone());
    let state = Arc::new(State {
        cfg: Arc::new(cfg),
        active: Arc::new(AtomicUsize::new(0)),
        last_active: Arc::new(AtomicU64::new(crate::now_secs())),
        tls,
        record_seq: AtomicUsize::new(0),
    });

    banner(&state, local);
    let shutdown = spawn_timeout_watchdog(state.clone());

    tokio::select! {
        result = accept_loop(listener, state) => result,
        _ = wait_shutdown(shutdown) => {
            info!("no connections for a while, exiting (--timeout)");
            Ok(())
        }
        _ = tokio::signal::ctrl_c() => {
            info!("interrupted, shutting down");
            Ok(())
        }
    }
}

async fn wait_shutdown(receiver: Option<tokio::sync::oneshot::Receiver<()>>) {
    match receiver {
        Some(receiver) => {
            let _ = receiver.await;
        }
        None => std::future::pending::<()>().await,
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<State>) -> Result<()> {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = serve_connection(stream, peer, state).await {
                        debug!("connection from {peer} ended: {err}");
                    }
                });
            }
            Err(err) => {
                // EMFILE and friends: back off instead of spinning.
                warn!("accept failed: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn serve_connection(stream: TcpStream, peer: SocketAddr, state: Arc<State>) -> Result<()> {
    let _ = stream.set_nodelay(true);

    // Peek at the first byte so a single port can serve both HTTPS and plain
    // HTTP, like websockify does. 0x16 starts a TLS handshake record.
    let mut first = [0u8; 1];
    let n = match tokio::time::timeout(Duration::from_secs(30), stream.peek(&mut first)).await {
        Ok(Ok(n)) => n,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => return Ok(()), // client connected and said nothing
    };
    if n == 0 {
        return Ok(());
    }

    let io = match state.tls.clone() {
        Some(tls) if first[0] == 0x16 => {
            let acceptor = TlsAcceptor::from(tls);
            match acceptor.accept(stream).await {
                Ok(stream) => Io::Tls(Box::new(stream)),
                Err(err) => {
                    debug!("TLS handshake with {peer} failed: {err}");
                    return Ok(());
                }
            }
        }
        Some(_) if state.cfg.ssl_only => {
            let mut stream = stream;
            let body = "This server only accepts HTTPS connections.\n";
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\
                 Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            // Half-close and drain the request before dropping the socket;
            // otherwise Windows answers with a reset and the client never
            // sees the error.
            let _ = stream.shutdown().await;
            let mut sink = [0u8; 1024];
            let drain = async {
                loop {
                    match stream.read(&mut sink).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs(2), drain).await;
            return Ok(());
        }
        _ => Io::Plain(stream),
    };

    let service = service_fn(move |request: Request<Incoming>| {
        let state = state.clone();
        async move { Ok::<Resp, std::convert::Infallible>(handle(request, state).await) }
    });

    if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(TokioIo::new(io), service)
        .await
    {
        debug!("connection from {peer} failed: {err}");
    }

    Ok(())
}

async fn handle(request: Request<Incoming>, state: Arc<State>) -> Resp {
    if let Some(auth) = &state.cfg.auth {
        let header = request
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        if !auth.check_header(header) {
            return web::unauthorized();
        }
    }

    if ws::is_websocket(&request) {
        return ws::upgrade(request, state).await;
    }

    let method = request.method().clone();
    if method != hyper::Method::GET && method != hyper::Method::HEAD {
        return web::text(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
    }

    let path = request.uri().path().to_string();
    web::serve(
        &state.cfg.web,
        &path,
        state.cfg.file_only,
        method == hyper::Method::HEAD,
    )
    .await
}

fn banner(state: &State, local: SocketAddr) {
    info!("listening on {local}");
    info!("VNC target: {}", state.cfg.target);
    info!("web root: {}", state.cfg.web.describe());
    if let Some(tls) = &state.cfg.tls {
        info!("TLS certificate: {}", tls.source.display());
    }

    let scheme = if state.cfg.tls.is_some() {
        "https"
    } else {
        "http"
    };
    let host = display_host(local.ip());
    println!("\nNavigate to this URL:\n");
    println!("    {scheme}://{host}:{}/vnc.html\n", local.port());
    if state.cfg.tls.is_some() && !state.cfg.ssl_only {
        println!("Plain HTTP is accepted on the same port as well.\n");
    }
    println!("Press Ctrl-C to exit\n");
}

fn display_host(ip: IpAddr) -> String {
    if ip.is_unspecified() {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "localhost".to_string())
    } else if ip.is_ipv6() {
        format!("[{ip}]")
    } else {
        ip.to_string()
    }
}

fn spawn_timeout_watchdog(state: Arc<State>) -> Option<tokio::sync::oneshot::Receiver<()>> {
    let timeout = state.cfg.exit_after_idle?;
    let seconds = timeout.as_secs().max(1);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if state.active.load(Ordering::SeqCst) > 0 {
                continue;
            }
            let idle = crate::now_secs().saturating_sub(state.last_active.load(Ordering::SeqCst));
            if idle >= seconds {
                let _ = sender.send(());
                return;
            }
        }
    });
    Some(receiver)
}

/// Either a plain TCP stream or a TLS one, so the connection loop can stay
/// generic (no `dyn` boxing needed).
enum Io {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl AsyncRead for Io {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            Io::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Io::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Io {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Io::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Io::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Io::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Io::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Io::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Io::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Io::Plain(stream) => Pin::new(stream).poll_write_vectored(cx, bufs),
            Io::Tls(stream) => Pin::new(stream.as_mut()).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Io::Plain(stream) => stream.is_write_vectored(),
            Io::Tls(stream) => stream.is_write_vectored(),
        }
    }
}
