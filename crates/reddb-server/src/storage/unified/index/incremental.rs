//! Incremental Index Maintainer (issue #160 — closes BASELINE.md Finding #4).
//!
//! Secondary indexes used to be a snapshot of the table as it looked at
//! `CREATE INDEX` time: subsequent INSERT/UPDATE/DELETE on the base
//! relation never reached the index, so any query that pushed a predicate
//! down through the planner would silently miss the new rows. This module
//! is the write-path hook that closes that gap.
//!
//! # Design — Deep Module
//!
//! Two public entry points only:
//!
//! ```ignore
//! IncrementalIndexMaintainer::delta(pre, post, row_id, indexes) -> Vec<IndexDeltaOp>
//! IncrementalIndexMaintainer::apply(backend, ops)               -> Result<(), String>
//! ```
//!
//! Everything else (key encoding, the per-index per-column diff loop) is
//! private. `delta` is a pure function — it can be unit-tested without
//! touching disk or any manager singleton — while `apply` is the only
//! place that mutates the live secondary indexes, and it routes every op
//! through a [`SecondaryIndexBackend`] trait that the runtime's
//! `IndexStore` implements (see
//! `crate::runtime::index_store::IndexStore`'s `impl
//! SecondaryIndexBackend`). No parallel write paths — every variant
//! routes through the same insert/remove/update primitive the existing
//! `index_entity_*` callers already use.
//!
//! # Crash recovery
//!
//! No on-disk format changes (the issue is explicit about this). The
//! WAL replay path already calls into the same row-level
//! INSERT/UPDATE/DELETE primitives that wire through this maintainer,
//! so replaying the WAL on startup re-derives the correct in-memory
//! index state by re-running each apply. If the process crashes
//! mid-batch, the next replay computes the same deltas and yields the
//! same final state — `apply` is idempotent for INSERT/DELETE
//! (duplicate inserts coalesce on the (key, id) pair, deletes for
//! missing keys are a no-op) and `Update` is just `Delete(old) +
//! Insert(new)`, both idempotent.
//!
//! # API note vs. issue sketch
//!
//! Issue #160 sketches `apply(ops) -> Result<()>`. We pass `&impl
//! SecondaryIndexBackend` as well — there is no global index registry
//! singleton in this crate, so `apply` needs an explicit handle. The
//! deviation is documented here and in the function doc.

use std::sync::Arc;

use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::types::Value;
use crate::storage::unified::entity::EntityId;

/// Index method classification, mirrored locally so this deep module
/// has no dependency on `runtime::index_store`. Callers that already
/// have a `runtime::index_store::IndexMethodKind` can convert via
/// `From`/`Into` (see the impl block at the bottom of
/// `runtime/index_store.rs` once the wiring lands).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexMethodKind {
    Hash,
    Bitmap,
    Spatial,
    BTree,
}

/// One incremental change to a single secondary index.
///
/// The maintainer emits these as a flat stream so callers can audit,
/// log, or batch-apply. Variants line up with the three primitive
/// operations a secondary index supports: `Insert` (new key→row
/// association), `Delete` (drop a key→row association), and `Update`
/// (re-key a row from `old_key` to `new_key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexDeltaOp {
    /// Add `row_id` under `key` in the index named `index`.
    Insert {
        index: SecondaryIndexHandle,
        key: Vec<u8>,
        row_id: EntityId,
    },
    /// Move `row_id` from `old_key` to `new_key` in `index`.
    Update {
        index: SecondaryIndexHandle,
        old_key: Vec<u8>,
        new_key: Vec<u8>,
        row_id: EntityId,
    },
    /// Remove `row_id` from `key` in `index`.
    Delete {
        index: SecondaryIndexHandle,
        key: Vec<u8>,
        row_id: EntityId,
    },
}

/// Handle to a single secondary index registered in the runtime's
/// `IndexStore`.
///
/// Cheap to clone — interior strings are `Arc`-shared. The maintainer
/// borrows a slice of these to compute deltas; `apply` reads them back
/// to route each op into the right manager (hash / bitmap / btree /
/// spatial / bloom / context).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondaryIndexHandle {
    pub name: Arc<str>,
    pub collection: Arc<str>,
    pub columns: Arc<[Arc<str>]>,
    pub method: IndexMethodKind,
    pub unique: bool,
}

impl SecondaryIndexHandle {
    /// Construct a handle from owned strings.
    pub fn new(
        name: impl Into<Arc<str>>,
        collection: impl Into<Arc<str>>,
        columns: Vec<String>,
        method: IndexMethodKind,
        unique: bool,
    ) -> Self {
        Self {
            name: name.into(),
            collection: collection.into(),
            columns: columns.into_iter().map(Arc::<str>::from).collect(),
            method,
            unique,
        }
    }

    /// Return the leading column name. Single-column indexes use this;
    /// composite-BTree handles use [`Self::columns`] for the full tuple.
    pub fn leading_column(&self) -> Option<&str> {
        self.columns.first().map(|s| s.as_ref())
    }
}

/// Trait the live index registry implements so the maintainer can
/// route ops without depending on the `runtime::index_store` module
/// directly. The runtime's `IndexStore` implements this by delegating
/// to the same per-manager methods (`hash.insert`, `bitmap.remove`, …)
/// the legacy `index_entity_*` paths already use, so there is no
/// parallel write path.
///
/// Implementors should make every method idempotent — `apply` may be
/// invoked twice for the same op during WAL replay.
pub trait SecondaryIndexBackend {
    /// Insert `row_id` under `key` in the named index. May fail with
    /// a unique-constraint violation, missing-index error, etc.
    fn insert(
        &self,
        idx: &SecondaryIndexHandle,
        key: &[u8],
        row_id: EntityId,
    ) -> Result<(), String>;

    /// Remove the `(key, row_id)` association from the named index.
    /// Missing index / missing key are non-fatal.
    fn remove(&self, idx: &SecondaryIndexHandle, key: &[u8], row_id: EntityId);
}

/// Public entry point for incremental index maintenance.
///
/// Stateless — every method is associated and pure (with the
/// exception of `apply`, which threads through a backend handle).
pub struct IncrementalIndexMaintainer;

impl IncrementalIndexMaintainer {
    /// Compute the index-delta stream for a single row mutation.
    ///
    /// `pre` is the row image **before** the write (None for INSERT).
    /// `post` is the row image **after** the write (None for DELETE).
    /// `row_id` is the entity-id of the affected row.
    /// `indexes` is the slice of secondary indexes registered on the
    /// affected collection.
    ///
    /// The function only allocates ops for indexes whose covered
    /// column actually changed value (or transitioned in/out of the
    /// row). Touching a non-indexed column produces zero ops.
    pub fn delta(
        pre: Option<&UnifiedRecord>,
        post: Option<&UnifiedRecord>,
        row_id: EntityId,
        indexes: &[SecondaryIndexHandle],
    ) -> Vec<IndexDeltaOp> {
        let mut ops = Vec::new();
        for idx in indexes {
            // Composite BTree (multi-column tuple) — collapse to a
            // single Insert/Delete/Update op keyed on the concatenated
            // tuple. This matches `composite_entity_update`'s behaviour.
            if matches!(idx.method, IndexMethodKind::BTree) && idx.columns.len() > 1 {
                let pre_key = pre.and_then(|r| composite_key(r, &idx.columns));
                let post_key = post.and_then(|r| composite_key(r, &idx.columns));
                emit_for_pair(&mut ops, idx, row_id, pre_key, post_key);
                continue;
            }

            let Some(col) = idx.leading_column() else {
                continue;
            };
            let pre_key = pre.and_then(|r| r.get(col)).map(value_to_bytes);
            let post_key = post.and_then(|r| r.get(col)).map(value_to_bytes);
            emit_for_pair(&mut ops, idx, row_id, pre_key, post_key);
        }
        ops
    }

    /// Apply a previously-computed op stream against the live index
    /// registry via the [`SecondaryIndexBackend`] trait.
    ///
    /// Idempotent — replaying the same op set is a no-op.
    pub fn apply(
        backend: &dyn SecondaryIndexBackend,
        ops: Vec<IndexDeltaOp>,
    ) -> Result<(), String> {
        for op in ops {
            apply_one(backend, op)?;
        }
        Ok(())
    }
}

// --------------------------------------------------------------------
// Internals (private)
// --------------------------------------------------------------------

/// Emit the right delta variant for a (pre_key, post_key) pair.
fn emit_for_pair(
    ops: &mut Vec<IndexDeltaOp>,
    idx: &SecondaryIndexHandle,
    row_id: EntityId,
    pre_key: Option<Vec<u8>>,
    post_key: Option<Vec<u8>>,
) {
    match (pre_key, post_key) {
        (None, None) => {} // not present before or after — nothing to do.
        (None, Some(k)) => ops.push(IndexDeltaOp::Insert {
            index: idx.clone(),
            key: k,
            row_id,
        }),
        (Some(k), None) => ops.push(IndexDeltaOp::Delete {
            index: idx.clone(),
            key: k,
            row_id,
        }),
        (Some(old), Some(new)) if old == new => {
            // unchanged → skip; this is the optimisation that turns
            // "UPDATE all rows SET unrelated_col = ..." into zero
            // index work.
        }
        (Some(old), Some(new)) => ops.push(IndexDeltaOp::Update {
            index: idx.clone(),
            old_key: old,
            new_key: new,
            row_id,
        }),
    }
}

/// Concatenate a composite tuple key for a multi-column BTree index.
/// Returns None if any required column is missing from the record —
/// matches the existing `composite_entity_*` behaviour, which only
/// indexes the row when every component is present.
fn composite_key(rec: &UnifiedRecord, columns: &[Arc<str>]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(columns.len() * 16);
    for col in columns {
        let v = rec.get(col)?;
        let bytes = value_to_bytes(v);
        // Length-prefix each component so `(a, b)` ≠ `(ab, "")`.
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    Some(out)
}

/// Encode a `Value` to the canonical index-key byte form. Mirrors
/// `runtime::index_store::value_to_bytes` so handles and ops compare
/// equal across both write paths.
fn value_to_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::Text(s) => s.as_bytes().to_vec(),
        Value::Integer(n) => n.to_le_bytes().to_vec(),
        Value::UnsignedInteger(n) => n.to_le_bytes().to_vec(),
        Value::Float(n) => n.to_le_bytes().to_vec(),
        Value::Boolean(b) => vec![*b as u8],
        Value::Null => Vec::new(),
        _ => format!("{:?}", value).into_bytes(),
    }
}

/// Route one op through the backend trait — every variant terminates
/// in a single `insert` or `remove` call (Update = remove(old) +
/// insert(new)).
fn apply_one(backend: &dyn SecondaryIndexBackend, op: IndexDeltaOp) -> Result<(), String> {
    match op {
        IndexDeltaOp::Insert { index, key, row_id } => backend.insert(&index, &key, row_id),
        IndexDeltaOp::Delete { index, key, row_id } => {
            backend.remove(&index, &key, row_id);
            Ok(())
        }
        IndexDeltaOp::Update {
            index,
            old_key,
            new_key,
            row_id,
        } => {
            backend.remove(&index, &old_key, row_id);
            backend.insert(&index, &new_key, row_id)
        }
    }
}

// ====================================================================
// Tests
// ====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::unified::UnifiedRecord;
    use crate::storage::schema::types::Value;
    use crate::storage::unified::entity::EntityId;
    use parking_lot::Mutex;
    use std::collections::HashMap;

    fn rec_one(col: &str, v: Value) -> UnifiedRecord {
        let mut r = UnifiedRecord::new();
        r.set(col, v);
        r
    }

    fn handle_hash(name: &str, collection: &str, col: &str) -> SecondaryIndexHandle {
        SecondaryIndexHandle::new(
            name,
            collection,
            vec![col.to_string()],
            IndexMethodKind::Hash,
            false,
        )
    }

    /// Test backend — an in-memory model of a hash index. Lets us
    /// exercise the maintainer end-to-end without dragging in the
    /// runtime crate.
    #[derive(Default)]
    struct MockBackend {
        // (collection, name, key) -> Vec<row_id>
        store: Mutex<HashMap<(String, String, Vec<u8>), Vec<EntityId>>>,
        unique: Mutex<HashMap<(String, String), bool>>,
    }

    impl MockBackend {
        fn lookup(&self, collection: &str, name: &str, key: &[u8]) -> Vec<EntityId> {
            let store = self.store.lock();
            store
                .get(&(collection.to_string(), name.to_string(), key.to_vec()))
                .cloned()
                .unwrap_or_default()
        }
        fn register(&self, collection: &str, name: &str, unique: bool) {
            self.unique
                .lock()
                .insert((collection.to_string(), name.to_string()), unique);
        }
    }

    impl SecondaryIndexBackend for MockBackend {
        fn insert(
            &self,
            idx: &SecondaryIndexHandle,
            key: &[u8],
            row_id: EntityId,
        ) -> Result<(), String> {
            let mut store = self.store.lock();
            let entry = store
                .entry((
                    idx.collection.to_string(),
                    idx.name.to_string(),
                    key.to_vec(),
                ))
                .or_default();
            if idx.unique && !entry.is_empty() && !entry.contains(&row_id) {
                return Err("duplicate key in unique index".to_string());
            }
            if !entry.contains(&row_id) {
                entry.push(row_id);
            }
            Ok(())
        }

        fn remove(&self, idx: &SecondaryIndexHandle, key: &[u8], row_id: EntityId) {
            let mut store = self.store.lock();
            let k = (
                idx.collection.to_string(),
                idx.name.to_string(),
                key.to_vec(),
            );
            if let Some(ids) = store.get_mut(&k) {
                ids.retain(|id| *id != row_id);
                if ids.is_empty() {
                    store.remove(&k);
                }
            }
        }
    }

    #[test]
    fn delta_insert_emits_insert_op() {
        let post = rec_one("email", Value::text("a@x".to_string()));
        let h = handle_hash("idx_email", "users", "email");
        let ops = IncrementalIndexMaintainer::delta(None, Some(&post), EntityId::new(1), &[h]);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], IndexDeltaOp::Insert { .. }));
    }

    #[test]
    fn delta_delete_emits_delete_op() {
        let pre = rec_one("email", Value::text("a@x".to_string()));
        let h = handle_hash("idx_email", "users", "email");
        let ops = IncrementalIndexMaintainer::delta(Some(&pre), None, EntityId::new(1), &[h]);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], IndexDeltaOp::Delete { .. }));
    }

    #[test]
    fn delta_update_changed_emits_update_op() {
        let pre = rec_one("email", Value::text("a@x".to_string()));
        let post = rec_one("email", Value::text("b@x".to_string()));
        let h = handle_hash("idx_email", "users", "email");
        let ops = IncrementalIndexMaintainer::delta(
            Some(&pre),
            Some(&post),
            EntityId::new(1),
            &[h],
        );
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], IndexDeltaOp::Update { .. }));
    }

    #[test]
    fn delta_update_unchanged_emits_nothing() {
        let pre = rec_one("email", Value::text("a@x".to_string()));
        let post = rec_one("email", Value::text("a@x".to_string()));
        let h = handle_hash("idx_email", "users", "email");
        let ops = IncrementalIndexMaintainer::delta(
            Some(&pre),
            Some(&post),
            EntityId::new(1),
            &[h],
        );
        assert!(ops.is_empty());
    }

    #[test]
    fn delta_unindexed_column_change_emits_nothing() {
        let mut pre = UnifiedRecord::new();
        pre.set("email", Value::text("a@x".to_string()));
        pre.set("name", Value::text("alice".to_string()));
        let mut post = UnifiedRecord::new();
        post.set("email", Value::text("a@x".to_string()));
        post.set("name", Value::text("ALICE".to_string()));
        let h = handle_hash("idx_email", "users", "email");
        let ops = IncrementalIndexMaintainer::delta(
            Some(&pre),
            Some(&post),
            EntityId::new(1),
            &[h],
        );
        assert!(ops.is_empty());
    }

    #[test]
    fn delta_composite_btree_changes_when_any_column_changes() {
        let mut pre = UnifiedRecord::new();
        pre.set("city", Value::text("nyc".to_string()));
        pre.set("age", Value::Integer(30));
        let mut post = UnifiedRecord::new();
        post.set("city", Value::text("nyc".to_string()));
        post.set("age", Value::Integer(31));
        let h = SecondaryIndexHandle::new(
            "idx_cc",
            "users",
            vec!["city".to_string(), "age".to_string()],
            IndexMethodKind::BTree,
            false,
        );
        let ops = IncrementalIndexMaintainer::delta(
            Some(&pre),
            Some(&post),
            EntityId::new(1),
            &[h],
        );
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], IndexDeltaOp::Update { .. }));
    }

    #[test]
    fn apply_insert_then_lookup_finds_row() {
        let backend = MockBackend::default();
        backend.register("users", "idx_email", false);
        let post = rec_one("email", Value::text("a@x".to_string()));
        let h = handle_hash("idx_email", "users", "email");
        let ops = IncrementalIndexMaintainer::delta(
            None,
            Some(&post),
            EntityId::new(7),
            &[h.clone()],
        );
        IncrementalIndexMaintainer::apply(&backend, ops).unwrap();
        assert_eq!(
            backend.lookup("users", "idx_email", b"a@x"),
            vec![EntityId::new(7)]
        );
    }

    #[test]
    fn apply_update_moves_row_to_new_key() {
        let backend = MockBackend::default();
        backend.register("users", "idx_email", false);
        let h = handle_hash("idx_email", "users", "email");
        let pre = rec_one("email", Value::text("a@x".to_string()));
        let post = rec_one("email", Value::text("b@x".to_string()));

        let ops = IncrementalIndexMaintainer::delta(
            None,
            Some(&pre),
            EntityId::new(3),
            &[h.clone()],
        );
        IncrementalIndexMaintainer::apply(&backend, ops).unwrap();

        let ops = IncrementalIndexMaintainer::delta(
            Some(&pre),
            Some(&post),
            EntityId::new(3),
            &[h],
        );
        IncrementalIndexMaintainer::apply(&backend, ops).unwrap();

        assert!(backend.lookup("users", "idx_email", b"a@x").is_empty());
        assert_eq!(
            backend.lookup("users", "idx_email", b"b@x"),
            vec![EntityId::new(3)]
        );
    }

    #[test]
    fn apply_delete_clears_key() {
        let backend = MockBackend::default();
        backend.register("users", "idx_email", false);
        let h = handle_hash("idx_email", "users", "email");
        let post = rec_one("email", Value::text("a@x".to_string()));

        let ops = IncrementalIndexMaintainer::delta(
            None,
            Some(&post),
            EntityId::new(11),
            &[h.clone()],
        );
        IncrementalIndexMaintainer::apply(&backend, ops).unwrap();

        let ops = IncrementalIndexMaintainer::delta(
            Some(&post),
            None,
            EntityId::new(11),
            &[h],
        );
        IncrementalIndexMaintainer::apply(&backend, ops).unwrap();

        assert!(backend.lookup("users", "idx_email", b"a@x").is_empty());
    }

    #[test]
    fn apply_is_idempotent_for_inserts() {
        let backend = MockBackend::default();
        backend.register("users", "idx_email", false);
        let h = handle_hash("idx_email", "users", "email");
        let post = rec_one("email", Value::text("a@x".to_string()));
        for _ in 0..3 {
            let ops = IncrementalIndexMaintainer::delta(
                None,
                Some(&post),
                EntityId::new(1),
                &[h.clone()],
            );
            IncrementalIndexMaintainer::apply(&backend, ops).unwrap();
        }
        assert_eq!(
            backend.lookup("users", "idx_email", b"a@x"),
            vec![EntityId::new(1)]
        );
    }

    #[test]
    fn apply_delete_for_missing_key_is_noop() {
        let backend = MockBackend::default();
        backend.register("users", "idx_email", false);
        let h = handle_hash("idx_email", "users", "email");
        let pre = rec_one("email", Value::text("a@x".to_string()));
        let ops = IncrementalIndexMaintainer::delta(
            Some(&pre),
            None,
            EntityId::new(99),
            &[h],
        );
        // Should not error.
        IncrementalIndexMaintainer::apply(&backend, ops).unwrap();
    }

    // ----------------------------------------------------------------
    // Property test (issue #160 acceptance criterion)
    //
    // Random workload of 1..200 mixed INSERT / UPDATE / DELETE ops
    // against a table with 1 secondary hash index. After every step,
    // the live hash index must agree with a from-scratch re-scan of
    // the source-of-truth row map.
    // ----------------------------------------------------------------
    use proptest::prelude::*;

    /// Source-of-truth model: row_id → email.
    type Truth = HashMap<u64, String>;

    #[derive(Debug, Clone)]
    enum Step {
        Insert { id: u64, email: String },
        Update { id: u64, email: String },
        Delete { id: u64 },
    }

    fn arb_email() -> impl Strategy<Value = String> {
        // Tiny domain — forces collisions on the (key, multiple ids) path.
        prop_oneof![
            Just("a@x".to_string()),
            Just("b@x".to_string()),
            Just("c@x".to_string()),
            Just("d@x".to_string()),
        ]
    }

    fn arb_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            (0u64..16, arb_email()).prop_map(|(id, email)| Step::Insert { id, email }),
            (0u64..16, arb_email()).prop_map(|(id, email)| Step::Update { id, email }),
            (0u64..16).prop_map(|id| Step::Delete { id }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 256,
            .. ProptestConfig::default()
        })]

        #[test]
        fn index_stays_consistent_under_random_workload(
            workload in proptest::collection::vec(arb_step(), 1..200)
        ) {
            let backend = MockBackend::default();
            backend.register("users", "idx_email", false);
            let h = handle_hash("idx_email", "users", "email");
            let mut truth: Truth = HashMap::new();

            for step in &workload {
                match step {
                    Step::Insert { id, email } => {
                        if truth.contains_key(id) {
                            // Treat as no-op insert (matches "INSERT IGNORE" / dedupe).
                            continue;
                        }
                        let post = rec_one("email", Value::text(email.clone()));
                        let ops = IncrementalIndexMaintainer::delta(
                            None,
                            Some(&post),
                            EntityId::new(*id),
                            &[h.clone()],
                        );
                        IncrementalIndexMaintainer::apply(&backend, ops).unwrap();
                        truth.insert(*id, email.clone());
                    }
                    Step::Update { id, email } => {
                        if let Some(old) = truth.get(id).cloned() {
                            let pre = rec_one("email", Value::text(old));
                            let post = rec_one("email", Value::text(email.clone()));
                            let ops = IncrementalIndexMaintainer::delta(
                                Some(&pre),
                                Some(&post),
                                EntityId::new(*id),
                                &[h.clone()],
                            );
                            IncrementalIndexMaintainer::apply(&backend, ops).unwrap();
                            truth.insert(*id, email.clone());
                        }
                    }
                    Step::Delete { id } => {
                        if let Some(old) = truth.remove(id) {
                            let pre = rec_one("email", Value::text(old));
                            let ops = IncrementalIndexMaintainer::delta(
                                Some(&pre),
                                None,
                                EntityId::new(*id),
                                &[h.clone()],
                            );
                            IncrementalIndexMaintainer::apply(&backend, ops).unwrap();
                        }
                    }
                }

                // After every step, re-derive the expected (key → ids)
                // map from the truth table and assert the live index
                // matches.
                let mut expected: HashMap<String, Vec<u64>> = HashMap::new();
                for (id, email) in &truth {
                    expected.entry(email.clone()).or_default().push(*id);
                }
                for (email, mut ids) in expected {
                    ids.sort();
                    let mut got: Vec<u64> = backend
                        .lookup("users", "idx_email", email.as_bytes())
                        .iter()
                        .map(|e| e.raw())
                        .collect();
                    got.sort();
                    prop_assert_eq!(got, ids,
                        "index disagrees with truth on key '{}' after step {:?}",
                        email, step);
                }
            }
        }
    }

    /// Replay invariant: applying the same op stream twice yields the
    /// same final state as applying it once. Models WAL replay after
    /// a crash mid-batch.
    #[test]
    fn replay_yields_same_state() {
        let backend1 = MockBackend::default();
        backend1.register("users", "idx_email", false);
        let backend2 = MockBackend::default();
        backend2.register("users", "idx_email", false);
        let h = handle_hash("idx_email", "users", "email");

        let post = rec_one("email", Value::text("a@x".to_string()));
        let ops = IncrementalIndexMaintainer::delta(
            None,
            Some(&post),
            EntityId::new(5),
            &[h.clone()],
        );

        IncrementalIndexMaintainer::apply(&backend1, ops.clone()).unwrap();
        IncrementalIndexMaintainer::apply(&backend2, ops.clone()).unwrap();
        IncrementalIndexMaintainer::apply(&backend2, ops).unwrap();

        assert_eq!(
            backend1.lookup("users", "idx_email", b"a@x"),
            backend2.lookup("users", "idx_email", b"a@x"),
        );
    }
}
