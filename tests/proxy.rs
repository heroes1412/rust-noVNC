//! End-to-end tests: a real proxy process-in-process, a fake VNC server, and
//! real WebSocket/HTTP clients.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use novnc::config::Config;
use novnc::tls::TlsConfig;
use novnc::web::WebSource;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "novnc-test-{}-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::SeqCst),
            name
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn config(target: SocketAddr) -> Config {
    Config {
        listen: ([127, 0, 0, 1], 0).into(),
        target: target.to_string(),
        tls: None,
        ssl_only: false,
        web: WebSource::Embedded,
        file_only: false,
        auth: None,
        record: None,
        heartbeat: None,
        idle: None,
        exit_after_idle: None,
    }
}

async fn start(cfg: Config) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = novnc::run_on(cfg, listener).await;
    });
    addr
}

/// Minimal VNC server: sends the protocol banner, then echoes.
async fn fake_vnc() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                if socket.write_all(b"RFB 003.008\n").await.is_err() {
                    return;
                }
                let mut buffer = [0u8; 4096];
                loop {
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if socket.write_all(&buffer[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    addr
}

async fn request(addr: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).await.unwrap();
    String::from_utf8_lossy(&buffer).into_owned()
}

async fn get(addr: SocketAddr, path: &str) -> String {
    request(
        addr,
        &format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"),
    )
    .await
}

#[tokio::test]
async fn serves_embedded_client() {
    let addr = start(config("127.0.0.1:5900".parse().unwrap())).await;

    let response = get(addr, "/vnc.html").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("text/html"));
    assert!(response.contains("noVNC"));

    let response = get(addr, "/core/rfb.js").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("text/javascript"));

    let response = get(addr, "/vendor/pako/lib/zlib/inflate.js").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    let response = get(addr, "/app/styles/base.css").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    // Files the client fetches at start-up; a 404 here shows up as a console
    // error in the browser.
    for path in ["/defaults.json", "/mandatory.json", "/package.json"] {
        let response = get(addr, path).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{path}: {response}");
    }
    let response = get(addr, "/package.json").await;
    assert!(response.contains("\"version\""), "{response}");

    // Root redirects to the client.
    let response = get(addr, "/").await;
    assert!(response.starts_with("HTTP/1.1 302"), "{response}");
    assert!(response.contains("location: /vnc.html"));
}

#[tokio::test]
async fn rejects_traversal_and_missing_files() {
    let addr = start(config("127.0.0.1:5900".parse().unwrap())).await;

    for path in [
        "/../Cargo.toml",
        "/core/../../Cargo.toml",
        "/%2e%2e/Cargo.toml",
        "/nope.js",
    ] {
        let response = get(addr, path).await;
        assert!(response.starts_with("HTTP/1.1 404"), "{path}: {response}");
        assert!(!response.contains("novnc"));
    }
}

#[tokio::test]
async fn head_has_headers_without_body() {
    let addr = start(config("127.0.0.1:5900".parse().unwrap())).await;
    let response = request(
        addr,
        "HEAD /vnc.html HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("content-length"));
    assert!(!response.contains("<html"));
}

#[tokio::test]
async fn directory_listing_can_be_disabled() {
    let mut cfg = config("127.0.0.1:5900".parse().unwrap());
    cfg.file_only = true;
    let addr = start(cfg).await;

    let response = get(addr, "/core/").await;
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
}

#[tokio::test]
async fn proxies_websocket_to_tcp() {
    let vnc = fake_vnc().await;
    let addr = start(config(vnc)).await;

    let (mut ws, _) = connect_async(format!("ws://{addr}/websockify"))
        .await
        .unwrap();

    let banner = ws.next().await.unwrap().unwrap();
    assert_eq!(
        banner,
        Message::Binary(b"RFB 003.008\n".to_vec().into()),
        "server banner must arrive untouched"
    );

    let payload = vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0x01];
    ws.send(Message::Binary(payload.clone().into()))
        .await
        .unwrap();
    let echo = ws.next().await.unwrap().unwrap();
    assert_eq!(echo, Message::Binary(payload.into()));

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn echoes_requested_binary_subprotocol() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::Request;

    let vnc = fake_vnc().await;
    let addr = start(config(vnc)).await;

    let mut request: Request<()> = format!("ws://{addr}/websockify")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "binary".parse().unwrap());

    let (mut ws, response) = connect_async(request).await.unwrap();
    assert_eq!(
        response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|value| value.to_str().ok()),
        Some("binary")
    );
    ws.next().await.unwrap().unwrap();
    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn unreachable_vnc_server_yields_502() {
    // Grab a port and release it, so nothing is listening there.
    let unused = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    };

    let addr = start(config(unused)).await;
    match connect_async(format!("ws://{addr}/websockify")).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 502);
        }
        other => panic!("expected HTTP 502, got {other:?}"),
    }
}

#[tokio::test]
async fn basic_auth_protects_everything() {
    let vnc = fake_vnc().await;
    let mut cfg = config(vnc);
    cfg.auth = Some(novnc::config::Credentials::parse("alice:s3cret").unwrap());
    let addr = start(cfg).await;

    let response = get(addr, "/vnc.html").await;
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(response.contains("www-authenticate: Basic"));

    let expected = base64_encode("alice:s3cret");
    let response = request(
        addr,
        &format!(
            "GET /vnc.html HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\
             Authorization: Basic {expected}\r\n\r\n"
        ),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    let wrong = base64_encode("alice:wrong");
    let response = request(
        addr,
        &format!(
            "GET /vnc.html HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\
             Authorization: Basic {wrong}\r\n\r\n"
        ),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
}

#[tokio::test]
async fn records_a_playable_session() {
    let dir = TempDir::new("record");
    let vnc = fake_vnc().await;
    let mut cfg = config(vnc);
    cfg.record = Some(dir.0.join("session"));
    let addr = start(cfg).await;

    let (mut ws, _) = connect_async(format!("ws://{addr}/websockify"))
        .await
        .unwrap();
    ws.next().await.unwrap().unwrap();
    ws.send(Message::Binary(vec![1, 2, 3, 4].into()))
        .await
        .unwrap();
    ws.next().await.unwrap().unwrap();
    ws.close(None).await.unwrap();

    let path = dir.0.join("session.session.js");
    let mut text = String::new();
    for _ in 0..100 {
        if let Ok(contents) = std::fs::read_to_string(&path)
            && contents.contains("'EOF'")
        {
            text = contents;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(
        text.starts_with("var VNC_frame_encoding = 'base64';"),
        "{text}"
    );
    assert!(text.contains("var VNC_frame_data = ["), "{text}");
    assert!(text.contains("'EOF'"), "{text}");
    assert!(
        text.contains("'{0{"),
        "server frames must be recorded: {text}"
    );
}

#[tokio::test]
async fn serves_tls_and_refuses_plain_http_when_ssl_only() {
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::pki_types::ServerName;
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};

    let dir = TempDir::new("tls");
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_path = dir.0.join("cert.pem");
    let key_path = dir.0.join("key.pem");
    std::fs::write(&cert_path, generated.cert.pem()).unwrap();
    std::fs::write(&key_path, generated.signing_key.serialize_pem()).unwrap();

    let vnc = fake_vnc().await;
    let mut cfg = config(vnc);
    cfg.tls = Some(TlsConfig::load(&cert_path, Some(&key_path)).unwrap());
    cfg.ssl_only = true;
    let addr = start(cfg).await;

    let mut roots = RootCertStore::empty();
    roots.add(generated.cert.der().clone()).unwrap();
    let client_config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(client_config));

    let tcp = TcpStream::connect(addr).await.unwrap();
    let name = ServerName::try_from("localhost").unwrap();
    let mut tls = connector.connect(name, tcp).await.unwrap();
    tls.write_all(b"GET /vnc.html HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    tls.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    // A plain HTTP client on the same port is refused.
    let response = get(addr, "/vnc.html").await;
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
}

#[tokio::test]
async fn serves_client_from_directory() {
    let dir = TempDir::new("web");
    std::fs::write(dir.0.join("vnc.html"), b"<html>custom client</html>").unwrap();
    std::fs::create_dir_all(dir.0.join("core")).unwrap();
    std::fs::write(dir.0.join("core/rfb.js"), b"// custom").unwrap();

    let mut cfg = config("127.0.0.1:5900".parse().unwrap());
    cfg.web = WebSource::Dir(dir.0.clone());
    let addr = start(cfg).await;

    let response = get(addr, "/vnc.html").await;
    assert!(response.contains("custom client"), "{response}");

    let response = get(addr, "/core/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("rfb.js"), "{response}");
}

fn base64_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}

#[tokio::test]
async fn exits_after_timeout_without_connections() {
    let mut cfg = config("127.0.0.1:5900".parse().unwrap());
    cfg.exit_after_idle = Some(Duration::from_secs(1));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let task = tokio::spawn(novnc::run_on(cfg, listener));

    let joined = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("server did not exit after --timeout")
        .unwrap();
    assert!(joined.is_ok());
}

#[tokio::test]
async fn sends_websocket_heartbeats() {
    let vnc = fake_vnc().await;
    let mut cfg = config(vnc);
    cfg.heartbeat = Some(Duration::from_secs(1));
    let addr = start(cfg).await;

    let (mut ws, _) = connect_async(format!("ws://{addr}/websockify"))
        .await
        .unwrap();
    ws.next().await.unwrap().unwrap();

    let message = tokio::time::timeout(Duration::from_secs(6), ws.next())
        .await
        .expect("no heartbeat ping arrived")
        .unwrap()
        .unwrap();
    assert!(matches!(message, Message::Ping(_)), "got {message:?}");
    ws.close(None).await.ok();
}

#[tokio::test]
async fn closes_idle_connections() {
    let vnc = fake_vnc().await;
    let mut cfg = config(vnc);
    cfg.idle = Some(Duration::from_secs(1));
    let addr = start(cfg).await;

    let (mut ws, _) = connect_async(format!("ws://{addr}/websockify"))
        .await
        .unwrap();
    ws.next().await.unwrap().unwrap();

    // Stay silent; the proxy must hang up on its own.
    let closed = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) => break,
                Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "idle connection was not closed");
}
