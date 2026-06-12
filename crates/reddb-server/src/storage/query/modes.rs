//! Multi-mode query translators — re-export shim.
//!
//! The seven mode translators (SQL, Gremlin, Cypher, SPARQL, Path, Natural,
//! and the vector extensions) were re-homed into the `reddb-io-rql` language
//! front-end crate (#1104, ADR 0053). Each stays a thin pure translator into
//! the canonical SQL AST that already lives in that crate (#1113): SQL,
//! Cypher, and Path dispatch through the relocated parser (#1103), while the
//! Gremlin/SPARQL/Natural translators and the mode detector moved verbatim
//! alongside it. All of it depends only on `reddb-io-types`, so the crate
//! graph stays acyclic — no `reddb-io-rql -> reddb-server` back-edge.
//!
//! This shim preserves the historical `crate::storage::query::modes::*` import
//! path — including the `modes::gremlin::*`, `modes::sparql::*`,
//! `modes::natural::*`, and `modes::detect::*` submodule paths that the
//! in-server executors reach into — so every runtime, presentation, wire, and
//! executor call-site across the workspace keeps resolving unchanged. A
//! byte-faithful move with zero call-site edits.

pub use reddb_rql::modes::*;

pub mod detect {
    pub use reddb_rql::modes::detect::*;
}
pub mod gremlin {
    pub use reddb_rql::modes::gremlin::*;
}
pub mod natural {
    pub use reddb_rql::modes::natural::*;
}
pub mod sparql {
    pub use reddb_rql::modes::sparql::*;
}
