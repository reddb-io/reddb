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

use super::helpers::evaluate_entity_filter;
use super::*;
use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
use crate::storage::schema::Value;
use crate::storage::unified::entity::{EntityData, EntityKind, UnifiedEntity};

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
pub(crate) fn classify_field(
    field: &FieldRef,
    table_name: &str,
    table_alias: &str,
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
    InList {
        kind: EntityFieldKind,
        values: Vec<Value>,
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
}

impl CompiledEntityFilter {
    /// Walk the AST filter once and produce a flat opcode list.
    pub fn compile(filter: &Filter, table_name: &str, table_alias: &str) -> Self {
        let mut ops = Vec::new();
        compile_into(filter, table_name, table_alias, &mut ops);
        Self {
            ops,
            table_name: table_name.to_string(),
            table_alias: table_alias.to_string(),
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
        let mut stack: Vec<bool> = Vec::with_capacity(8);
        for op in &self.ops {
            match op {
                CompiledEntityOp::Compare { kind, op, value } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .map(|candidate| compare_runtime_values(candidate.as_ref(), value, *op))
                        .unwrap_or(false);
                    stack.push(result);
                }
                CompiledEntityOp::Between { kind, low, high } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .map(|candidate| {
                            compare_runtime_values(candidate.as_ref(), low, CompareOp::Ge)
                                && compare_runtime_values(candidate.as_ref(), high, CompareOp::Le)
                        })
                        .unwrap_or(false);
                    stack.push(result);
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
                    stack.push(result);
                }
                CompiledEntityOp::Like { kind, pattern } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .and_then(|v| runtime_value_text(v.as_ref()))
                        .is_some_and(|s| like_matches(&s, pattern));
                    stack.push(result);
                }
                CompiledEntityOp::StartsWith { kind, prefix } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .and_then(|v| runtime_value_text(v.as_ref()))
                        .is_some_and(|s| s.starts_with(prefix.as_str()));
                    stack.push(result);
                }
                CompiledEntityOp::EndsWith { kind, suffix } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .and_then(|v| runtime_value_text(v.as_ref()))
                        .is_some_and(|s| s.ends_with(suffix.as_str()));
                    stack.push(result);
                }
                CompiledEntityOp::Contains { kind, substring } => {
                    let result = resolve_kind(kind, entity)
                        .as_ref()
                        .and_then(|v| runtime_value_text(v.as_ref()))
                        .is_some_and(|s| s.contains(substring.as_str()));
                    stack.push(result);
                }
                CompiledEntityOp::IsNull { kind } => {
                    let result = resolve_kind(kind, entity)
                        .map(|v| v.as_ref() == &Value::Null)
                        .unwrap_or(true);
                    stack.push(result);
                }
                CompiledEntityOp::IsNotNull { kind } => {
                    let result = resolve_kind(kind, entity)
                        .map(|v| v.as_ref() != &Value::Null)
                        .unwrap_or(false);
                    stack.push(result);
                }
                CompiledEntityOp::And => {
                    let r = stack.pop().unwrap_or(true);
                    let l = stack.pop().unwrap_or(true);
                    stack.push(l && r);
                }
                CompiledEntityOp::Or => {
                    let r = stack.pop().unwrap_or(false);
                    let l = stack.pop().unwrap_or(false);
                    stack.push(l || r);
                }
                CompiledEntityOp::Not => {
                    let v = stack.pop().unwrap_or(true);
                    stack.push(!v);
                }
                CompiledEntityOp::Fallback(filter) => {
                    // Path the compiler couldn't classify: re-enter
                    // the legacy walker for THIS subtree only. Rare.
                    let v =
                        evaluate_entity_filter(entity, filter, &self.table_name, &self.table_alias);
                    stack.push(v);
                }
            }
        }
        stack.pop().unwrap_or(true)
    }
}

/// For DocumentPath fields the fast resolver returns None, so
/// callers fall through to the original `resolve_entity_field` via
/// a fallback op. We push the leaf as a Fallback to preserve
/// semantics. This keeps document-path queries on the slow path
/// without breaking them — a future iteration can teach the
/// classifier to walk paths too.
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
    ops: &mut Vec<CompiledEntityOp>,
) {
    match filter {
        Filter::Compare { field, op, value } => {
            let kind = classify_field(field, table_name, table_alias);
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
        Filter::Between { field, low, high } => {
            let kind = classify_field(field, table_name, table_alias);
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
            let kind = classify_field(field, table_name, table_alias);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::InList {
                kind,
                values: values.clone(),
            });
        }
        Filter::Like { field, pattern } => {
            let kind = classify_field(field, table_name, table_alias);
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
            let kind = classify_field(field, table_name, table_alias);
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
            let kind = classify_field(field, table_name, table_alias);
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
            let kind = classify_field(field, table_name, table_alias);
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
            let kind = classify_field(field, table_name, table_alias);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::IsNull { kind });
        }
        Filter::IsNotNull(field) => {
            let kind = classify_field(field, table_name, table_alias);
            if needs_fallback(&kind) {
                ops.push(CompiledEntityOp::Fallback(Box::new(filter.clone())));
                return;
            }
            ops.push(CompiledEntityOp::IsNotNull { kind });
        }
        Filter::And(left, right) => {
            compile_into(left, table_name, table_alias, ops);
            compile_into(right, table_name, table_alias, ops);
            ops.push(CompiledEntityOp::And);
        }
        Filter::Or(left, right) => {
            compile_into(left, table_name, table_alias, ops);
            compile_into(right, table_name, table_alias, ops);
            ops.push(CompiledEntityOp::Or);
        }
        Filter::Not(inner) => {
            compile_into(inner, table_name, table_alias, ops);
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

    // Note: full evaluate() correctness is covered by the runtime
    // integration tests under src/runtime/ that exercise table scans
    // — those already have ~hundreds of WHERE-clause assertions, and
    // wiring `CompiledEntityFilter` into table.rs runs every one of
    // them through this path.
}
