//! TLS setup.
//!
//! `websockify` accepts a "combined" PEM file that holds both the certificate
//! chain and the private key (typically `self.pem`, produced by
//! `openssl req -new -x509 -nodes ... -out self.pem`). We support that plus a
//! separate `--key` file.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio_rustls::rustls::ServerConfig;

#[derive(Clone)]
pub struct TlsConfig {
    pub server: Arc<ServerConfig>,
    /// Where the certificate came from, for logging only.
    pub source: PathBuf,
}

impl std::fmt::Debug for TlsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsConfig")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl TlsConfig {
    pub fn load(cert: &Path, key: Option<&Path>) -> Result<Self> {
        let certs = read_certs(cert)?;
        let key_path = key.unwrap_or(cert);
        let key = read_key(key_path).with_context(|| {
            format!(
                "failed to read private key from {} (encrypted keys are not supported)",
                key_path.display()
            )
        })?;

        let server = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("certificate and private key do not match")?;

        Ok(Self {
            server: Arc::new(server),
            source: cert.to_path_buf(),
        })
    }
}

fn read_certs(
    path: &Path,
) -> Result<Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>> {
    let file =
        File::open(path).with_context(|| format!("cannot open certificate {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let certs: Vec<_> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("invalid PEM certificate {}", path.display()))?;
    if certs.is_empty() {
        bail!("no certificate found in {}", path.display());
    }
    Ok(certs)
}

fn read_key(path: &Path) -> Result<tokio_rustls::rustls::pki_types::PrivateKeyDer<'static>> {
    let file = File::open(path).with_context(|| format!("cannot open key {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("invalid PEM private key {}", path.display()))?;
    key.with_context(|| format!("no private key found in {}", path.display()))
}
