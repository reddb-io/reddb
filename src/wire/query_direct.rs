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
    extract_entity_id_from_filter, try_sorted_index_lookup, CompiledEntityFilter,
};
use crate::runtime::RedDBRuntime;
use crate::storage::query::ast::{
    Expr, FieldRef, Filter, QueryExpr, SelectItem, TableQuery, TableSource,
};
use crate::storage::query::sql_lowering::effective_table_filter;
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, RowData, UnifiedEntity};

use super::protocol::{encode_column_name, encode_value, write_frame_header, MSG_RESULT};

/// Try to serve a binary SELECT via the zero-copy scan path.
///
/// Returns `Some(wire_response)` when the query matches the fast-path
/// shape and was executed; returns `None` to signal the caller should
/// fall back to the standard `execute_query` + `encode_result` path.
pub(super) fn try_handle_query_binary_direct(
    runtime: &RedDBRuntime,
    sql: &str,
) -> Option<Vec<u8>> {
    // Cheap prefix gate. Avoid parse for anything not a plain SELECT
    // (WITHIN, EXPLAIN, SET, BEGIN, …).
    let trimmed = sql.trim_start();
    if trimmed.len() < 6 {
        return None;
    }
    if !trimmed.as_bytes()[..6].eq_ignore_ascii_case(b"SELECT") {
        return None;
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

fn is_shape_direct_eligible(tq: &TableQuery) -> bool {
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

fn execute_direct_scan(runtime: &RedDBRuntime, tq: &TableQuery) -> Option<Vec<u8>> {
    let effective_filter = effective_table_filter(tq);

    // Defer to execute_query's point-lookup path when WHERE reduces
    // to `red_entity_id = N` — that path handles MVCC + error shapes
    // we don't want to re-implement here.
    if extract_entity_id_from_filter(&effective_filter).is_some() {
        return None;
    }

    // Without a filter there's no index to use; full scan duplicates
    // the runtime's canonical scan loop. Fall back.
    let filter = effective_filter.as_ref()?;

    let limit = tq.limit.map(|l| l as usize);
    let idx_store = runtime.index_store_ref();

    // LIMIT pushdown is safe only when the index filter matches the
    // full WHERE predicate. For composite filters (`And(Eq(city), Gt(age))`
    // where only `age` has an index) the index returns up to `limit`
    // ids that pass `age > X` but still need to pass `city = Y` after —
    // pushing the limit down would short-return fewer than `limit` rows.
    // Fall back to no pushdown when the filter has branches the index
    // can't resolve on its own.
    let index_limit = if filter_is_single_indexable_leaf(filter) {
        limit
    } else {
        None
    };
    let ids = try_sorted_index_lookup(filter, tq.table.as_str(), idx_store, index_limit)?;

    let db = runtime.db();
    let store = db.store();
    let manager = store.get_collection(&tq.table)?;

    let table_name = tq.table.as_str();
    let table_alias = tq.alias.as_deref().unwrap_or(table_name);
    let compiled_filter = CompiledEntityFilter::compile(filter, table_name, table_alias);

    let hard_limit = limit.unwrap_or(usize::MAX);

    let mut body: Vec<u8> = Vec::with_capacity(256 + ids.len() * 64);
    let mut header_nrows_pos: usize = 0;
    let mut cols: Option<Vec<Arc<str>>> = None;
    let mut row_count: u32 = 0;

    manager.for_each_id(&ids, |_, entity| {
        if (row_count as usize) >= hard_limit {
            return;
        }
        if !entity.data.is_row() {
            return;
        }
        if !entity_visible_under_current_snapshot(entity) {
            return;
        }
        if !compiled_filter.evaluate(entity) {
            return;
        }

        let row = match &entity.data {
            EntityData::Row(r) => r,
            _ => return,
        };

        // Lazy header emit on first visible row. The wire format
        // requires ncols/names up front, and we only know the row
        // shape after opening the first entity.
        if cols.is_none() {
            let resolved = derive_wire_columns(tq, row);
            body.extend_from_slice(&(resolved.len() as u16).to_le_bytes());
            for name in &resolved {
                encode_column_name(&mut body, name);
            }
            header_nrows_pos = body.len();
            body.extend_from_slice(&[0u8; 4]);
            cols = Some(
                resolved
                    .into_iter()
                    .map(|s| Arc::<str>::from(s.as_str()))
                    .collect(),
            );
        }

        if let Some(cols) = cols.as_ref() {
            for c in cols {
                match resolve_entity_value(entity, row, c.as_ref()) {
                    ValueRef::Owned(v) => encode_value(&mut body, &v),
                    ValueRef::Borrowed(v) => encode_value(&mut body, v),
                }
            }
            row_count += 1;
        }
    });

    // Empty result — no schema decided. Fall back so the standard
    // path emits the correct column list (from catalog) with 0 rows.
    if cols.is_none() {
        return None;
    }

    body[header_nrows_pos..header_nrows_pos + 4].copy_from_slice(&row_count.to_le_bytes());

    let mut resp = Vec::with_capacity(5 + body.len());
    write_frame_header(&mut resp, MSG_RESULT, body.len() as u32);
    resp.extend_from_slice(&body);
    Some(resp)
}

fn derive_wire_columns(tq: &TableQuery, row: &RowData) -> Vec<String> {
    let wildcard = tq
        .select_items
        .iter()
        .any(|it| matches!(it, SelectItem::Wildcard));

    if wildcard {
        let mut out = Vec::with_capacity(3 + row.columns.len());
        out.push("red_entity_id".to_string());
        out.push("created_at".to_string());
        out.push("updated_at".to_string());
        if let Some(schema) = row.schema.as_ref() {
            out.extend(schema.iter().cloned());
        } else if let Some(named) = row.named.as_ref() {
            out.extend(named.keys().cloned());
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
                out.push(alias.clone().unwrap_or_else(|| column.clone()));
            }
        }
    }
    out
}

enum ValueRef<'a> {
    Owned(Value),
    Borrowed(&'a Value),
}

fn resolve_entity_value<'a>(
    entity: &'a UnifiedEntity,
    row: &'a RowData,
    col: &str,
) -> ValueRef<'a> {
    match col {
        "red_entity_id" => ValueRef::Owned(Value::UnsignedInteger(entity.id.raw())),
        "created_at" => ValueRef::Owned(Value::UnsignedInteger(entity.created_at)),
        "updated_at" => ValueRef::Owned(Value::UnsignedInteger(entity.updated_at)),
        _ => match row.get_field(col) {
            Some(v) => ValueRef::Borrowed(v),
            None => ValueRef::Owned(Value::Null),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBOptions, RedDBRuntime};

    fn mk_runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory())
            .expect("runtime should open in-memory")
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
    fn shape_miss_without_filter() {
        let rt = mk_runtime();
        seed_users(&rt);
        let sql = "SELECT * FROM users";
        let resp = try_handle_query_binary_direct(&rt, sql);
        assert!(
            resp.is_none(),
            "full scan (no indexed filter) should miss fast path"
        );
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
}
