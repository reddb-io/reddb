use super::{query_error, Result};
use crate::{
    catalog::{
        enforcement::{CollectionContractEnforcer, ContractError, INTERNAL_TTL_METADATA_KEYS},
        schema::resolved_contract_columns,
        CollectionContract, CollectionModel,
    },
    query::{
        evaluator,
        local_read::{self, host::ReadHost, LocalReadError, LOCAL_READ_ROWS_MAX},
        planning::aggregation::validate_expression_depth,
        records::runtime_table_record_from_entity_ref_with_schema,
    },
    storage::{
        entity::{
            compute_entity_field_bloom, EntityData, EntityId, RowData, UnifiedEntity,
            FIRST_USER_ENTITY_ID,
        },
        resident_store::ResidentStore,
        store_wal::StoreWalAction,
    },
};
use reddb_rql::ast::{
    CreateTableQuery, Expr, FieldRef, InsertEntityType, QueryExpr, SelectItem, TableQuery,
    UpdateTarget,
};
use reddb_types::{DataType, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{atomic::Ordering, Arc},
};

fn unsupported() -> LocalReadError {
    LocalReadError::Unsupported("local writes require ordinary tables without TTL, AI/events, encrypted columns, RETURNING or conflict clauses")
}
fn contract_error(error: ContractError) -> LocalReadError {
    query_error(error.to_string())
}
fn public_name(name: &str) -> Result<()> {
    if name.starts_with('_')
        || name.starts_with("red.")
        || crate::query::runtime::is_universal_entity_source(name)
    {
        return Err(unsupported());
    }
    Ok(())
}
fn user_column(name: &str) -> Result<()> {
    if name.starts_with('_')
        || [
            "rid",
            "entity_id",
            "collection",
            "kind",
            "tenant",
            "created_at",
            "updated_at",
        ]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        return Err(query_error(format!("reserved system field '{name}'")));
    }
    Ok(())
}

pub(super) fn admit_create(query: &CreateTableQuery) -> Result<()> {
    public_name(&query.name)?;
    if query.collection_model != CollectionModel::Table
        || query.default_ttl_ms.is_some()
        || query.ai_policy.is_some()
        || !query.subscriptions.is_empty()
        || query.partition_by.is_some()
        || query.tenant_by.is_some()
        || query.context_index_enabled
        || !query.context_index_fields.is_empty()
        || !query.analytics_config.is_empty()
        || !query.metrics_rollup_policies.is_empty()
        || query.vault_own_master_key
    {
        return Err(unsupported());
    }
    for column in &query.columns {
        user_column(&column.name)?;
    }
    let contract = crate::catalog::ddl::collection_contract_from_create_table(query, 0)
        .map_err(query_error)?;
    admit_contract(&contract)
}

fn admit_contract(contract: &CollectionContract) -> Result<()> {
    if contract.declared_model != CollectionModel::Table
        || contract.default_ttl_ms.is_some()
        || contract.retention_duration_ms.is_some()
        || contract.ai_policy.is_some()
        || !contract.subscriptions.is_empty()
        || contract.context_index_enabled
        || !contract.context_index_fields.is_empty()
    {
        return Err(unsupported());
    }
    for column in resolved_contract_columns(contract).map_err(|error| query_error(error.message))? {
        if matches!(column.data_type, DataType::Secret | DataType::Password) {
            return Err(unsupported());
        }
    }
    Ok(())
}

pub(super) fn plan(
    statement: &QueryExpr,
    resident: &ResidentStore,
    contracts: &HashMap<String, CollectionContract>,
    now_ms: u64,
) -> Result<Vec<StoreWalAction>> {
    let (table, expressions): (&str, Vec<&Expr>) = match statement {
        QueryExpr::Insert(query) => {
            if query.entity_type != InsertEntityType::Row
                || query.on_conflict.is_some()
                || query.returning.is_some()
                || query.ttl_ms.is_some()
                || query.expires_at_ms.is_some()
                || !query.with_metadata.is_empty()
                || query.auto_embed.is_some()
                || query.suppress_events
            {
                return Err(unsupported());
            }
            (&query.table, query.value_exprs.iter().flatten().collect())
        }
        QueryExpr::Update(query) => {
            if query.target != UpdateTarget::Rows
                || query.returning.is_some()
                || query.ttl_ms.is_some()
                || query.expires_at_ms.is_some()
                || !query.with_metadata.is_empty()
                || query.claim_limit.is_some()
                || query.claim_exact
                || !query.order_by.is_empty()
                || query.limit.is_some()
                || query.suppress_events
                || query.compound_assignment_ops.iter().any(Option::is_some)
                || (query.filter.is_some() && query.where_expr.is_none())
            {
                return Err(unsupported());
            }
            (
                &query.table,
                query
                    .assignment_exprs
                    .iter()
                    .map(|(_, expression)| expression)
                    .chain(query.where_expr.iter())
                    .collect(),
            )
        }
        QueryExpr::Delete(query) => {
            if query.returning.is_some()
                || query.suppress_events
                || (query.filter.is_some() && query.where_expr.is_none())
            {
                return Err(unsupported());
            }
            (&query.table, query.where_expr.iter().collect())
        }
        _ => unreachable!("only admitted mutation statements reach planning"),
    };
    public_name(table)?;
    let contract = contracts.get(table).ok_or_else(|| {
        LocalReadError::Catalog(format!("no persisted collection contract for {table}"))
    })?;
    admit_contract(contract)?;
    let columns =
        resolved_contract_columns(contract).map_err(|error| query_error(error.message))?;
    let mut query = TableQuery::new(table);
    for expression in expressions {
        validate_expression_depth(expression).map_err(query_error)?;
        query.select_items.push(SelectItem::Expr {
            expr: expression.clone(),
            alias: None,
        });
    }
    local_read::admission::expressions(&query, &columns)?;
    let manager = resident
        .get_collection(table)
        .ok_or_else(|| LocalReadError::Catalog(format!("collection {table} is missing")))?;
    let mut entities = Vec::new();
    let mut exceeded = false;
    manager.scan_for_each(None, |entity| {
        if entities.len() == LOCAL_READ_ROWS_MAX {
            exceeded = true;
            return false;
        }
        entities.push(entity.clone());
        true
    });
    if exceeded {
        return Err(LocalReadError::ResourceLimit);
    }
    for entity in &entities {
        if !matches!(entity.data, EntityData::Row(_))
            || manager.get_metadata(entity.id).is_some_and(|metadata| {
                INTERNAL_TTL_METADATA_KEYS
                    .iter()
                    .any(|key| metadata.has(key))
            })
        {
            return Err(unsupported());
        }
    }
    let enforcer = CollectionContractEnforcer::new(contract);
    let schema = Arc::new(
        columns
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>(),
    );
    let host = ReadHost::new(&query, &columns, &[], Vec::new());
    let context = Context { schema, host };
    let mut actions = Vec::new();
    let mut final_fields: Vec<_> = entities
        .iter()
        .map(|entity| fields(entity, &context.schema))
        .collect();
    match statement {
        QueryExpr::Insert(insert) => {
            let names = if insert.columns.is_empty() {
                contract
                    .declared_columns
                    .iter()
                    .filter(|column| {
                        column.generated.is_none()
                            && !(contract.timestamps_enabled
                                && matches!(column.name.as_str(), "created_at" | "updated_at"))
                    })
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>()
            } else {
                insert.columns.clone()
            };
            check_columns(contract, &names)?;
            if insert.value_exprs.is_empty()
                || entities
                    .len()
                    .checked_add(insert.value_exprs.len())
                    .is_none_or(|count| count > LOCAL_READ_ROWS_MAX)
            {
                return Err(LocalReadError::ResourceLimit);
            }
            let first_id = resident
                .next_entity_id
                .load(Ordering::SeqCst)
                .max(FIRST_USER_ENTITY_ID);
            for (index, row) in insert.value_exprs.iter().enumerate() {
                if row.len() != names.len() {
                    return Err(query_error("INSERT column/value counts differ"));
                }
                let values = row
                    .iter()
                    .map(|expression| {
                        evaluator::evaluate(expression, &|_: &FieldRef| None)
                            .map_err(LocalReadError::from)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let fields = enforcer
                    .normalize_insert_at(names.iter().cloned().zip(values).collect(), now_ms)
                    .map_err(contract_error)?;
                let id = first_id
                    .checked_add(u64::try_from(index).expect("bounded row index"))
                    .filter(|id| *id < u64::MAX)
                    .ok_or_else(|| query_error("entity ID space exhausted"))?;
                let mut entity = UnifiedEntity::table_row(EntityId::new(id), table, id, Vec::new());
                entity.created_at = now_ms / 1000;
                entity.updated_at = now_ms / 1000;
                entity.sequence_id = id;
                entity.ensure_table_logical_id();
                set_fields(&mut entity, &fields);
                actions.push(StoreWalAction::upsert_entity(
                    table,
                    &entity,
                    None,
                    reddb_file::STORE_VERSION_CURRENT,
                ));
                final_fields.push(fields);
            }
        }
        QueryExpr::Update(update) => {
            if contract.append_only {
                return Err(query_error("append-only tables cannot be updated"));
            }
            check_columns(
                contract,
                &update
                    .assignment_exprs
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>(),
            )?;
            if update.assignment_exprs.is_empty() {
                return Err(query_error("UPDATE requires assignments"));
            }
            for (index, original) in entities.iter().enumerate() {
                if !context.matches(update.where_expr.as_ref(), original)? {
                    continue;
                }
                let mut fields: BTreeMap<_, _> = final_fields[index].iter().cloned().collect();
                // All RHS expressions see the same row pre-image.
                for (name, expression) in &update.assignment_exprs {
                    fields.insert(name.clone(), context.evaluate(expression, original)?);
                }
                let fields = enforcer
                    .normalize_update_at(fields.into_iter().collect(), now_ms)
                    .map_err(contract_error)?;
                let mut entity = original.clone();
                entity.updated_at = now_ms / 1000;
                set_fields(&mut entity, &fields);
                actions.push(StoreWalAction::upsert_entity(
                    table,
                    &entity,
                    manager.get_metadata(original.id).as_ref(),
                    reddb_file::STORE_VERSION_CURRENT,
                ));
                final_fields[index] = fields;
            }
        }
        QueryExpr::Delete(delete) => {
            if contract.append_only {
                return Err(query_error("append-only tables cannot be deleted"));
            }
            for entity in &entities {
                if context.matches(delete.where_expr.as_ref(), entity)? {
                    actions.push(StoreWalAction::DeleteEntityRecord {
                        collection: table.into(),
                        entity_id: entity.id.raw(),
                    });
                }
            }
            // Deletion cannot introduce a uniqueness conflict.
            return Ok(actions);
        }
        _ => unreachable!("mutation variant was validated"),
    }
    enforcer
        .enforce_batch_uniqueness(&final_fields)
        .map_err(contract_error)?;
    Ok(actions)
}

fn check_columns(contract: &CollectionContract, names: &[String]) -> Result<()> {
    let mut seen = HashSet::new();
    for name in names {
        user_column(name)?;
        let Some(column) = contract
            .declared_columns
            .iter()
            .find(|column| &column.name == name)
        else {
            return Err(query_error(format!("unknown column {name}")));
        };
        if column.generated.is_some() {
            return Err(query_error(format!(
                "generated column '{name}' cannot be assigned"
            )));
        }
        if !seen.insert(name) {
            return Err(query_error(format!("duplicate column {name}")));
        }
    }
    Ok(())
}
fn fields(entity: &UnifiedEntity, schema: &[String]) -> Vec<(String, Value)> {
    let EntityData::Row(row) = &entity.data else {
        unreachable!("table rows were admitted")
    };
    if row.named.is_some() || row.schema.is_some() {
        return row
            .iter_fields()
            .map(|(name, value)| (name.into(), value.clone()))
            .collect();
    }
    schema
        .iter()
        .cloned()
        .zip(row.columns.iter().cloned())
        .collect()
}
fn set_fields(entity: &mut UnifiedEntity, fields: &[(String, Value)]) {
    let (names, values) = fields.iter().cloned().unzip();
    entity.data = EntityData::Row(RowData::with_names(values, names));
    entity.field_bloom = compute_entity_field_bloom(&entity.data);
}
struct Context<'a> {
    schema: Arc<Vec<String>>,
    host: ReadHost<'a>,
}
impl Context<'_> {
    fn evaluate(&self, expression: &Expr, entity: &UnifiedEntity) -> Result<Value> {
        let record = runtime_table_record_from_entity_ref_with_schema(entity, Some(&self.schema))
            .ok_or_else(|| query_error("table row could not be materialized"))?;
        self.host
            .evaluate(expression, &record)
            .map_err(LocalReadError::from)
    }
    fn matches(&self, predicate: Option<&Expr>, entity: &UnifiedEntity) -> Result<bool> {
        match predicate
            .map(|predicate| self.evaluate(predicate, entity))
            .transpose()?
        {
            None | Some(Value::Boolean(true)) => Ok(true),
            Some(Value::Boolean(false) | Value::Null) => Ok(false),
            _ => Err(query_error("WHERE must evaluate to boolean or NULL")),
        }
    }
}
