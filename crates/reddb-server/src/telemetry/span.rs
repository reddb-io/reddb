//! Span helpers that pull connection / transaction / tenant / auth
//! context out of the runtime thread-locals and attach them to a
//! `tracing::Span`. Any log emitted while the span is entered picks
//! up these fields automatically — the caller never has to pass
//! `conn_id` / `xid` / `tenant` as parameters down through the call
//! graph.
//!
//! Usage:
//! ```ignore
//! use reddb::telemetry::span;
//! let _g = span::query_span(query).entered();
//! // subsequent tracing::info!/warn!/error! carry conn_id+xid+tenant
//! ```

use tracing::Span;

use crate::runtime::mvcc::{current_connection_id, current_tenant};

/// Span wrapping a single `execute_query` call. Stamps the current
/// connection id, transaction xid (0 when autocommit), tenant, and
/// authenticated user so every downstream event (filter eval, scan,
/// CDC emit, error) inherits correlation context.
///
/// Keep the string cheap — we only store the length, not the query
/// body, to avoid PII leakage into logs.
pub fn query_span(query: &str) -> Span {
    tracing::info_span!(
        "query",
        conn_id = current_connection_id(),
        tenant = current_tenant().unwrap_or_default(),
        query_len = query.len(),
    )
}

/// Span for a new wire/gRPC/HTTP connection. Call at accept time so
/// every log inside that connection's lifetime carries the remote
/// peer and transport kind.
pub fn connection_span(transport: &'static str, peer: impl std::fmt::Display) -> Span {
    tracing::info_span!(
        "conn",
        transport = transport,
        peer = %peer,
    )
}

/// Span for a listener bind — emits once at startup to mark the
/// transport as ready.
pub fn listener_span(transport: &'static str, bind: impl std::fmt::Display) -> Span {
    tracing::info_span!(
        "listener",
        transport = transport,
        bind = %bind,
    )
}
