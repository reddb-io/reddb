//! Schema diff engine — `EXPLAIN ALTER FOR` runtime.
//!
//! Computes the set of `ALTER TABLE` operations that would
//! close the gap between an existing `CollectionContract` and
//! the column shape of a new `CreateTableQuery`. Used by the
//! `EXPLAIN ALTER FOR` SQL command exposed at the parser /
//! executor boundary.
//!
//! Pure logic — zero side effects, no DB access. The executor
//! loads the current contract, calls `compute_column_diff`,
//! and formats the result via `format_as_sql` /
//! `format_as_json`.
//!
//! ## Design highlights
//!
//! - **Equivalence check matches `apply_alter_operations_to_contract`'s
//!   semantics** so the round-trip property holds: applying the
//!   diff to a clone of `current` produces a contract byte-equal
//!   to the target.
//! - **`sql_type` is `Option<SqlTypeName>`** in the live contract
//!   (legacy tables don't carry it). When the current side has
//!   `None`, we fall back to comparing the legacy `data_type`
//!   string instead of declaring everything a TypeChange.
//! - **Rename detection is consultative.** Even when the heuristic
//!   is high-confidence, the SQL output still emits `DROP + ADD`
//!   plus a `-- hint:` comment. Only a human (or a client with
//!   more context) confirms.
//! - **Three confidence tiers** explicit:
//!   - `High`   → `sql_type` + every constraint + `default` match
//!   - `Medium` → `sql_type` + every constraint match
//!   - `Low`    → only `sql_type` matches (constraints differ)
//!   Lower than `Low` produces no candidate.
//!
//! ## Out of scope (v1)
//!
//! - Indexes (`CREATE INDEX`) — not in `CollectionContract`.
//! - Constraint-only changes (e.g. `NOT NULL` added with the
//!   same type) — folded into `TypeChange` for now.
//! - `default_ttl_ms`, `context_index_fields`, `timestamps`
//!   from `CreateTableQuery` — ignored. Reserved for v2 once
//!   the corresponding `AlterOperation` variants exist.
//! - Constraint normalisation (e.g. `'foo'` vs `foo`) — best-
//!   effort string compare with leading/trailing-quote strip.

use std::collections::HashMap;

use crate::physical::DeclaredColumnContract;
use crate::storage::query::ast::CreateColumnDef;

/// Aggregate result of a column-level schema diff.
#[derive(Debug, Clone)]
pub struct SchemaDiff {
    pub table: String,
    /// True when at least one operation is required to make the
    /// current contract match the target.
    pub drifted: bool,
    pub operations: Vec<DiffOp>,
    pub rename_candidates: Vec<RenameCandidate>,
    pub summary: DiffSummary,
}

/// One operation in a schema diff.
#[derive(Debug, Clone)]
pub enum DiffOp {
    /// Column present in target but not in current.
    AddColumn(DeclaredColumnContract),
    /// Column present in current but not in target.
    DropColumn(String),
    /// Same name, different shape — emitted as a single
    /// `TypeChange` so callers can render `DROP + ADD` or a
    /// future `ALTER COLUMN ... TYPE ...` form.
    TypeChange {
        name: String,
        from: DeclaredColumnContract,
        to: DeclaredColumnContract,
    },
}

/// Heuristic match between an unpaired `DropColumn` and an
/// unpaired `AddColumn`. Always advisory — clients decide.
#[derive(Debug, Clone)]
pub struct RenameCandidate {
    pub from: String,
    pub to: String,
    pub confidence: RenameConfidence,
    /// Short tag describing the heuristic that matched the
    /// pair. Stable for tooling.
    pub basis: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameConfidence {
    Low,
    Medium,
    High,
}

impl RenameConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Per-category counters for the diff. Useful for dashboards
/// and CI guardrails ("fail the build if a migration would
/// drop more than N columns").
#[derive(Debug, Clone, Default)]
pub struct DiffSummary {
    pub add_columns: usize,
    pub drop_columns: usize,
    pub type_changes: usize,
    pub rename_candidates: usize,
}

// ────────────────────────────────────────────────────────────────
// Core diff entry point
// ────────────────────────────────────────────────────────────────

/// Compute the column-level diff between a live `current`
/// contract and a target list of `CreateColumnDef`s parsed
/// from the embedded `CREATE TABLE` statement.
pub fn compute_column_diff(
    table: &str,
    current: &[DeclaredColumnContract],
    target: &[CreateColumnDef],
) -> SchemaDiff {
    let current_by_name: HashMap<&str, &DeclaredColumnContract> =
        current.iter().map(|c| (c.name.as_str(), c)).collect();
    let target_by_name: HashMap<&str, &CreateColumnDef> =
        target.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut operations: Vec<DiffOp> = Vec::new();
    let mut unpaired_drops: Vec<&DeclaredColumnContract> = Vec::new();
    let mut unpaired_adds: Vec<&CreateColumnDef> = Vec::new();

    // Pass 1: walk target → emit Add or TypeChange.
    for (name, t) in &target_by_name {
        match current_by_name.get(name) {
            None => {
                unpaired_adds.push(t);
            }
            Some(c) => {
                if !column_equivalent(c, t) {
                    operations.push(DiffOp::TypeChange {
                        name: name.to_string(),
                        from: (*c).clone(),
                        to: declared_column_contract_from_create(t),
                    });
                }
            }
        }
    }

    // Pass 2: walk current → emit Drop for anything not in target.
    for (name, c) in &current_by_name {
        if !target_by_name.contains_key(name) {
            unpaired_drops.push(*c);
        }
    }

    // Pass 3: rename detection across unpaired drop ↔ add pairs.
    // We greedy-match each drop against the best candidate add
    // by confidence tier (High > Medium > Low). Once paired,
    // both sides are still emitted as DROP + ADD operations
    // (rename is consultative — clients decide). We only attach
    // a rename hint to the output.
    let rename_candidates = detect_rename_candidates(&unpaired_drops, &unpaired_adds);

    // Emit all unpaired drops + adds as raw DropColumn /
    // AddColumn operations.
    for c in unpaired_drops {
        operations.push(DiffOp::DropColumn(c.name.clone()));
    }
    for t in unpaired_adds {
        operations.push(DiffOp::AddColumn(declared_column_contract_from_create(t)));
    }

    let summary = DiffSummary {
        add_columns: operations
            .iter()
            .filter(|o| matches!(o, DiffOp::AddColumn(_)))
            .count(),
        drop_columns: operations
            .iter()
            .filter(|o| matches!(o, DiffOp::DropColumn(_)))
            .count(),
        type_changes: operations
            .iter()
            .filter(|o| matches!(o, DiffOp::TypeChange { .. }))
            .count(),
        rename_candidates: rename_candidates.len(),
    };
    let drifted = !operations.is_empty();

    SchemaDiff {
        table: table.to_string(),
        drifted,
        operations,
        rename_candidates,
        summary,
    }
}

// ────────────────────────────────────────────────────────────────
// Equivalence
// ────────────────────────────────────────────────────────────────

/// Returns true when a live `current` column and a target
/// `CreateColumnDef` describe the same column shape.
///
/// Comparison rules (must mirror the no-op behavior of
/// `apply_alter_operations_to_contract`):
///
/// 1. **`sql_type`** — primary identity. `current.sql_type` is
///    `Option`; when `None` (legacy contract), fall back to
///    comparing the legacy `data_type` string. Both sides
///    `None` → use `data_type`.
/// 2. **constraint flags** — `not_null`, `unique`,
///    `primary_key`, `compress` must all match exactly.
/// 3. **`default`** — string compare after normalisation
///    (trim + strip surrounding single quotes).
/// 4. **enum_variants** — exact ordered Vec match.
/// 5. **array_element** — string compare.
/// 6. **decimal_precision** — exact match.
///
/// Legacy `data_type` field is intentionally not compared
/// when both sides have `sql_type` — `data_type` is derived
/// from `sql_type` and may differ in case or aliasing
/// without semantic impact.
pub fn column_equivalent(c: &DeclaredColumnContract, t: &CreateColumnDef) -> bool {
    // 1. sql_type / data_type equivalence with Option fallback.
    let type_match = match c.sql_type.as_ref() {
        Some(cur_sql_type) => *cur_sql_type == t.sql_type,
        None => c.data_type.eq_ignore_ascii_case(&t.data_type),
    };
    if !type_match {
        return false;
    }

    // 2. flags
    if c.not_null != t.not_null
        || c.unique != t.unique
        || c.primary_key != t.primary_key
        || c.compress != t.compress
    {
        return false;
    }

    // 3. default — normalize before compare.
    if normalize_default(&c.default) != normalize_default(&t.default) {
        return false;
    }

    // 4. enum_variants — exact ordered.
    if c.enum_variants != t.enum_variants {
        return false;
    }

    // 5. array_element — string compare.
    if c.array_element != t.array_element {
        return false;
    }

    // 6. decimal_precision.
    if c.decimal_precision != t.decimal_precision {
        return false;
    }

    true
}

/// Normalize a default-value string for cross-source comparison.
/// Strips outer whitespace and a single layer of surrounding
/// `'…'` or `"…"` quotes. Returns `None` for empty / missing.
fn normalize_default(d: &Option<String>) -> Option<String> {
    let s = d.as_ref()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = if (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
        || (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    Some(stripped.to_string())
}

// ────────────────────────────────────────────────────────────────
// Rename detection
// ────────────────────────────────────────────────────────────────

/// Walks every (drop, add) cross product and emits a candidate
/// when the heuristic matches. Greedy: an add can only be
/// matched against one drop at a time — once paired, we move on
/// to the next drop. Higher-confidence matches are preferred.
fn detect_rename_candidates(
    drops: &[&DeclaredColumnContract],
    adds: &[&CreateColumnDef],
) -> Vec<RenameCandidate> {
    let mut candidates = Vec::new();
    let mut taken_adds: Vec<bool> = vec![false; adds.len()];

    for drop_col in drops {
        // Find the best (highest-confidence) unpaired add for
        // this drop. Walk twice so we prefer High over Medium
        // over Low without N² sort overhead.
        let mut best: Option<(usize, RenameConfidence, &'static str)> = None;
        for (i, add_col) in adds.iter().enumerate() {
            if taken_adds[i] {
                continue;
            }
            let pair_score = score_rename_pair(drop_col, add_col);
            if let Some((conf, basis)) = pair_score {
                let better = match (&best, conf) {
                    (None, _) => true,
                    (Some((_, prev, _)), new) => confidence_rank(new) > confidence_rank(*prev),
                };
                if better {
                    best = Some((i, conf, basis));
                }
            }
        }

        if let Some((idx, conf, basis)) = best {
            taken_adds[idx] = true;
            candidates.push(RenameCandidate {
                from: drop_col.name.clone(),
                to: adds[idx].name.clone(),
                confidence: conf,
                basis,
            });
        }
    }

    candidates
}

fn confidence_rank(c: RenameConfidence) -> u8 {
    match c {
        RenameConfidence::Low => 1,
        RenameConfidence::Medium => 2,
        RenameConfidence::High => 3,
    }
}

/// Score a (drop, add) pair as a potential rename. Returns
/// `None` when the pair fails the minimum bar (sql_type
/// mismatch). Otherwise returns the strongest tier the pair
/// qualifies for.
fn score_rename_pair(
    drop_col: &DeclaredColumnContract,
    add_col: &CreateColumnDef,
) -> Option<(RenameConfidence, &'static str)> {
    // Minimum bar: sql_type must match.
    let type_match = match drop_col.sql_type.as_ref() {
        Some(cur) => *cur == add_col.sql_type,
        None => drop_col.data_type.eq_ignore_ascii_case(&add_col.data_type),
    };
    if !type_match {
        return None;
    }

    // High: type + every constraint + normalized default match.
    let constraints_match = drop_col.not_null == add_col.not_null
        && drop_col.unique == add_col.unique
        && drop_col.primary_key == add_col.primary_key
        && drop_col.compress == add_col.compress;

    if constraints_match
        && normalize_default(&drop_col.default) == normalize_default(&add_col.default)
    {
        return Some((RenameConfidence::High, "type_match+constraints+default"));
    }

    // Medium: type + every constraint match (default may differ).
    if constraints_match {
        return Some((RenameConfidence::Medium, "type_match+constraints"));
    }

    // Low: only the type matches.
    Some((RenameConfidence::Low, "type_match"))
}

// ────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────

/// Convert a `CreateColumnDef` (parser AST) into a
/// `DeclaredColumnContract` (live contract shape). Mirrors the
/// existing `runtime::impl_ddl::declared_column_contract_from_ddl`
/// — duplicated here to avoid pulling the whole `impl_ddl`
/// module into the diff path's import graph. The two functions
/// must stay in sync; if one grows a field, the other must too.
fn declared_column_contract_from_create(column: &CreateColumnDef) -> DeclaredColumnContract {
    DeclaredColumnContract {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        sql_type: Some(column.sql_type.clone()),
        not_null: column.not_null,
        default: column.default.clone(),
        compress: column.compress,
        unique: column.unique,
        primary_key: column.primary_key,
        enum_variants: column.enum_variants.clone(),
        array_element: column.array_element.clone(),
        decimal_precision: column.decimal_precision,
    }
}

// ────────────────────────────────────────────────────────────────
// SQL formatter
// ────────────────────────────────────────────────────────────────

/// Format a `SchemaDiff` as a copy-paste-friendly SQL string.
///
/// Layout:
///
/// ```text
/// -- EXPLAIN ALTER FOR <table>
/// -- N changes detected (A adds, D drops, T type changes)
/// -- rename candidates: R
/// ALTER TABLE <table> ADD COLUMN <col> <type>;
/// ALTER TABLE <table> DROP COLUMN <col>;
/// -- hint: `from` -> `to` could be a rename (confidence: ..., basis: ...)
/// ```
///
/// When the diff is empty ("no drift"), the function returns
/// just the `-- ` header so clients can grep for "0 changes".
pub fn format_as_sql(diff: &SchemaDiff) -> String {
    let mut out = String::new();
    out.push_str(&format!("-- EXPLAIN ALTER FOR {}\n", diff.table));
    let total = diff.summary.add_columns + diff.summary.drop_columns + diff.summary.type_changes;
    out.push_str(&format!(
        "-- {} changes detected ({} adds, {} drops, {} type changes)\n",
        total, diff.summary.add_columns, diff.summary.drop_columns, diff.summary.type_changes
    ));
    if !diff.rename_candidates.is_empty() {
        out.push_str(&format!(
            "-- rename candidates: {}\n",
            diff.rename_candidates.len()
        ));
    }
    if !diff.drifted {
        out.push_str("-- no drift detected\n");
        return out;
    }

    for op in &diff.operations {
        match op {
            DiffOp::AddColumn(col) => {
                out.push_str(&format!(
                    "ALTER TABLE {} ADD COLUMN {} {};\n",
                    diff.table,
                    col.name,
                    render_column_type(col)
                ));
            }
            DiffOp::DropColumn(name) => {
                out.push_str(&format!(
                    "ALTER TABLE {} DROP COLUMN {};\n",
                    diff.table, name
                ));
            }
            DiffOp::TypeChange { name, to, .. } => {
                // No native ALTER COLUMN ... TYPE in reddb yet,
                // so emit DROP + ADD with a comment that the
                // column was a type change.
                out.push_str(&format!(
                    "-- type change on `{}`: emitting DROP + ADD\n",
                    name
                ));
                out.push_str(&format!(
                    "ALTER TABLE {} DROP COLUMN {};\n",
                    diff.table, name
                ));
                out.push_str(&format!(
                    "ALTER TABLE {} ADD COLUMN {} {};\n",
                    diff.table,
                    name,
                    render_column_type(to)
                ));
            }
        }
    }

    for cand in &diff.rename_candidates {
        out.push_str(&format!(
            "-- hint: `{}` -> `{}` could be a rename (confidence: {}, basis: {})\n",
            cand.from,
            cand.to,
            cand.confidence.as_str(),
            cand.basis
        ));
    }

    out
}

/// Render a column's type modifier suffix for inline SQL.
/// Falls back to the legacy `data_type` string when
/// `sql_type` is absent.
fn render_column_type(col: &DeclaredColumnContract) -> String {
    let base = match col.sql_type.as_ref() {
        Some(t) => t.to_string(),
        None => col.data_type.clone(),
    };
    let mut out = base;
    if col.primary_key {
        out.push_str(" PRIMARY KEY");
    }
    if col.not_null && !col.primary_key {
        out.push_str(" NOT NULL");
    }
    if col.unique && !col.primary_key {
        out.push_str(" UNIQUE");
    }
    if let Some(default) = col.default.as_ref() {
        out.push_str(&format!(" DEFAULT {}", default));
    }
    out
}

// ────────────────────────────────────────────────────────────────
// JSON formatter
// ────────────────────────────────────────────────────────────────

/// Format a `SchemaDiff` as a structured JSON string. Hand-
/// rolled emitter so this module stays free of `serde_json`
/// — reddb's existing `crate::serde_json` module provides
/// the tiny JSON writer the rest of the codebase uses, but
/// here we just produce text directly because the schema is
/// small and stable.
pub fn format_as_json(diff: &SchemaDiff) -> String {
    let sql = format_as_sql(diff);
    let mut out = String::with_capacity(512);
    out.push_str("{\n");
    out.push_str(&format!("  \"table\": {},\n", json_string(&diff.table)));
    out.push_str(&format!("  \"drifted\": {},\n", diff.drifted));
    out.push_str(&format!("  \"sql\": {},\n", json_string(&sql)));
    out.push_str("  \"operations\": [\n");
    for (i, op) in diff.operations.iter().enumerate() {
        let comma = if i + 1 < diff.operations.len() {
            ","
        } else {
            ""
        };
        out.push_str(&format!("    {}{}\n", json_op(op), comma));
    }
    out.push_str("  ],\n");
    out.push_str("  \"rename_candidates\": [\n");
    for (i, cand) in diff.rename_candidates.iter().enumerate() {
        let comma = if i + 1 < diff.rename_candidates.len() {
            ","
        } else {
            ""
        };
        out.push_str(&format!("    {}{}\n", json_rename(cand), comma));
    }
    out.push_str("  ],\n");
    out.push_str(&format!(
        "  \"summary\": {{ \"add_columns\": {}, \"drop_columns\": {}, \"type_changes\": {}, \"rename_candidates\": {} }}\n",
        diff.summary.add_columns,
        diff.summary.drop_columns,
        diff.summary.type_changes,
        diff.summary.rename_candidates
    ));
    out.push_str("}\n");
    out
}

fn json_op(op: &DiffOp) -> String {
    match op {
        DiffOp::AddColumn(col) => format!(
            "{{ \"op\": \"add_column\", \"column\": {}, \"reason\": \"column present in target but not in current contract\" }}",
            json_column(col)
        ),
        DiffOp::DropColumn(name) => format!(
            "{{ \"op\": \"drop_column\", \"name\": {}, \"reason\": \"column present in current contract but not in target\" }}",
            json_string(name)
        ),
        DiffOp::TypeChange { name, from, to } => format!(
            "{{ \"op\": \"type_change\", \"name\": {}, \"from\": {}, \"to\": {} }}",
            json_string(name),
            json_column(from),
            json_column(to)
        ),
    }
}

fn json_column(col: &DeclaredColumnContract) -> String {
    let mut out = String::from("{ ");
    out.push_str(&format!("\"name\": {}", json_string(&col.name)));
    let sql_type = col
        .sql_type
        .as_ref()
        .map(|t| t.to_string())
        .unwrap_or_else(|| col.data_type.clone());
    out.push_str(&format!(", \"sql_type\": {}", json_string(&sql_type)));
    out.push_str(&format!(", \"not_null\": {}", col.not_null));
    out.push_str(&format!(", \"primary_key\": {}", col.primary_key));
    out.push_str(&format!(", \"unique\": {}", col.unique));
    if let Some(default) = col.default.as_ref() {
        out.push_str(&format!(", \"default\": {}", json_string(default)));
    }
    out.push_str(" }");
    out
}

fn json_rename(cand: &RenameCandidate) -> String {
    format!(
        "{{ \"from\": {}, \"to\": {}, \"confidence\": {}, \"basis\": {} }}",
        json_string(&cand.from),
        json_string(&cand.to),
        json_string(cand.confidence.as_str()),
        json_string(cand.basis)
    )
}

/// Minimal JSON string escaper — handles `"` and `\` and
/// control characters. Inline because reddb avoids dragging
/// `serde_json` into the runtime dep graph.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::SqlTypeName;

    fn declared(name: &str, sql_type: &str, not_null: bool) -> DeclaredColumnContract {
        DeclaredColumnContract {
            name: name.to_string(),
            data_type: sql_type.to_string(),
            sql_type: Some(SqlTypeName::new(sql_type)),
            not_null,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        }
    }

    fn target(name: &str, sql_type: &str, not_null: bool) -> CreateColumnDef {
        CreateColumnDef {
            name: name.to_string(),
            data_type: sql_type.to_string(),
            sql_type: SqlTypeName::new(sql_type),
            not_null,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        }
    }

    #[test]
    fn diff_identical_columns_returns_empty() {
        let current = vec![
            declared("id", "TEXT", true),
            declared("name", "TEXT", false),
        ];
        let target = vec![target("id", "TEXT", true), target("name", "TEXT", false)];
        let diff = compute_column_diff("users", &current, &target);
        assert!(!diff.drifted);
        assert!(diff.operations.is_empty());
        assert_eq!(diff.summary.add_columns, 0);
    }

    #[test]
    fn diff_adds_missing_column() {
        let current = vec![declared("id", "TEXT", true)];
        let target = vec![target("id", "TEXT", true), target("name", "TEXT", false)];
        let diff = compute_column_diff("users", &current, &target);
        assert!(diff.drifted);
        assert_eq!(diff.summary.add_columns, 1);
        assert_eq!(diff.summary.drop_columns, 0);
        assert!(matches!(&diff.operations[0], DiffOp::AddColumn(_)));
    }

    #[test]
    fn diff_drops_extra_column() {
        let current = vec![
            declared("id", "TEXT", true),
            declared("legacy", "TEXT", false),
        ];
        let target = vec![target("id", "TEXT", true)];
        let diff = compute_column_diff("users", &current, &target);
        assert!(diff.drifted);
        assert_eq!(diff.summary.add_columns, 0);
        assert_eq!(diff.summary.drop_columns, 1);
        assert!(matches!(&diff.operations[0], DiffOp::DropColumn(_)));
    }

    #[test]
    fn diff_detects_type_change() {
        let current = vec![declared("age", "TEXT", false)];
        let target = vec![target("age", "INTEGER", false)];
        let diff = compute_column_diff("users", &current, &target);
        assert_eq!(diff.summary.type_changes, 1);
        assert_eq!(diff.summary.add_columns, 0);
        assert_eq!(diff.summary.drop_columns, 0);
    }

    #[test]
    fn diff_detects_not_null_change() {
        let current = vec![declared("email", "TEXT", false)];
        let target = vec![target("email", "TEXT", true)];
        let diff = compute_column_diff("users", &current, &target);
        assert_eq!(diff.summary.type_changes, 1);
    }

    #[test]
    fn rename_candidate_medium_confidence() {
        let current = vec![declared("legacy_ts", "TIMESTAMP", false)];
        let target = vec![target("created_at", "TIMESTAMP", false)];
        let diff = compute_column_diff("events", &current, &target);
        // Both DROP + ADD are still emitted.
        assert_eq!(diff.summary.add_columns, 1);
        assert_eq!(diff.summary.drop_columns, 1);
        // Plus a single rename hint at high confidence (defaults match → High).
        assert_eq!(diff.rename_candidates.len(), 1);
        assert_eq!(diff.rename_candidates[0].from, "legacy_ts");
        assert_eq!(diff.rename_candidates[0].to, "created_at");
        assert_eq!(diff.rename_candidates[0].confidence, RenameConfidence::High);
    }

    #[test]
    fn rename_candidate_low_confidence_constraints_differ() {
        let current = vec![declared("legacy", "TEXT", false)];
        let target = vec![target("renamed", "TEXT", true)]; // not_null differs
        let diff = compute_column_diff("t", &current, &target);
        assert_eq!(diff.rename_candidates.len(), 1);
        assert_eq!(diff.rename_candidates[0].confidence, RenameConfidence::Low);
    }

    #[test]
    fn no_rename_when_type_differs() {
        let current = vec![declared("legacy", "TEXT", false)];
        let target = vec![target("renamed", "INTEGER", false)];
        let diff = compute_column_diff("t", &current, &target);
        assert!(diff.rename_candidates.is_empty());
    }

    #[test]
    fn legacy_contract_without_sql_type_falls_back_to_data_type() {
        // Current contract carries `sql_type: None` (legacy table).
        let mut c = declared("id", "TEXT", true);
        c.sql_type = None;
        let current = vec![c];
        let target = vec![target("id", "TEXT", true)];
        let diff = compute_column_diff("users", &current, &target);
        assert!(!diff.drifted, "legacy data_type should match TEXT target");
    }

    #[test]
    fn format_sql_output_shape() {
        let current = vec![declared("id", "TEXT", true)];
        let target = vec![target("id", "TEXT", true), target("name", "TEXT", false)];
        let diff = compute_column_diff("users", &current, &target);
        let sql = format_as_sql(&diff);
        assert!(sql.contains("-- EXPLAIN ALTER FOR users"));
        assert!(sql.contains("ALTER TABLE users ADD COLUMN name TEXT"));
    }

    #[test]
    fn format_json_output_shape() {
        let current = vec![declared("id", "TEXT", true)];
        let target = vec![target("id", "TEXT", true), target("name", "TEXT", false)];
        let diff = compute_column_diff("users", &current, &target);
        let json = format_as_json(&diff);
        assert!(json.contains("\"drifted\": true"));
        assert!(json.contains("\"add_column\""));
        assert!(json.contains("\"summary\""));
    }

    #[test]
    fn empty_diff_renders_no_drift_marker() {
        let current = vec![declared("id", "TEXT", true)];
        let target = vec![target("id", "TEXT", true)];
        let diff = compute_column_diff("users", &current, &target);
        let sql = format_as_sql(&diff);
        assert!(sql.contains("-- no drift detected"));
    }
}
