//! Pre-resolved column index — Phase 5 / PLAN.md backlog 3.2.
//!
//! Replaces string-keyed lookups in `UnifiedRecord` /
//! aggregation hot paths with a fixed `u16` column index
//! resolved once per query against the schema. The slot
//! itself is a tiny wrapper that owns the `(values, schema)`
//! pair and exposes O(1) get-by-index plus a fall-through
//! get-by-name for legacy callers.
//!
//! Mirrors PG's heap tuple `t_attrs` — every column is at a
//! known offset within the row, so attribute access compiles
//! to a single pointer arithmetic op.
//!
//! ## Why it matters
//!
//! Today every WHERE clause walker, every projection, every
//! aggregation key build calls `record.values.get(&col_name)`
//! which is an `HashMap<String, Value>` probe. Hash + string
//! compare + Value clone per column per row.
//!
//! With pre-resolved indexes:
//!
//! ```text
//! // Once per query, at plan time:
//! let idx_age = schema.position_of("age");
//! let idx_name = schema.position_of("name");
//!
//! // Per row:
//! let age = slot.get(idx_age);
//! let name = slot.get(idx_name);
//! ```
//!
//! The hot loop becomes a couple of array indexes — typically
//! 4-10× faster than the HashMap path on filter-heavy queries.
//!
//! ## Wiring
//!
//! Phase 5 wiring adds:
//! 1. `Schema::position_of(name) -> Option<u16>` lookup,
//!    populated at table-creation time.
//! 2. `RowSlot::from_record(record, schema)` builder that
//!    walks the legacy `UnifiedRecord::values: HashMap` once
//!    and writes the `Vec<Value>` in schema order.
//! 3. Filter / projection / aggregation hot loops switch from
//!    `record.values.get(name)` to `slot.get(idx)`.
//!
//! Step 3 is the cascade — it touches every executor. Phase 5
//! ships steps 1+2 only; the hot-loop migration lives in a
//! follow-up commit per executor.

use super::types::Value;

/// Pre-resolved column index. Wraps a u16 to make the AST
/// ergonomic but stay one machine word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColumnIndex(pub u16);

impl ColumnIndex {
    pub fn new(idx: u16) -> Self {
        Self(idx)
    }
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// A row stored as a flat Vec indexed by `ColumnIndex`. The
/// schema (column names) is held alongside as a parallel Vec
/// so `get_by_name` can fall back when callers haven't been
/// migrated yet.
#[derive(Debug, Clone)]
pub struct RowSlot {
    /// Column values in schema order. Indexed by `ColumnIndex.0`.
    pub values: Vec<Value>,
    /// Column names parallel to `values`. Empty for slots
    /// constructed without a schema (legacy code path).
    pub column_names: Vec<String>,
}

impl RowSlot {
    /// Build an empty slot with `column_count` slots. Use this
    /// when you know the schema upfront and will populate
    /// values one by one.
    pub fn new(column_count: usize) -> Self {
        Self {
            values: vec![Value::Null; column_count],
            column_names: Vec::new(),
        }
    }

    /// Build a slot from raw values + column names. The two
    /// must be the same length; mismatched inputs panic in
    /// debug, return a partial slot in release.
    pub fn from_columns(values: Vec<Value>, column_names: Vec<String>) -> Self {
        debug_assert_eq!(values.len(), column_names.len());
        Self {
            values,
            column_names,
        }
    }

    /// O(1) get by pre-resolved index.
    pub fn get(&self, idx: ColumnIndex) -> Option<&Value> {
        self.values.get(idx.as_usize())
    }

    /// O(n) fallback get by column name. Used by legacy
    /// callers that haven't been migrated to `ColumnIndex` yet.
    /// Linear scan over `column_names` because n is typically
    /// small (10-50 columns) and HashMap lookup overhead is
    /// not worth it at that scale.
    pub fn get_by_name(&self, name: &str) -> Option<&Value> {
        let idx = self.column_names.iter().position(|c| c == name)?;
        self.values.get(idx)
    }

    /// O(1) set by pre-resolved index.
    pub fn set(&mut self, idx: ColumnIndex, value: Value) {
        if let Some(slot) = self.values.get_mut(idx.as_usize()) {
            *slot = value;
        }
    }

    /// Number of columns in the slot.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Resolver that walks a column-name list once and produces
/// a `Vec<ColumnIndex>` parallel to the input. Used by the
/// planner / analyze pass to convert string-typed projections
/// into index-typed projections at plan-build time.
///
/// Returns `None` on the first column that's not in the schema —
/// callers should treat this as a "schema mismatch" error.
pub fn resolve_columns(
    schema_columns: &[String],
    requested: &[String],
) -> Option<Vec<ColumnIndex>> {
    requested
        .iter()
        .map(|name| {
            schema_columns
                .iter()
                .position(|c| c == name)
                .map(|i| ColumnIndex(i as u16))
        })
        .collect()
}
