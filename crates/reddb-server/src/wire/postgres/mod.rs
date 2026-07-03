//! PostgreSQL wire protocol compatibility layer (Phase 3.1 PG parity).
//!
//! Exposes a minimal subset of the PG v3 protocol so `psql`, JDBC drivers
//! (pgjdbc), node-postgres, pgAdmin, DBeaver, and friends can connect to
//! RedDB as if it were PostgreSQL.
//!
//! # Scope (Phase 3.1)
//!
//! * Startup message negotiation (protocol version 3.0 / 196608).
//! * Authentication: `trust` only (no password). Clients that send an
//!   actual password are accepted — the password is ignored.
//! * Simple query protocol (`Q` frames): parse → execute → stream rows.
//! * Extended query protocol (`Parse` / `Bind` / `Describe` / `Execute` /
//!   `Close` / `Sync`), including prepared statements, portals,
//!   `ParameterDescription`, and row-limited `Execute` with
//!   `PortalSuspended`.
//! * Minimal row description using a small OID mapping table.
//! * ReadyForQuery + Command­Complete + ErrorResponse framing.
//! * TLS: `SSLRequest` is answered with `'S'` and the connection is
//!   upgraded via a rustls handshake when the listener carries TLS
//!   material, sharing the native wire's cert/key config
//!   (`PgWireConfig::tls`). Without TLS material `SSLRequest` is declined
//!   with `'N'` and the client continues in plaintext. A cleartext
//!   password is only challenged over an encrypted link or a loopback bind
//!   (see `authenticate_startup`).
//!
//! Not in this phase (future 3.1.x):
//! * SASL / SCRAM auth, GSSAPI encryption.
//! * Function-call protocol.
//! * COPY protocol.
//! * NOTIFY / LISTEN.

pub mod protocol;
pub mod server;
pub mod types;

mod catalog_views;

pub use protocol::{BackendMessage, FrontendMessage, PgWireError};
pub use server::{start_pg_wire_listener, PgWireConfig};
pub use types::{value_to_pg_wire_bytes, PgOid};
