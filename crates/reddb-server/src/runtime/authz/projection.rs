//! Relational SELECT/JOIN column authorization extracted from
//! `impl_core` (issue #1622, PRD #1619). Behaviour-preserving move; the
//! free policy-column helpers these methods consume live in
//! [`super::policy_columns`].
use super::super::impl_core::{inject_rls_filters, inject_rls_into_join};
use super::super::*;
use super::policy_columns::*;
use crate::auth::column_policy_gate::ColumnAccessRequest;
use crate::auth::UserId;
use crate::storage::query::ast::TableSource;

impl RedDBRuntime {
    /// Apply table-level read authorization and RLS rewriting for a
    /// relational SELECT leaf.
    pub(crate) fn authorize_relational_table_select(
        &self,
        mut table: TableQuery,
        frame: &dyn super::super::statement_frame::ReadFrame,
    ) -> RedDBResult<Option<TableQuery>> {
        if let Some(TableSource::Subquery(inner)) = table.source.take() {
            let authorized_inner = self.authorize_relational_select_expr(*inner, frame)?;
            table.source = Some(TableSource::Subquery(Box::new(authorized_inner)));
            return Ok(Some(table));
        }

        self.check_table_column_projection_authz(&table, frame)?;

        if self.inner.rls_enabled_tables.read().contains(&table.table) {
            return Ok(inject_rls_filters(self, frame, table));
        }

        Ok(Some(table))
    }

    pub(crate) fn authorize_relational_join_select(
        &self,
        mut join: JoinQuery,
        frame: &dyn super::super::statement_frame::ReadFrame,
    ) -> RedDBResult<Option<JoinQuery>> {
        self.check_join_column_projection_authz(&join, frame)?;
        join.left = Box::new(self.authorize_relational_join_child(*join.left, frame)?);
        join.right = Box::new(self.authorize_relational_join_child(*join.right, frame)?);
        Ok(inject_rls_into_join(self, frame, join))
    }

    pub(crate) fn authorize_relational_join_child(
        &self,
        expr: QueryExpr,
        frame: &dyn super::super::statement_frame::ReadFrame,
    ) -> RedDBResult<QueryExpr> {
        match expr {
            QueryExpr::Table(mut table) => {
                if let Some(TableSource::Subquery(inner)) = table.source.take() {
                    let authorized_inner = self.authorize_relational_select_expr(*inner, frame)?;
                    table.source = Some(TableSource::Subquery(Box::new(authorized_inner)));
                }
                Ok(QueryExpr::Table(table))
            }
            QueryExpr::Join(join) => self
                .authorize_relational_join_select(join, frame)?
                .map(QueryExpr::Join)
                .ok_or_else(|| {
                    RedDBError::Query("permission denied: RLS denied relational subquery".into())
                }),
            other => Ok(other),
        }
    }

    pub(crate) fn authorize_relational_select_expr(
        &self,
        expr: QueryExpr,
        frame: &dyn super::super::statement_frame::ReadFrame,
    ) -> RedDBResult<QueryExpr> {
        match expr {
            QueryExpr::Table(table) => self
                .authorize_relational_table_select(table, frame)?
                .map(QueryExpr::Table)
                .ok_or_else(|| {
                    RedDBError::Query("permission denied: RLS denied relational subquery".into())
                }),
            QueryExpr::Join(join) => self
                .authorize_relational_join_select(join, frame)?
                .map(QueryExpr::Join)
                .ok_or_else(|| {
                    RedDBError::Query("permission denied: RLS denied relational subquery".into())
                }),
            other => Ok(other),
        }
    }

    pub(crate) fn check_table_column_projection_authz(
        &self,
        table: &TableQuery,
        frame: &dyn super::super::statement_frame::ReadFrame,
    ) -> RedDBResult<()> {
        // A source-free scalar SELECT (`SELECT CURRENT_USER()`, `SELECT 1`,
        // ...) parses to the synthetic `any` table but reads no user rows,
        // so table-read authorization does not apply. Secret/KV/CONFIG
        // scalars carry their own action gates enforced at execution time,
        // so skipping the table gate here does not weaken those.
        if super::super::query_exec::table_query_is_implicit_scalar_select(table) {
            return Ok(());
        }
        let Some((username, role)) = frame.identity() else {
            return Ok(());
        };
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };

        let columns = self.resolved_table_projection_columns(table)?;
        let request = ColumnAccessRequest::select(table.table.clone(), columns);
        let principal = UserId::from_parts(frame.effective_scope(), username);
        let ctx = runtime_iam_context(role, frame.effective_scope());
        let outcome = auth_store.check_column_projection_authz(&principal, &request, &ctx);
        if outcome.allowed() {
            return Ok(());
        }

        if let Some(denied) = outcome.first_denied_column() {
            return Err(RedDBError::Query(format!(
                "permission denied: principal=`{username}` cannot select column `{}`",
                denied.resource.name
            )));
        }
        Err(RedDBError::Query(format!(
            "permission denied: principal=`{username}` cannot select table `{}`",
            table.table
        )))
    }

    pub(crate) fn check_join_column_projection_authz(
        &self,
        join: &JoinQuery,
        frame: &dyn super::super::statement_frame::ReadFrame,
    ) -> RedDBResult<()> {
        let mut by_table: HashMap<String, BTreeSet<String>> = HashMap::new();
        let projections = crate::storage::query::sql_lowering::effective_join_projections(join);
        self.collect_join_projection_columns(join, &projections, &mut by_table)?;

        for (table, columns) in by_table {
            let query = TableQuery {
                table,
                source: None,
                alias: None,
                select_items: Vec::new(),
                columns: columns.into_iter().map(Projection::Column).collect(),
                where_expr: None,
                filter: None,
                group_by_exprs: Vec::new(),
                group_by: Vec::new(),
                having_expr: None,
                having: None,
                order_by: Vec::new(),
                limit: None,
                limit_param: None,
                offset: None,
                offset_param: None,
                expand: None,
                as_of: None,
                sessionize: None,
                distinct: false,
            };
            self.check_table_column_projection_authz(&query, frame)?;
        }
        Ok(())
    }

    pub(crate) fn collect_join_projection_columns(
        &self,
        join: &JoinQuery,
        projections: &[Projection],
        out: &mut HashMap<String, BTreeSet<String>>,
    ) -> RedDBResult<()> {
        let left = table_side_context(join.left.as_ref());
        let right = table_side_context(join.right.as_ref());

        if projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
        {
            for side in [left.as_ref(), right.as_ref()].into_iter().flatten() {
                out.entry(side.table.clone())
                    .or_default()
                    .extend(self.table_all_projection_columns(&side.table)?);
            }
            return Ok(());
        }

        for projection in projections {
            collect_projection_columns_for_join_side(
                projection,
                left.as_ref(),
                right.as_ref(),
                out,
            )?;
        }
        Ok(())
    }

    pub(crate) fn resolved_table_projection_columns(
        &self,
        table: &TableQuery,
    ) -> RedDBResult<Vec<String>> {
        let projections = crate::storage::query::sql_lowering::effective_table_projections(table);
        if projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
        {
            return self.table_all_projection_columns(&table.table);
        }

        let mut columns = BTreeSet::new();
        for projection in &projections {
            collect_projection_columns_for_table(
                projection,
                &table.table,
                table.alias.as_deref(),
                &mut columns,
            );
        }
        Ok(columns.into_iter().collect())
    }

    pub(crate) fn table_all_projection_columns(&self, table: &str) -> RedDBResult<Vec<String>> {
        if let Some(contract) = self.inner.db.collection_contract_arc(table) {
            let columns: Vec<String> = contract
                .declared_columns
                .iter()
                .map(|column| column.name.clone())
                .collect();
            if !columns.is_empty() {
                return Ok(columns);
            }
        }

        let records = scan_runtime_table_source_records_limited(&self.inner.db, table, Some(1))?;
        Ok(records
            .first()
            .map(|record| {
                record
                    .column_names()
                    .into_iter()
                    .map(|column| column.to_string())
                    .collect()
            })
            .unwrap_or_default())
    }
}
