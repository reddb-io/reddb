//! Persisted function catalog. Parse once at definition/load; clone an Arc per CALL.
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use crate::{RedDB, RedDBResult};
use reddb_rql::ast::QueryExpr;
use reddb_rql::stored_function::{
    FunctionCommand, FunctionDefinition, FunctionEffect, FunctionReturn,
};
use reddb_types::Value;

use super::function_validation::{error, validate_definition};

pub(crate) const REGISTRY_KEY: &str = "red.rql.function_catalog";
const CATALOG_BYTES_MAX: usize = 4 * 1024 * 1024;
const FUNCTIONS_MAX: usize = 1024;
type FunctionKey = (Option<String>, String);

pub(super) struct CompiledFunction {
    pub definition: FunctionDefinition,
    pub source: String,
    pub statements: Vec<(String, QueryExpr)>,
}

impl CompiledFunction {
    pub fn compile(definition: FunctionDefinition) -> RedDBResult<Self> {
        let statements = validate_definition(&definition)?;
        let source = render_definition(&definition);
        Ok(Self {
            definition,
            source,
            statements,
        })
    }
}

#[derive(Default)]
pub(super) struct FunctionCatalog {
    entries: BTreeMap<FunctionKey, Arc<CompiledFunction>>,
    persistence_failed: bool,
}

impl FunctionCatalog {
    pub fn load(db: &RedDB) -> RedDBResult<Self> {
        let Some(manager) = db.store().get_collection("red_config") else {
            return Ok(Self::default());
        };
        let latest = manager.query_all(|entity| {
            matches!(&entity.data, EntityData::Row(row) if matches!(row.get_field("key"), Some(Value::Text(key)) if key.as_ref() == REGISTRY_KEY))
        }).into_iter().max_by_key(|entity| entity.id.raw());
        let Some(latest) = latest else {
            return Ok(Self::default());
        };
        let EntityData::Row(row) = &latest.data else {
            return Err(error("invalid catalog row"));
        };
        let Some(Value::Text(source)) = row.get_field("value") else {
            return Err(error("invalid persisted catalog encoding"));
        };
        if source.len() > CATALOG_BYTES_MAX {
            return Err(error("catalog exceeds 4 MiB"));
        }
        let json: crate::serde_json::Value = crate::serde_json::from_str(source)
            .map_err(|cause| error(format!("invalid persisted catalog: {cause}")))?;
        let entries = json
            .as_array()
            .ok_or_else(|| error("catalog must be an array"))?;
        if entries.len() > FUNCTIONS_MAX {
            return Err(error("catalog exceeds 1024 functions"));
        }
        let mut catalog = Self::default();
        for entry in entries {
            let tenant = match entry.get("tenant") {
                Some(crate::serde_json::Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| error("invalid catalog tenant"))?
                        .to_string(),
                ),
                None => return Err(error("missing catalog tenant")),
            };
            let source = entry
                .get("definition")
                .and_then(|value| value.as_str())
                .ok_or_else(|| error("missing catalog definition"))?;
            let parsed = reddb_rql::parser::parse(source)
                .map_err(|cause| error(format!("invalid persisted function: {cause}")))?;
            let QueryExpr::Function(command) = parsed.query else {
                return Err(error("catalog entry is not a function"));
            };
            let FunctionCommand::Create { definition, .. } = *command else {
                return Err(error("catalog entry is not a definition"));
            };
            let name = definition.name.clone();
            let compiled = Arc::new(CompiledFunction::compile(definition)?);
            if catalog.entries.insert((tenant, name), compiled).is_some() {
                return Err(error("duplicate persisted function"));
            }
        }
        Ok(catalog)
    }

    fn check_available(&self) -> RedDBResult<()> {
        if self.persistence_failed {
            return Err(error("catalog persistence outcome is indeterminate; reopen the runtime before using functions"));
        }
        Ok(())
    }

    pub fn get(
        &self,
        tenant: Option<&str>,
        name: &str,
    ) -> RedDBResult<Option<Arc<CompiledFunction>>> {
        self.check_available()?;
        Ok(self
            .entries
            .get(&(tenant.map(str::to_string), name.to_string()))
            .cloned())
    }

    pub fn list(&self, tenant: Option<&str>) -> RedDBResult<Vec<Arc<CompiledFunction>>> {
        self.check_available()?;
        Ok(self
            .entries
            .iter()
            .filter(|((owner, _), _)| owner.as_deref() == tenant)
            .map(|(_, function)| function.clone())
            .collect())
    }

    pub fn define(
        &mut self,
        db: &RedDB,
        tenant: Option<String>,
        function: CompiledFunction,
        replace: bool,
    ) -> RedDBResult<()> {
        self.check_available()?;
        let key = (tenant, function.definition.name.clone());
        if self.entries.contains_key(&key) != replace {
            return Err(error(if replace {
                "ALTER target does not exist"
            } else {
                "function already exists"
            }));
        }
        let mut next = self.entries.clone();
        next.insert(key, Arc::new(function));
        let source = Self::encode(&next)?;
        if let Err(cause) = Self::persist(db, source) {
            self.persistence_failed = true;
            return Err(error(format!(
                "catalog persistence outcome is indeterminate; reopen the runtime: {cause}"
            )));
        }
        self.entries = next;
        Ok(())
    }

    pub fn remove(
        &mut self,
        db: &RedDB,
        tenant: Option<String>,
        name: &str,
        if_exists: bool,
    ) -> RedDBResult<()> {
        self.check_available()?;
        let key = (tenant, name.to_string());
        if !self.entries.contains_key(&key) {
            return if if_exists {
                Ok(())
            } else {
                Err(error("function does not exist"))
            };
        }
        let mut next = self.entries.clone();
        next.remove(&key);
        let source = Self::encode(&next)?;
        if let Err(cause) = Self::persist(db, source) {
            self.persistence_failed = true;
            return Err(error(format!(
                "catalog persistence outcome is indeterminate; reopen the runtime: {cause}"
            )));
        }
        self.entries = next;
        Ok(())
    }

    fn encode(entries: &BTreeMap<FunctionKey, Arc<CompiledFunction>>) -> RedDBResult<String> {
        if entries.len() > FUNCTIONS_MAX {
            return Err(error("catalog exceeds 1024 functions"));
        }
        let json = crate::serde_json::Value::Array(entries.iter().map(|((tenant, _), function)| {
            crate::json!({ "tenant": tenant.clone(), "definition": function.source.clone() })
        }).collect());
        let source = json.to_string();
        if source.len() > CATALOG_BYTES_MAX {
            return Err(error("catalog exceeds 4 MiB"));
        }
        Ok(source)
    }

    fn persist(db: &RedDB, source: String) -> RedDBResult<()> {
        let store = db.store();
        store.get_or_create_collection("red_config");
        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from("red_config"),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                schema: None,
                named: Some(
                    [
                        ("key".to_string(), Value::text(REGISTRY_KEY)),
                        ("value".to_string(), Value::text(source)),
                    ]
                    .into_iter()
                    .collect(),
                ),
            }),
        );
        store.insert_auto("red_config", entity).map_err(error)?;
        db.flush().map_err(error)?;
        Ok(())
    }
}

fn render_definition(definition: &FunctionDefinition) -> String {
    fn identifier(value: &str) -> String {
        format!("\"{}\"", value.replace('"', "\"\""))
    }
    fn parameters(parameters: &[reddb_rql::stored_function::FunctionParameter]) -> String {
        parameters
            .iter()
            .map(|parameter| format!("{} {}", identifier(&parameter.name), parameter.sql_type))
            .collect::<Vec<_>>()
            .join(", ")
    }
    let returns = match &definition.returns {
        FunctionReturn::Scalar(sql_type) => sql_type.to_string(),
        FunctionReturn::Table(columns) => format!("TABLE ({})", parameters(columns)),
    };
    let effect = match definition.effect {
        FunctionEffect::Pure => "PURE",
        FunctionEffect::Read => "READ",
        FunctionEffect::Write => "WRITE",
    };
    format!(
        "CREATE FUNCTION {}({}) RETURNS {returns} EFFECT {effect} AS '{}'",
        definition
            .name
            .split('.')
            .map(identifier)
            .collect::<Vec<_>>()
            .join("."),
        parameters(&definition.parameters),
        definition.body.replace('\'', "''")
    )
}
