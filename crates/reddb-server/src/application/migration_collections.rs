//! Internal collection names for the migrations layer.
//!
//! All migration-owned collections share the `red_*` prefix — matching
//! `red_config`, `red_stats`, `red_commits`, etc. Keeping every name in
//! one file prevents accidental divergence between bootstrap code, runtime
//! access, and schema documentation.

/// Migration definitions: id, name, status, kind, body, author,
/// created_at, applied_at, rows_total, rows_processed, vcs_commit_hash.
pub const MIGRATIONS: &str = "red_migrations";

/// Dependency edges between migrations: migration_id, depends_on_id, inferred.
pub const MIGRATION_DEPS: &str = "red_migration_deps";

/// All migration collections, in bootstrap order.
pub const ALL: &[&str] = &[MIGRATIONS, MIGRATION_DEPS];
