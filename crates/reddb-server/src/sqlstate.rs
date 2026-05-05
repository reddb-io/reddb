//! SQLSTATE error codes — Post-MVP credibility item.
//!
//! Mirrors PostgreSQL's `errcodes.h` 5-character SQLSTATE codes
//! so reddb errors carry the same standardized identifiers
//! drivers / client libs / monitoring tools already understand.
//! A SQLSTATE is a fixed 5-character ASCII string where the
//! first 2 characters identify a class and the next 3 identify
//! a specific condition within that class.
//!
//! Examples:
//! - `08006` connection_failure
//! - `22012` division_by_zero
//! - `23505` unique_violation
//! - `42601` syntax_error
//! - `42P01` undefined_table
//! - `XX000` internal_error
//!
//! ## Why this matters
//!
//! ODBC / JDBC / psycopg / pgx all switch on SQLSTATE for retry
//! logic, error categorization, and i18n. A reddb client that
//! sees `42P01` knows the table doesn't exist regardless of the
//! human-readable message language. Without SQLSTATE, drivers
//! have to string-match error text — fragile across versions.
//!
//! ## Mapping to RedDBError
//!
//! `RedDBError -> SqlState` is a one-line lookup. The reverse
//! direction (parse a 5-char code into a category) is also
//! available via `SqlState::class()` for filter UIs.
//!
//! ## Wiring
//!
//! Phase post-MVP wiring adds a `sqlstate()` method on
//! `RedDBError` and threads the code through the wire protocol
//! `ErrorResponse` frame so HTTP / gRPC / stdio clients all
//! receive it.

/// 5-character SQLSTATE code wrapper. Stored as a fixed
/// `[u8; 5]` so it's `Copy`, fits in a register, and can be
/// compared with a single `==`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SqlState(pub [u8; 5]);

impl SqlState {
    /// Build from a string literal at compile time. Caller is
    /// responsible for ensuring the input is exactly 5 ASCII
    /// characters; non-ASCII / wrong length panics in debug,
    /// truncates in release.
    pub const fn new(s: &str) -> Self {
        let bytes = s.as_bytes();
        debug_assert!(bytes.len() == 5, "SQLSTATE must be 5 chars");
        let mut buf = [b'?'; 5];
        let mut i = 0;
        while i < 5 && i < bytes.len() {
            buf[i] = bytes[i];
            i += 1;
        }
        Self(buf)
    }

    /// Return the 2-character class prefix as a string slice.
    pub fn class(&self) -> &str {
        // Safety: SqlState is built from ASCII only.
        std::str::from_utf8(&self.0[..2]).unwrap_or("??")
    }

    /// Render as a stack-allocated string for display.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("?????")
    }
}

impl std::fmt::Display for SqlState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ────────────────────────────────────────────────────────────────
// Class 00 — Successful Completion
// ────────────────────────────────────────────────────────────────
pub const SUCCESSFUL_COMPLETION: SqlState = SqlState::new("00000");

// ────────────────────────────────────────────────────────────────
// Class 08 — Connection Exception
// ────────────────────────────────────────────────────────────────
pub const CONNECTION_EXCEPTION: SqlState = SqlState::new("08000");
pub const CONNECTION_FAILURE: SqlState = SqlState::new("08006");
pub const CONNECTION_DOES_NOT_EXIST: SqlState = SqlState::new("08003");
pub const SQLSERVER_REJECTED_ESTABLISHMENT: SqlState = SqlState::new("08004");

// ────────────────────────────────────────────────────────────────
// Class 22 — Data Exception
// ────────────────────────────────────────────────────────────────
pub const DATA_EXCEPTION: SqlState = SqlState::new("22000");
pub const DIVISION_BY_ZERO: SqlState = SqlState::new("22012");
pub const NUMERIC_VALUE_OUT_OF_RANGE: SqlState = SqlState::new("22003");
pub const NULL_VALUE_NOT_ALLOWED_DATA: SqlState = SqlState::new("22004");
pub const STRING_DATA_RIGHT_TRUNCATION: SqlState = SqlState::new("22001");
pub const INVALID_DATETIME_FORMAT: SqlState = SqlState::new("22007");
pub const INVALID_TEXT_REPRESENTATION: SqlState = SqlState::new("22P02");

// ────────────────────────────────────────────────────────────────
// Class 23 — Integrity Constraint Violation
// ────────────────────────────────────────────────────────────────
pub const INTEGRITY_CONSTRAINT_VIOLATION: SqlState = SqlState::new("23000");
pub const NOT_NULL_VIOLATION: SqlState = SqlState::new("23502");
pub const FOREIGN_KEY_VIOLATION: SqlState = SqlState::new("23503");
pub const UNIQUE_VIOLATION: SqlState = SqlState::new("23505");
pub const CHECK_VIOLATION: SqlState = SqlState::new("23514");

// ────────────────────────────────────────────────────────────────
// Class 25 — Invalid Transaction State
// ────────────────────────────────────────────────────────────────
pub const INVALID_TRANSACTION_STATE: SqlState = SqlState::new("25000");
pub const ACTIVE_SQL_TRANSACTION: SqlState = SqlState::new("25001");
pub const NO_ACTIVE_SQL_TRANSACTION: SqlState = SqlState::new("25P01");
pub const READ_ONLY_SQL_TRANSACTION: SqlState = SqlState::new("25006");

// ────────────────────────────────────────────────────────────────
// Class 28 — Invalid Authorization Specification
// ────────────────────────────────────────────────────────────────
pub const INVALID_PASSWORD: SqlState = SqlState::new("28P01");

// ────────────────────────────────────────────────────────────────
// Class 40 — Transaction Rollback
// ────────────────────────────────────────────────────────────────
pub const TRANSACTION_ROLLBACK: SqlState = SqlState::new("40000");
pub const SERIALIZATION_FAILURE: SqlState = SqlState::new("40001");
pub const DEADLOCK_DETECTED: SqlState = SqlState::new("40P01");

// ────────────────────────────────────────────────────────────────
// Class 42 — Syntax Error or Access Rule Violation
// ────────────────────────────────────────────────────────────────
pub const SYNTAX_ERROR_OR_ACCESS_RULE_VIOLATION: SqlState = SqlState::new("42000");
pub const SYNTAX_ERROR: SqlState = SqlState::new("42601");
pub const UNDEFINED_COLUMN: SqlState = SqlState::new("42703");
pub const UNDEFINED_FUNCTION: SqlState = SqlState::new("42883");
pub const UNDEFINED_TABLE: SqlState = SqlState::new("42P01");
pub const UNDEFINED_PARAMETER: SqlState = SqlState::new("42P02");
pub const DUPLICATE_COLUMN: SqlState = SqlState::new("42701");
pub const DUPLICATE_TABLE: SqlState = SqlState::new("42P07");
pub const AMBIGUOUS_COLUMN: SqlState = SqlState::new("42702");
pub const DATATYPE_MISMATCH: SqlState = SqlState::new("42804");

// ────────────────────────────────────────────────────────────────
// Class 53 — Insufficient Resources
// ────────────────────────────────────────────────────────────────
pub const INSUFFICIENT_RESOURCES: SqlState = SqlState::new("53000");
pub const DISK_FULL: SqlState = SqlState::new("53100");
pub const OUT_OF_MEMORY: SqlState = SqlState::new("53200");
pub const TOO_MANY_CONNECTIONS: SqlState = SqlState::new("53300");

// ────────────────────────────────────────────────────────────────
// Class 57 — Operator Intervention
// ────────────────────────────────────────────────────────────────
pub const OPERATOR_INTERVENTION: SqlState = SqlState::new("57000");
pub const QUERY_CANCELED: SqlState = SqlState::new("57014");

// ────────────────────────────────────────────────────────────────
// Class 58 — System Error (errors external to PostgreSQL itself)
// ────────────────────────────────────────────────────────────────
pub const SYSTEM_ERROR: SqlState = SqlState::new("58000");
pub const IO_ERROR: SqlState = SqlState::new("58030");

// ────────────────────────────────────────────────────────────────
// Class XX — Internal Error (PG extension)
// ────────────────────────────────────────────────────────────────
pub const INTERNAL_ERROR: SqlState = SqlState::new("XX000");
pub const DATA_CORRUPTED: SqlState = SqlState::new("XX001");

/// Map a `RedDBError` variant to its SQLSTATE code. Used by
/// the wire protocol's error response frame so clients get
/// the standardized identifier alongside the human message.
pub fn sqlstate_for_reddb_error(err: &crate::api::RedDBError) -> SqlState {
    use crate::api::RedDBError as E;
    match err {
        E::InvalidConfig(_) => SYNTAX_ERROR_OR_ACCESS_RULE_VIOLATION,
        E::SchemaVersionMismatch { .. } => DATA_CORRUPTED,
        E::FeatureNotEnabled(_) => SYNTAX_ERROR_OR_ACCESS_RULE_VIOLATION,
        E::NotFound(_) => UNDEFINED_TABLE,
        E::ReadOnly(_) => READ_ONLY_SQL_TRANSACTION,
        E::Engine(_) => INTERNAL_ERROR,
        E::Catalog(_) => INTERNAL_ERROR,
        E::Query(_) => SYNTAX_ERROR,
        E::Io(_) => IO_ERROR,
        E::VersionUnavailable => INTERNAL_ERROR,
        // 53400 = configuration_limit_exceeded — closest PG class for
        // operator-pinned RED_MAX_* enforcement (PLAN.md Phase 4.1).
        E::QuotaExceeded(_) => SqlState::new("53400"),
        E::Internal(_) => INTERNAL_ERROR,
    }
}
