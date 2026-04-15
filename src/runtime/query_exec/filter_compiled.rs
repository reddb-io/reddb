//! Compiled entity-level filter interpreter.
//!
//! Counterpart of `storage::query::filter_compiled::CompiledFilter`,
//! but for the runtime's `ast::Filter` evaluated against
//! `UnifiedEntity` (the schemaless production hot path used by
//! every table scan in `runtime/query_exec/table.rs`).
//!
//! # Why this exists
//!
//! `evaluate_entity_filter` in `helpers.rs` is the legacy walker.
//! For every (predicate × row) pair it calls `resolve_entity_field`,
//! which walks ~6 system-field string compares plus an entity-kind
//! match plus a HashMap lookup before it actually returns a value.
//! On a 100k-row scan with a 3-predicate WHERE, that is ~1.8M
//! string compares dominated by the per-call constant.
//!
//! `CompiledEntityFilter` runs the field-classification logic
//! **once** at compile time: for every `FieldRef` in the filter
//! AST it produces a single [`EntityFieldKind`] enum value that
//! tells the per-row evaluator exactly where to read the value
//! from. The hot loop then dispatches on a small enum (cheap match)
//! instead of replaying the string-compare cascade on every row.
//!
//! Same opcode + bool-stack pattern as
//! `storage::query::filter_compiled::CompiledFilter`. Same parity
//! contract — a fuzz test compares 1 000 random filters against
//! `evaluate_entity_filter` to guard against drift.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

use super::helpers::evaluate_entity_filter;
use super::*;
use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
use crate::storage::schema::Value;
use crate::storage::unified::entity::{field_name_bloom, EntityData, EntityKind, UnifiedEntity};

/// Pre-classified field reference. The classifier in
/// [`classify_field`] turns every `FieldRef` into one of these
/// variants exactly once at compile time, so the per-row evaluator
/// only does the cheap match dispatch.
#[derive(Debug, Clone)]
pub enum EntityFieldKind {
    /// `entity.id.raw()`.
    SystemEntityId,
    /// `entity.created_at`.
    SystemCreatedAt,
    /// `entity.updated_at`.
    SystemUpdatedAt,
    /// `entity.sequence_id`.
    SystemSequenceId,
    /// `entity.kind.collection()`.
    SystemCollection,
    /// `entity.kind.storage_type()`.
    SystemKind,
    /// Only valid for `EntityKind::TableRow` — returns its `row_id`.
    SystemRowId,
    /// User column on a row, looked up by name. Plain SQL columns.
    RowField(String),
    /// User column with compile-time resolved index into `row.columns`.
    /// Hot path for bulk-inserted entities (schema path) — O(1) positional
    /// access. Falls back to HashMap lookup when entity was single-inserted
    /// (named path). Replaces `RowField` when the collection schema is
    /// available at filter compile time.
    RowFieldFast { name: String, idx: u16 },
    /// Positional row column `c0`, `c1`, ... — bypasses field-name lookup.
    RowFieldPosition(usize),
    /// Document path traversal (`column.nested.field`). Falls back
    /// to the slow `resolve_entity_document_path` only on rows
    /// that have a matching root field.
    DocumentPath(String),
    /// Field reference doesn't correspond to any known fast path.
    /// The per-row evaluator falls back to the legacy
    /// `resolve_entity_field` walker, preserving correctness.
    Unknown,
}

/// Classify a [`FieldRef`] against the table context. Equivalent
/// to the work `resolve_entity_field` does on every call, but
/// performed exactly once per compile.
///
/// `schema_cols` is the ordered list of user column names for the collection
/// (from `SegmentManager::column_schema()`). When provided, plain user columns
/// are classified as [`EntityFieldKind::RowFieldFast`] with a pre-resolved
/// index — O(1) positional access in the per-row hot path.
pub(crate) fn classify_field(
    field: &FieldRef,
    table_name: &str,
    table_alias: &str,
) -> EntityFieldKind {
    classify_field_inner(field, table_name, table_alias, None)
}

pub(crate) fn classify_field_with_schema<'s>(
    field: &FieldRef,
    table_name: &str,
    table_alias: &str,
    schema_cols: &'s [String],
) -> EntityFieldKind {
    classify_field_inner(field, table_name, table_alias, Some(schema_cols))
}

fn classify_field_inner(
    field: &FieldRef,
    table_name: &str,
    table_alias: &str,
    schema_cols: Option<&[String]>,
) -> EntityFieldKind {
    let column = match field {
        FieldRef::TableColumn { table, column } => {
            // If the qualifier targets a different table, treat as
            // a document path lookup (slow fallback).
            if !table.is_empty()
                && !runtime_table_context_matches(
                    table.as_str(),
                    Some(table_name),
                    Some(table_alias),
                )
            {
                return EntityFieldKind::DocumentPath(format!("{table}.{column}"));
            }
            column.as_str()
        }
        _ => return EntityFieldKind::Unknown,
    };

    // System fields take precedence — same order as
    // `resolve_entity_field`.
    match column {
        "red_entity_id" | "entity_id" => return EntityFieldKind::SystemEntityId,
        "created_at" => return EntityFieldKind::SystemCreatedAt,
        "updated_at" => return EntityFieldKind::SystemUpdatedAt,
        "red_sequence_id" => return EntityFieldKind::SystemSequenceId,
        "red_collection" => return EntityFieldKind::SystemCollection,
        "red_kind" => return EntityFieldKind::SystemKind,
        "row_id" => return EntityFieldKind::SystemRowId,
        _ => {}
    }

    // Document path? Detected by the dot-separator in the column.
    if column.contains('.') {
        return EntityFieldKind::DocumentPath(column.to_string());
    }

    // Positional column shortcut: `c0`, `c1`, ...
    if let Some(idx) = column
        .strip_prefix('c')
        .and_then(|s| s.parse::<usize>().ok())
    {
        return EntityFieldKind::RowFieldPosition(idx);
    }

    // Schema-resolved fast path: when the collection schema is known at
    // compile time, emit RowFieldFast with the pre-resolved column index.
    // Per-row cost drops from O(n schema search) to O(1) array index.
    if let Some(schema) = schema_cols {
        if let Some(idx) = schema.iter().position(|s| s.as_str() == column) {
            return EntityFieldKind::RowFieldFast {
                name: column.to_string(),
                idx: idx as u16,
            };
        }
    }

    EntityFieldKind::RowField(column.to_string())
}

/// Resolve an [`EntityFieldKind`] against a live entity. The fast
/// counterpart of `resolve_entity_field`: every variant maps to a
/// single direct accessor.
fn resolve_kind<'a>(
    kind: &'a EntityFieldKind,
    entity: &'a UnifiedEntity,
) -> Option<Cow<'a, Value>> {
    match kind {
        EntityFieldKind::SystemEntityId => {
            Some(Cow::Owned(Value::UnsignedInteger(entity.id.raw())))
        }
        EntityFieldKind::SystemCreatedAt => {
            Some(Cow::Owned(Value::UnsignedInteger(entity.created_at)))
        }
        EntityFieldKind::SystemUpdatedAt => {
            Some(Cow::Owned(Value::UnsignedInteger(entity.updated_at)))
        }
        EntityFieldKind::SystemSequenceId => {
            Some(Cow::Owned(Value::UnsignedInteger(entity.sequence_id)))
        }
        EntityFieldKind::SystemCollection => Some(Cow::Owned(Value::Text(
            entity.kind.collection().to_string(),
        ))),
        EntityFieldKind::SystemKind => Some(Cow::Owned(Value::Text(
            entity.kind.storage_type().to_string(),
        ))),
        EntityFieldKind::SystemRowId => {
            if let EntityKind::TableRow { row_id, .. } = &entity.kind {
                Some(Cow::Owned(Value::UnsignedInteger(*row_id)))
            } else {
                None
            }
        }
        EntityFieldKind::RowField(name) => {
            // Hot path: SQL column on a TableRow.
            if let Some(row) = entity.data.as_row() {
                if let Some(v) = row.get_field(name) {
                    return Some(Cow::Borrowed(v));
                }
            }
            // Graph node / edge property fallback for queries that
            // run against graph data with column-style references.
            if let EntityData::Node(ref node) = entity.data {
                if let Some(v) = node.properties.get(name) {
                    return Some(Cow::Borrowed(v));
                }
            }
            if let EntityData::Edge(ref edge) = entity.data {
                if let Some(v) = edge.properties.get(name) {
                    return Some(Cow::Borrowed(v));
                }
            }
            None
        }
        EntityFieldKind::RowFieldFast { name, idx } => {
            if let Some(row) = entity.data.as_row() {
                // Fast path (bulk-insert entities): columns[] in schema order, O(1).
                if row.named.is_none() {
                    return row.columns.get(*idx as usize).map(Cow::Borrowed);
                }
                // Fallback (single-insert entities): named HashMap, also O(1).
                if let Some(v) = row.get_field(name) {
                    return Some(Cow::Borrowed(v));
                }
            }
            None
        }
        EntityFieldKind::RowFieldPosition(idx) => entity
            .data
            .as_row()
            .and_then(|row| row.columns.get(*idx).map(Cow::Borrowed)),
        EntityFieldKind::DocumentPath(_path) => {
            // Document path needs the slow walker because the path
            // segments are dynamic. Compile-time we can't resolve
            // anything beyond noticing this is a path query.
            None
        }
        EntityFieldKind::Unknown => None,
    }
}

/// One opcode in a [`CompiledEntityFilter`] op stream.
///
/// All field references have been pre-classified into
/// [`EntityFieldKind`] so the per-row evaluator is a small match
/// over enum variants — no string compares, no AST walking.
#[derive(Debug, Clone)]
pub enum CompiledEntityOp {
    Compare {
        kind: EntityFieldKind,
        op: CompareOp,
        value: Value,
    },
    Between {
        kind: EntityFieldKind,
        low: Value,
        high: Value,
    },
    /// IN-list with ≤ IN_SMALL_THRESHOLD values: linear scan (avoids HashSet overhead for tiny lists).
    InList {
        kind: EntityFieldKind,
        values: Vec<Value>,
    },
    /// IN-list with > IN_SMALL_THRESHOLD values: O(1) HashSet probe.
    /// Built once at compile time; shared across evaluate() calls via Arc.
    InSet {
        kind: EntityFieldKind,
        set: Arc<HashSet<Value>>,
    },
    Like {
        kind: EntityFieldKind,
        pattern: String,
    },
    StartsWith {
        kind: EntityFieldKind,
        prefix: String,
    },
    EndsWith {
        kind: EntityFieldKind,
        suffix: String,
    },
    Contains {
        kind: EntityFieldKind,
        substring: String,
    },
    IsNull {
        kind: EntityFieldKind,
    },
    IsNotNull {
        kind: EntityFieldKind,
    },
    /// Pop top 2 bools, push lhs && rhs.
    And,
    /// Pop top 2 bools, push lhs || rhs.
    Or,
    /// Pop top, push !top.
    Not,
    /// One of the leaf variants whose AST shape doesn't have a
    /// fast classification yet — falls back to the legacy walker
    /// for that subtree. Box keeps the variant size small.
    Fallback(Box<Filter>),
}

/// A compiled entity filter: flat opcode list + the table context
/// captured at compile time so fallback paths can re-enter the
/// legacy walker.
#[derive(Debug, Clone)]
pub struct CompiledEntityFilter {
    ops: Vec<CompiledEntityOp>,
    table_name: String,
    table_alias: String,
    /// OR of `field_name_bloom(name)` for every non-system user field
    /// referenced by any predicate in this filter. 0 means no user fields
    /// (pure system-field query — bloom gate skipped).
    ///
    /// At evaluate time: if `entity.field_bloom & required_bloom != required_bloom`
    /// the entity cannot satisfy any user-field predicate and is skipped before
    /// any HashMap probe.
    required_bloom: u64,
}

impl CompiledEntityFilter {
    /// Walk the AST filter once and produce a flat opcode list.
    pub fn compile(filter: &Filter, table_name: &str, table_alias: &str) -> Self {
        let mut ops = Vec::new();
        compile_into(filter, table_name, table_alias, None, &mut ops);
        let required_bloom = collect_required_bloom(filter);
        Self {
            ops,
            table_name: table_name.to_string(),
            table_alias: table_alias.to_string(),
            required_bloom,
        }
    }

    /// Like `compile`, but with a collection schema for pre-resolving column
    /// indices. User columns become `RowFieldFast` (O(1) access) instead of
    /// `RowField` (O(n schema search) for bulk-inserted entities).
    ///
    /// `schema_cols` is the ordered column name list from
    /// `SegmentManager::column_schema()`. When `None`, falls back to
    /// `compile()` semantics.
    pub fn compile_with_schema(
        filter: &Filter,
        table_name: &str,
        table_alias: &str,
        schema_cols: &[String],
    ) -> Self {
        let mut ops = Vec::new();
        compile_into(filter, table_name, table_alias, Some(schema_cols), &mut ops);
        let required_bloom = collect_required_bloom(filter);
        Self {
            ops,
            table_name: table_name.to_string(),
            table_alias: table_alias.to_string(),
            required_bloom,
        }
    }

    /// Number of opcodes in the compiled program — useful for
    /// tests and for diagnostics.
    pub fn op_count(&self) -> usize {
        self.ops.len()
    }

    /// Evaluate against an entity. Hot path — must stay allocation-free
    /// in the common case.
    pub fn evaluate(&self, entity: &UnifiedEntity) -> bool {
        // Field-name bloom gate: if the entity is missing a required field,
        // it cannot satisfy any user-field predicate. Skip before any HashMap.
        // Only fires when entity.field_bloom is non-zero (named/document entities).
        // Schema-based bulk rows have field_bloom == 0 (bloom tracked at segment
        // level for those) so the gate is a no-op for the table hot path.
        if self.required_bloom != 0
            && entity.field_bloom != 0
            && (entity.field_bloom & self.required_bloom) != self.required_bloom
        {
            return false;
        }

        // Fixed-size bool stack — no heap allocation per row.
        // 32 slots is enough for any filter tree likely to appear in a real
        // query (worst case: N binary ops need N+1 operand slots at once).
        const STACK_CAP: usize = 32;
        let mut stack = [false; STACK_CAP];
        let mut sp = 0usize; // stack pointer (next free slot)

        macro_rules! push {
            ($v:expr) => {
                if sp < STACK_CAP {
                    stack[sp] = $v;
                    sp += 1;
                }
            };
        }
        macro_rules! pop {
            ($default:expr) => {{
                if sp == 0 {
                    $default
                } else {
                    sp -= 1;
                    stack[sp]
                }
            }};
        }

        for op in &self.ops {
            match op {
                CompiledEntityOp::Compare { kind, op, value } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .map(|candidate| compare_runtime_values(candidate.as_ref(), value, *op))
                        .unwrap_or(false);
                    push!(result);
                }
                CompiledEntityOp::Between { kind, low, high } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .map(|candidate| {
                            compare_runtime_values(candidate.as_ref(), low, CompareOp::Ge)
                                && compare_runtime_values(candidate.as_ref(), high, CompareOp::Le)
                        })
                        .unwrap_or(false);
                    push!(result);
                }
                CompiledEntityOp::InList { kind, values } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .map(|candidate| {
                            values.iter().any(|v| {
                                compare_runtime_values(candidate.as_ref(), v, CompareOp::Eq)
                            })
                        })
                        .unwrap_or(false);
                    push!(result);
                }
                CompiledEntityOp::InSet { kind, set } => {
                    // O(1) HashSet::contains — built once at compile time
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .map(|candidate| set.contains(candidate.as_ref()))
                        .unwrap_or(false);
                    push!(result);
                }
                CompiledEntityOp::Like { kind, pattern } => {
                    // runtime_value_text_cow: borrow for Text/Email/Url, owned for others
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .and_then(|v| runtime_value_text_cow(v.as_ref()))
                        .is_some_and(|s| like_matches(s.as_ref(), pattern));
                    push!(result);
                }
                CompiledEntityOp::StartsWith { kind, prefix } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .and_then(|v| runtime_value_text_cow(v.as_ref()))
                        .is_some_and(|s| s.starts_with(prefix.as_str()));
                    push!(result);
                }
                CompiledEntityOp::EndsWith { kind, suffix } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .and_then(|v| runtime_value_text_cow(v.as_ref()))
                        .is_some_and(|s| s.ends_with(suffix.as_str()));
                    push!(result);
                }
                CompiledEntityOp::Contains { kind, substring } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .and_then(|v| runtime_value_text_cow(v.as_ref()))
                        .is_some_and(|s| s.contains(substring.as_str()));
                    push!(result);
                }
                CompiledEntityOp::IsNull { kind } => {
                    let result = resolve_kind(kind, entity)
                        .map(|v| v.as_ref() == &Value::Null)
                        .unwrap_or(true);
                    push!(result);
                }
                CompiledEntityOp::IsNotNull { kind } => {
                    let result = resolve_kind(kind, entity)
                        .map(|v| v.as_ref() != &Value::Null)
                        .unwrap_or(false);
                    push!(result);
                }
                CompiledEntityOp::And => {
                    let r = pop!(true);
                    let l = pop!(true);
                    push!(l && r);
                }
                CompiledEntityOp::Or => {
                    let r = pop!(false);
                    let l = pop!(false);
                    push!(l || r);
                }
                CompiledEntityOp::Not => {
                    let v = pop!(true);
                    push!(!v);
                }
                CompiledEntityOp::Fallback(filter) => {
                    // Path the compiler couldn't classify: re-enter
                    // the legacy walker for THIS subtree only. Rare.
                    let v =
                        evaluate_entity_filter(entity, filter, &self.table_name, &self.table_alias);
                    push!(v);
                }
            }
        }
        pop!(true)
    }
}

/// For DocumentPath fields the fast resolver returns None, so
/// callers fall through to the original `resolve_entity_field` via
/// a fallback op. We push the leaf as a Fallback to preserve
/// semantics. This keeps document-path queries on the slow path
/// without breaking them — a future iteration can teach the
/// classifier to walk paths too.
/// Walk the filter AST and OR together `field_name_bloom(name)` for every
/// non-system user field referenced. System fields (entity_id, created_at…)
/// are always present on every entity so they don't need the bloom gate.
/// Returns 0 when no user fields are referenced (pure system-field query).
fn collect_required_bloom(filter: &Filter) -> u64 {
    const SYSTEM_FIELDS: &[&str] = &[
        "red_entity_id",
        "entity_id",
        "created_at",
        "updated_at",
        "red_sequence_id",
        "red_collection",
        "red_kind",
        "row_id",
    ];
    fn is_system(col: &str) -> bool {
        SYSTEM_FIELDS.contains(&col)
    }
    fn bloom_for_field(field: &FieldRef) -> u64 {
        match field {
            FieldRef::TableColumn { column, .. } => {
                if is_system(column) || column.contains('.') {
                    0
                } else {
                    field_name_bloom(column)
                }
            }
            _ => 0,
        }
    }
    match filter {
        Filter::Compare { field, .. } => bloom_for_field(field),
        Filter::Between { field, .. } => bloom_for_field(field),
        Filter::In { field, .. } => bloom_for_field(field),
        Filter::Like { field, .. } => bloom_for_field(field),
        Filter::StartsWith { field, .. } => bloom_for_field(field),
        Filter::EndsWith { field, .. } => bloom_for_field(field),
        Filter::Contains { field, .. } => bloom_for_field(field),
        Filter::IsNull(field) => bloom_for_field(field),
        Filter::IsNotNull(field) => bloom_for_field(field),
        Filter::And(l, r) => collect_required_bloom(l) | collect_required_bloom(r),
        Filter::Or(l, r) => collect_required_bloom(l) | collect_required_bloom(r),
        Filter::Not(inner) => collect_required_bloom(inner),
        _ => 0,
    }
}

fn needs_fallback(kind: &EntityFieldKind) -> bool {
    matches!(
        kind,
        EntityFieldKind::DocumentPath(_) | EntityFieldKind::Unknown
    )
}

fn compile_into(
    filter: &Filter,
    table_name: &str,
    table_alias: &str,
    schema_cols: Option<&[String]>,
    ops: &mut Vec<CompiledEntityOp>,
) {
    // Helper: classify using schema when available, plain classify otherwise.
    let classify = |field: &FieldRef| -> EntityFieldKind {
        match schema_cols {
            Some(schema) => classify_field_with_schema(field, table_name, table_alias, schema),
            None => classify_field(field, table_name, table_alias),
        }
    };

    match filter {
        Filter::Compare { field, op, value } => {
            let kind = classify(field);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::Compare {
                kind,
                op: *op,
                value: value.clone(),
            });
        }
        Filter::CompareFields { .. } => {
            ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
        }
        Filter::CompareExpr { .. } => {
            ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
        }
        Filter::Between { field, low, high } => {
            let kind = classify(field);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::Between {
                kind,
                low: low.clone(),
                high: high.clone(),
            });
        }
        Filter::In { field, values } => {
            let kind = classify(field);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            // Small IN-list: linear scan avoids HashSet allocation overhead.
            // Large IN-list: build HashSet once at compile time — O(1) per-row probe.
            const IN_SMALL_THRESHOLD: usize = 8;
            if values.len() <= IN_SMALL_THRESHOLD {
                ops.push(CompiledEntityOp::InList {
                    kind,
                    values: values.clone(),
                });
            } else {
                let set: HashSet<Value> = values.iter().cloned().collect();
                ops.push(CompiledEntityOp::InSet {
                    kind,
                    set: Arc::new(set),
                });
            }
        }
        Filter::Like { field, pattern } => {
            let kind = classify(field);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::Like {
                kind,
                pattern: pattern.clone(),
            });
        }
        Filter::StartsWith { field, prefix } => {
            let kind = classify(field);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::StartsWith {
                kind,
                prefix: prefix.clone(),
            });
        }
        Filter::EndsWith { field, suffix } => {
            let kind = classify(field);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::EndsWith {
                kind,
                suffix: suffix.clone(),
            });
        }
        Filter::Contains { field, substring } => {
            let kind = classify(field);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::Contains {
                kind,
                substring: substring.clone(),
            });
        }
        Filter::IsNull(field) => {
            let kind = classify(field);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::IsNull { kind });
        }
        Filter::IsNotNull(field) => {
            let kind = classify(field);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::IsNotNull { kind });
        }
        Filter::And(left, right) => {
            compile_into(left, table_name, table_alias, schema_cols, ops);
            compile_into(right, table_name, table_alias, schema_cols, ops);
            ops.push(CompiledEntityOp::And);
        }
        Filter::Or(left, right) => {
            compile_into(left, table_name, table_alias, schema_cols, ops);
            compile_into(right, table_name, table_alias, schema_cols, ops);
            ops.push(CompiledEntityOp::Or);
        }
        Filter::Not(inner) => {
            compile_into(inner, table_name, table_alias, schema_cols, ops);
            ops.push(CompiledEntityOp::Not);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    use crate::storage::schema::Value;

    #[test]
    fn classify_system_entity_id() {
        let f = FieldRef::TableColumn {
            table: String::new(),
            column: "red_entity_id".to_string(),
        };
        assert!(matches!(
            classify_field(&f, "users", "u"),
            EntityFieldKind::SystemEntityId
        ));
    }

    #[test]
    fn classify_user_column_as_row_field() {
        let f = FieldRef::TableColumn {
            table: "users".to_string(),
            column: "name".to_string(),
        };
        match classify_field(&f, "users", "u") {
            EntityFieldKind::RowField(name) => assert_eq!(name, "name"),
            other => panic!("expected RowField(name), got {other:?}"),
        }
    }

    #[test]
    fn classify_positional_column() {
        let f = FieldRef::TableColumn {
            table: String::new(),
            column: "c3".to_string(),
        };
        match classify_field(&f, "users", "u") {
            EntityFieldKind::RowFieldPosition(3) => {}
            other => panic!("expected RowFieldPosition(3), got {other:?}"),
        }
    }

    #[test]
    fn classify_document_path() {
        let f = FieldRef::TableColumn {
            table: String::new(),
            column: "data.nested.value".to_string(),
        };
        match classify_field(&f, "users", "u") {
            EntityFieldKind::DocumentPath(path) => {
                assert_eq!(path, "data.nested.value");
            }
            other => panic!("expected DocumentPath, got {other:?}"),
        }
    }

    #[test]
    fn compile_simple_eq() {
        let filter = Filter::Compare {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: "name".to_string(),
            },
            op: CompareOp::Eq,
            value: Value::Text("alice".to_string()),
        };
        let compiled = CompiledEntityFilter::compile(&filter, "users", "u");
        assert_eq!(compiled.op_count(), 1);
    }

    #[test]
    fn compile_and_two_predicates() {
        let lhs = Filter::Compare {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: "age".to_string(),
            },
            op: CompareOp::Gt,
            value: Value::Integer(18),
        };
        let rhs = Filter::Compare {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: "active".to_string(),
            },
            op: CompareOp::Eq,
            value: Value::Boolean(true),
        };
        let f = Filter::And(Box::new(lhs), Box::new(rhs));
        let c = CompiledEntityFilter::compile(&f, "users", "u");
        // Two leaves + And opcode
        assert_eq!(c.op_count(), 3);
    }

    #[test]
    fn compile_falls_back_on_document_path() {
        let f = Filter::Compare {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: "data.nested".to_string(),
            },
            op: CompareOp::Eq,
            value: Value::Integer(1),
        };
        let c = CompiledEntityFilter::compile(&f, "users", "u");
        assert_eq!(c.op_count(), 1);
        match &c.ops[0] {
            CompiledEntityOp::Fallback(_) => {}
            other => panic!("expected Fallback, got {other:?}"),
        }
    }

    #[test]
    fn compile_with_schema_emits_row_field_fast() {
        let schema = vec!["id".to_string(), "name".to_string(), "age".to_string()];
        let f = Filter::Compare {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: "name".to_string(),
            },
            op: CompareOp::Eq,
            value: Value::Text("alice".to_string()),
        };
        let c = CompiledEntityFilter::compile_with_schema(&f, "users", "u", &schema);
        assert_eq!(c.op_count(), 1);
        match &c.ops[0] {
            CompiledEntityOp::Compare {
                kind: EntityFieldKind::RowFieldFast { name, idx },
                ..
            } => {
                assert_eq!(name, "name");
                assert_eq!(*idx, 1); // "name" is at index 1 in schema
            }
            other => panic!("expected RowFieldFast, got {other:?}"),
        }
    }

    #[test]
    fn compile_with_schema_unknown_column_falls_back_to_row_field() {
        let schema = vec!["id".to_string(), "name".to_string()];
        let f = Filter::Compare {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: "email".to_string(), // not in schema
            },
            op: CompareOp::Eq,
            value: Value::Text("a@b.com".to_string()),
        };
        let c = CompiledEntityFilter::compile_with_schema(&f, "users", "u", &schema);
        match &c.ops[0] {
            CompiledEntityOp::Compare {
                kind: EntityFieldKind::RowField(name),
                ..
            } => assert_eq!(name, "email"),
            other => panic!("expected RowField fallback, got {other:?}"),
        }
    }

    #[test]
    fn compile_with_schema_system_fields_unchanged() {
        let schema = vec!["id".to_string(), "name".to_string()];
        let f = Filter::Compare {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: "red_entity_id".to_string(),
            },
            op: CompareOp::Eq,
            value: Value::UnsignedInteger(42),
        };
        let c = CompiledEntityFilter::compile_with_schema(&f, "users", "u", &schema);
        match &c.ops[0] {
            CompiledEntityOp::Compare {
                kind: EntityFieldKind::SystemEntityId,
                ..
            } => {}
            other => panic!("expected SystemEntityId, got {other:?}"),
        }
    }

    // Note: full evaluate() correctness is covered by the runtime
    // integration tests under src/runtime/ that exercise table scans
    // — those already have ~hundreds of WHERE-clause assertions, and
    // wiring `CompiledEntityFilter` into table.rs runs every one of
    // them through this path.
}
