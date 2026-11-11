//! Internal collection names for the migrations layer.
//!
//! All migration-owned collections share the `red.*` prefix — matching
//! `red.config`, `red.query.stats_catalog`, and `red.vcs.commits`. Keeping every name in
//! one file prevents accidental divergence between bootstrap code, runtime
//! access, and schema documentation.

/// Migration definitions: id, name, status, kind, body, author,
/// created_at, applied_at, rows_total, rows_processed, vcs_commit_hash.
pub const MIGRATIONS: &str = "red.migration.definitions";

/// Dependency edges between migrations: migration_id, depends_on_id, inferred.
pub const MIGRATION_DEPS: &str = "red.migration.dependencies";

/// All migration collections, in bootstrap order.
pub const ALL: &[&str] = &[MIGRATIONS, MIGRATION_DEPS];
