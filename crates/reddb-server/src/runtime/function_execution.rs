//! CALL runs under the invoker identity through the ordinary statement pipeline.
use std::sync::atomic::{AtomicU64, Ordering};

use reddb_rql::ast::{Expr, QueryExpr};
use reddb_rql::stored_function::{FunctionCommand, FunctionEffect, FunctionReturn};
use reddb_types::Value;

use super::function_catalog::CompiledFunction;
use super::function_validation::{error, normalize, validate_expression, validate_statement};
use super::impl_core::{current_connection_id, current_tenant};
use super::{RedDBRuntime, RuntimeQueryResult};
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::RedDBResult;

static SAVEPOINT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const RETURN_ROWS_MAX: usize = 10_000;

impl RedDBRuntime {
    pub(super) fn execute_function(
        &self,
        source: &str,
        command: &FunctionCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        let tenant = current_tenant();
        match command {
            FunctionCommand::Create {
                definition,
                replace,
            } => {
                self.check_function_ddl_context()?;
                let compiled = CompiledFunction::compile(definition.clone())?;
                self.inner
                    .functions
                    .write()
                    .define(&self.db(), tenant, compiled, *replace)?;
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    source.to_string(),
                    "function stored",
                    "create_function",
                ))
            }
            FunctionCommand::Drop { name, if_exists } => {
                self.check_function_ddl_context()?;
                self.inner
                    .functions
                    .write()
                    .remove(&self.db(), tenant, name, *if_exists)?;
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    source.to_string(),
                    "function dropped",
                    "drop_function",
                ))
            }
            FunctionCommand::Show { name } => {
                let functions = self.inner.functions.read().list(tenant.as_deref())?;
                let mut result =
                    UnifiedResult::with_columns(vec!["name".into(), "effect".into(), "ddl".into()]);
                for function in functions {
                    if name
                        .as_ref()
                        .is_some_and(|name| *name != function.definition.name)
                    {
                        continue;
                    }
                    let command = QueryExpr::Function(Box::new(FunctionCommand::Call {
                        name: function.definition.name.clone(),
                        arguments: Vec::new(),
                    }));
                    if let Err(cause) = self.check_query_privilege(&command) {
                        if name.is_some() {
                            return Err(error(cause));
                        }
                        continue;
                    }
                    let mut record = UnifiedRecord::default();
                    record.set("name", Value::text(function.definition.name.as_str()));
                    record.set(
                        "effect",
                        Value::text(
                            format!("{:?}", function.definition.effect).to_ascii_lowercase(),
                        ),
                    );
                    record.set("ddl", Value::text(function.source.as_str()));
                    result.records.push(record);
                }
                if name.is_some() && result.records.is_empty() {
                    return Err(error("function does not exist"));
                }
                let mut response = RuntimeQueryResult::ok_message(
                    source.to_string(),
                    "functions",
                    "show_functions",
                );
                response.result = result;
                Ok(response)
            }
            FunctionCommand::Call { name, arguments } => {
                let function = self
                    .inner
                    .functions
                    .read()
                    .get(tenant.as_deref(), name)?
                    .ok_or_else(|| error("function does not exist in this tenant"))?;
                self.call_function(source, &function, arguments)
            }
        }
    }

    fn check_function_ddl_context(&self) -> RedDBResult<()> {
        self.check_write(super::write_gate::WriteKind::Ddl)?;
        if self
            .inner
            .transaction_state
            .in_transaction(current_connection_id())
        {
            return Err(error("function DDL requires autocommit; transactional catalog changes are not yet supported"));
        }
        Ok(())
    }

    fn call_function(
        &self,
        source: &str,
        function: &CompiledFunction,
        arguments: &[Expr],
    ) -> RedDBResult<RuntimeQueryResult> {
        if arguments.len() != function.definition.parameters.len() {
            return Err(error(format!(
                "expected {} arguments, got {}",
                function.definition.parameters.len(),
                arguments.len()
            )));
        }
        let budget = super::function_budget::Scope::enter(
            self.config_u64("functions.execution.work_max", 1_000_000)
                .min(1_000_000),
            self.config_u64("functions.execution.timeout_ms", 5_000)
                .min(5_000),
        )?;
        let parameters = arguments
            .iter()
            .zip(&function.definition.parameters)
            .map(|(argument, parameter)| {
                validate_expression(argument)?;
                let value = crate::storage::query::evaluator::evaluate(
                    argument,
                    &|_: &reddb_rql::ast::FieldRef| None,
                )
                .map_err(error)?;
                normalize(
                    &function.definition.name,
                    &parameter.name,
                    &parameter.sql_type,
                    value,
                )
            })
            .collect::<RedDBResult<Vec<_>>>()?;

        // Pure bodies cannot access storage. Other effects pin one SI snapshot,
        // or enter a savepoint in the invoker's existing transaction.
        let transactional = function.definition.effect != FunctionEffect::Pure;
        let owns_transaction = transactional
            && !self
                .inner
                .transaction_state
                .in_transaction(current_connection_id());
        let savepoint = if transactional && !owns_transaction {
            let name = loop {
                let name = format!(
                    "__rql_function_{}",
                    SAVEPOINT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                );
                if !self
                    .inner
                    .transaction_state
                    .has_savepoint(current_connection_id(), &name)
                {
                    break name;
                }
            };
            Some(name)
        } else {
            None
        };
        if owns_transaction {
            self.execute_query("BEGIN")?;
        }
        if let Some(name) = &savepoint {
            self.execute_query(&format!("SAVEPOINT {name}"))?;
        }

        let execution = self.execute_function_body(function, &parameters);
        // Deadline/result checks happen before success, but commit and rollback
        // must finish even after the execution budget has been exhausted.
        let execution = super::function_budget::charge(0).and(execution);
        drop(budget);
        let mut result = match execution {
            Ok(mut result) => {
                if let Some(name) = &savepoint {
                    self.execute_query(&format!("RELEASE SAVEPOINT {name}"))?;
                }
                if owns_transaction {
                    result.bookmark = self.execute_query("COMMIT")?.bookmark;
                }
                result
            }
            Err(cause) => {
                if let Some(name) = &savepoint {
                    // Runtime rollback_to_savepoint removes the named savepoint
                    // as well as its children; releasing again masks the cause.
                    self.execute_query(&format!("ROLLBACK TO SAVEPOINT {name}"))?;
                }
                if owns_transaction {
                    self.execute_query("ROLLBACK")?;
                }
                return Err(cause);
            }
        };
        result.query = source.to_string();
        result.engine = "runtime-function";
        result.statement = "call";
        // Never cache CALL results: schema replacement and nested writes are
        // resolved afresh, even when the final statement happens to be SELECT.
        result.statement_type = "call";
        if function.definition.effect == FunctionEffect::Write {
            self.invalidate_result_cache();
        }
        Ok(result)
    }

    fn execute_function_body(
        &self,
        function: &CompiledFunction,
        parameters: &[Value],
    ) -> RedDBResult<RuntimeQueryResult> {
        let mut last = None;
        let mut affected_rows = 0u64;
        for (source, expression) in &function.statements {
            super::function_budget::charge(0)?;
            super::function_budget::charge(1)?;
            let bound = crate::storage::query::user_params::bind_available_parameters(
                expression, parameters,
            )
            .map_err(error)?;
            let bound = self.rewrite_view_refs(bound);
            validate_statement(&bound, function.definition.effect)?;
            let result = self.execute_prepared_query(source, bound)?;
            super::function_budget::charge(0)?;
            super::function_budget::charge(result.affected_rows)?;
            if result.result.records.len() > RETURN_ROWS_MAX {
                return Err(error("statement result exceeds 10000 rows"));
            }
            affected_rows = affected_rows.saturating_add(result.affected_rows);
            last = Some(result);
        }
        let mut result = last.expect("validated function has at least one statement");
        result.affected_rows = affected_rows;
        let mut output = UnifiedResult::empty();
        match &function.definition.returns {
            FunctionReturn::Scalar(sql_type) => {
                if result.result.records.len() != 1 || result.result.columns.len() != 1 {
                    return Err(error(
                        "scalar return requires exactly one row and one column",
                    ));
                }
                let value = result.result.records[0]
                    .get(&result.result.columns[0])
                    .ok_or_else(|| error("missing scalar result"))?
                    .clone();
                let value = normalize(&function.definition.name, "return", sql_type, value)?;
                output.columns.push("value".into());
                let mut record = UnifiedRecord::default();
                record.set("value", value);
                output.records.push(record);
            }
            FunctionReturn::Table(columns) => {
                output.columns = columns.iter().map(|column| column.name.clone()).collect();
                for record in &result.result.records {
                    super::function_budget::charge(1)?;
                    let mut projected = UnifiedRecord::default();
                    for column in columns {
                        let value = record
                            .get(&column.name)
                            .ok_or_else(|| {
                                error(format!("missing return field '{}'", column.name))
                            })?
                            .clone();
                        projected.set(
                            &column.name,
                            normalize(
                                &function.definition.name,
                                &column.name,
                                &column.sql_type,
                                value,
                            )?,
                        );
                    }
                    output.records.push(projected);
                }
            }
        }
        output.stats = result.result.stats;
        result.result = output;
        Ok(result)
    }
}
