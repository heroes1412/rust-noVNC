//! Static file serving for the noVNC web client.
//!
//! The client can either be served from a directory (`--web DIR`) or from a
//! build-time snapshot embedded in the executable, which is what makes the
//! single-binary deployment possible.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::Full;
use hyper::StatusCode;
use hyper::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use include_dir::{Dir, include_dir};
use log::{info, warn};

/// Response type used by the whole server (small bodies only).
pub type Resp = hyper::Response<Full<Bytes>>;

/// Build-time snapshot of the noVNC client, produced by `build.rs`.
static EMBED: Dir<'static> = include_dir!("$OUT_DIR/web");

/// Version of the embedded noVNC client, read from its `package.json`.
pub const CLIENT_VERSION: &str = env!("NOVNC_CLIENT_VERSION");

#[derive(Debug, Clone)]
pub enum WebSource {
    Dir(PathBuf),
    Embedded,
}

pub struct Entry {
    pub name: String,
    pub dir: bool,
}

pub enum Node {
    File(Vec<u8>),
    Dir(Vec<Entry>),
    NotFound,
}

impl WebSource {
    /// Resolution order: `--web`, `$NOVNC_WEB`, `./vnc.html`, next to the
    /// executable, then the embedded copy.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        if let Some(dir) = explicit {
            return Self::from_dir(dir).context("invalid --web directory");
        }

        if let Some(dir) = std::env::var_os("NOVNC_WEB") {
            return Self::from_dir(PathBuf::from(dir)).context("invalid NOVNC_WEB directory");
        }

        let mut candidates = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd);
        }
        if let Ok(exe) = std::env::current_exe()
            && let Some(parent) = exe.parent()
        {
            candidates.push(parent.to_path_buf());
        }
        for candidate in candidates {
            if candidate.join("vnc.html").is_file() {
                return Ok(Self::Dir(candidate));
            }
        }

        info!("using the noVNC web client embedded in this executable");
        Ok(Self::Embedded)
    }

    fn from_dir(dir: PathBuf) -> Result<Self> {
        if !dir.is_dir() {
            bail!("{} is not a directory", dir.display());
        }
        if !dir.join("vnc.html").is_file() {
            warn!(
                "{} does not contain vnc.html; the client entry point will not work",
                dir.display()
            );
        }
        Ok(Self::Dir(dir))
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Dir(dir) => format!("{}", dir.display()),
            Self::Embedded => format!("embedded (noVNC {CLIENT_VERSION})"),
        }
    }

    async fn open(&self, rel: &str) -> Node {
        match self {
            Self::Dir(root) => {
                let path = root.join(rel);
                match tokio::fs::metadata(&path).await {
                    Err(_) => Node::NotFound,
                    Ok(meta) if meta.is_dir() => {
                        let Ok(mut read_dir) = tokio::fs::read_dir(&path).await else {
                            return Node::NotFound;
                        };
                        let mut entries = Vec::new();
                        while let Ok(Some(entry)) = read_dir.next_entry().await {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            let dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                            entries.push(Entry { name, dir });
                        }
                        entries.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.cmp(&b.name)));
                        Node::Dir(entries)
                    }
                    Ok(_) => match tokio::fs::read(&path).await {
                        Ok(data) => Node::File(data),
                        Err(_) => Node::NotFound,
                    },
                }
            }
            Self::Embedded => embedded_open(rel),
        }
    }
}

fn embedded_open(rel: &str) -> Node {
    let rel = rel.trim_matches('/');

    if rel.is_empty() {
        return Node::Dir(entries_of(&EMBED));
    }

    if let Some(file) = EMBED.get_file(rel) {
        return Node::File(file.contents().to_vec());
    }
    if let Some(dir) = EMBED.get_dir(rel) {
        return Node::Dir(entries_of(dir));
    }

    Node::NotFound
}

fn entries_of(dir: &Dir<'static>) -> Vec<Entry> {
    let mut entries: Vec<Entry> = dir
        .entries()
        .iter()
        .map(|entry| Entry {
            name: entry
                .path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dir: entry.as_dir().is_some(),
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.cmp(&b.name)));
    entries
}

/// Serve `raw_path` from `web`. `head` suppresses the body but keeps the
/// headers, as required for HEAD requests.
pub async fn serve(web: &WebSource, raw_path: &str, file_only: bool, head: bool) -> Resp {
    if raw_path == "/" || raw_path.is_empty() {
        match web.open("index.html").await {
            Node::File(data) => return file_response("index.html", data, head),
            _ => return redirect("/vnc.html"),
        }
    }

    let Some(decoded) = percent_decode(raw_path) else {
        return not_found();
    };
    let Some(segments) = sanitize(&decoded) else {
        return not_found();
    };
    if segments.is_empty() {
        return redirect("/vnc.html");
    }

    let rel = segments.join("/");
    match web.open(&rel).await {
        Node::File(data) => file_response(&rel, data, head),
        Node::Dir(entries) => {
            if !decoded.ends_with('/') {
                return redirect(&format!("/{rel}/"));
            }
            match web.open(&format!("{rel}/index.html")).await {
                Node::File(data) => file_response(&format!("{rel}/index.html"), data, head),
                _ if file_only => text(StatusCode::FORBIDDEN, "Directory listing is disabled"),
                _ => listing(&rel, entries, head),
            }
        }
        Node::NotFound => not_found(),
    }
}

fn file_response(path: &str, data: Vec<u8>, head: bool) -> Resp {
    let len = data.len();
    let body = if head { Vec::new() } else { data };
    hyper::Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, mime_for(path))
        .header(CONTENT_LENGTH, len)
        .header(CACHE_CONTROL, "no-cache")
        .header("X-Content-Type-Options", "nosniff")
        .body(Full::new(Bytes::from(body)))
        .expect("static response is always valid")
}

fn listing(rel: &str, entries: Vec<Entry>, head: bool) -> Resp {
    let mut html = String::from(
        "<!doctype html><meta charset=\"utf-8\"><title>Index</title>\
         <style>body{font-family:system-ui,sans-serif;margin:2rem}\
         a{display:block;padding:.15rem 0}</style><h1>Index</h1>",
    );
    if !rel.is_empty() {
        html.push_str("<a href=\"../\">../</a>");
    }
    for entry in entries {
        let suffix = if entry.dir { "/" } else { "" };
        let name = escape_html(&entry.name);
        html.push_str(&format!(
            "<a href=\"{name}{suffix}\">{name}{suffix}</a>",
            name = name,
            suffix = suffix
        ));
    }
    let body = if head { String::new() } else { html };
    hyper::Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .header(CACHE_CONTROL, "no-cache")
        .body(Full::new(Bytes::from(body)))
        .expect("listing response is always valid")
}

fn redirect(location: &str) -> Resp {
    hyper::Response::builder()
        .status(StatusCode::FOUND)
        .header(LOCATION, location)
        .header(CONTENT_LENGTH, 0)
        .body(Full::new(Bytes::new()))
        .expect("redirect response is always valid")
}

pub fn not_found() -> Resp {
    text(StatusCode::NOT_FOUND, "Not Found")
}

pub fn unauthorized() -> Resp {
    hyper::Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Basic realm=\"noVNC\"")
        .header(CONTENT_LENGTH, 0)
        .body(Full::new(Bytes::new()))
        .expect("unauthorized response is always valid")
}

pub fn text(status: StatusCode, message: &str) -> Resp {
    let body = format!("{message}\n");
    hyper::Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(CONTENT_LENGTH, body.len())
        .body(Full::new(Bytes::from(body)))
        .expect("text response is always valid")
}

fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hi = hex_value(*bytes.get(i + 1)?)?;
                let lo = hex_value(*bytes.get(i + 2)?)?;
                out.push(hi << 4 | lo);
                i += 3;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Split a decoded path into safe segments. `..`, `.`, backslashes, NULs and
/// drive/stream separators are rejected rather than normalised, so nothing can
/// escape the web root.
fn sanitize(path: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    for segment in path.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." {
            return None;
        }
        if segment.contains('\\') || segment.contains(':') || segment.contains('\0') {
            return None;
        }
        if segment.chars().any(|c| c.is_control()) {
            return None;
        }
        segments.push(segment.to_string());
    }
    Some(segments)
}

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn mime_for(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "mp3" => "audio/mpeg",
        "oga" | "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain; charset=utf-8",
        "pem" => "application/x-pem-file",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        assert!(sanitize("/../secret").is_none());
        assert!(sanitize("/a/../../b").is_none());
        assert!(sanitize("/a\\..\\b").is_none());
        assert!(sanitize("/C:/Windows").is_none());
        assert_eq!(sanitize("/core/rfb.js").unwrap(), vec!["core", "rfb.js"]);
        assert_eq!(sanitize("//core//rfb.js").unwrap(), vec!["core", "rfb.js"]);
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("/core%2Frfb.js").unwrap(), "/core/rfb.js");
        assert_eq!(percent_decode("/a%20b").unwrap(), "/a b");
        assert!(percent_decode("/%zz").is_none());
        assert!(percent_decode("/%2").is_none());
    }

    #[test]
    fn embedded_client_is_present() {
        match embedded_open("vnc.html") {
            Node::File(data) => assert!(String::from_utf8_lossy(&data).contains("noVNC")),
            _ => panic!("embedded vnc.html missing"),
        }
        assert!(matches!(embedded_open("core/rfb.js"), Node::File(_)));
        assert!(matches!(
            embedded_open("core/decoders/tight.js"),
            Node::File(_)
        ));
        assert!(matches!(
            embedded_open("vendor/pako/lib/zlib/inflate.js"),
            Node::File(_)
        ));
        assert!(matches!(embedded_open("defaults.json"), Node::File(_)));
        assert!(matches!(embedded_open("mandatory.json"), Node::File(_)));
        assert!(matches!(embedded_open("package.json"), Node::File(_)));
        assert!(matches!(embedded_open("../Cargo.toml"), Node::NotFound));
    }

    #[test]
    fn knows_mime_types() {
        assert_eq!(mime_for("vnc.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("core/rfb.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("app/images/connect.svg"), "image/svg+xml");
    }
}
