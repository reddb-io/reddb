//! AST → SQL/RQL renderer — re-export shim.
//!
//! The renderer moved into `reddb-io-rql` (#1103, ADR 0053) so the parser
//! family's embedded property round-trip test (`render(parse(render(ast)))`)
//! keeps building inside the crate as a single type instance — routing it
//! through the `reddb-server` dev-dependency would pull in a second copy of
//! the AST and break the round-trip with a spurious type mismatch. The
//! renderer is pure `QueryExpr`/`Value` → text and depends only on
//! `reddb-io-types`, so the crate graph stays acyclic.
//!
//! This shim preserves the historical `crate::storage::query::renderer::*`
//! path so the server-runtime call-sites (`render_value_sql` in
//! `runtime::red_schema`) keep resolving unchanged.

pub use reddb_rql::renderer::*;
