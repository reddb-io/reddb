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
//! * Minimal row description using a small OID mapping table.
//! * ReadyForQuery + Command­Complete + ErrorResponse framing.
//!
//! Not in this phase (future 3.1.x):
//! * Extended query (Parse / Bind / Describe / Execute).
//! * SASL / SCRAM auth, TLS.
//! * Function-call protocol.
//! * COPY protocol.
//! * NOTIFY / LISTEN.

pub mod protocol;
pub mod server;
pub mod types;

pub use protocol::{BackendMessage, FrontendMessage, PgWireError};
pub use server::{start_pg_wire_listener, PgWireConfig};
pub use types::{value_to_pg_wire_bytes, PgOid};
