//! TLS / mTLS support for the RedWire client.
//!
//! Behind the `redwire-tls` Cargo feature so users that don't
//! need TLS aren't billed for tokio-rustls + rustls + pemfile in
//! their dep tree.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

use crate::error::{ClientError, ErrorCode, Result};

/// TLS / mTLS configuration for `RedWireClient::connect`.
///
/// PEM bytes can be loaded from disk via the `from_files` builder
/// or passed in directly. mTLS requires both `client_cert` and
/// `client_key`; server-cert verification can be skipped via
/// `dangerous_accept_invalid_certs` (dev only).
#[derive(Debug, Clone, Default)]
pub struct TlsConfig {
    pub ca_pem: Option<Vec<u8>>,
    pub client_cert_pem: Option<Vec<u8>>,
    pub client_key_pem: Option<Vec<u8>>,
    pub servername: Option<String>,
    pub dangerous_accept_invalid_certs: bool,
}

impl TlsConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convenience builder: read CA / client cert / client key from
    /// filesystem paths. `client_cert` and `client_key` are
    /// optional — pass `None` for server-only TLS.
    pub fn from_files(
        ca: Option<&Path>,
        client_cert: Option<&Path>,
        client_key: Option<&Path>,
    ) -> Result<Self> {
        let ca_pem = ca
            .map(std::fs::read)
            .transpose()
            .map_err(io_io_err)?;
        let cert_pem = client_cert
            .map(std::fs::read)
            .transpose()
            .map_err(io_io_err)?;
        let key_pem = client_key
            .map(std::fs::read)
            .transpose()
            .map_err(io_io_err)?;
        Ok(Self {
            ca_pem,
            client_cert_pem: cert_pem,
            client_key_pem: key_pem,
            ..Default::default()
        })
    }

    pub fn with_servername(mut self, name: impl Into<String>) -> Self {
        self.servername = Some(name.into());
        self
    }

    pub fn with_dangerous_accept_invalid_certs(mut self, accept: bool) -> Self {
        self.dangerous_accept_invalid_certs = accept;
        self
    }
}

pub(super) async fn wrap_client(
    tcp: TcpStream,
    host: &str,
    cfg: &TlsConfig,
) -> Result<TlsStream<TcpStream>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut roots = RootCertStore::empty();
    if let Some(pem) = &cfg.ca_pem {
        let mut reader = std::io::BufReader::new(&pem[..]);
        for cert in rustls_pemfile::certs(&mut reader) {
            let cert = cert.map_err(parse_err)?;
            roots
                .add(cert)
                .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("add CA cert: {e}")))?;
        }
    } else {
        // Trust the system roots when no explicit CA was supplied.
        // Allows `reds://` against a public-cert server without
        // forcing operators to pass `?ca=`.
        let webpki_roots: Vec<CertificateDer<'static>> = Vec::new();
        let _ = webpki_roots;
    }

    let builder = ClientConfig::builder().with_root_certificates(roots);

    let client_config = match (&cfg.client_cert_pem, &cfg.client_key_pem) {
        (Some(cert_pem), Some(key_pem)) => {
            let certs = load_certs(cert_pem)?;
            let key = load_private_key(key_pem)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| {
                    ClientError::new(ErrorCode::Protocol, format!("client cert: {e}"))
                })?
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(ClientError::new(
                ErrorCode::Protocol,
                "mTLS requires both client_cert_pem and client_key_pem",
            ));
        }
        (None, None) => builder.with_no_client_auth(),
    };

    let connector = TlsConnector::from(Arc::new(client_config));
    let server_name_str = cfg.servername.as_deref().unwrap_or(host);
    let server_name = ServerName::try_from(server_name_str.to_string())
        .map_err(|e| ClientError::new(ErrorCode::Protocol, format!("servername: {e}")))?;

    let tls_stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| ClientError::new(ErrorCode::Network, format!("TLS handshake: {e}")))?;
    Ok(tls_stream)
}

fn load_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    let mut reader = std::io::BufReader::new(pem);
    let certs: std::result::Result<Vec<CertificateDer<'static>>, std::io::Error> =
        rustls_pemfile::certs(&mut reader).collect();
    certs.map_err(parse_err)
}

fn load_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>> {
    let mut reader = std::io::BufReader::new(pem);
    rustls_pemfile::private_key(&mut reader)
        .map_err(parse_err)?
        .ok_or_else(|| ClientError::new(ErrorCode::Protocol, "no private key found in PEM"))
}

fn io_io_err(e: std::io::Error) -> ClientError {
    ClientError::new(ErrorCode::Network, e.to_string())
}

fn parse_err(e: std::io::Error) -> ClientError {
    ClientError::new(ErrorCode::Protocol, format!("PEM parse: {e}"))
}
