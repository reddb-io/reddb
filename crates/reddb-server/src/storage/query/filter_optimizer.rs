//! Filter AST optimizer — re-export shim.
//!
//! The bottom-up `Filter` rewrites (OR-of-equalities → `IN`, AND/OR flatten)
//! moved into the `reddb-io-rql` crate alongside the canonical SQL AST they
//! rewrite (#1113, ADR 0053). They depend only on the AST plus
//! `reddb-io-types`. This shim preserves the historical
//! `crate::storage::query::filter_optimizer::*` import path so existing
//! call-sites keep resolving unchanged.

pub use reddb_rql::filter_optimizer::*;
