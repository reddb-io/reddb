//! Canonical native aggregate result assembly over admitted group states.
//! Hosts retain materialization admission, expression effects and secret output policy.
use crate::query::planning::aggregation::{visible_aggregate_output_name, AggregateRuntimePlan};
use crate::query::runtime::aggregation::{
    aggregate_projection_result_with_host, AggregateHost, CompiledAggPlan, SlottedAggState,
};
use crate::query::runtime::projection::{
    expression_uses_secret, projection_name, projection_uses_secret,
};
use crate::query::unified::{UnifiedRecord, UnifiedResult};
use reddb_rql::ast::{Filter, OrderByClause, Projection, TableQuery};
use reddb_types::Value;

pub trait AggregateResultHost: AggregateHost {
    type Error;
    fn guard_materialization(&self, groups: usize) -> Result<(), Self::Error>;
    fn having_matches(&self, filter: &Filter, record: &UnifiedRecord) -> bool;
    fn sort_records(&self, records: &mut Vec<UnifiedRecord>, clauses: &[OrderByClause]);
    fn try_sort_records(
        &self,
        records: &mut Vec<UnifiedRecord>,
        clauses: &[OrderByClause],
    ) -> Result<(), Self::Error> {
        self.sort_records(records, clauses);
        Ok(())
    }
    fn secret_query_output(&self, value: Value) -> Result<Value, Self::Error>;
}

/// Prepared, admitted query metadata. Slot states must belong to `slots`.
/// The runtime plan contains resolved group bindings. A sensitivity source summarizes all
/// admitted secret inputs, including rows outside the final result page.
pub struct AggregateResultContext<'a> {
    pub query: &'a TableQuery,
    pub visible_projections: &'a [Projection],
    pub runtime_plan: &'a AggregateRuntimePlan,
    pub slots: &'a CompiledAggPlan,
    pub sensitivity_source: Option<&'a UnifiedRecord>,
}

/// Borrow group state without cloning aggregate buffers or introducing a spill format.
pub struct AggregateGroupRef<'a> {
    pub group_values: &'a [Value],
    pub state: &'a SlottedAggState,
}

pub fn finish_aggregate_results_with_host<'a, H: AggregateResultHost>(
    host: &H,
    context: AggregateResultContext<'_>,
    groups: impl ExactSizeIterator<Item = AggregateGroupRef<'a>>,
) -> Result<UnifiedResult, H::Error> {
    let group_count = groups.len();
    host.guard_materialization(group_count)?;
    let AggregateResultContext {
        query,
        visible_projections: effective_projections,
        runtime_plan,
        slots: agg_plan,
        sensitivity_source,
    } = context;
    let has_group_by = !runtime_plan.group_bindings.is_empty();

    // Build result records from accumulated groups
    let mut records = Vec::with_capacity(group_count.max(1));
    let mut columns = Vec::new();

    for group in groups {
        let mut record = UnifiedRecord::new();

        // Add GROUP BY columns
        if has_group_by {
            for (index, binding) in runtime_plan.group_bindings.iter().enumerate() {
                let Some(value) = group.group_values.get(index).cloned() else {
                    continue;
                };
                let label = &binding.output_label;
                if !columns.contains(label) {
                    columns.push(label.clone());
                }
                record.set(binding.key.as_deref().unwrap_or(label), value.clone());
                record.set(label, value);
            }
        }

        // Add visible aggregate results
        for proj in effective_projections {
            if let Some((result_name, result_val)) =
                aggregate_projection_result_with_host(host, proj, group.state, agg_plan)
            {
                if !columns.contains(&result_name) {
                    columns.push(result_name.clone());
                }
                record.set(&result_name, result_val);
            }
        }

        for proj in &runtime_plan.hidden_aggregates {
            if let Some((result_name, result_val)) =
                aggregate_projection_result_with_host(host, proj, group.state, agg_plan)
            {
                record.set(&result_name, result_val);
            }
        }

        if runtime_plan
            .having
            .as_ref()
            .is_none_or(|filter| host.having_matches(filter, &record))
        {
            records.push(record);
        }
    }

    // If no input rows matched, return a single aggregate row.
    let empty_state = SlottedAggState::new(agg_plan);
    if group_count == 0 && !has_group_by {
        let mut record = UnifiedRecord::new();
        for proj in effective_projections {
            if let Some((result_name, result_val)) =
                aggregate_projection_result_with_host(host, proj, &empty_state, agg_plan)
            {
                if !columns.contains(&result_name) {
                    columns.push(result_name.clone());
                }
                record.set(&result_name, result_val);
            }
        }
        for proj in &runtime_plan.hidden_aggregates {
            if let Some((result_name, result_val)) =
                aggregate_projection_result_with_host(host, proj, &empty_state, agg_plan)
            {
                record.set(&result_name, result_val);
            }
        }
        if runtime_plan
            .having
            .as_ref()
            .is_none_or(|filter| host.having_matches(filter, &record))
        {
            records.push(record);
        }
    }

    if !runtime_plan.order_by.is_empty() {
        host.try_sort_records(&mut records, &runtime_plan.order_by)?;
    }

    if let Some(offset) = query.offset {
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        if offset < records.len() {
            records = records.into_iter().skip(offset).collect();
        } else {
            records.clear();
        }
    }

    if let Some(limit) = query.limit {
        records.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }

    if let Some(sensitivity_source) = sensitivity_source {
        for record in &mut records {
            for projection in effective_projections
                .iter()
                .chain(&runtime_plan.hidden_aggregates)
            {
                if projection_uses_secret(
                    projection,
                    sensitivity_source,
                    Some(&query.table),
                    query.alias.as_deref().or(Some(&query.table)),
                ) {
                    let label = visible_aggregate_output_name(projection)
                        .map(|(_, label)| label)
                        .unwrap_or_else(|| projection_name(projection));
                    if let Some(value) = record.get(&label).cloned() {
                        record.set(&label, host.secret_query_output(value)?);
                    }
                }
            }
            for binding in &runtime_plan.group_bindings {
                if expression_uses_secret(
                    &binding.expression,
                    sensitivity_source,
                    Some(&query.table),
                    query.alias.as_deref().or(Some(&query.table)),
                ) {
                    let label = &binding.output_label;
                    if let Some(value) = record.get(label).cloned() {
                        record.set(label, host.secret_query_output(value)?);
                    }
                }
            }
        }
    }
    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}
