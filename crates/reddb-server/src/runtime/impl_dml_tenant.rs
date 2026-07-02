//! DML tenant-column injection helpers extracted from `impl_dml`.
//!
//! Behaviour-preserving move (issue #1634). Names and behaviour are unchanged
//! from `impl_dml`; the only adjustment is `pub(super)` visibility on
//! `maybe_inject_tenant_column` so the sibling `impl_dml` INSERT path keeps
//! calling it by bare name. The dotted-path helpers `dotted_tail_already_set`
//! / `merge_dotted_tenant` live in `impl_dml_support`.

use super::impl_dml_support::*;
use super::*;

impl RedDBRuntime {
    /// Phase 2.5.4: inject `CURRENT_TENANT()` into an INSERT when the
    /// target table is tenant-scoped and the user's column list does
    /// not already name the tenant column.
    ///
    /// Returns:
    /// * `Ok(None)` — no injection needed (non-tenant table, or user
    ///   supplied the column explicitly). Caller uses the original
    ///   query unchanged.
    /// * `Ok(Some(augmented))` — a cloned query with the tenant column
    ///   + literal value appended to every row.
    /// * `Err(..)` — table is tenant-scoped but no tenant is bound to
    ///   the current session. Fails loudly so callers don't produce
    ///   rows that RLS would then hide on read.
    pub(super) fn maybe_inject_tenant_column(
        &self,
        query: &InsertQuery,
    ) -> RedDBResult<Option<InsertQuery>> {
        let Some(tenant_col) = self.tenant_column(&query.table) else {
            return Ok(None);
        };
        // User already named the column (literal match) — trust them.
        if query
            .columns
            .iter()
            .any(|c| c.eq_ignore_ascii_case(&tenant_col))
        {
            return Ok(None);
        }

        // Phase 2 PG parity: dotted-path tenancy. When `tenant_col` is a
        // nested key like `headers.tenant` we operate on the root
        // column (`headers`) and set / add the nested path inside its
        // JSON value. If the user named the root column we mutate in
        // place; otherwise we create a fresh JSON column for every row.
        if let Some(dot_pos) = tenant_col.find('.') {
            let (root, tail) = tenant_col.split_at(dot_pos);
            let tail = &tail[1..]; // drop leading '.'
            return self.inject_dotted_tenant(query, root, tail);
        }

        let Some(tenant_id) = crate::runtime::impl_core::current_tenant() else {
            return Err(RedDBError::Query(format!(
                "INSERT into tenant-scoped table '{}' requires an active tenant — \
                 run SET TENANT '<id>' first or name column '{}' explicitly",
                query.table, tenant_col
            )));
        };

        let mut augmented = query.clone();
        augmented.columns.push(tenant_col);
        let lit = Value::text(tenant_id.clone());
        for row in augmented.values.iter_mut() {
            row.push(lit.clone());
        }
        for row in augmented.value_exprs.iter_mut() {
            row.push(crate::storage::query::ast::Expr::Literal {
                value: lit.clone(),
                span: crate::storage::query::ast::Span::synthetic(),
            });
        }
        Ok(Some(augmented))
    }

    /// Dotted-path auto-fill — set `root.tail` to `CURRENT_TENANT()` on
    /// every row. Mirrors `maybe_inject_tenant_column` but mutates
    /// nested JSON instead of appending a flat column.
    ///
    /// Cases:
    /// * Root column already in the INSERT list → mutate per-row JSON
    ///   (parse, set path, re-serialize).
    /// * Root column absent → create a fresh `{tail: tenant}` JSON
    ///   object and append the root column to the INSERT.
    fn inject_dotted_tenant(
        &self,
        query: &InsertQuery,
        root: &str,
        tail: &str,
    ) -> RedDBResult<Option<InsertQuery>> {
        let active_tenant = crate::runtime::impl_core::current_tenant();
        let mut augmented = query.clone();
        let root_idx = augmented
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(root));

        if let Some(idx) = root_idx {
            // User supplied the root column. Per-row: if the dotted
            // tail is already present we trust the user (admin / bulk
            // loader scenario); otherwise fill from the active
            // tenant. An unbound tenant is only an error when some
            // row actually needs filling.
            for row in augmented.values.iter_mut() {
                let Some(slot) = row.get_mut(idx) else {
                    continue;
                };
                if dotted_tail_already_set(slot, tail) {
                    continue;
                }
                let Some(tenant_id) = &active_tenant else {
                    return Err(RedDBError::Query(format!(
                        "INSERT into tenant-scoped table '{}' requires an active tenant — \
                         run SET TENANT '<id>' first or set '{}.{}' explicitly in each row",
                        query.table, root, tail
                    )));
                };
                *slot = merge_dotted_tenant(slot.clone(), tail, tenant_id)?;
            }
            // Expression row is kept in sync by re-wrapping the
            // mutated literal; the canonical path will re-evaluate
            // against the same JSON shape.
            for (row_idx, row) in augmented.value_exprs.iter_mut().enumerate() {
                if let Some(slot) = row.get_mut(idx) {
                    let new_value = augmented
                        .values
                        .get(row_idx)
                        .and_then(|v| v.get(idx))
                        .cloned()
                        .unwrap_or(Value::Null);
                    *slot = crate::storage::query::ast::Expr::Literal {
                        value: new_value,
                        span: crate::storage::query::ast::Span::synthetic(),
                    };
                }
            }
        } else {
            // No root column in the INSERT list — auto-fill needs a
            // bound tenant to synthesise one. Error loud so we never
            // create a tenant-less row that RLS would then hide.
            let Some(tenant_id) = &active_tenant else {
                return Err(RedDBError::Query(format!(
                    "INSERT into tenant-scoped table '{}' requires an active tenant — \
                     run SET TENANT '<id>' first or name path '{}.{}' explicitly",
                    query.table, root, tail
                )));
            };
            // Create a fresh JSON column with only the tenant path set.
            augmented.columns.push(root.to_string());
            let fresh = merge_dotted_tenant(Value::Null, tail, tenant_id)?;
            for row in augmented.values.iter_mut() {
                row.push(fresh.clone());
            }
            for row in augmented.value_exprs.iter_mut() {
                row.push(crate::storage::query::ast::Expr::Literal {
                    value: fresh.clone(),
                    span: crate::storage::query::ast::Span::synthetic(),
                });
            }
        }

        Ok(Some(augmented))
    }
}
