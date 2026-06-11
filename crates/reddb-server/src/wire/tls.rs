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
///
/// The wire cert carries CN `"RedDB Wire {hostname}"` **and**
/// `OrganizationName "RedDB"`. The HTTP edge shares this generator but
/// passes `org = None` (see [`generate_self_signed_dev_cert`]) so its
/// cert keeps no organization.
pub fn generate_self_signed_cert(
    hostname: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    generate_self_signed_dev_cert(hostname, "RedDB Wire", Some("RedDB"))
}

/// Shared self-signed dev-cert generator for the wire and HTTP edges.
///
/// Behaviour-preserving parameterization of two formerly near-identical
/// rcgen blocks (issue #1055): the CN is `"{cn_label} {hostname}"`, and
/// `org` — when `Some` — sets `OrganizationName`. The wire edge passes
/// `Some("RedDB")`; the HTTP edge passes `None` so its cert keeps CN
/// `"RedDB HTTP …"` with **no** organization (do NOT silently add
/// `O=RedDB` to the HTTP cert). The SAN block (`hostname`, `localhost`
/// when distinct, and `127.0.0.1`) is identical for both.
pub(crate) fn generate_self_signed_dev_cert(
    hostname: &str,
    cn_label: &str,
    org: Option<&str>,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    use rcgen::{CertificateParams, KeyPair};

    let mut params = CertificateParams::new(vec![hostname.to_string()])?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String(format!("{cn_label} {hostname}")),
    );
    if let Some(org) = org {
        params.distinguished_name.push(
            rcgen::DnType::OrganizationName,
            rcgen::DnValue::Utf8String(org.to_string()),
        );
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the subject CN + Organization out of a self-signed PEM cert,
    /// so the wire-vs-HTTP DN difference is pinned end-to-end (issue #1055).
    fn subject_cn_and_orgs(cert_pem: &str) -> (Vec<String>, Vec<String>) {
        let der = rustls_pemfile::certs(&mut io::BufReader::new(cert_pem.as_bytes()))
            .next()
            .expect("one certificate in PEM")
            .expect("valid certificate PEM");
        let (_, parsed) =
            x509_parser::parse_x509_certificate(der.as_ref()).expect("parse generated X.509");
        let subject = parsed.subject();
        let cns = subject
            .iter_common_name()
            .filter_map(|cn| cn.as_str().ok().map(str::to_string))
            .collect();
        let orgs = subject
            .iter_organization()
            .filter_map(|o| o.as_str().ok().map(str::to_string))
            .collect();
        (cns, orgs)
    }

    #[test]
    fn wire_cert_keeps_wire_cn_and_reddb_org() {
        let (cert_pem, key_pem) =
            generate_self_signed_cert("localhost").expect("generate wire cert");
        assert!(key_pem.contains("PRIVATE KEY"), "key PEM emitted");
        let (cns, orgs) = subject_cn_and_orgs(&cert_pem);
        assert_eq!(cns, vec!["RedDB Wire localhost".to_string()]);
        assert_eq!(orgs, vec!["RedDB".to_string()]);
    }

    #[test]
    fn http_cert_keeps_http_cn_and_no_org() {
        // The non-obvious diff (issue #1055): the HTTP cert must keep CN
        // "RedDB HTTP …" and carry NO organization. The shared generator
        // passes org = None so we never silently add O=RedDB to it.
        let (cert_pem, _key) = generate_self_signed_dev_cert("localhost", "RedDB HTTP", None)
            .expect("generate http cert");
        let (cns, orgs) = subject_cn_and_orgs(&cert_pem);
        assert_eq!(cns, vec!["RedDB HTTP localhost".to_string()]);
        assert!(orgs.is_empty(), "HTTP cert must not carry an Organization");
    }
}
