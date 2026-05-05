//! Centralized CollectionContract enforcement for the DML path.
//!
//! Lifts the per-mutation contract checks (`APPEND ONLY` rejection of
//! UPDATE/DELETE, future versioned/vault rules) out of inline DML
//! dispatch into a single Module that INSERT, UPDATE, and DELETE all
//! consult via one call.
//!
//! Behaviour preservation
//! ----------------------
//! Today the only contract flag enforced on the DML path is
//! `append_only`, which rejects UPDATE and DELETE with a fixed error
//! string and a hint pointing at the DDL.  This module reproduces
//! those error strings byte-for-byte so the operator-facing diagnostic
//! (and any callers grepping for it) keeps working.
//!
//! INSERT is still allowed against `APPEND ONLY` collections — that is
//! the whole point of the flag — so calling `check` on the INSERT path
//! is a no-op for the only contract bit currently defined.  Routing
//! INSERT through the gate now means future contract bits (versioned,
//! vault-only writes, …) can plug in here without touching every DML
//! entry point again.
//!
//! The Interface is intentionally tiny: a `MutationKind` discriminator
//! and a single `check(runtime, table, kind)` associated function.

use super::RedDBRuntime;
use crate::api::{RedDBError, RedDBResult};

/// Which DML verb is being dispatched.
///
/// Carried into the contract gate so a single shape covers INSERT,
/// UPDATE, and DELETE without one-off helpers per verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MutationKind {
    Insert,
    Update,
    Delete,
}

impl MutationKind {
    /// Human-readable verb for error messages.
    fn verb(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }
}

/// Gate that consolidates CollectionContract enforcement for DML.
pub(super) struct CollectionContractGate;

impl CollectionContractGate {
    /// Run all collection-contract checks for `table` under
    /// mutation verb `kind`.
    ///
    /// Returns `Ok(())` when the mutation is permitted (the common
    /// case — collections without a contract or contracts whose flags
    /// don't restrict `kind`).  Returns `Err(RedDBError::Query(..))`
    /// with the same operator-facing message the inline check used to
    /// produce.
    ///
    /// Tables without a registered contract are treated as
    /// unrestricted, matching the previous inline `if let Some(..)`
    /// behaviour.
    pub(super) fn check(
        runtime: &RedDBRuntime,
        table: &str,
        kind: MutationKind,
    ) -> RedDBResult<()> {
        let Some(contract) = runtime.db().collection_contract_arc(table) else {
            return Ok(());
        };

        if contract.append_only {
            match kind {
                MutationKind::Update => {
                    return Err(RedDBError::Query(format!(
                        "table '{}' is APPEND ONLY — UPDATE is rejected. \
                         Drop the APPEND ONLY clause (ALTER TABLE ... SET APPEND_ONLY = false) \
                         or insert a new row instead.",
                        table
                    )));
                }
                MutationKind::Delete => {
                    return Err(RedDBError::Query(format!(
                        "table '{}' is APPEND ONLY — DELETE is rejected. \
                         Drop the APPEND ONLY clause (ALTER TABLE ... SET APPEND_ONLY = false) \
                         or use a retention policy / drop_chunks for time-series.",
                        table
                    )));
                }
                MutationKind::Insert => {
                    // APPEND ONLY explicitly permits INSERT — that's
                    // the whole point of the flag.
                }
            }
        }

        // Future contract bits (versioned, vault-only, ...) plug in
        // here.  Each new rule should branch on `kind` so the
        // semantics are explicit per verb.
        let _ = kind.verb(); // Reserved for future error strings.

        Ok(())
    }
}
