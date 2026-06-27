//! All-at-once, reversible migration to the native binary document body
//! (PRD-1398, ADR-0063).
//!
//! A legacy DOCUMENT store keeps two copies of every document — a plain-JSON
//! `body` and the materialised promoted columns. The single-source slices
//! (#1402/#1403) make the binary `body` the one source of truth, but existing
//! stores still hold plain-JSON bodies with promoted columns. This tool
//! performs the cutover, modelled on the maintainer's explicit safety margin:
//! the canonical data is never touched until the very end.
//!
//! The migration is **all-at-once but reversible**:
//!
//! 1. Read every document from the source store (untouched, read-only).
//! 2. Build a **fresh store in new files alongside** the source, rewriting each
//!    document into the native binary container ([`crate::document_body`]).
//! 3. Auto-`CREATE INDEX` for **every previously-promoted field** so queries
//!    that relied on the implicit promoted-column filter stay fast — avoiding a
//!    silent post-deploy performance regression.
//! 4. **Verify document counts** match per collection; any mismatch aborts the
//!    migration *before* the swap, leaving the source store untouched.
//! 5. **Atomically swap** the new files into place and **retain the old files**
//!    as the rollback point.
//!
//! The unit of migration is a store **directory** (the directory that holds the
//! `db.rdb` data file and its sibling artifacts — WAL, snapshots, audit log).
//! Swapping whole directories with `rename(2)` is atomic on POSIX and lets the
//! pre-migration directory survive untouched as the rollback point.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::application::{CreateDocumentInput, EntityUseCases};
use crate::catalog::CollectionModel;
use crate::presentation::entity_json::storage_json_bytes_to_json;
use crate::storage::schema::Value;
use crate::storage::EntityData;
use crate::{RedDBError, RedDBOptions, RedDBResult, RedDBRuntime};

/// The data file name inside a store directory. Matches the convention used by
/// [`RedDBOptions::in_memory`], whose siblings (WAL, audit, snapshots) derive
/// from the data path's parent directory.
const DATA_FILE: &str = "db.rdb";

/// Suffix for the freshly-built store directory written alongside the source.
const MIGRATING_SUFFIX: &str = "migrating";

/// Suffix for the retained pre-migration store directory (the rollback point).
const BACKUP_SUFFIX: &str = "pre-migration";

/// Per-collection outcome of a migration.
#[derive(Debug, Clone)]
pub struct CollectionMigration {
    /// Collection name.
    pub name: String,
    /// Document count read from the source store.
    pub source_documents: usize,
    /// Document count written into the migrated store.
    pub migrated_documents: usize,
    /// Previously-promoted fields that received an auto-created index, sorted.
    pub auto_indexed_fields: Vec<String>,
}

/// Summary of a completed, swapped migration.
#[derive(Debug, Clone)]
pub struct MigrationReport {
    /// Per-collection outcomes, in collection-name order.
    pub collections: Vec<CollectionMigration>,
    /// Directory holding the retained pre-migration files (rollback point).
    pub backup_dir: PathBuf,
    /// Total documents migrated across all collections.
    pub total_documents: usize,
}

/// One document collection's data gathered from the source store.
struct SourceCollection {
    name: String,
    bodies: Vec<crate::json::Value>,
    /// Previously-promoted top-level fields (materialised as columns), sorted
    /// and de-duplicated across every document.
    promoted_fields: Vec<String>,
}

/// Migrate the DOCUMENT collections of the store in `store_dir` to the native
/// binary body, reversibly.
///
/// On success the store at `store_dir` holds the migrated (binary-body) files
/// and the pre-migration files are retained at the returned
/// [`MigrationReport::backup_dir`]. On any failure *before* the swap, the
/// source store is left exactly as it was and the partially-built migrated
/// directory is removed.
pub fn migrate_store_to_binary_body(store_dir: &Path) -> RedDBResult<MigrationReport> {
    if !store_dir.is_dir() {
        return Err(RedDBError::InvalidOperation(format!(
            "migration source is not a directory: {}",
            store_dir.display()
        )));
    }

    // Phase 1 — read the source store. Scoped so the runtime (and its file
    // handles) are dropped before we touch the filesystem for the swap.
    let collections = read_source_collections(store_dir)?;

    // Phase 2 — build the migrated store in fresh files alongside the source.
    let migrating_dir = sibling_dir(store_dir, MIGRATING_SUFFIX)?;
    let outcome = build_migrated_store(&migrating_dir, &collections);
    let report_collections = match outcome {
        Ok(report) => report,
        Err(err) => {
            // Never leave a half-built store around; the source is untouched.
            let _ = std::fs::remove_dir_all(&migrating_dir);
            return Err(err);
        }
    };

    // Phase 3 — atomic swap, retaining the old files for rollback.
    let backup_dir = swap_in_migrated_store(store_dir, &migrating_dir)?;

    let total_documents = report_collections
        .iter()
        .map(|c| c.migrated_documents)
        .sum();
    Ok(MigrationReport {
        collections: report_collections,
        backup_dir,
        total_documents,
    })
}

/// Open the source store read-only-ish and pull every document body plus the
/// set of previously-promoted fields per DOCUMENT collection.
fn read_source_collections(store_dir: &Path) -> RedDBResult<Vec<SourceCollection>> {
    let source = RedDBRuntime::with_options(RedDBOptions::persistent(store_dir.join(DATA_FILE)))?;

    let mut document_collections: Vec<String> = source
        .catalog()
        .collections
        .into_iter()
        .filter(|descriptor| descriptor.model == CollectionModel::Document)
        .map(|descriptor| descriptor.name)
        .collect();
    document_collections.sort();

    let mut out = Vec::with_capacity(document_collections.len());
    for name in document_collections {
        let mut bodies = Vec::new();
        let mut promoted: BTreeSet<String> = BTreeSet::new();
        let mut cursor = None;
        loop {
            let page = source.scan_collection(&name, cursor, 1_000)?;
            for entity in &page.items {
                let EntityData::Row(row) = &entity.data else {
                    continue;
                };
                // Every previously-promoted field is materialised as a named
                // column next to `body`; the body itself is the full document.
                for (field, _value) in row.iter_fields() {
                    if field != "body"
                        && !crate::reserved_fields::is_reserved_public_item_field(field)
                    {
                        promoted.insert(field.to_string());
                    }
                }
                bodies.push(decode_body(row)?);
            }
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        out.push(SourceCollection {
            name,
            bodies,
            promoted_fields: promoted.into_iter().collect(),
        });
    }

    Ok(out)
}

/// Decode a document row's `body` column to its JSON form, transparently
/// handling both legacy plain-JSON and (already-)binary bodies.
fn decode_body(row: &crate::storage::RowData) -> RedDBResult<crate::json::Value> {
    match row.get_field("body") {
        Some(Value::Json(bytes)) => Ok(storage_json_bytes_to_json(bytes)),
        Some(other) => Ok(crate::presentation::entity_json::storage_value_to_json(
            other,
        )),
        None => Err(RedDBError::InvalidOperation(
            "document row is missing its `body` field".to_string(),
        )),
    }
}

/// Build a fresh binary-body store in `migrating_dir` from the gathered source
/// collections, verifying counts before returning.
fn build_migrated_store(
    migrating_dir: &Path,
    collections: &[SourceCollection],
) -> RedDBResult<Vec<CollectionMigration>> {
    std::fs::create_dir_all(migrating_dir).map_err(RedDBError::Io)?;

    let target =
        RedDBRuntime::with_options(RedDBOptions::persistent(migrating_dir.join(DATA_FILE)))?;
    // Every document write below stores the body in the native binary
    // container. Reads stay JSON regardless of this flag (self-describing
    // `RDOC` magic), so the wire/clients are unaffected.
    target.execute_query("SET CONFIG storage.binary_document_body = true")?;

    let entities = EntityUseCases::new(&target);
    let mut report = Vec::with_capacity(collections.len());

    for collection in collections {
        target.execute_query(&format!("CREATE DOCUMENT {}", collection.name))?;

        for body in &collection.bodies {
            entities.create_document(CreateDocumentInput {
                collection: collection.name.clone(),
                body: body.clone(),
                metadata: Vec::new(),
                node_links: Vec::new(),
                vector_links: Vec::new(),
            })?;
        }

        // Auto-`CREATE INDEX` for every previously-promoted field so queries
        // that relied on the implicit promoted-column filter stay fast.
        for field in &collection.promoted_fields {
            target.execute_query(&format!(
                "CREATE INDEX {index} ON {collection} ({field}) USING BTREE",
                index = auto_index_name(&collection.name, field),
                collection = collection.name,
                field = field,
            ))?;
        }

        // Verify the document count before this becomes live: a bad rewrite
        // must be caught here, not after the swap.
        let migrated = target.scan_collection(&collection.name, None, 1)?.total;
        let source = collection.bodies.len();
        ensure_counts_match(&collection.name, source, migrated)?;

        report.push(CollectionMigration {
            name: collection.name.clone(),
            source_documents: source,
            migrated_documents: migrated,
            auto_indexed_fields: collection.promoted_fields.clone(),
        });
    }

    // Flush + checkpoint so the migrated files are durable before we drop the
    // runtime and rename the directory into place.
    target.flush()?;
    target.checkpoint()?;
    drop(target);

    Ok(report)
}

/// Atomically swap `migrating_dir` into `store_dir`, moving the existing files
/// aside to a retained backup directory. Returns the backup directory path.
fn swap_in_migrated_store(store_dir: &Path, migrating_dir: &Path) -> RedDBResult<PathBuf> {
    let backup_dir = sibling_dir(store_dir, BACKUP_SUFFIX)?;

    // Move the pre-migration files aside (rollback point) …
    std::fs::rename(store_dir, &backup_dir).map_err(RedDBError::Io)?;
    // … then move the migrated files into place. If this second step fails,
    // restore the original directory so the store is never left missing.
    if let Err(err) = std::fs::rename(migrating_dir, store_dir) {
        let _ = std::fs::rename(&backup_dir, store_dir);
        return Err(RedDBError::Io(err));
    }

    Ok(backup_dir)
}

/// A sibling path of `store_dir` with `<name>.<suffix>`, guaranteed not to
/// already exist (so a stale directory never silently shadows the swap).
fn sibling_dir(store_dir: &Path, suffix: &str) -> RedDBResult<PathBuf> {
    let file_name = store_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            RedDBError::InvalidOperation(format!(
                "store directory has no usable name: {}",
                store_dir.display()
            ))
        })?;
    let parent = store_dir.parent().unwrap_or_else(|| Path::new("."));
    let candidate = parent.join(format!("{file_name}.{suffix}"));
    if candidate.exists() {
        return Err(RedDBError::InvalidOperation(format!(
            "migration sibling directory already exists: {}",
            candidate.display()
        )));
    }
    Ok(candidate)
}

/// Guard the pre-swap invariant: the migrated collection must hold exactly as
/// many documents as the source. A mismatch aborts the migration *before* the
/// swap, so the canonical source store is never touched.
fn ensure_counts_match(collection: &str, source: usize, migrated: usize) -> RedDBResult<()> {
    if migrated != source {
        return Err(RedDBError::InvalidOperation(format!(
            "document count mismatch for collection '{collection}': source {source} != \
             migrated {migrated}; aborting before swap"
        )));
    }
    Ok(())
}

/// Deterministic, identifier-safe name for an auto-created migration index.
fn auto_index_name(collection: &str, field: &str) -> String {
    fn sanitize(input: &str) -> String {
        input
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }
    format!("mig_{}_{}", sanitize(collection), sanitize(field))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_counts_pass_mismatch_aborts() {
        assert!(ensure_counts_match("docs", 4, 4).is_ok());
        // A short rewrite must abort before the swap.
        let err = ensure_counts_match("docs", 4, 3).expect_err("mismatch aborts");
        assert!(matches!(err, RedDBError::InvalidOperation(_)));
    }

    #[test]
    fn auto_index_name_is_identifier_safe() {
        assert_eq!(auto_index_name("docs", "score"), "mig_docs_score");
        assert_eq!(
            auto_index_name("my-docs", "user.name"),
            "mig_my_docs_user_name"
        );
    }

    #[test]
    fn sibling_dir_rejects_existing() {
        let base = std::env::temp_dir().join(format!(
            "reddb-mig-sibling-{}-{}",
            std::process::id(),
            "store"
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // No sibling yet → ok.
        let sib = sibling_dir(&base, "migrating").unwrap();
        assert!(sib
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".migrating")));

        // Create it, then the helper must refuse to reuse it.
        std::fs::create_dir_all(&sib).unwrap();
        assert!(sibling_dir(&base, "migrating").is_err());

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&sib);
    }
}
