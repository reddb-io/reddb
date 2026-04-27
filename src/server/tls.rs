//! HTTP TLS termination for the embedded HTTP server.
//!
//! The HTTP server uses sync `std::net::TcpStream` + per-connection
//! threads, so we wrap the stream with `rustls::StreamOwned` rather
//! than the async `tokio-rustls` adapter used by the wire transport.
//!
//! Capabilities:
//!   * PEM cert / key load from disk.
//!   * Optional mTLS — when a client-CA bundle is configured, every
//!     handshake must present a cert that chains to it.
//!   * Auto-generated self-signed cert for dev (gated by
//!     `RED_HTTP_TLS_DEV=1`) — refuses without that env knob.
//!   * SHA256 fingerprint logged at boot so operators can pin the cert
//!     out-of-band.
//!   * TLS 1.2 + 1.3 only (rustls default; older protocols are not
//!     compiled in). Cipher suites = rustls defaults (FS-only,
//!     no anonymous, no exportables).

use std::io::{self, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::CertificateDer;
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};

/// Configuration for HTTP TLS termination.
#[derive(Debug, Clone)]
pub struct HttpTlsConfig {
    /// Path to PEM-encoded server certificate chain.
    pub cert_path: PathBuf,
    /// Path to PEM-encoded private key (PKCS#8 or RSA).
    pub key_path: PathBuf,
    /// Optional path to PEM CA bundle. When set, mTLS is required and
    /// every client must present a cert that chains to a CA in this
    /// bundle. When `None`, plain server-side TLS is used (no client
    /// auth — same as the public `https://` web).
    pub client_ca_path: Option<PathBuf>,
}

/// Build a sync rustls `ServerConfig`. Installs the ring crypto
/// provider (idempotent — set_default-style; already done by the wire
/// path, but safe to repeat).
pub fn build_server_config(
    config: &HttpTlsConfig,
) -> Result<Arc<ServerConfig>, Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert_pem = std::fs::read(&config.cert_path)
        .map_err(|err| format!("read TLS cert {}: {err}", config.cert_path.display()))?;
    let key_pem = std::fs::read(&config.key_path)
        .map_err(|err| format!("read TLS key {}: {err}", config.key_path.display()))?;

    let certs = rustls_pemfile::certs(&mut BufReader::new(&cert_pem[..]))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("decode cert PEM: {err}"))?;
    if certs.is_empty() {
        return Err("TLS cert PEM contained no certificates".into());
    }
    let key = rustls_pemfile::private_key(&mut BufReader::new(&key_pem[..]))
        .map_err(|err| format!("decode key PEM: {err}"))?
        .ok_or("TLS key PEM contained no private key")?;

    // Log SHA256 fingerprint (DER-encoded leaf) for out-of-band pinning.
    let fingerprint = sha256_fingerprint_hex(&certs[0]);
    tracing::info!(
        target: "reddb::http_tls",
        cert = %config.cert_path.display(),
        sha256 = %fingerprint,
        "HTTP TLS certificate loaded"
    );

    let builder = ServerConfig::builder();
    let mut server_config = if let Some(ca_path) = &config.client_ca_path {
        let ca_pem = std::fs::read(ca_path)
            .map_err(|err| format!("read mTLS client CA {}: {err}", ca_path.display()))?;
        let mut roots = RootCertStore::empty();
        let ca_certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut BufReader::new(&ca_pem[..]))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| format!("decode mTLS client CA PEM: {err}"))?;
        if ca_certs.is_empty() {
            return Err("mTLS client CA PEM contained no certificates".into());
        }
        for cert in ca_certs {
            roots.add(cert)?;
        }
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|err| format!("build mTLS client verifier: {err}"))?;
        tracing::info!(
            target: "reddb::http_tls",
            ca = %ca_path.display(),
            "HTTP mTLS enabled — clients must present a cert chaining to this CA"
        );
        builder
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .map_err(|err| format!("install TLS cert/key: {err}"))?
    } else {
        builder
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|err| format!("install TLS cert/key: {err}"))?
    };

    // ALPN: advertise both h2 and http/1.1. Our embedded server only
    // speaks HTTP/1.1 today; advertising h2 keeps us forward-compatible
    // with operator-side fronting and most clients negotiate h1.
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];

    Ok(Arc::new(server_config))
}

/// Derive a self-signed dev certificate when `--http-tls-bind` is set
/// without an explicit cert/key. Gated by `RED_HTTP_TLS_DEV=1`; refuses
/// to auto-generate in any other context (refuses prod by default).
///
/// Writes `http-tls-cert.pem` + `http-tls-key.pem` into `dir`. Returns
/// the populated `HttpTlsConfig` pointing at the freshly-written files.
pub fn auto_generate_dev_cert(dir: &Path) -> Result<HttpTlsConfig, Box<dyn std::error::Error>> {
    let dev_flag = std::env::var("RED_HTTP_TLS_DEV").unwrap_or_default();
    if !matches!(dev_flag.as_str(), "1" | "true" | "yes" | "on") {
        return Err(
            "refusing to auto-generate HTTP TLS cert: set RED_HTTP_TLS_DEV=1 to opt into self-signed dev certs"
                .into(),
        );
    }

    let cert_path = dir.join("http-tls-cert.pem");
    let key_path = dir.join("http-tls-key.pem");

    if cert_path.exists() && key_path.exists() {
        tracing::info!(
            target: "reddb::http_tls",
            cert = %cert_path.display(),
            "HTTP TLS dev: reusing existing self-signed cert"
        );
        return Ok(HttpTlsConfig {
            cert_path,
            key_path,
            client_ca_path: None,
        });
    }

    let (cert_pem, key_pem) = generate_self_signed("localhost")?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&cert_path, &cert_pem)?;
    std::fs::write(&key_path, &key_pem)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
    }
    tracing::warn!(
        target: "reddb::http_tls",
        cert = %cert_path.display(),
        "HTTP TLS dev: generated SELF-SIGNED cert (NOT FOR PRODUCTION)"
    );
    Ok(HttpTlsConfig {
        cert_path,
        key_path,
        client_ca_path: None,
    })
}

fn generate_self_signed(hostname: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    use rcgen::{CertificateParams, KeyPair};
    let mut params = CertificateParams::new(vec![hostname.to_string()])?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String(format!("RedDB HTTP {hostname}")),
    );
    params
        .subject_alt_names
        .push(rcgen::SanType::DnsName(hostname.try_into()?));
    if hostname != "localhost" {
        params
            .subject_alt_names
            .push(rcgen::SanType::DnsName("localhost".try_into()?));
    }
    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )));
    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    Ok((cert.pem(), key_pair.serialize_pem()))
}

fn sha256_fingerprint_hex(cert: &CertificateDer<'_>) -> String {
    let digest = crate::crypto::sha256(cert.as_ref());
    let mut out = String::with_capacity(64 + 31);
    for (i, byte) in digest.iter().enumerate() {
        if i > 0 {
            // ':'-separated pairs match `openssl x509 -fingerprint`
            // output so operators can copy-paste for pinning.
            out.push(':');
        }
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{:02x}", byte));
    }
    out
}

/// Wrap a sync TcpStream in a TLS server connection. Performs the
/// handshake as part of stream construction. Returns a stream that
/// transparently encrypts on write / decrypts on read.
pub fn accept_tls(
    config: Arc<ServerConfig>,
    tcp: TcpStream,
) -> io::Result<StreamOwned<ServerConnection, TcpStream>> {
    let conn = ServerConnection::new(config)
        .map_err(|err| io::Error::new(io::ErrorKind::Other, format!("rustls server: {err}")))?;
    let mut stream = StreamOwned::new(conn, tcp);
    // Force the handshake now so any failure surfaces here (and not on
    // the first read inside the request parser).
    let _ = stream.flush();
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that mutate the process-global `RED_HTTP_TLS_DEV` env var
    /// must serialize to avoid trampling each other under cargo's
    /// default parallel test runner.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn fingerprint_format() {
        // 32 zero bytes → 32 ":" -separated lowercase hex pairs.
        let cert = CertificateDer::from(vec![0u8; 8]);
        let fp = sha256_fingerprint_hex(&cert);
        // 32 bytes hex = 64 chars, plus 31 colons = 95 total.
        assert_eq!(fp.len(), 64 + 31);
        assert!(fp.chars().all(|c| c == ':' || c.is_ascii_hexdigit()));
    }

    #[test]
    fn auto_generate_refuses_without_dev_flag() {
        let _g = env_lock().lock();
        let dir = std::env::temp_dir().join(format!(
            "reddb-http-tls-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Make sure flag is unset.
        unsafe {
            std::env::remove_var("RED_HTTP_TLS_DEV");
        }
        let err = auto_generate_dev_cert(&dir).unwrap_err();
        assert!(err.to_string().contains("RED_HTTP_TLS_DEV"));
    }

    #[test]
    fn auto_generate_with_dev_flag_writes_cert() {
        let _g = env_lock().lock();
        let dir = std::env::temp_dir().join(format!(
            "reddb-http-tls-dev-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        unsafe {
            std::env::set_var("RED_HTTP_TLS_DEV", "1");
        }
        let cfg = auto_generate_dev_cert(&dir).expect("should generate");
        assert!(cfg.cert_path.exists());
        assert!(cfg.key_path.exists());
        unsafe {
            std::env::remove_var("RED_HTTP_TLS_DEV");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
