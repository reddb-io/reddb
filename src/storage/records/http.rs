//! HTTP capture record types

/// Snapshot of TLS handshake data captured alongside an HTTP request.
#[derive(Debug, Clone, Default)]
pub struct HttpTlsSnapshot {
    pub authority: Option<String>,
    pub tls_version: Option<String>,
    pub cipher: Option<String>,
    pub alpn: Option<String>,
    pub peer_subjects: Vec<String>,
    pub peer_fingerprints: Vec<String>,
    pub ja3: Option<String>,
    pub ja3s: Option<String>,
    pub ja3_raw: Option<String>,
    pub ja3s_raw: Option<String>,
    pub certificate_chain_pem: Vec<String>,
}

/// HTTP capture - response metadata + headers
#[derive(Debug, Clone)]
pub struct HttpHeadersRecord {
    pub host: String,
    pub url: String,
    pub method: String,
    pub scheme: String,
    pub http_version: String,
    pub status_code: u16,
    pub status_text: String,
    pub server: Option<String>,
    pub body_size: u32,
    pub headers: Vec<(String, String)>,
    pub timestamp: u32,
    pub tls: Option<HttpTlsSnapshot>,
}
