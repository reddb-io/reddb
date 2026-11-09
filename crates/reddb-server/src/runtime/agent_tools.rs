//! Protocol-neutral RedDB tools shared by ASK, MCP, and ACP adapters.
//!
//! This module owns the canonical definitions, argument validation, and query
//! execution for the common read/discovery surface. Protocol adapters decide
//! which profile to expose and how to serialize [`RuntimeQueryResult`].

use crate::api::{RedDBError, RedDBResult};
use crate::json::{Map, Value as JsonValue};
use crate::runtime::ai::ask_rql_planner::{validate_candidate, CandidateDisposition};
use crate::runtime::query_request::ParamValue;
use crate::runtime::statement_frame::EffectiveScope;
use crate::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb_types::Value;

pub const CATALOG_TOOL: &str = "reddb_catalog";
pub const DESCRIBE_COLLECTION_TOOL: &str = "reddb_describe_collection";
pub const FIND_FIELD_TOOL: &str = "reddb_find_field";
pub const QUERY_TOOL: &str = "reddb_query";
pub const FINAL_QUERY_TOOL: &str = "reddb_final_query";

pub(crate) const METADATA_ARGUMENT_BYTES_MAX: usize = 256;
const ASK_QUERY_BYTES_MAX: usize = 32 * 1024;
const ASK_QUERY_PARAMETER_BYTES_MAX: usize = 16 * 1024;
pub(crate) const ASK_QUERY_PARAMETER_COUNT_MAX: usize = 64;
const ASK_QUERY_PURPOSE_BYTES_MAX: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSurface {
    Ask,
    Mcp,
    Acp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    Read,
    Dynamic,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RedToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub command_id: &'static str,
    pub effect: ToolEffect,
    pub input_schema: JsonValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryPhase {
    Explore,
    Final,
}

impl QueryPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::Final => "final",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RedToolCall {
    Catalog,
    DescribeCollection {
        collection: String,
    },
    FindField {
        field: String,
    },
    Query {
        rql: String,
        params: Vec<JsonValue>,
        purpose: String,
        phase: QueryPhase,
    },
}

impl RedToolCall {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Catalog => CATALOG_TOOL,
            Self::DescribeCollection { .. } => DESCRIBE_COLLECTION_TOOL,
            Self::FindField { .. } => FIND_FIELD_TOOL,
            Self::Query {
                phase: QueryPhase::Explore,
                ..
            } => QUERY_TOOL,
            Self::Query {
                phase: QueryPhase::Final,
                ..
            } => FINAL_QUERY_TOOL,
        }
    }

    pub fn arguments_json(&self) -> JsonValue {
        let mut arguments = Map::new();
        match self {
            Self::Catalog => {}
            Self::DescribeCollection { collection } => {
                arguments.insert(
                    "collection".to_string(),
                    JsonValue::String(collection.clone()),
                );
            }
            Self::FindField { field } => {
                arguments.insert("field".to_string(), JsonValue::String(field.clone()));
            }
            Self::Query {
                rql,
                params,
                purpose,
                phase,
            } => {
                arguments.insert("sql".to_string(), JsonValue::String(rql.clone()));
                arguments.insert("params".to_string(), JsonValue::Array(params.clone()));
                arguments.insert("purpose".to_string(), JsonValue::String(purpose.clone()));
                arguments.insert(
                    "phase".to_string(),
                    JsonValue::String(phase.as_str().to_string()),
                );
            }
        }
        JsonValue::Object(arguments)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolParseMode {
    AskNative,
    AskLegacy,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionProfile {
    Ask,
    Direct,
}

fn string_schema(description: &str) -> JsonValue {
    let mut schema = Map::new();
    schema.insert("type".to_string(), JsonValue::String("string".to_string()));
    schema.insert(
        "description".to_string(),
        JsonValue::String(description.to_string()),
    );
    JsonValue::Object(schema)
}

fn params_schema() -> JsonValue {
    let mut item = Map::new();
    item.insert(
        "anyOf".to_string(),
        JsonValue::Array(
            ["null", "boolean", "number", "string", "array", "object"]
                .into_iter()
                .map(|kind| {
                    let mut schema = Map::new();
                    schema.insert("type".to_string(), JsonValue::String(kind.to_string()));
                    JsonValue::Object(schema)
                })
                .collect(),
        ),
    );
    item.insert(
        "description".to_string(),
        JsonValue::String(
            "Bind value for the matching $N placeholder. Accepts null, boolean, number, string, array, or object."
                .to_string(),
        ),
    );
    let mut schema = Map::new();
    schema.insert("type".to_string(), JsonValue::String("array".to_string()));
    schema.insert("items".to_string(), JsonValue::Object(item));
    schema.insert(
        "description".to_string(),
        JsonValue::String(
            "Positional bind values for `$1`, `$2`, ... in `sql`. Index 0 binds `$1`.".to_string(),
        ),
    );
    JsonValue::Object(schema)
}

fn object_schema(properties: Vec<(&str, JsonValue)>, required: &[&str]) -> JsonValue {
    let mut property_map = Map::new();
    for (name, schema) in properties {
        property_map.insert(name.to_string(), schema);
    }
    let mut schema = Map::new();
    schema.insert("type".to_string(), JsonValue::String("object".to_string()));
    schema.insert("properties".to_string(), JsonValue::Object(property_map));
    schema.insert(
        "required".to_string(),
        JsonValue::Array(
            required
                .iter()
                .map(|name| JsonValue::String((*name).to_string()))
                .collect(),
        ),
    );
    schema.insert("additionalProperties".to_string(), JsonValue::Bool(false));
    JsonValue::Object(schema)
}

fn query_schema(require_agent_fields: bool) -> JsonValue {
    let sql_description = if require_agent_fields {
        "Read-only SQL or RQL with $N placeholders for every data value"
    } else {
        "SQL or universal query to execute. Use `$1`, `$2`, ... placeholders for any value that came from the user."
    };
    let mut properties = vec![
        ("sql", string_schema(sql_description)),
        ("params", params_schema()),
    ];
    if require_agent_fields {
        properties.push((
            "purpose",
            string_schema("One short auditable sentence explaining this query"),
        ));
    }
    if require_agent_fields {
        object_schema(properties, &["sql", "params", "purpose"])
    } else {
        object_schema(properties, &["sql"])
    }
}

/// Canonical tool registry projected for one protocol/runtime surface.
pub fn definitions(surface: ToolSurface) -> Vec<RedToolDefinition> {
    let agent_surface = matches!(surface, ToolSurface::Ask | ToolSurface::Acp);
    let (
        catalog_description,
        describe_description,
        find_description,
        query_description,
    ) = match surface {
        ToolSurface::Ask => (
            "List a bounded summary of collections visible to the asking user",
            "Describe columns of one collection visible to the asking user",
            "Find visible collections containing an exact field name",
            "Run one bounded exploratory read under the asking user's access",
        ),
        ToolSurface::Mcp => (
            "List visible collections with model, schema, size, index, and attention metadata. Remote calls execute under the connected user's tenant and policies.",
            "Describe the fields visible on one collection using the canonical red.columns catalog. The database applies the connected user's tenant and policies before returning metadata.",
            "Find every visible collection with an exact field name using the canonical red.columns catalog. Useful before a global SELECT when the user names an attribute such as passport_number. Tenant and policy filtering is enforced by the database.",
            "Execute a SQL or universal query against RedDB. Supports SELECT, INSERT, UPDATE, DELETE, and graph queries (Gremlin, Cypher, SPARQL).\n\nALWAYS pass user-provided values via the `params` array using `$1`, `$2`, ... placeholders rather than interpolating them into the SQL string. Example: `{\"sql\": \"SELECT * FROM users WHERE id = $1\", \"params\": [42]}`. Interpolating user input directly is unsafe and brittle; the parameterized form is type-checked and immune to injection.",
        ),
        ToolSurface::Acp => (
            "List a bounded summary of collections visible to the connected user",
            "Describe columns of one collection visible to the connected user",
            "Find visible collections containing an exact field name",
            "Run one bounded read-only query under the connected user's access",
        ),
    };
    let query_effect = if agent_surface {
        ToolEffect::Read
    } else {
        ToolEffect::Dynamic
    };
    let mut tools = vec![
        RedToolDefinition {
            name: CATALOG_TOOL,
            description: catalog_description,
            command_id: "query.execute",
            effect: ToolEffect::Read,
            input_schema: object_schema(Vec::new(), &[]),
        },
        RedToolDefinition {
            name: DESCRIBE_COLLECTION_TOOL,
            description: describe_description,
            command_id: "query.execute",
            effect: ToolEffect::Read,
            input_schema: object_schema(
                vec![(
                    "collection",
                    string_schema("Exact collection name from authorized metadata"),
                )],
                &["collection"],
            ),
        },
        RedToolDefinition {
            name: FIND_FIELD_TOOL,
            description: find_description,
            command_id: "query.execute",
            effect: ToolEffect::Read,
            input_schema: object_schema(
                vec![("field", string_schema("Exact field name to locate"))],
                &["field"],
            ),
        },
        RedToolDefinition {
            name: QUERY_TOOL,
            description: query_description,
            command_id: "query.execute",
            effect: query_effect,
            input_schema: query_schema(agent_surface),
        },
    ];
    if surface == ToolSurface::Ask {
        tools.push(RedToolDefinition {
            name: FINAL_QUERY_TOOL,
            description: "Provide the terminal read-only query whose rows answer the user",
            command_id: "query.execute",
            effect: ToolEffect::Read,
            input_schema: query_schema(true),
        });
    }
    tools
}

pub fn definition(surface: ToolSurface, name: &str) -> Option<RedToolDefinition> {
    definitions(surface)
        .into_iter()
        .find(|definition| definition.name == name)
}

/// Parse and bound one call from any adapter into the common representation.
pub fn parse_call(
    name: &str,
    arguments: &JsonValue,
    mode: ToolParseMode,
) -> RedDBResult<RedToolCall> {
    let arguments = arguments.as_object().ok_or_else(|| {
        RedDBError::Query(format!("tool `{name}` arguments must be a JSON object"))
    })?;
    match name {
        CATALOG_TOOL => Ok(RedToolCall::Catalog),
        DESCRIBE_COLLECTION_TOOL => Ok(RedToolCall::DescribeCollection {
            collection: bounded_string(arguments, "collection", METADATA_ARGUMENT_BYTES_MAX)?,
        }),
        FIND_FIELD_TOOL => Ok(RedToolCall::FindField {
            field: bounded_string(arguments, "field", METADATA_ARGUMENT_BYTES_MAX)?,
        }),
        QUERY_TOOL | FINAL_QUERY_TOOL => {
            let ask_mode = !matches!(mode, ToolParseMode::Direct);
            let sql_limit = if ask_mode {
                ASK_QUERY_BYTES_MAX
            } else {
                usize::MAX
            };
            let rql = bounded_string(arguments, "sql", sql_limit)?;
            let params = parse_params(arguments, ask_mode)?;
            let purpose =
                optional_bounded_string(arguments, "purpose", ASK_QUERY_PURPOSE_BYTES_MAX)?
                    .unwrap_or_else(|| match mode {
                        ToolParseMode::Direct => {
                            "Execute the caller's parameterized query".to_string()
                        }
                        ToolParseMode::AskNative | ToolParseMode::AskLegacy => {
                            "Answer the user's question from authorized rows".to_string()
                        }
                    });
            let phase = match mode {
                ToolParseMode::AskNative if name == QUERY_TOOL => QueryPhase::Explore,
                ToolParseMode::AskNative => QueryPhase::Final,
                ToolParseMode::AskLegacy if name == FINAL_QUERY_TOOL => QueryPhase::Final,
                ToolParseMode::AskLegacy => {
                    match arguments
                        .get("phase")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("final")
                    {
                        "explore" => QueryPhase::Explore,
                        "final" => QueryPhase::Final,
                        other => {
                            return Err(RedDBError::Query(format!(
                                "tool query phase must be `explore` or `final`, got `{other}`"
                            )))
                        }
                    }
                }
                ToolParseMode::Direct => QueryPhase::Explore,
            };
            Ok(RedToolCall::Query {
                rql,
                params,
                purpose,
                phase,
            })
        }
        other => Err(RedDBError::Query(format!("unsupported tool `{other}`"))),
    }
}

fn optional_bounded_string(
    arguments: &Map<String, JsonValue>,
    name: &str,
    bytes_max: usize,
) -> RedDBResult<Option<String>> {
    let Some(value) = arguments.get(name) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| RedDBError::Query(format!("tool argument `{name}` must be a string")))?;
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > bytes_max {
        return Err(RedDBError::Query(format!(
            "tool argument `{name}` exceeds {bytes_max} bytes"
        )));
    }
    Ok(Some(value.to_string()))
}

fn bounded_string(
    arguments: &Map<String, JsonValue>,
    name: &str,
    bytes_max: usize,
) -> RedDBResult<String> {
    optional_bounded_string(arguments, name, bytes_max)?
        .ok_or_else(|| RedDBError::Query(format!("tool is missing string argument `{name}`")))
}

fn parse_params(arguments: &Map<String, JsonValue>, bounded: bool) -> RedDBResult<Vec<JsonValue>> {
    let Some(value) = arguments.get("params") else {
        return Ok(Vec::new());
    };
    let params = value
        .as_array()
        .ok_or_else(|| RedDBError::Query("tool `params` must be an array".to_string()))?;
    if bounded && params.len() > ASK_QUERY_PARAMETER_COUNT_MAX {
        return Err(RedDBError::Query(format!(
            "tool `params` exceeds {ASK_QUERY_PARAMETER_COUNT_MAX} values"
        )));
    }
    if bounded {
        let encoded = crate::json::to_vec(value).map_err(|error| {
            RedDBError::Query(format!("tool `params` cannot be serialized: {error}"))
        })?;
        if encoded.len() > ASK_QUERY_PARAMETER_BYTES_MAX {
            return Err(RedDBError::Query(format!(
                "tool `params` exceeds {ASK_QUERY_PARAMETER_BYTES_MAX} bytes"
            )));
        }
    }
    for param in params {
        ParamValue::decode_json(param)
            .map_err(|error| RedDBError::Query(format!("invalid tool parameter: {error}")))?;
    }
    Ok(params.to_vec())
}

fn params_to_values(params: &[JsonValue]) -> RedDBResult<Vec<Value>> {
    params
        .iter()
        .map(ParamValue::decode_json)
        .collect::<Result<Vec<_>, _>>()
        .map(|params| params.into_iter().map(Value::from).collect())
        .map_err(RedDBError::Query)
}

/// Common execution authority. The captured scope is always reinstalled, so
/// adapters cannot accidentally execute under a later ambient identity.
pub struct RedToolExecutor<'a> {
    runtime: &'a RedDBRuntime,
    scope: &'a EffectiveScope,
    profile: ToolExecutionProfile,
}

impl<'a> RedToolExecutor<'a> {
    pub fn new(
        runtime: &'a RedDBRuntime,
        scope: &'a EffectiveScope,
        profile: ToolExecutionProfile,
    ) -> Self {
        Self {
            runtime,
            scope,
            profile,
        }
    }

    pub fn execute(&self, call: &RedToolCall) -> RedDBResult<RuntimeQueryResult> {
        let _scope_guard =
            crate::runtime::ask_pipeline::AskScopeGuard::install(self.runtime, self.scope);
        match call {
            RedToolCall::Catalog => self.execute_query(
                match self.profile {
                    ToolExecutionProfile::Ask => {
                        "SELECT name, model, schema_mode, entities, indices FROM red.collections \
                         WHERE internal = false ORDER BY name LIMIT 128"
                    }
                    ToolExecutionProfile::Direct => "SHOW COLLECTIONS",
                },
                &[],
            ),
            RedToolCall::DescribeCollection { collection } => self.execute_query(
                match self.profile {
                    ToolExecutionProfile::Ask => {
                        "SELECT collection, name, type, nullable, default_value, is_primary_key, \
                         is_unique FROM red.columns WHERE collection = $1 ORDER BY name LIMIT 128"
                    }
                    ToolExecutionProfile::Direct => {
                        "SELECT collection, name, type, nullable, default_value, is_primary_key, \
                         is_unique FROM red.columns WHERE collection = $1 ORDER BY name"
                    }
                },
                &[Value::text(collection.clone())],
            ),
            RedToolCall::FindField { field } => self.execute_query(
                match self.profile {
                    ToolExecutionProfile::Ask => {
                        "SELECT collection, name, type, nullable FROM red.columns WHERE name = $1 \
                         ORDER BY collection, name LIMIT 128"
                    }
                    ToolExecutionProfile::Direct => {
                        "SELECT collection, name, type, nullable FROM red.columns WHERE name = $1 \
                         ORDER BY collection, name"
                    }
                },
                &[Value::text(field.clone())],
            ),
            RedToolCall::Query { rql, params, .. } => {
                let rql = if self.profile == ToolExecutionProfile::Ask {
                    let candidate = validate_candidate(rql)?;
                    if candidate.disposition != CandidateDisposition::ReadOnly {
                        return Err(RedDBError::Query(
                            "ASK common query tool accepts read-only RQL only".to_string(),
                        ));
                    }
                    candidate.rql
                } else {
                    rql.clone()
                };
                self.execute_query(&rql, &params_to_values(params)?)
            }
        }
    }

    fn execute_query(&self, query: &str, params: &[Value]) -> RedDBResult<RuntimeQueryResult> {
        if params.is_empty() {
            self.runtime.execute_query(query)
        } else {
            self.runtime.execute_query_with_params(query, params)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(entries: Vec<(&str, JsonValue)>) -> JsonValue {
        let mut map = Map::new();
        for (name, value) in entries {
            map.insert(name.to_string(), value);
        }
        JsonValue::Object(map)
    }

    #[test]
    fn all_surfaces_share_the_common_read_tool_names() {
        for name in [
            CATALOG_TOOL,
            DESCRIBE_COLLECTION_TOOL,
            FIND_FIELD_TOOL,
            QUERY_TOOL,
        ] {
            assert!(definition(ToolSurface::Ask, name).is_some(), "ASK: {name}");
            assert!(definition(ToolSurface::Mcp, name).is_some(), "MCP: {name}");
            assert!(definition(ToolSurface::Acp, name).is_some(), "ACP: {name}");
        }
        assert!(definition(ToolSurface::Ask, FINAL_QUERY_TOOL).is_some());
        assert!(definition(ToolSurface::Mcp, FINAL_QUERY_TOOL).is_none());
        assert!(definition(ToolSurface::Acp, FINAL_QUERY_TOOL).is_none());
    }

    #[test]
    fn native_query_cannot_smuggle_the_final_phase() {
        let call = parse_call(
            QUERY_TOOL,
            &object(vec![
                ("sql", JsonValue::String("SELECT 1".to_string())),
                ("params", JsonValue::Array(Vec::new())),
                ("purpose", JsonValue::String("inspect".to_string())),
                ("phase", JsonValue::String("final".to_string())),
            ]),
            ToolParseMode::AskNative,
        )
        .expect("native tool call");
        assert!(matches!(
            call,
            RedToolCall::Query {
                phase: QueryPhase::Explore,
                ..
            }
        ));
    }

    #[test]
    fn legacy_final_tool_cannot_smuggle_an_exploratory_phase() {
        let call = parse_call(
            FINAL_QUERY_TOOL,
            &object(vec![
                ("sql", JsonValue::String("SELECT 1".to_string())),
                ("params", JsonValue::Array(Vec::new())),
                ("purpose", JsonValue::String("answer".to_string())),
                ("phase", JsonValue::String("explore".to_string())),
            ]),
            ToolParseMode::AskLegacy,
        )
        .expect("legacy final tool call");
        assert!(matches!(
            call,
            RedToolCall::Query {
                phase: QueryPhase::Final,
                ..
            }
        ));
    }
}
