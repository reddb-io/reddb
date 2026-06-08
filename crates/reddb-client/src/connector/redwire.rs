//! Compatibility adapter for the `red_client` binary.
//!
//! The RedWire implementation lives in [`crate::redwire`]. This module keeps
//! the historical `reddb_client::connector::redwire::*` import path used by
//! the internal CLI without carrying a second frame/handshake implementation.

use std::fmt;

use crate::error::{ClientError, ErrorCode};
use crate::redwire::{ConnectOptions, RedWireClient as CanonicalRedWireClient};

#[derive(Debug, Clone)]
pub enum Auth {
    Anonymous,
    Bearer(String),
}

#[derive(Debug)]
pub enum RedWireError {
    Network(String),
    Protocol(String),
    AuthRefused(String),
    Engine(String),
    TlsNotImplemented,
}

impl fmt::Display for RedWireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(m) => write!(f, "network: {m}"),
            Self::Protocol(m) => write!(f, "protocol: {m}"),
            Self::AuthRefused(m) => write!(f, "auth refused: {m}"),
            Self::Engine(m) => write!(f, "engine error: {m}"),
            Self::TlsNotImplemented => write!(
                f,
                "RedWire-over-TLS (reds://) is not yet wired through red_client; \
                 use red:// (plain) or the full `red` binary for now"
            ),
        }
    }
}

impl std::error::Error for RedWireError {}

type Result<T> = std::result::Result<T, RedWireError>;

#[derive(Debug)]
pub struct RedWireClient {
    inner: CanonicalRedWireClient,
}

impl RedWireClient {
    pub async fn connect(host: &str, port: u16, tls: bool, auth: Auth) -> Result<Self> {
        if tls {
            return Err(RedWireError::TlsNotImplemented);
        }
        let opts = ConnectOptions::new(host, port).with_auth(match auth {
            Auth::Anonymous => crate::redwire::Auth::Anonymous,
            Auth::Bearer(token) => crate::redwire::Auth::Bearer(token),
        });
        let inner = CanonicalRedWireClient::connect(opts)
            .await
            .map_err(RedWireError::from_client_error)?;
        Ok(Self { inner })
    }

    pub async fn query(&mut self, sql: &str) -> Result<String> {
        self.inner
            .query_raw(sql)
            .await
            .map_err(RedWireError::from_client_error)
    }
}

impl RedWireError {
    fn from_client_error(err: ClientError) -> Self {
        match err.code {
            ErrorCode::Network => Self::Network(err.message),
            ErrorCode::Protocol => Self::Protocol(err.message),
            ErrorCode::AuthRefused => Self::AuthRefused(err.message),
            ErrorCode::Engine => Self::Engine(err.message),
            _ => Self::Protocol(err.to_string()),
        }
    }
}
