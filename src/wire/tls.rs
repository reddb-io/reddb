/// Wire Protocol TLS support
///
/// Provides:
/// - Auto-generated self-signed certificates for dev mode
/// - TLS acceptor configuration from cert/key files
/// - TLS-wrapped TCP listener
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// TLS configuration for the wire protocol.
#[derive(Debug, Clone)]
pub struct WireTlsConfig {
    /// Path to PEM certificate file
    pub cert_path: PathBuf,
    /// Path to PEM private key file
    pub key_path: PathBuf,
}

/// Generate a self-signed certificate for development.
/// Returns (cert_pem, key_pem) as strings.
pub fn generate_self_signed_cert(
    hostname: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    use rcgen::{CertificateParams, KeyPair};

    let mut params = CertificateParams::new(vec![hostname.to_string()])?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String(format!("RedDB Wire {hostname}")),
    );
    params.distinguished_name.push(
        rcgen::DnType::OrganizationName,
        rcgen::DnValue::Utf8String("RedDB".to_string()),
    );

    // Add localhost + IP SANs for dev
    params
        .subject_alt_names
        .push(rcgen::SanType::DnsName(hostname.try_into()?));
    if hostname != "localhost" {
        params
            .subject_alt_names
            .push(rcgen::SanType::DnsName("localhost".try_into()?));
    }
    // 127.0.0.1
    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )));

    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}

/// Generate self-signed cert and write to files in the given directory.
/// Returns the WireTlsConfig pointing to the written files.
pub fn auto_generate_cert(dir: &Path) -> Result<WireTlsConfig, Box<dyn std::error::Error>> {
    let cert_path = dir.join("wire-tls-cert.pem");
    let key_path = dir.join("wire-tls-key.pem");

    // If files already exist, reuse them
    if cert_path.exists() && key_path.exists() {
        tracing::info!(cert = %cert_path.display(), "wire TLS: reusing existing cert");
        return Ok(WireTlsConfig {
            cert_path,
            key_path,
        });
    }

    tracing::info!("wire TLS: generating self-signed certificate");
    let (cert_pem, key_pem) = generate_self_signed_cert("localhost")?;

    std::fs::create_dir_all(dir)?;
    std::fs::write(&cert_path, &cert_pem)?;
    std::fs::write(&key_path, &key_pem)?;

    // Restrict key file permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
    }

    tracing::info!(
        cert = %cert_path.display(),
        key = %key_path.display(),
        "wire TLS: wrote self-signed cert"
    );

    Ok(WireTlsConfig {
        cert_path,
        key_path,
    })
}

/// Build a TLS acceptor from cert and key PEM files.
pub fn build_tls_acceptor(
    config: &WireTlsConfig,
) -> Result<TlsAcceptor, Box<dyn std::error::Error>> {
    // Ensure the ring crypto provider is installed
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert_pem = std::fs::read(&config.cert_path)?;
    let key_pem = std::fs::read(&config.key_path)?;

    let certs = rustls_pemfile::certs(&mut io::BufReader::new(&cert_pem[..]))
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut io::BufReader::new(&key_pem[..]))?
        .ok_or("no private key found in PEM file")?;

    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}
