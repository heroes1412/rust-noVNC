//! Command line interface and resolved configuration.
//!
//! The flags intentionally mirror `utils/novnc_proxy` / `websockify` so an
//! existing wrapper script keeps working after swapping the binary.

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use clap::Parser;

use crate::tls::TlsConfig;
use crate::web::WebSource;

/// Sentinel value used to detect `--auth` without an argument.
const WEB_AUTH_FLAG: &str = "__flag__";

#[derive(Parser, Debug, Clone)]
#[command(
    name = "novnc",
    version,
    about = "noVNC web server + WebSocket-to-TCP proxy",
    long_about = "Serves the noVNC web client and proxies WebSocket connections to a \
                  VNC server. Drop-in replacement for utils/novnc_proxy + websockify, \
                  without Python.",
    after_help = "Examples:\n  \
        novnc --vnc localhost:5900\n  \
        novnc --listen 127.0.0.1:6080 --vnc 192.168.1.10:5900\n  \
        novnc --auth admin:secret --cert self.pem --ssl-only --vnc localhost:5900\n  \
        novnc --auth admin:secret --web ./original-novnc --vnc localhost:5900\n"
)]
pub struct Args {
    /// Address to listen on, as [HOST:]PORT (default: all interfaces, 6080)
    #[arg(long, value_name = "[HOST:]PORT", default_value = "6080")]
    pub listen: String,

    /// VNC server to proxy to, as HOST:PORT
    #[arg(long, value_name = "HOST:PORT", default_value = "localhost:5900")]
    pub vnc: String,

    /// PEM certificate file (may contain the private key as well)
    #[arg(long, value_name = "FILE")]
    pub cert: Option<PathBuf>,

    /// PEM private key file, when not combined with --cert
    #[arg(long, value_name = "FILE")]
    pub key: Option<PathBuf>,

    /// Directory with the web client files (default: embedded noVNC)
    #[arg(long, value_name = "DIR")]
    pub web: Option<PathBuf>,

    /// Refuse plain HTTP connections when TLS is enabled
    #[arg(long)]
    pub ssl_only: bool,

    /// Disable directory listings
    #[arg(long)]
    pub file_only: bool,

    /// Record the session to FILE.session.js (playable with noVNC tests)
    #[arg(long, value_name = "FILE")]
    pub record: Option<PathBuf>,

    /// Send a WebSocket ping to the client every SEC seconds
    #[arg(long, value_name = "SEC")]
    pub heartbeat: Option<u64>,

    /// Exit when there has been no connection for SEC seconds
    #[arg(long, value_name = "SEC")]
    pub timeout: Option<u64>,

    /// Close a connection that has been idle for SEC seconds
    #[arg(long, value_name = "SEC")]
    pub idle_timeout: Option<u64>,

    /// Require HTTP Basic authentication (USER:PASS)
    #[arg(
        long,
        visible_alias = "web-auth",
        value_name = "USER:PASS",
        num_args = 0..=1,
        default_missing_value = WEB_AUTH_FLAG
    )]
    pub auth: Option<String>,

    /// HTTP Basic credentials, websockify compatible (USER:PASS)
    #[arg(long, value_name = "USER:PASS")]
    pub auth_source: Option<String>,

    /// Accepted for websockify compatibility, not implemented
    #[arg(long, value_name = "SERVER")]
    pub syslog: Option<String>,
}

/// Everything the server needs, validated and resolved.
#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub target: String,
    pub tls: Option<TlsConfig>,
    pub ssl_only: bool,
    pub web: WebSource,
    pub file_only: bool,
    pub auth: Option<Credentials>,
    pub record: Option<PathBuf>,
    pub heartbeat: Option<Duration>,
    pub idle: Option<Duration>,
    pub exit_after_idle: Option<Duration>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    user: String,
    password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl Credentials {
    /// Parse a `user:password` pair. An empty user is rejected, an empty
    /// password is allowed on purpose (some deployments use user-only auth).
    pub fn parse(raw: &str) -> Result<Self> {
        let (user, password) = raw
            .split_once(':')
            .context("basic auth credentials must be in USER:PASS form")?;
        if user.is_empty() {
            bail!("basic auth user name must not be empty");
        }
        Ok(Self {
            user: user.to_string(),
            password: password.to_string(),
        })
    }

    /// Validate an `Authorization` header value.
    pub fn check_header(&self, header: Option<&str>) -> bool {
        let Some(header) = header else { return false };
        let encoded = header
            .strip_prefix("Basic ")
            .or_else(|| header.strip_prefix("basic "));
        let Some(encoded) = encoded else {
            return false;
        };
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
            return false;
        };
        let Ok(decoded) = String::from_utf8(decoded) else {
            return false;
        };
        let Some((user, password)) = decoded.split_once(':') else {
            return false;
        };
        constant_time_eq(user.as_bytes(), self.user.as_bytes())
            && constant_time_eq(password.as_bytes(), self.password.as_bytes())
    }
}

impl Config {
    pub fn from_args(args: Args) -> Result<Self> {
        if args.syslog.is_some() {
            log::warn!("--syslog is accepted for compatibility but not implemented");
        }

        let listen = parse_listen(&args.listen)
            .with_context(|| format!("invalid --listen value {:?}", args.listen))?;
        let target = normalize_target(&args.vnc);

        let tls = match (&args.cert, &args.key) {
            (None, Some(_)) => bail!("--key requires --cert"),
            (None, None) => {
                if args.ssl_only {
                    bail!("--ssl-only requires --cert");
                }
                None
            }
            (Some(cert), key) => Some(TlsConfig::load(cert, key.as_deref())?),
        };

        let auth = resolve_auth(args.auth.as_deref(), args.auth_source.as_deref())?;

        Ok(Self {
            listen,
            target,
            tls,
            ssl_only: args.ssl_only,
            web: WebSource::resolve(args.web)?,
            file_only: args.file_only,
            auth,
            record: args.record,
            heartbeat: args.heartbeat.map(Duration::from_secs),
            idle: args.idle_timeout.map(Duration::from_secs),
            exit_after_idle: args.timeout.map(Duration::from_secs),
        })
    }
}

/// Resolve `--auth` / `--auth-source` into credentials.
///
/// `--auth user:pass` is the primary spelling. Bare `--auth` combined with
/// `--auth-source user:pass` mirrors websockify and stays supported.
fn resolve_auth(auth: Option<&str>, source: Option<&str>) -> Result<Option<Credentials>> {
    match (auth, source) {
        (None, None) => Ok(None),
        (Some(value), None) if value != WEB_AUTH_FLAG => Ok(Some(
            Credentials::parse(value).context("invalid --auth value")?,
        )),
        (Some(value), Some(source)) if value == WEB_AUTH_FLAG => Ok(Some(
            Credentials::parse(source).context("invalid --auth-source value")?,
        )),
        (Some(_), Some(_)) => {
            bail!("use either --auth USER:PASS or --auth-source USER:PASS, not both")
        }
        (Some(_), None) => bail!("--auth needs USER:PASS (or --auth-source USER:PASS)"),
        (None, Some(source)) => Ok(Some(
            Credentials::parse(source).context("invalid --auth-source value")?,
        )),
    }
}

/// Parse `[HOST:]PORT`, supporting IPv6 in brackets, a bare port, and
/// hostnames. A missing host means "all interfaces".
fn parse_listen(raw: &str) -> Result<SocketAddr> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("empty listen address");
    }

    if let Ok(addr) = raw.parse::<SocketAddr>() {
        return Ok(addr);
    }

    if let Some(rest) = raw.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .context("missing closing ']' in listen address")?;
        let port = tail.strip_prefix(':').unwrap_or(tail);
        let port: u16 = port.parse().context("invalid port")?;
        let host = if host.is_empty() { "::" } else { host };
        return resolve(&format!("{host}:{port}"), port);
    }

    if let Ok(port) = raw.parse::<u16>() {
        return Ok(SocketAddr::from(([0, 0, 0, 0], port)));
    }

    if let Some((host, port)) = raw.rsplit_once(':') {
        let port: u16 = port.parse().context("invalid port")?;
        let host = if host.is_empty() { "0.0.0.0" } else { host };
        return resolve(&format!("{host}:{port}"), port);
    }

    bail!("expected [HOST:]PORT")
}

fn resolve(host_port: &str, port: u16) -> Result<SocketAddr> {
    host_port
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve {host_port}"))?
        .next()
        .with_context(|| format!("{host_port} did not resolve to an address"))
        .map(|mut addr| {
            addr.set_port(port);
            addr
        })
}

/// Accept `host`, `host:port`, `:port` and bare `port` for the VNC target.
fn normalize_target(raw: &str) -> String {
    let raw = raw.trim();
    if let Ok(port) = raw.parse::<u16>() {
        return format!("localhost:{port}");
    }
    if raw.starts_with('[') || raw.contains(':') {
        return raw.to_string();
    }
    format!("{raw}:5900")
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listen_forms() {
        assert_eq!(parse_listen("6080").unwrap().port(), 6080);
        assert_eq!(parse_listen("6080").unwrap().ip().to_string(), "0.0.0.0");
        assert_eq!(
            parse_listen("127.0.0.1:8080").unwrap(),
            "127.0.0.1:8080".parse().unwrap()
        );
        assert_eq!(
            parse_listen("[::1]:8080").unwrap(),
            "[::1]:8080".parse().unwrap()
        );
        assert!(parse_listen("").is_err());
        assert!(parse_listen("nope:nope").is_err());
    }

    #[test]
    fn normalizes_target() {
        assert_eq!(normalize_target("5900"), "localhost:5900");
        assert_eq!(normalize_target("vnc.example.com"), "vnc.example.com:5900");
        assert_eq!(
            normalize_target("vnc.example.com:5901"),
            "vnc.example.com:5901"
        );
    }

    #[test]
    fn basic_auth_roundtrip() {
        let creds = Credentials::parse("alice:s3cret").unwrap();
        let header = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("alice:s3cret")
        );
        assert!(creds.check_header(Some(&header)));
        assert!(!creds.check_header(Some("Basic bm9wZTpub3Bl")));
        assert!(!creds.check_header(None));
        assert!(Credentials::parse(":pass").is_err());
        assert!(Credentials::parse("nocolon").is_err());
    }

    #[test]
    fn auth_flag_is_primary_spelling() {
        let args = Args::try_parse_from(["novnc", "--auth", "alice:s3cret"]).unwrap();
        assert_eq!(args.auth.as_deref(), Some("alice:s3cret"));

        let args = Args::try_parse_from(["novnc", "--web-auth", "alice:s3cret"]).unwrap();
        assert_eq!(args.auth.as_deref(), Some("alice:s3cret"));

        let args = Args::try_parse_from(["novnc", "--auth"]).unwrap();
        assert_eq!(args.auth.as_deref(), Some(WEB_AUTH_FLAG));
    }

    #[test]
    fn auth_resolution_rules() {
        let creds = resolve_auth(Some("alice:s3cret"), None).unwrap().unwrap();
        assert!(creds.check_header(Some(&basic_header("alice:s3cret"))));

        let creds = resolve_auth(Some(WEB_AUTH_FLAG), Some("bob:hunter2"))
            .unwrap()
            .unwrap();
        assert!(creds.check_header(Some(&basic_header("bob:hunter2"))));

        let creds = resolve_auth(None, Some("bob:hunter2")).unwrap().unwrap();
        assert!(creds.check_header(Some(&basic_header("bob:hunter2"))));

        assert!(resolve_auth(None, None).unwrap().is_none());
        assert!(resolve_auth(Some(WEB_AUTH_FLAG), None).is_err());
        assert!(resolve_auth(Some("alice:s3cret"), Some("bob:hunter2")).is_err());
        assert!(resolve_auth(Some("nocolon"), None).is_err());
    }

    fn basic_header(credentials: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(credentials)
        )
    }
}
