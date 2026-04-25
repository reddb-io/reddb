//! Zero-copy scan path for MSG_QUERY_BINARY.
//!
//! Serves a narrow class of SELECT queries directly from segment
//! entities to the wire response buffer, bypassing the allocation of
//! intermediate `UnifiedRecord`s that `runtime.execute_query` →
//! `encode_result` would otherwise build (~4200 records × 1k queries
//! on the `select_range` bench, where each record clones a `Vec<Value>`
//! and bumps an `Arc<Vec<String>>` schema).
//!
//! The fast path is a pure optimisation: callers fall back to the
//! standard path whenever `try_handle_query_binary_direct` returns
//! None. Shape constraints are deliberately tight — only eligible
//! cases go direct; everything else returns None unchanged.

use std::sync::Arc;

use crate::runtime::mvcc::entity_visible_under_current_snapshot;
use crate::runtime::query_exec::{
    extract_entity_id_from_filter, try_hash_eq_lookup, try_sorted_index_lookup,
    CompiledEntityFilter,
};
use crate::runtime::RedDBRuntime;
use crate::storage::query::ast::{
    Expr, FieldRef, Filter, QueryExpr, SelectItem, TableQuery, TableSource,
};
use crate::storage::query::sql_lowering::effective_table_filter;
use crate::storage::schema::{value_to_canonical_key, CanonicalKey, Value};
use crate::storage::unified::{EntityData, EntityId, RowData, UnifiedEntity};

use super::protocol::{
    encode_column_name, encode_value, write_frame_header, MSG_RESULT, VAL_NULL, VAL_U64,
};

/// Try to serve a binary SELECT via the zero-copy scan path.
///
/// Returns `Some(wire_response)` when the query matches the fast-path
/// shape and was executed; returns `None` to signal the caller should
/// fall back to the standard `execute_query` + `encode_result` path.
pub(super) fn try_handle_query_binary_direct(runtime: &RedDBRuntime, sql: &str) -> Option<Vec<u8>> {
    // Cheap prefix gate. Avoid parse for anything not a plain SELECT
    // (WITHIN, EXPLAIN, SET, BEGIN, …).
    let trimmed = sql.trim_start();
    if trimmed.len() < 6 {
        return None;
    }
    if !trimmed.as_bytes()[..6].eq_ignore_ascii_case(b"SELECT") {
        return None;
    }

    // Micro-fast-path for the benchmark's point lookup shape:
    // `SELECT id, ... FROM t WHERE id = N`. This avoids the full SQL
    // parser on high-QPS indexed point reads. The recognizer is deliberately
    // tiny; any alias/function/operator/extra clause falls back below.
    if let Some(parsed) = parse_simple_hash_eq_select(trimmed) {
        if let Some(resp) = execute_simple_hash_eq_select(runtime, &parsed) {
            return Some(resp);
        }
    }
    if let Some(parsed) = parse_simple_between_select(trimmed) {
        if let Some(resp) = execute_simple_between_select(runtime, &parsed) {
            return Some(resp);
        }
    }
    if let Some(parsed) = parse_simple_ordered_complex_select(trimmed) {
        if let Some(resp) = execute_simple_ordered_complex_select(runtime, &parsed) {
            return Some(resp);
        }
    }
    if let Some(parsed) = parse_simple_text_eq_int_gt_select(trimmed) {
        if let Some(resp) = execute_simple_text_eq_int_gt_select(runtime, &parsed) {
            return Some(resp);
        }
    }

    // Full parse. Cost ~50µs; amortised by the record allocations
    // skipped on hit. On miss the caller re-parses via `handle_query`.
    let expr = crate::storage::query::modes::parse_multi(sql).ok()?;
    let tq = match &expr {
        QueryExpr::Table(tq) => tq,
        _ => return None,
    };

    if !is_shape_direct_eligible(tq) {
        return None;
    }

    execute_direct_scan(runtime, tq)
}

/// True when `filter` is a single leaf that `try_sorted_index_lookup`
/// can resolve without needing post-filter evaluation. For leaves the
/// index scan's LIMIT pushdown returns exactly the rows we want;
/// for composite `And`/`Or`/`Not` we need every row the index returns
/// to clear the full predicate before we can count it towards LIMIT.
fn filter_is_single_indexable_leaf(filter: &Filter) -> bool {
    matches!(
        filter,
        Filter::Between { .. } | Filter::Compare { .. } | Filter::In { .. }
    )
}

/// Detects the `And(leaf_a, leaf_b)` shape where **both** leaves resolve to
/// an exact index result (sorted range or hash-eq). When this holds,
/// `try_sorted_index_lookup` intersects the two id sets, and the intersection
/// is a complete answer to the full filter — so the outer LIMIT can be pushed
/// through to the intersector without losing rows.

pub(super) fn is_shape_direct_eligible(tq: &TableQuery) -> bool {
    if let Some(source) = &tq.source {
        if !matches!(source, TableSource::Name(_)) {
            return false;
        }
    }
    if !tq.group_by.is_empty() || !tq.group_by_exprs.is_empty() {
        return false;
    }
    if tq.having.is_some() || tq.having_expr.is_some() {
        return false;
    }
    if !tq.order_by.is_empty() || tq.expand.is_some() {
        return false;
    }
    if tq.offset.is_some() {
        return false;
    }
    if tq.select_items.is_empty() {
        return false;
    }

    for item in &tq.select_items {
        match item {
            SelectItem::Wildcard => {}
            SelectItem::Expr { expr, alias: _ } => {
                if !matches!(expr, Expr::Column { .. }) {
                    return false;
                }
            }
        }
    }

    true
}

pub(super) fn execute_direct_scan(runtime: &RedDBRuntime, tq: &TableQuery) -> Option<Vec<u8>> {
    let effective_filter = effective_table_filter(tq);

    // Defer to execute_query's point-lookup path when WHERE reduces
    // to `red_entity_id = N` — that path handles MVCC + error shapes
    // we don't want to re-implement here.
    if extract_entity_id_from_filter(&effective_filter).is_some() {
        return None;
    }

    let db = runtime.db();
    let store = db.store();
    let manager = store.get_collection(&tq.table)?;
    let limit = tq.limit.map(|l| l as usize);
    let hard_limit = limit.unwrap_or(usize::MAX);
    let schema_columns = manager.column_schema();
    let schema_slice = schema_columns.as_ref().map(|schema| schema.as_slice());
    let pre_resolved_cols =
        schema_slice.map(|schema| resolve_wire_columns_from_query_schema(tq, schema));

    let mut body: Vec<u8> = Vec::with_capacity(estimate_direct_response_capacity(
        tq,
        schema_slice,
        limit.unwrap_or(0),
    ));
    let mut header_nrows_pos: usize = 0;
    let mut cols: Option<Vec<WireColumn>> = None;
    let mut row_count: u32 = 0;

    // Inline row-emit macro — avoids a shared `FnMut` closure whose
    // state captures force every call through an indirect dispatch,
    // measurably slower on the select_filtered hot loop (AND filter
    // with single-indexed leaf can iterate tens of thousands of ids).
    macro_rules! emit_one {
        ($entity:expr) => {{
            let entity: &UnifiedEntity = $entity;
            if !entity.data.is_row() || !entity_visible_under_current_snapshot(entity) {
                // skip
            } else if let EntityData::Row(ref row) = entity.data {
                if cols.is_none() {
                    let resolved = pre_resolved_cols
                        .clone()
                        .unwrap_or_else(|| resolve_wire_columns(tq, row));
                    body.extend_from_slice(&(resolved.len() as u16).to_le_bytes());
                    for col in &resolved {
                        encode_column_name(&mut body, col.name.as_ref());
                    }
                    header_nrows_pos = body.len();
                    body.extend_from_slice(&[0u8; 4]);
                    cols = Some(resolved);
                }
                if let Some(cols_ref) = cols.as_ref() {
                    for c in cols_ref {
                        encode_entity_wire_value(&mut body, entity, row, c);
                    }
                    row_count += 1;
                }
            }
        }};
    }

    if let Some(filter) = effective_filter.as_ref() {
        // ── Filtered / indexed path ───────────────────────────────
        let idx_store = runtime.index_store_ref();
        // LIMIT pushdown is only safe when the filter is a single
        // indexable leaf. For composite `And(_, _)` we can't push the
        // outer LIMIT to `try_sorted_index_lookup`, because that
        // propagates it to each leaf's index scan — truncating the
        // candidate sets before the intersection and dropping matches.
        // (See `fast_path_and_with_limit_returns_same_row_count_as_standard`
        //  for the regression-guard.)
        let index_limit = if filter_is_single_indexable_leaf(filter) {
            limit
        } else {
            None
        };
        let ids = try_sorted_index_lookup(filter, tq.table.as_str(), idx_store, index_limit)
            .or_else(|| try_hash_eq_lookup(filter, tq.table.as_str(), idx_store))?;
        if ids.is_empty() {
            return encode_empty_direct_select(tq, schema_slice);
        }
        let target_capacity =
            estimate_direct_response_capacity(tq, schema_slice, ids.len().min(hard_limit));
        if target_capacity > body.capacity() {
            body.reserve(target_capacity - body.capacity());
        }
        let table_name = tq.table.as_str();
        let table_alias = tq.alias.as_deref().unwrap_or(table_name);
        let compiled_filter = match schema_slice {
            Some(schema) => {
                CompiledEntityFilter::compile_with_schema(filter, table_name, table_alias, schema)
            }
            None => CompiledEntityFilter::compile(filter, table_name, table_alias),
        };
        manager.for_each_id(&ids, |_, entity| {
            if (row_count as usize) >= hard_limit {
                return;
            }
            if !compiled_filter.evaluate(entity) {
                return;
            }
            emit_one!(entity);
        });
    } else {
        // ── Unfiltered LIMIT path ─────────────────────────────────
        // Require explicit LIMIT — we don't want to materialise an
        // unbounded scan here (the runtime's canonical path has the
        // parallel-scan branch for that).
        if limit.is_none() {
            return None;
        }
        manager.for_each_entity(|entity| {
            if (row_count as usize) >= hard_limit {
                return false;
            }
            emit_one!(entity);
            (row_count as usize) < hard_limit
        });
    }

    if cols.is_none() {
        return None;
    }

    body[header_nrows_pos..header_nrows_pos + 4].copy_from_slice(&row_count.to_le_bytes());

    let mut resp = Vec::with_capacity(5 + body.len());
    write_frame_header(&mut resp, MSG_RESULT, body.len() as u32);
    resp.extend_from_slice(&body);
    let _ = db;
    let _ = store;
    Some(resp)
}

struct SimpleHashEqSelect {
    table: String,
    columns: Vec<String>,
    filter_column: String,
    value: u64,
}

struct SimpleBetweenSelect {
    table: String,
    columns: Vec<String>,
    filter_column: String,
    low: i64,
    high: i64,
}

struct SimpleTextEqIntGtSelect {
    table: String,
    columns: Vec<String>,
    eq_column: String,
    eq_value: String,
    range_column: String,
    threshold: i64,
}

struct SimpleOrderedComplexSelect {
    table: String,
    columns: Vec<String>,
    eq_column: String,
    eq_value: String,
    range_column: String,
    low: i64,
    high: i64,
    float_column: String,
    threshold: f64,
    limit: Option<usize>,
}

struct SimpleSelectParts<'a> {
    table: &'a str,
    columns: Vec<String>,
    predicate: &'a str,
}

struct SimpleOrderedSelectParts<'a> {
    table: &'a str,
    columns: Vec<String>,
    predicate: &'a str,
    order_clause: &'a str,
}

const SIMPLE_INDEX_BREAK_EVEN_CAP: usize = 200_000;

fn parse_simple_projection_columns(projection: &str) -> Option<Vec<String>> {
    let mut columns = Vec::new();
    for raw in projection.split(',') {
        let col = raw.trim();
        if col.is_empty() || !is_simple_identifier(col) {
            return None;
        }
        columns.push(col.to_string());
    }
    if columns.is_empty() {
        return None;
    }
    Some(columns)
}

fn parse_simple_select_parts(sql: &str) -> Option<SimpleSelectParts<'_>> {
    let mut rest = strip_keyword(sql.trim(), "SELECT")?.trim_start();
    let from_pos = find_ascii_ci(rest, " FROM ")?;
    let projection = rest[..from_pos].trim();
    rest = rest[from_pos + " FROM ".len()..].trim_start();

    let where_pos = find_ascii_ci(rest, " WHERE ")?;
    let table = rest[..where_pos].trim();
    let predicate = rest[where_pos + " WHERE ".len()..].trim();

    if table.is_empty() || !is_simple_identifier(table) {
        return None;
    }

    Some(SimpleSelectParts {
        table,
        columns: parse_simple_projection_columns(projection)?,
        predicate,
    })
}

fn parse_simple_ordered_select_parts(sql: &str) -> Option<SimpleOrderedSelectParts<'_>> {
    let mut rest = strip_keyword(sql.trim(), "SELECT")?.trim_start();
    let from_pos = find_ascii_ci(rest, " FROM ")?;
    let projection = rest[..from_pos].trim();
    rest = rest[from_pos + " FROM ".len()..].trim_start();

    let where_pos = find_ascii_ci(rest, " WHERE ")?;
    let table = rest[..where_pos].trim();
    rest = rest[where_pos + " WHERE ".len()..].trim();

    let order_pos = find_ascii_ci(rest, " ORDER BY ")?;
    let predicate = rest[..order_pos].trim();
    let order_clause = rest[order_pos + " ORDER BY ".len()..].trim();

    if table.is_empty() || !is_simple_identifier(table) {
        return None;
    }

    Some(SimpleOrderedSelectParts {
        table,
        columns: parse_simple_projection_columns(projection)?,
        predicate,
        order_clause,
    })
}

fn parse_simple_hash_eq_select(sql: &str) -> Option<SimpleHashEqSelect> {
    let parts = parse_simple_select_parts(sql)?;
    let eq_pos = parts.predicate.find('=')?;
    let field = parts.predicate[..eq_pos].trim();
    let value_text = parts.predicate[eq_pos + 1..].trim();
    if field.is_empty() || !is_simple_identifier(field) {
        return None;
    }

    let value_text = value_text.strip_suffix(';').unwrap_or(value_text).trim();
    if value_text.is_empty() || !value_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    Some(SimpleHashEqSelect {
        table: parts.table.to_string(),
        columns: parts.columns,
        filter_column: field.to_string(),
        value: value_text.parse().ok()?,
    })
}

fn parse_simple_between_select(sql: &str) -> Option<SimpleBetweenSelect> {
    let parts = parse_simple_select_parts(sql)?;
    let between_pos = find_ascii_ci(parts.predicate, " BETWEEN ")?;
    let field = parts.predicate[..between_pos].trim();
    let bounds = parts.predicate[between_pos + " BETWEEN ".len()..].trim();
    if field.is_empty() || !is_simple_identifier(field) {
        return None;
    }

    let and_pos = find_ascii_ci(bounds, " AND ")?;
    let low_text = bounds[..and_pos].trim();
    let high_text = bounds[and_pos + " AND ".len()..].trim();
    let high_text = high_text.strip_suffix(';').unwrap_or(high_text).trim();
    let low = parse_i64_literal(low_text)?;
    let high = parse_i64_literal(high_text)?;
    if high < low {
        return None;
    }

    Some(SimpleBetweenSelect {
        table: parts.table.to_string(),
        columns: parts.columns,
        filter_column: field.to_string(),
        low,
        high,
    })
}

fn parse_simple_text_eq_int_gt_select(sql: &str) -> Option<SimpleTextEqIntGtSelect> {
    let parts = parse_simple_select_parts(sql)?;
    let and_pos = find_ascii_ci(parts.predicate, " AND ")?;
    let left = parts.predicate[..and_pos].trim();
    let right = parts.predicate[and_pos + " AND ".len()..].trim();

    let ((eq_column, eq_value), (range_column, threshold)) =
        match (parse_text_eq_clause(left), parse_int_gt_clause(right)) {
            (Some(eq), Some(gt)) => (eq, gt),
            _ => match (parse_text_eq_clause(right), parse_int_gt_clause(left)) {
                (Some(eq), Some(gt)) => (eq, gt),
                _ => return None,
            },
        };

    Some(SimpleTextEqIntGtSelect {
        table: parts.table.to_string(),
        columns: parts.columns,
        eq_column,
        eq_value,
        range_column,
        threshold,
    })
}

fn parse_simple_ordered_complex_select(sql: &str) -> Option<SimpleOrderedComplexSelect> {
    let parts = parse_simple_ordered_select_parts(sql)?;
    let (order_column, limit) = parse_order_by_desc_clause(parts.order_clause)?;

    let first_and = find_ascii_ci(parts.predicate, " AND ")?;
    let eq_clause = parts.predicate[..first_and].trim();
    let rest = parts.predicate[first_and + " AND ".len()..].trim();
    let (eq_column, eq_value) = parse_text_eq_clause(eq_clause)?;

    let between_pos = find_ascii_ci(rest, " BETWEEN ")?;
    let range_column = rest[..between_pos].trim();
    if range_column.is_empty() || !is_simple_identifier(range_column) {
        return None;
    }
    let bounds_and_float = rest[between_pos + " BETWEEN ".len()..].trim();
    let low_end = find_ascii_ci(bounds_and_float, " AND ")?;
    let low_text = bounds_and_float[..low_end].trim();
    let high_and_float = bounds_and_float[low_end + " AND ".len()..].trim();
    let high_end = find_ascii_ci(high_and_float, " AND ")?;
    let high_text = high_and_float[..high_end].trim();
    let float_clause = high_and_float[high_end + " AND ".len()..].trim();

    let low = parse_i64_literal(low_text)?;
    let high = parse_i64_literal(high_text)?;
    if high < low {
        return None;
    }

    let (float_column, threshold) = parse_float_gt_clause(float_clause)?;
    if order_column != float_column {
        return None;
    }

    Some(SimpleOrderedComplexSelect {
        table: parts.table.to_string(),
        columns: parts.columns,
        eq_column,
        eq_value,
        range_column: range_column.to_string(),
        low,
        high,
        float_column,
        threshold,
        limit,
    })
}

fn parse_text_eq_clause(clause: &str) -> Option<(String, String)> {
    let eq_pos = clause.find('=')?;
    let field = clause[..eq_pos].trim();
    let value_text = clause[eq_pos + 1..].trim();
    if field.is_empty() || !is_simple_identifier(field) {
        return None;
    }
    Some((field.to_string(), parse_sql_single_quoted_text(value_text)?))
}

fn parse_int_gt_clause(clause: &str) -> Option<(String, i64)> {
    let gt_pos = clause.find('>')?;
    let field = clause[..gt_pos].trim();
    let value_text = clause[gt_pos + 1..].trim();
    if field.is_empty() || !is_simple_identifier(field) {
        return None;
    }
    if value_text.starts_with('=') {
        return None;
    }
    let value_text = value_text.strip_suffix(';').unwrap_or(value_text).trim();
    Some((field.to_string(), parse_i64_literal(value_text)?))
}

fn parse_float_gt_clause(clause: &str) -> Option<(String, f64)> {
    let gt_pos = clause.find('>')?;
    let field = clause[..gt_pos].trim();
    let value_text = clause[gt_pos + 1..].trim();
    if field.is_empty() || !is_simple_identifier(field) {
        return None;
    }
    if value_text.starts_with('=') {
        return None;
    }
    let value = parse_f64_literal(value_text)?;
    Some((field.to_string(), value))
}

fn parse_order_by_desc_clause(clause: &str) -> Option<(String, Option<usize>)> {
    let mut order_text = clause.strip_suffix(';').unwrap_or(clause).trim();
    let mut limit = None;
    if let Some(limit_pos) = find_ascii_ci(order_text, " LIMIT ") {
        let limit_text = order_text[limit_pos + " LIMIT ".len()..].trim();
        limit = Some(parse_usize_literal(
            limit_text.strip_suffix(';').unwrap_or(limit_text),
        )?);
        order_text = order_text[..limit_pos].trim();
    }

    let mut parts = order_text.split_whitespace();
    let column = parts.next()?;
    let direction = parts.next()?;
    if parts.next().is_some() || !is_simple_identifier(column) {
        return None;
    }
    if !direction.eq_ignore_ascii_case("DESC") {
        return None;
    }
    Some((column.to_string(), limit))
}

fn parse_sql_single_quoted_text(text: &str) -> Option<String> {
    let text = text.strip_suffix(';').unwrap_or(text).trim();
    if !text.starts_with('\'') || !text.ends_with('\'') || text.len() < 2 {
        return None;
    }
    let inner = &text[1..text.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if chars.next_if_eq(&'\'').is_some() {
                out.push('\'');
            } else {
                return None;
            }
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

fn execute_simple_hash_eq_select(
    runtime: &RedDBRuntime,
    parsed: &SimpleHashEqSelect,
) -> Option<Vec<u8>> {
    let idx_store = runtime.index_store_ref();
    let idx = idx_store.find_index_for_column(&parsed.table, &parsed.filter_column)?;
    let key = parsed.value.to_le_bytes();
    let ids = idx_store
        .hash_lookup(&parsed.table, idx.hash_lookup_name().as_ref(), &key)
        .ok()?;
    if ids.is_empty() {
        return Some(encode_empty_simple_select(&parsed.columns));
    }

    execute_simple_indexed_select(runtime, &parsed.table, &parsed.columns, &ids, |schema| {
        SimpleRowPredicate::EqU64 {
            field: RowFieldAccessor::for_column(&parsed.filter_column, schema),
            expected: parsed.value,
        }
    })
}

fn execute_simple_between_select(
    runtime: &RedDBRuntime,
    parsed: &SimpleBetweenSelect,
) -> Option<Vec<u8>> {
    let idx_store = runtime.index_store_ref();
    if !idx_store
        .sorted
        .has_index(&parsed.table, &parsed.filter_column)
    {
        return None;
    }
    let low_value = Value::Integer(parsed.low);
    let high_value = Value::Integer(parsed.high);
    let low = value_to_canonical_key(&low_value)?;
    let high = value_to_canonical_key(&high_value)?;
    let ids = idx_store.sorted.range_lookup_limited(
        &parsed.table,
        &parsed.filter_column,
        low,
        high,
        SIMPLE_INDEX_BREAK_EVEN_CAP + 1,
    )?;
    if ids.is_empty() {
        return Some(encode_empty_simple_select(&parsed.columns));
    }
    if ids.len() > SIMPLE_INDEX_BREAK_EVEN_CAP {
        return None;
    }

    execute_simple_indexed_select(runtime, &parsed.table, &parsed.columns, &ids, |schema| {
        SimpleRowPredicate::BetweenI64 {
            field: RowFieldAccessor::for_column(&parsed.filter_column, schema),
            low: parsed.low,
            high: parsed.high,
        }
    })
}

fn execute_simple_text_eq_int_gt_select(
    runtime: &RedDBRuntime,
    parsed: &SimpleTextEqIntGtSelect,
) -> Option<Vec<u8>> {
    let idx_store = runtime.index_store_ref();
    let index_columns = vec![parsed.eq_column.clone(), parsed.range_column.clone()];
    if !idx_store
        .sorted
        .has_composite_index(&parsed.table, &index_columns)
    {
        return None;
    }

    let eq_key = value_to_canonical_key(&Value::text(parsed.eq_value.as_str()))?;
    let low_threshold = parsed.threshold.checked_add(1)?;
    let low_value = Value::Integer(low_threshold);
    let low = value_to_canonical_key(&low_value)?;
    let high = match &low {
        CanonicalKey::Signed(family, _) => CanonicalKey::Signed(*family, i64::MAX),
        _ => return None,
    };
    let ids = idx_store.sorted.composite_prefix_range_lookup(
        &parsed.table,
        &index_columns,
        &[eq_key],
        low,
        high,
        SIMPLE_INDEX_BREAK_EVEN_CAP + 1,
    )?;
    if ids.is_empty() {
        return Some(encode_empty_simple_select(&parsed.columns));
    }
    if ids.len() > SIMPLE_INDEX_BREAK_EVEN_CAP {
        return None;
    }

    execute_simple_indexed_select(runtime, &parsed.table, &parsed.columns, &ids, |schema| {
        SimpleRowPredicate::TextEqIntGt {
            text_field: RowFieldAccessor::for_column(&parsed.eq_column, schema),
            expected_text: parsed.eq_value.clone(),
            int_field: RowFieldAccessor::for_column(&parsed.range_column, schema),
            threshold: parsed.threshold,
        }
    })
}

struct OrderedCandidate {
    score: f64,
    tie_id: u64,
    entity_id: EntityId,
}

fn compare_ordered_candidate(
    left: &OrderedCandidate,
    right: &OrderedCandidate,
) -> std::cmp::Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(left.tie_id.cmp(&right.tie_id))
}

fn execute_simple_ordered_complex_select(
    runtime: &RedDBRuntime,
    parsed: &SimpleOrderedComplexSelect,
) -> Option<Vec<u8>> {
    let idx_store = runtime.index_store_ref();
    let index_columns = vec![parsed.eq_column.clone(), parsed.range_column.clone()];
    if !idx_store
        .sorted
        .has_composite_index(&parsed.table, &index_columns)
    {
        return None;
    }

    let eq_key = value_to_canonical_key(&Value::text(parsed.eq_value.as_str()))?;
    let low_value = Value::Integer(parsed.low);
    let high_value = Value::Integer(parsed.high);
    let low = value_to_canonical_key(&low_value)?;
    let high = value_to_canonical_key(&high_value)?;
    let ids = idx_store.sorted.composite_prefix_range_lookup(
        &parsed.table,
        &index_columns,
        &[eq_key],
        low,
        high,
        SIMPLE_INDEX_BREAK_EVEN_CAP + 1,
    )?;
    if ids.is_empty() {
        return Some(encode_empty_simple_select(&parsed.columns));
    }
    if ids.len() > SIMPLE_INDEX_BREAK_EVEN_CAP {
        return None;
    }

    let db = runtime.db();
    let store = db.store();
    let manager = store.get_collection(&parsed.table)?;
    let schema_columns = manager.column_schema();
    let schema_slice = schema_columns.as_ref().map(|schema| schema.as_slice());
    let city_field = RowFieldAccessor::for_column(&parsed.eq_column, schema_slice);
    let age_field = RowFieldAccessor::for_column(&parsed.range_column, schema_slice);
    let score_field = RowFieldAccessor::for_column(&parsed.float_column, schema_slice);
    let id_field = RowFieldAccessor::for_column("id", schema_slice);
    let mut candidates: Vec<OrderedCandidate> = Vec::new();

    manager.for_each_id(&ids, |_, entity| {
        if !entity.data.is_row() || !entity_visible_under_current_snapshot(entity) {
            return;
        }
        let EntityData::Row(ref row) = entity.data else {
            return;
        };
        if !value_text_eq(city_field.value(row), parsed.eq_value.as_str()) {
            return;
        }
        if !value_between_i64(age_field.value(row), parsed.low, parsed.high) {
            return;
        }
        let Some(score) = value_as_f64(score_field.value(row)) else {
            return;
        };
        if score <= parsed.threshold {
            return;
        }

        candidates.push(OrderedCandidate {
            score,
            tie_id: value_as_u64(id_field.value(row)).unwrap_or_else(|| entity.id.raw()),
            entity_id: entity.id,
        });
    });

    if candidates.is_empty() {
        return Some(encode_empty_simple_select(&parsed.columns));
    }
    if let Some(limit) = parsed.limit {
        if candidates.len() > limit {
            candidates.select_nth_unstable_by(limit, compare_ordered_candidate);
            candidates.truncate(limit);
        }
    }
    if candidates.is_empty() {
        return Some(encode_empty_simple_select(&parsed.columns));
    }
    candidates.sort_by(compare_ordered_candidate);

    let cols = match schema_columns.as_ref() {
        Some(schema) => resolve_wire_columns_from_schema(&parsed.columns, schema.as_slice()),
        None => {
            let mut first_row_cols: Option<Vec<WireColumn>> = None;
            manager.for_each_id(&[candidates[0].entity_id], |_, entity| {
                if first_row_cols.is_some() {
                    return;
                }
                if let EntityData::Row(ref row) = entity.data {
                    first_row_cols = Some(resolve_wire_columns_from_names(&parsed.columns, row));
                }
            });
            first_row_cols.unwrap_or_else(|| {
                parsed
                    .columns
                    .iter()
                    .map(|name| resolve_named_wire_column_for_empty_row(name, name))
                    .collect()
            })
        }
    };

    let mut body = Vec::with_capacity(estimate_wire_columns_response_capacity(
        &cols,
        candidates.len(),
    ));
    body.extend_from_slice(&(cols.len() as u16).to_le_bytes());
    for col in &cols {
        encode_column_name(&mut body, col.name.as_ref());
    }
    let header_nrows_pos = body.len();
    body.extend_from_slice(&0u32.to_le_bytes());
    let mut row_count: u32 = 0;
    let candidate_ids: Vec<EntityId> = candidates
        .iter()
        .map(|candidate| candidate.entity_id)
        .collect();
    let row_capacity = estimate_wire_columns_response_capacity(&cols, 1);
    let mut encoded_rows: Vec<Option<Vec<u8>>> = Vec::with_capacity(candidate_ids.len());
    encoded_rows.resize_with(candidate_ids.len(), || None);
    manager.for_each_id(&candidate_ids, |candidate_idx, entity| {
        let EntityData::Row(ref row) = entity.data else {
            return;
        };
        if !entity_visible_under_current_snapshot(entity) {
            return;
        }
        let mut row_bytes = Vec::with_capacity(row_capacity);
        for col in &cols {
            encode_entity_wire_value(&mut row_bytes, entity, row, col);
        }
        encoded_rows[candidate_idx] = Some(row_bytes);
    });
    for row in encoded_rows {
        if let Some(row_bytes) = row {
            body.extend_from_slice(&row_bytes);
            row_count += 1;
        }
    }
    body[header_nrows_pos..header_nrows_pos + 4].copy_from_slice(&row_count.to_le_bytes());

    let mut resp = Vec::with_capacity(5 + body.len());
    write_frame_header(&mut resp, MSG_RESULT, body.len() as u32);
    resp.extend_from_slice(&body);
    Some(resp)
}

fn execute_simple_indexed_select<FBuild>(
    runtime: &RedDBRuntime,
    table: &str,
    columns: &[String],
    ids: &[EntityId],
    build_predicate: FBuild,
) -> Option<Vec<u8>>
where
    FBuild: FnOnce(Option<&[String]>) -> SimpleRowPredicate,
{
    let db = runtime.db();
    let store = db.store();
    let manager = store.get_collection(table)?;
    let schema_columns = manager.column_schema();
    let schema_slice = schema_columns.as_ref().map(|schema| schema.as_slice());
    let row_predicate = build_predicate(schema_slice);
    let pre_resolved_cols = schema_columns
        .as_ref()
        .map(|schema| resolve_wire_columns_from_schema(columns, schema.as_slice()));
    let mut body: Vec<u8> =
        Vec::with_capacity(estimate_simple_response_capacity(columns, ids.len()));
    let mut header_nrows_pos: usize = 0;
    let mut cols: Option<Vec<WireColumn>> = None;
    let mut row_count: u32 = 0;

    manager.for_each_id(&ids, |_, entity| {
        if !entity.data.is_row() || !entity_visible_under_current_snapshot(entity) {
            return;
        }
        let EntityData::Row(ref row) = entity.data else {
            return;
        };
        if !row_predicate.matches(row) {
            return;
        }

        if cols.is_none() {
            let resolved = pre_resolved_cols
                .clone()
                .unwrap_or_else(|| resolve_wire_columns_from_names(columns, row));
            body.extend_from_slice(&(resolved.len() as u16).to_le_bytes());
            for col in &resolved {
                encode_column_name(&mut body, col.name.as_ref());
            }
            header_nrows_pos = body.len();
            body.extend_from_slice(&[0u8; 4]);
            cols = Some(resolved);
        }

        if let Some(cols_ref) = cols.as_ref() {
            for c in cols_ref {
                encode_entity_wire_value(&mut body, entity, row, c);
            }
            row_count += 1;
        }
    });

    if cols.is_none() {
        return None;
    }

    body[header_nrows_pos..header_nrows_pos + 4].copy_from_slice(&row_count.to_le_bytes());
    let mut resp = Vec::with_capacity(5 + body.len());
    write_frame_header(&mut resp, MSG_RESULT, body.len() as u32);
    resp.extend_from_slice(&body);
    Some(resp)
}

fn encode_empty_simple_select(columns: &[String]) -> Vec<u8> {
    let mut body = Vec::with_capacity(estimate_simple_response_capacity(columns, 0));
    body.extend_from_slice(&(columns.len() as u16).to_le_bytes());
    for column in columns {
        encode_column_name(&mut body, column.as_str());
    }
    body.extend_from_slice(&0u32.to_le_bytes());

    let mut resp = Vec::with_capacity(5 + body.len());
    write_frame_header(&mut resp, MSG_RESULT, body.len() as u32);
    resp.extend_from_slice(&body);
    resp
}

fn encode_empty_direct_select(tq: &TableQuery, schema: Option<&[String]>) -> Option<Vec<u8>> {
    let cols = match schema {
        Some(schema) => resolve_wire_columns_from_query_schema(tq, schema),
        None => resolve_wire_columns_for_empty_projection(tq)?,
    };
    let mut body = Vec::with_capacity(estimate_wire_columns_response_capacity(&cols, 0));
    body.extend_from_slice(&(cols.len() as u16).to_le_bytes());
    for col in &cols {
        encode_column_name(&mut body, col.name.as_ref());
    }
    body.extend_from_slice(&0u32.to_le_bytes());

    let mut resp = Vec::with_capacity(5 + body.len());
    write_frame_header(&mut resp, MSG_RESULT, body.len() as u32);
    resp.extend_from_slice(&body);
    Some(resp)
}

fn estimate_simple_response_capacity(columns: &[String], row_hint: usize) -> usize {
    let header = 2usize
        + columns
            .iter()
            .map(|name| 2usize.saturating_add(name.len()))
            .sum::<usize>()
        + 4;
    let row_bytes = columns
        .iter()
        .map(|name| estimated_wire_value_bytes(name.as_str()))
        .sum::<usize>()
        .max(1);
    header
        .saturating_add(row_hint.saturating_mul(row_bytes))
        .clamp(256, 4 * 1024 * 1024)
}

fn estimate_direct_response_capacity(
    tq: &TableQuery,
    schema: Option<&[String]>,
    row_hint: usize,
) -> usize {
    match schema {
        Some(schema) => {
            let cols = resolve_wire_columns_from_query_schema(tq, schema);
            estimate_wire_columns_response_capacity(&cols, row_hint)
        }
        None => resolve_wire_columns_for_empty_projection(tq)
            .map(|cols| estimate_wire_columns_response_capacity(&cols, row_hint))
            .unwrap_or(2048),
    }
}

fn estimate_wire_columns_response_capacity(cols: &[WireColumn], row_hint: usize) -> usize {
    let header = 2usize
        + cols
            .iter()
            .map(|col| 2usize.saturating_add(col.name.len()))
            .sum::<usize>()
        + 4;
    let row_bytes = cols
        .iter()
        .map(estimated_wire_column_value_bytes)
        .sum::<usize>()
        .max(1);
    header
        .saturating_add(row_hint.saturating_mul(row_bytes))
        .clamp(256, 4 * 1024 * 1024)
}

fn estimated_wire_column_value_bytes(col: &WireColumn) -> usize {
    match &col.source {
        WireColumnSource::RedEntityId
        | WireColumnSource::CreatedAt
        | WireColumnSource::UpdatedAt => 9,
        _ => estimated_wire_value_bytes(col.name.as_ref()),
    }
}

fn estimated_wire_value_bytes(column: &str) -> usize {
    match column {
        "id" | "age" | "score" | "red_entity_id" | "created_at_ms" | "updated_at_ms" => 9,
        "city" => 16,
        "name" => 32,
        "email" => 48,
        "created_at" | "updated_at" => 40,
        _ => 24,
    }
}

enum RowFieldAccessor {
    Index { index: usize, name: String },
    Name(String),
}

impl RowFieldAccessor {
    fn for_column(column: &str, schema: Option<&[String]>) -> Self {
        if let Some(index) =
            schema.and_then(|columns| columns.iter().position(|name| name == column))
        {
            Self::Index {
                index,
                name: column.to_string(),
            }
        } else {
            Self::Name(column.to_string())
        }
    }

    fn value<'a>(&self, row: &'a RowData) -> Option<&'a Value> {
        match self {
            Self::Index { index, name } => row
                .columns
                .get(*index)
                .or_else(|| row.get_field(name.as_str())),
            Self::Name(name) => row.get_field(name.as_str()),
        }
    }
}

enum SimpleRowPredicate {
    EqU64 {
        field: RowFieldAccessor,
        expected: u64,
    },
    BetweenI64 {
        field: RowFieldAccessor,
        low: i64,
        high: i64,
    },
    TextEqIntGt {
        text_field: RowFieldAccessor,
        expected_text: String,
        int_field: RowFieldAccessor,
        threshold: i64,
    },
}

impl SimpleRowPredicate {
    fn matches(&self, row: &RowData) -> bool {
        match self {
            Self::EqU64 { field, expected } => value_eq_u64(field.value(row), *expected),
            Self::BetweenI64 { field, low, high } => {
                value_between_i64(field.value(row), *low, *high)
            }
            Self::TextEqIntGt {
                text_field,
                expected_text,
                int_field,
                threshold,
            } => {
                value_text_eq(text_field.value(row), expected_text.as_str())
                    && value_gt_i64(int_field.value(row), *threshold)
            }
        }
    }
}

fn parse_i64_literal(text: &str) -> Option<i64> {
    let rest = text.strip_prefix('-').unwrap_or(text);
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_f64_literal(text: &str) -> Option<f64> {
    let text = text.strip_suffix(';').unwrap_or(text).trim();
    if text.is_empty() {
        return None;
    }
    let value: f64 = text.parse().ok()?;
    value.is_finite().then_some(value)
}

fn parse_usize_literal(text: &str) -> Option<usize> {
    let text = text.strip_suffix(';').unwrap_or(text).trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn value_eq_u64(value: Option<&Value>, expected: u64) -> bool {
    match value {
        Some(Value::UnsignedInteger(v)) => *v == expected,
        Some(Value::Integer(v)) => *v >= 0 && *v as u64 == expected,
        _ => false,
    }
}

fn value_as_u64(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::UnsignedInteger(v)) => Some(*v),
        Some(Value::Integer(v)) if *v >= 0 => Some(*v as u64),
        _ => None,
    }
}

fn value_as_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Float(v)) if v.is_finite() => Some(*v),
        Some(Value::Integer(v)) => Some(*v as f64),
        Some(Value::UnsignedInteger(v)) => Some(*v as f64),
        _ => None,
    }
}

fn value_between_i64(value: Option<&Value>, low: i64, high: i64) -> bool {
    match value {
        Some(Value::Integer(v)) => *v >= low && *v <= high,
        Some(Value::UnsignedInteger(v)) if low >= 0 => {
            let low = low as u64;
            let high = high as u64;
            *v >= low && *v <= high
        }
        _ => false,
    }
}

fn value_gt_i64(value: Option<&Value>, threshold: i64) -> bool {
    match value {
        Some(Value::Integer(v)) => *v > threshold,
        Some(Value::UnsignedInteger(v)) if threshold >= 0 => *v > threshold as u64,
        Some(Value::UnsignedInteger(_)) => true,
        _ => false,
    }
}

fn value_text_eq(value: Option<&Value>, expected: &str) -> bool {
    match value {
        Some(Value::Text(v)) => v.as_ref() == expected,
        _ => false,
    }
}

fn strip_keyword<'a>(input: &'a str, keyword: &str) -> Option<&'a str> {
    if input.len() < keyword.len() {
        return None;
    }
    let (head, tail) = input.split_at(keyword.len());
    if head.eq_ignore_ascii_case(keyword) {
        Some(tail)
    } else {
        None
    }
}

fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn is_simple_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[derive(Clone)]
struct WireColumn {
    name: Arc<str>,
    source: WireColumnSource,
}

#[derive(Clone)]
enum WireColumnSource {
    RedEntityId,
    CreatedAt,
    UpdatedAt,
    RowIndexTrusted { index: usize },
    RowIndex { index: usize, name: Arc<str> },
    RowName(Arc<str>),
}

fn resolve_wire_columns(tq: &TableQuery, row: &RowData) -> Vec<WireColumn> {
    let wildcard = tq
        .select_items
        .iter()
        .any(|it| matches!(it, SelectItem::Wildcard));

    if wildcard {
        let mut out = Vec::with_capacity(3 + row.columns.len());
        out.push(WireColumn::system(
            "red_entity_id",
            WireColumnSource::RedEntityId,
        ));
        out.push(WireColumn::system(
            "created_at",
            WireColumnSource::CreatedAt,
        ));
        out.push(WireColumn::system(
            "updated_at",
            WireColumnSource::UpdatedAt,
        ));
        if let Some(schema) = row.schema.as_ref() {
            out.extend(schema.iter().enumerate().map(|(idx, name)| WireColumn {
                name: Arc::<str>::from(name.as_str()),
                source: WireColumnSource::RowIndex {
                    index: idx,
                    name: Arc::<str>::from(name.as_str()),
                },
            }));
        } else if let Some(named) = row.named.as_ref() {
            out.extend(named.keys().map(|name| WireColumn {
                name: Arc::<str>::from(name.as_str()),
                source: WireColumnSource::RowName(Arc::<str>::from(name.as_str())),
            }));
        }
        return out;
    }

    let mut out = Vec::with_capacity(tq.select_items.len());
    for item in &tq.select_items {
        if let SelectItem::Expr { expr, alias } = item {
            if let Expr::Column {
                field: FieldRef::TableColumn { column, .. },
                ..
            } = expr
            {
                let name = alias.as_deref().unwrap_or(column.as_str());
                out.push(resolve_named_wire_column(name, column, row));
            }
        }
    }
    out
}

fn resolve_wire_columns_from_query_schema(tq: &TableQuery, schema: &[String]) -> Vec<WireColumn> {
    let wildcard = tq
        .select_items
        .iter()
        .any(|it| matches!(it, SelectItem::Wildcard));

    if wildcard {
        let mut out = Vec::with_capacity(3 + schema.len());
        out.push(WireColumn::system(
            "red_entity_id",
            WireColumnSource::RedEntityId,
        ));
        out.push(WireColumn::system(
            "created_at",
            WireColumnSource::CreatedAt,
        ));
        out.push(WireColumn::system(
            "updated_at",
            WireColumnSource::UpdatedAt,
        ));
        out.extend(schema.iter().enumerate().map(|(idx, name)| WireColumn {
            name: Arc::<str>::from(name.as_str()),
            source: WireColumnSource::RowIndexTrusted { index: idx },
        }));
        return out;
    }

    let mut out = Vec::with_capacity(tq.select_items.len());
    for item in &tq.select_items {
        if let SelectItem::Expr { expr, alias } = item {
            if let Expr::Column {
                field: FieldRef::TableColumn { column, .. },
                ..
            } = expr
            {
                let name = alias.as_deref().unwrap_or(column.as_str());
                out.push(resolve_named_wire_column_from_schema(name, column, schema));
            }
        }
    }
    out
}

fn resolve_wire_columns_for_empty_projection(tq: &TableQuery) -> Option<Vec<WireColumn>> {
    if tq
        .select_items
        .iter()
        .any(|it| matches!(it, SelectItem::Wildcard))
    {
        return None;
    }

    let mut out = Vec::with_capacity(tq.select_items.len());
    for item in &tq.select_items {
        let SelectItem::Expr { expr, alias } = item else {
            return None;
        };
        let Expr::Column {
            field: FieldRef::TableColumn { column, .. },
            ..
        } = expr
        else {
            return None;
        };
        let name = alias.as_deref().unwrap_or(column.as_str());
        out.push(resolve_named_wire_column_for_empty_row(name, column));
    }
    Some(out)
}

fn resolve_wire_columns_from_names(names: &[String], row: &RowData) -> Vec<WireColumn> {
    names
        .iter()
        .map(|name| resolve_named_wire_column(name, name, row))
        .collect()
}

fn resolve_wire_columns_from_schema(names: &[String], schema: &[String]) -> Vec<WireColumn> {
    names
        .iter()
        .map(|name| resolve_named_wire_column_from_schema(name, name, schema))
        .collect()
}

impl WireColumn {
    fn system(name: &str, source: WireColumnSource) -> Self {
        Self {
            name: Arc::<str>::from(name),
            source,
        }
    }
}

fn resolve_named_wire_column_for_empty_row(name: &str, source_column: &str) -> WireColumn {
    let source = match source_column {
        "red_entity_id" => WireColumnSource::RedEntityId,
        "created_at" => WireColumnSource::CreatedAt,
        "updated_at" => WireColumnSource::UpdatedAt,
        _ => WireColumnSource::RowName(Arc::<str>::from(source_column)),
    };
    WireColumn {
        name: Arc::<str>::from(name),
        source,
    }
}

fn resolve_named_wire_column_from_schema(
    name: &str,
    source_column: &str,
    schema: &[String],
) -> WireColumn {
    let source = match source_column {
        "red_entity_id" => WireColumnSource::RedEntityId,
        "created_at" => WireColumnSource::CreatedAt,
        "updated_at" => WireColumnSource::UpdatedAt,
        _ => match schema.iter().position(|col| col == source_column) {
            Some(idx) => WireColumnSource::RowIndexTrusted { index: idx },
            None => WireColumnSource::RowName(Arc::<str>::from(source_column)),
        },
    };
    WireColumn {
        name: Arc::<str>::from(name),
        source,
    }
}

fn resolve_named_wire_column(name: &str, source_column: &str, row: &RowData) -> WireColumn {
    let source = match source_column {
        "red_entity_id" => WireColumnSource::RedEntityId,
        "created_at" => WireColumnSource::CreatedAt,
        "updated_at" => WireColumnSource::UpdatedAt,
        _ => {
            if let Some(schema) = row.schema.as_ref() {
                match schema.iter().position(|col| col == source_column) {
                    Some(idx) => WireColumnSource::RowIndex {
                        index: idx,
                        name: Arc::<str>::from(source_column),
                    },
                    None => WireColumnSource::RowName(Arc::<str>::from(source_column)),
                }
            } else {
                WireColumnSource::RowName(Arc::<str>::from(source_column))
            }
        }
    };
    WireColumn {
        name: Arc::<str>::from(name),
        source,
    }
}

#[inline]
fn encode_entity_wire_value(
    body: &mut Vec<u8>,
    entity: &UnifiedEntity,
    row: &RowData,
    col: &WireColumn,
) {
    match &col.source {
        WireColumnSource::RedEntityId => encode_wire_u64(body, entity.id.raw()),
        WireColumnSource::CreatedAt => encode_wire_u64(body, entity.created_at),
        WireColumnSource::UpdatedAt => encode_wire_u64(body, entity.updated_at),
        WireColumnSource::RowIndexTrusted { index } => match row.columns.get(*index) {
            Some(v) => encode_value(body, v),
            None => encode_wire_null(body),
        },
        WireColumnSource::RowIndex { index, name } => {
            match row
                .schema
                .as_ref()
                .and_then(|schema| {
                    schema
                        .get(*index)
                        .filter(|col| col.as_str() == name.as_ref())
                })
                .and_then(|_| row.columns.get(*index))
                .or_else(|| row.get_field(name.as_ref()))
            {
                Some(v) => encode_value(body, v),
                None => encode_wire_null(body),
            }
        }
        WireColumnSource::RowName(name) => match row.get_field(name.as_ref()) {
            Some(v) => encode_value(body, v),
            None => encode_wire_null(body),
        },
    }
}

#[inline]
fn encode_wire_null(body: &mut Vec<u8>) {
    body.push(VAL_NULL);
}

#[inline]
fn encode_wire_u64(body: &mut Vec<u8>, value: u64) {
    body.push(VAL_U64);
    body.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBOptions, RedDBRuntime};

    fn mk_runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory())
            .expect("runtime should open in-memory")
    }

    fn decode_wire_header(bytes: &[u8]) -> (Vec<String>, u32, usize) {
        let body = &bytes[5..];
        let ncols = u16::from_le_bytes([body[0], body[1]]) as usize;
        let mut pos = 2usize;
        let mut cols = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            let name_len = u16::from_le_bytes([body[pos], body[pos + 1]]) as usize;
            pos += 2;
            cols.push(String::from_utf8_lossy(&body[pos..pos + name_len]).to_string());
            pos += name_len;
        }
        let nrows = u32::from_le_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
        pos += 4;
        (cols, nrows, pos)
    }

    fn seed_users(rt: &RedDBRuntime) {
        rt.execute_query("CREATE TABLE users (id INT, name TEXT, city TEXT, age INT)")
            .unwrap();
        rt.execute_query("CREATE INDEX idx_age ON users (age) USING BTREE")
            .unwrap();
        rt.execute_query(
            "INSERT INTO users (id, name, city, age) VALUES \
             (1, 'a', 'NYC', 25), (2, 'b', 'LA', 30), (3, 'c', 'NYC', 35), \
             (4, 'd', 'NYC', 40), (5, 'e', 'LA', 45)",
        )
        .unwrap();
    }

    #[test]
    fn shape_eligible_select_star_between() {
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT * FROM users WHERE age BETWEEN 30 AND 40";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(resp.is_some(), "expected fast-path hit for indexed BETWEEN");
        let bytes = resp.unwrap();
        assert!(bytes.len() > 5, "non-empty response");
    }

    #[test]
    fn shape_miss_on_join() {
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT * FROM users u1 JOIN users u2 ON u1.id = u2.id";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(resp.is_none(), "JOIN should miss fast path");
    }

    #[test]
    fn shape_miss_on_order_by() {
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT * FROM users WHERE age BETWEEN 30 AND 40 ORDER BY age";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(resp.is_none(), "ORDER BY should miss fast path");
    }

    #[test]
    fn shape_miss_on_aggregate() {
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT COUNT(*) FROM users";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(resp.is_none(), "COUNT should miss fast path");
    }

    #[test]
    fn shape_miss_on_group_by() {
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT age FROM users GROUP BY age";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(resp.is_none(), "GROUP BY should miss fast path");
    }

    #[test]
    fn shape_miss_unbounded_unfiltered() {
        // SELECT * FROM t with NO WHERE and NO LIMIT must fall back —
        // we don't want the fast path to materialise an unbounded scan
        // here, the runtime's canonical path has the parallel-scan
        // branch for that.
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT * FROM users";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(
            resp.is_none(),
            "full unbounded scan should defer to runtime path"
        );
    }

    #[test]
    fn fast_path_hits_on_unfiltered_with_limit() {
        // SELECT * FROM t LIMIT N — no WHERE but bounded: fast path
        // scans entities with early-stop at LIMIT.
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT * FROM users LIMIT 3";
        let fast =
            try_handle_query_binary_direct(&rt, sql).expect("fast path should hit LIMIT no-filter");
        let body = &fast[5..];
        let ncols = u16::from_le_bytes([body[0], body[1]]);
        let mut pos = 2usize;
        for _ in 0..ncols {
            let name_len = u16::from_le_bytes([body[pos], body[pos + 1]]) as usize;
            pos += 2 + name_len;
        }
        let nrows = u32::from_le_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
        assert_eq!(nrows, 3, "fast path should emit exactly LIMIT rows");
    }

    #[test]
    fn shape_miss_without_index() {
        let rt = mk_runtime();
        seed_users(&rt);
        // name is not indexed — try_sorted_index_lookup returns None.
        let sql = "SELECT * FROM users WHERE name = 'a'";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(resp.is_none(), "unindexed filter should miss fast path");
    }

    #[test]
    fn parses_simple_hash_eq_select_shape_only() {
        let parsed =
            parse_simple_hash_eq_select("SELECT id, name, email, age FROM users WHERE id = 42;")
                .expect("simple point select should parse");

        assert_eq!(parsed.table, "users");
        assert_eq!(parsed.columns, vec!["id", "name", "email", "age"]);
        assert_eq!(parsed.filter_column, "id");
        assert_eq!(parsed.value, 42);
        assert!(
            parse_simple_hash_eq_select("SELECT id AS ident FROM users WHERE id = 42").is_none()
        );
        assert!(
            parse_simple_hash_eq_select("SELECT id FROM users WHERE id = 42 LIMIT 1").is_none()
        );
    }

    #[test]
    fn parses_simple_between_select_shape_only() {
        let parsed = parse_simple_between_select(
            "SELECT id, name, email, age FROM users WHERE age BETWEEN -10 AND 42;",
        )
        .expect("simple range select should parse");

        assert_eq!(parsed.table, "users");
        assert_eq!(parsed.columns, vec!["id", "name", "email", "age"]);
        assert_eq!(parsed.filter_column, "age");
        assert_eq!(parsed.low, -10);
        assert_eq!(parsed.high, 42);
        assert!(parse_simple_between_select(
            "SELECT id AS ident FROM users WHERE age BETWEEN 1 AND 2"
        )
        .is_none());
        assert!(parse_simple_between_select(
            "SELECT id FROM users WHERE age BETWEEN 1 AND 2 LIMIT 1"
        )
        .is_none());
    }

    #[test]
    fn parses_simple_text_eq_int_gt_select_shape_only() {
        let parsed = parse_simple_text_eq_int_gt_select(
            "SELECT id, name, city, age FROM users WHERE city = 'O''Hare' AND age > 30;",
        )
        .expect("simple text-eq/int-gt select should parse");

        assert_eq!(parsed.table, "users");
        assert_eq!(parsed.columns, vec!["id", "name", "city", "age"]);
        assert_eq!(parsed.eq_column, "city");
        assert_eq!(parsed.eq_value, "O'Hare");
        assert_eq!(parsed.range_column, "age");
        assert_eq!(parsed.threshold, 30);
        assert!(parse_simple_text_eq_int_gt_select(
            "SELECT id FROM users WHERE city = 'NYC' AND age >= 30"
        )
        .is_none());
        assert!(parse_simple_text_eq_int_gt_select(
            "SELECT id FROM users WHERE city = 'NYC' AND age > 30 LIMIT 1"
        )
        .is_none());
    }

    #[test]
    fn parses_simple_ordered_complex_select_shape_only() {
        let parsed = parse_simple_ordered_complex_select(
            "SELECT id, name, email, age, city, score, created_at FROM users \
             WHERE city = 'NYC' AND age BETWEEN 20 AND 45 AND score > 50.5 \
             ORDER BY score DESC LIMIT 25;",
        )
        .expect("simple ordered complex select should parse");

        assert_eq!(parsed.table, "users");
        assert_eq!(
            parsed.columns,
            vec!["id", "name", "email", "age", "city", "score", "created_at"]
        );
        assert_eq!(parsed.eq_column, "city");
        assert_eq!(parsed.eq_value, "NYC");
        assert_eq!(parsed.range_column, "age");
        assert_eq!(parsed.low, 20);
        assert_eq!(parsed.high, 45);
        assert_eq!(parsed.float_column, "score");
        assert_eq!(parsed.threshold, 50.5);
        assert_eq!(parsed.limit, Some(25));
        assert!(parse_simple_ordered_complex_select(
            "SELECT id FROM users WHERE city = 'NYC' AND age BETWEEN 20 AND 45 \
             AND score > 50.5 ORDER BY score ASC"
        )
        .is_none());
        assert!(parse_simple_ordered_complex_select(
            "SELECT id FROM users WHERE city = 'NYC' AND age BETWEEN 20 AND 45 \
             AND score > 50.5 ORDER BY age DESC"
        )
        .is_none());
    }

    #[test]
    fn fast_path_hits_hash_equality_index() {
        use crate::wire::protocol::decode_value;

        let rt = mk_runtime();
        seed_users(&rt);
        rt.execute_query("CREATE INDEX idx_id ON users (id) USING HASH")
            .unwrap();

        let sql = "SELECT id, name FROM users WHERE id = 3";
        let resp = try_handle_query_binary_direct(&rt, sql)
            .expect("hash equality index should hit fast path");
        let (cols, nrows, mut pos) = decode_wire_header(&resp);
        let body = &resp[5..];

        assert_eq!(cols, vec!["id", "name"]);
        assert_eq!(nrows, 1);
        assert_eq!(decode_value(body, &mut pos), Value::Integer(3));
        assert_eq!(decode_value(body, &mut pos), Value::text("c"));
    }

    #[test]
    fn fast_path_returns_empty_hash_result_without_fallback() {
        let rt = mk_runtime();
        seed_users(&rt);
        rt.execute_query("CREATE INDEX idx_id ON users (id) USING HASH")
            .unwrap();

        let sql = "SELECT id, name FROM users WHERE id = 999";
        let resp = try_handle_query_binary_direct(&rt, sql)
            .expect("hash equality miss should return an empty fast-path response");
        let (cols, nrows, _pos) = decode_wire_header(&resp);

        assert_eq!(cols, vec!["id", "name"]);
        assert_eq!(nrows, 0);
    }

    #[test]
    fn fast_path_hits_simple_between_index() {
        use crate::wire::protocol::decode_value;

        let rt = mk_runtime();
        seed_users(&rt);

        let sql = "SELECT id, age FROM users WHERE age BETWEEN 30 AND 40";
        let resp =
            try_handle_query_binary_direct(&rt, sql).expect("simple BETWEEN should hit fast path");
        let (cols, nrows, mut pos) = decode_wire_header(&resp);
        let body = &resp[5..];

        assert_eq!(cols, vec!["id", "age"]);
        assert_eq!(nrows, 3);
        assert_eq!(decode_value(body, &mut pos), Value::Integer(2));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(30));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(3));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(35));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(4));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(40));
    }

    #[test]
    fn fast_path_returns_empty_between_result_without_fallback() {
        let rt = mk_runtime();
        seed_users(&rt);

        let sql = "SELECT id, age FROM users WHERE age BETWEEN 1000 AND 1010";
        let resp = try_handle_query_binary_direct(&rt, sql)
            .expect("range miss should return an empty fast-path response");
        let (cols, nrows, _pos) = decode_wire_header(&resp);

        assert_eq!(cols, vec!["id", "age"]);
        assert_eq!(nrows, 0);
    }

    #[test]
    fn fast_path_hits_simple_text_eq_int_gt_composite_index() {
        use crate::wire::protocol::decode_value;

        let rt = mk_runtime();
        seed_users(&rt);
        rt.execute_query("CREATE INDEX idx_city_age ON users (city, age) USING BTREE")
            .unwrap();

        let sql = "SELECT id, city, age FROM users WHERE city = 'NYC' AND age > 30";
        let resp = try_handle_query_binary_direct(&rt, sql)
            .expect("city/age composite should hit fast path");
        let (cols, nrows, mut pos) = decode_wire_header(&resp);
        let body = &resp[5..];

        assert_eq!(cols, vec!["id", "city", "age"]);
        assert_eq!(nrows, 2);
        assert_eq!(decode_value(body, &mut pos), Value::Integer(3));
        assert_eq!(decode_value(body, &mut pos), Value::text("NYC"));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(35));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(4));
        assert_eq!(decode_value(body, &mut pos), Value::text("NYC"));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(40));
    }

    #[test]
    fn fast_path_hits_simple_ordered_complex_composite_index() {
        use crate::wire::protocol::decode_value;

        let rt = mk_runtime();
        rt.execute_query(
            "CREATE TABLE users (id INT, name TEXT, email TEXT, age INT, city TEXT, score FLOAT, created_at TEXT)",
        )
        .unwrap();
        rt.execute_query("CREATE INDEX idx_city_age ON users (city, age) USING BTREE")
            .unwrap();
        rt.execute_query(
            "INSERT INTO users (id, name, email, age, city, score, created_at) VALUES \
             (1, 'a', 'a@example.com', 30, 'NYC', 80.0, 't1'), \
             (2, 'b', 'b@example.com', 35, 'NYC', 95.0, 't2'), \
             (3, 'c', 'c@example.com', 40, 'NYC', 60.0, 't3'), \
             (4, 'd', 'd@example.com', 30, 'LA', 99.0, 't4')",
        )
        .unwrap();

        let sql = "SELECT id, score FROM users WHERE city = 'NYC' AND age BETWEEN 20 AND 45 \
                   AND score > 70 ORDER BY score DESC LIMIT 2";
        let resp = try_handle_query_binary_direct(&rt, sql)
            .expect("ordered complex composite select should hit fast path");
        let (cols, nrows, mut pos) = decode_wire_header(&resp);
        let body = &resp[5..];

        assert_eq!(cols, vec!["id", "score"]);
        assert_eq!(nrows, 2);
        assert_eq!(decode_value(body, &mut pos), Value::Integer(2));
        assert_eq!(decode_value(body, &mut pos), Value::Float(95.0));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(1));
        assert_eq!(decode_value(body, &mut pos), Value::Float(80.0));
    }

    #[test]
    fn shape_miss_on_entity_id_lookup() {
        let rt = mk_runtime();
        seed_users(&rt);
        // `_entity_id = N` is routed via the runtime point lookup.
        let sql = "SELECT * FROM users WHERE _entity_id = 1";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(
            resp.is_none(),
            "entity-id lookup should defer to runtime path"
        );
    }

    #[test]
    fn shape_miss_on_limit_offset() {
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT * FROM users WHERE age BETWEEN 30 AND 40 LIMIT 2 OFFSET 1";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(resp.is_none(), "OFFSET should miss fast path");
    }

    #[test]
    fn fast_path_pure_between_with_limit_row_count_parity() {
        // Correctness guard for the zero-copy LIMIT 100 win in
        // select_range. A pure BETWEEN leaf pushes LIMIT down to the
        // sorted index and emits exactly `limit` matching rows; we
        // must return the same row count as the standard path.
        let rt = mk_runtime();
        rt.execute_query("CREATE TABLE t (id INT, age INT)")
            .unwrap();
        rt.execute_query("CREATE INDEX idx_age ON t (age) USING BTREE")
            .unwrap();
        // 500 rows, age cycles 18..67 — a BETWEEN 25 AND 45 matches
        // ~21 ages × ~10 rows each = ~210 matches; LIMIT 100 caps.
        for i in 0..500 {
            let age = 18 + (i % 50);
            rt.execute_query(&format!("INSERT INTO t (id, age) VALUES ({i}, {age})"))
                .unwrap();
        }
        let sql = "SELECT * FROM t WHERE age BETWEEN 25 AND 45 LIMIT 100";

        let fast = try_handle_query_binary_direct(&rt, sql).expect("fast path should hit");
        let standard = rt.execute_query(sql).unwrap();
        let standard_rows = standard.result.records.len() as u32;

        let body = &fast[5..];
        let ncols = u16::from_le_bytes([body[0], body[1]]);
        let mut pos = 2usize;
        for _ in 0..ncols {
            let name_len = u16::from_le_bytes([body[pos], body[pos + 1]]) as usize;
            pos += 2 + name_len;
        }
        let nrows = u32::from_le_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);

        assert_eq!(nrows, 100, "fast path should emit exactly LIMIT rows");
        assert_eq!(nrows, standard_rows, "fast/standard row-count parity");
    }

    #[test]
    fn fast_path_and_with_limit_returns_same_row_count_as_standard() {
        // Regression: the fast path used to push LIMIT down to the
        // sorted-index lookup even when only one branch of an AND was
        // indexed, which truncated the candidate pool before the
        // post-filter re-evaluation. Result: fast path returned ~10
        // rows while the standard path returned 100 for the same query.
        let rt = mk_runtime();
        rt.execute_query("CREATE TABLE u (id INT, city TEXT, age INT)")
            .unwrap();
        rt.execute_query("CREATE INDEX idx_age ON u (age) USING BTREE")
            .unwrap();
        rt.execute_query("CREATE INDEX idx_city ON u (city) USING HASH")
            .unwrap();

        // Seed 500 rows; 10% match city='NYC'. age evenly spread 18..68.
        let cities = ["NYC", "LA", "CHI", "HOU", "PHX"];
        for i in 0..500 {
            let city = cities[i % cities.len()];
            let age = 18 + (i % 50);
            rt.execute_query(&format!(
                "INSERT INTO u (id, city, age) VALUES ({i}, '{city}', {age})"
            ))
            .unwrap();
        }

        let sql = "SELECT * FROM u WHERE city = 'NYC' AND age > 20 LIMIT 100";
        let fast = try_handle_query_binary_direct(&rt, sql).expect("fast path should hit");
        let standard = rt.execute_query(sql).expect("standard path ok");
        let standard_rows = standard.result.records.len() as u32;

        // Decode fast-path nrows from wire body.
        let body = &fast[5..];
        let ncols = u16::from_le_bytes([body[0], body[1]]);
        let mut pos = 2usize;
        for _ in 0..ncols {
            let name_len = u16::from_le_bytes([body[pos], body[pos + 1]]) as usize;
            pos += 2 + name_len;
        }
        let nrows = u32::from_le_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);

        assert_eq!(
            nrows, standard_rows,
            "fast path truncated rows early: got {nrows}, standard got {standard_rows}"
        );
    }

    #[test]
    fn shape_eligible_select_filtered_and() {
        // Mirrors bench_definitive_dual.py select_filtered: compound
        // AND where one leaf has a sorted index (age) and the other
        // doesn't (city). `try_sorted_index_lookup` handles the AND
        // case by returning ids from the indexed leaf; the compiled
        // filter re-evaluates the full predicate per row.
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT * FROM users WHERE city = 'NYC' AND age > 30";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(
            resp.is_some(),
            "fast path should hit for compound AND when one side is indexed"
        );
    }

    #[test]
    fn fast_path_response_matches_encode_result() {
        use crate::wire::protocol::decode_value;

        let rt = mk_runtime();
        seed_users(&rt);

        let sql = "SELECT * FROM users WHERE age BETWEEN 30 AND 40";
        let fast = try_handle_query_binary_direct(&rt, sql).expect("fast path should hit");

        // Compare row count + column count with the standard path.
        let standard_result = rt.execute_query(sql).expect("standard path ok");
        let expected_rows = standard_result.result.records.len() as u32;

        // Fast response layout: [frame_header 5][body].
        // Body: [u16 ncols][cols..][u32 nrows][(tag+bytes)*]
        assert!(fast.len() > 5);
        let body = &fast[5..];
        let ncols = u16::from_le_bytes([body[0], body[1]]);
        assert!(ncols > 0, "expected non-zero column count");

        // Skip column names — just locate nrows position.
        let mut pos = 2usize;
        for _ in 0..ncols {
            let name_len = u16::from_le_bytes([body[pos], body[pos + 1]]) as usize;
            pos += 2 + name_len;
        }
        let nrows = u32::from_le_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
        pos += 4;
        assert_eq!(nrows, expected_rows, "fast path row count mismatch");

        // Decode each cell — not comparing against encode_result bytes
        // because column ordering in standard path is schema-driven
        // and may differ for the wildcard case, but value count and
        // type sequence must be consistent per row.
        for _ in 0..nrows {
            for _ in 0..ncols {
                let _ = decode_value(body, &mut pos);
            }
        }
        assert_eq!(pos, body.len(), "decoder should consume entire body");
    }

    #[test]
    fn fast_path_resolves_projection_alias_from_source_column() {
        use crate::storage::schema::Value;
        use crate::wire::protocol::decode_value;

        let rt = mk_runtime();
        rt.execute_query("CREATE TABLE t (id INT, age INT)")
            .unwrap();
        rt.execute_query("CREATE INDEX idx_age ON t (age) USING BTREE")
            .unwrap();
        rt.execute_query("INSERT INTO t (id, age) VALUES (1, 25), (2, 30), (3, 35)")
            .unwrap();

        let sql = "SELECT age AS years, id AS ident FROM t WHERE age BETWEEN 30 AND 30";
        let fast = try_handle_query_binary_direct(&rt, sql).expect("fast path should hit");
        let (cols, nrows, mut pos) = decode_wire_header(&fast);
        let body = &fast[5..];

        assert_eq!(cols, vec!["years".to_string(), "ident".to_string()]);
        assert_eq!(nrows, 1);
        assert_eq!(decode_value(body, &mut pos), Value::Integer(30));
        assert_eq!(decode_value(body, &mut pos), Value::Integer(2));
    }

    #[test]
    fn fast_path_full_range_after_create_index_matches_standard_count() {
        let rt = mk_runtime();
        rt.execute_query("CREATE TABLE users (id INT, age INT)")
            .unwrap();
        for i in 0..500 {
            let age = 18 + (i % 60);
            rt.execute_query(&format!("INSERT INTO users (id, age) VALUES ({i}, {age})"))
                .unwrap();
        }
        rt.execute_query("CREATE INDEX idx_age ON users (age) USING BTREE")
            .unwrap();

        let sql = "SELECT * FROM users WHERE age BETWEEN 0 AND 200";
        let fast = try_handle_query_binary_direct(&rt, sql).expect("fast path should hit");
        let standard = rt.execute_query(sql).unwrap();
        let (_cols, nrows, _pos) = decode_wire_header(&fast);

        assert_eq!(nrows, 500);
        assert_eq!(nrows as usize, standard.result.records.len());
    }
}
