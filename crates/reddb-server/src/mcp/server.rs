//! MCP Server for RedDB.
//!
//! Runs an embedded RedDB runtime and exposes it to AI agents via the
//! Model Context Protocol JSON-RPC transport over stdio.

use crate::application::{
    CatalogUseCases, CreateDocumentInput, CreateEdgeInput, CreateNodeInput, CreateRowInput,
    CreateVectorInput, DeleteEntityInput, EntityUseCases, ExecuteQueryInput, GraphCentralityInput,
    GraphClusteringInput, GraphCommunitiesInput, GraphComponentsInput, GraphCyclesInput,
    GraphShortestPathInput, GraphTraversalInput, GraphUseCases, QueryUseCases, ScanCollectionInput,
    SearchSimilarInput, SearchTextInput,
};
use crate::auth::store::AuthStore;
use crate::auth::{AuthConfig, Role};
use crate::json::{
    from_str as json_from_str, to_string as json_to_string, Map, Value as JsonValue,
};
use crate::mcp::{protocol, tools};
use crate::presentation::entity_json::created_entity_output_json;
use crate::presentation::entity_json::storage_value_to_json;
use crate::presentation::query_result_json::{runtime_query_json, runtime_stats_json};
use crate::runtime::{
    RedDBRuntime, RuntimeGraphCentralityAlgorithm, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphComponentsMode, RuntimeGraphDirection, RuntimeGraphPathAlgorithm,
    RuntimeGraphTraversalStrategy,
};
use crate::storage::schema::Value;
use crate::storage::EntityId;

use std::io::{self, BufRead, Write};
use std::sync::Arc;

/// MCP server wrapping an embedded RedDB runtime.
pub struct McpServer {
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    initialized: bool,
}

impl McpServer {
    /// Create a new MCP server with the given runtime.
    pub fn new(runtime: RedDBRuntime) -> Self {
        let auth_store = Arc::new(AuthStore::new(AuthConfig {
            enabled: true,
            ..Default::default()
        }));
        auth_store.bootstrap_from_env();
        runtime.set_auth_store(Arc::clone(&auth_store));
        Self {
            runtime,
            auth_store,
            initialized: false,
        }
    }

    /// Run the MCP server reading from stdin and writing to stdout.
    ///
    /// This blocks until stdin is closed (EOF). Diagnostic messages are
    /// written to stderr so they do not interfere with the protocol.
    pub fn run_stdio(&mut self) {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = io::BufReader::new(stdin.lock());
        let mut writer = io::BufWriter::new(stdout.lock());

        tracing::info!(target: "reddb::mcp", "server started, waiting for messages on stdin");

        loop {
            let payload = match protocol::read_payload(&mut reader) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    tracing::info!(target: "reddb::mcp", "stdin closed, shutting down");
                    break;
                }
                Err(e) => {
                    tracing::error!(target: "reddb::mcp", err = %e, "read error");
                    continue;
                }
            };

            let request: JsonValue = match json_from_str(&payload) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(target: "reddb::mcp", err = %e, "invalid JSON");
                    let msg = protocol::build_error_message(None, -32700, "parse error");
                    let _ = protocol::write_message(&mut writer, &msg);
                    continue;
                }
            };

            let response = self.handle_message(&request);
            if let Some(resp) = response {
                if let Err(e) = protocol::write_message(&mut writer, &resp) {
                    tracing::error!(target: "reddb::mcp", err = %e, "write error");
                    break;
                }
            }
        }
    }

    /// Route a JSON-RPC message to the appropriate handler.
    fn handle_message(&mut self, msg: &JsonValue) -> Option<String> {
        let method = msg.get("method").and_then(|v| v.as_str())?;
        let id = msg.get("id");

        match method {
            "initialize" => Some(self.handle_initialize(id)),
            "initialized" | "notifications/initialized" => {
                // Notification -- no response required.
                None
            }
            "tools/list" => Some(self.handle_tools_list(id)),
            "tools/call" => Some(self.handle_tools_call(id, msg.get("params"))),
            "ping" => {
                let mut result = Map::new();
                result.insert("status".to_string(), JsonValue::String("ok".to_string()));
                Some(protocol::build_result_message(
                    id,
                    JsonValue::Object(result),
                ))
            }
            _ => Some(protocol::build_error_message(
                id,
                -32601,
                &format!("unknown method: {}", method),
            )),
        }
    }

    // ------------------------------------------------------------------
    // MCP lifecycle
    // ------------------------------------------------------------------

    fn handle_initialize(&mut self, id: Option<&JsonValue>) -> String {
        self.initialized = true;

        let mut capabilities = Map::new();
        {
            let mut tools_cap = Map::new();
            tools_cap.insert("listChanged".to_string(), JsonValue::Bool(false));
            capabilities.insert("tools".to_string(), JsonValue::Object(tools_cap));
        }

        let mut server_info = Map::new();
        server_info.insert(
            "name".to_string(),
            JsonValue::String("reddb-mcp".to_string()),
        );
        server_info.insert(
            "version".to_string(),
            JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
        );

        let mut result = Map::new();
        result.insert(
            "protocolVersion".to_string(),
            JsonValue::String("2024-11-05".to_string()),
        );
        result.insert("capabilities".to_string(), JsonValue::Object(capabilities));
        result.insert("serverInfo".to_string(), JsonValue::Object(server_info));

        protocol::build_result_message(id, JsonValue::Object(result))
    }

    // ------------------------------------------------------------------
    // tools/list
    // ------------------------------------------------------------------

    fn handle_tools_list(&self, id: Option<&JsonValue>) -> String {
        let defs = tools::all_tools();
        let mut tools_json: Vec<JsonValue> = defs
            .into_iter()
            .map(|def| {
                let mut obj = Map::new();
                obj.insert("name".to_string(), JsonValue::String(def.name.to_string()));
                obj.insert(
                    "description".to_string(),
                    JsonValue::String(def.description.to_string()),
                );
                obj.insert("inputSchema".to_string(), def.input_schema);
                JsonValue::Object(obj)
            })
            .collect();
        tools_json.push(crate::runtime::ai::mcp_ask_tool::descriptor());

        let mut result = Map::new();
        result.insert("tools".to_string(), JsonValue::Array(tools_json));
        protocol::build_result_message(id, JsonValue::Object(result))
    }

    // ------------------------------------------------------------------
    // tools/call dispatcher
    // ------------------------------------------------------------------

    fn handle_tools_call(&self, id: Option<&JsonValue>, params: Option<&JsonValue>) -> String {
        let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
        let name = match name {
            Some(n) => n,
            None => {
                return protocol::build_error_message(id, -32602, "missing tool name");
            }
        };

        let empty = JsonValue::Object(Map::new());
        let args = params.and_then(|p| p.get("arguments")).unwrap_or(&empty);

        let result = match name {
            "reddb_query" => self.tool_query(args),
            "reddb_collections" => self.tool_collections(),
            "reddb_insert_row" => self.tool_insert_row(args),
            "reddb_insert_node" => self.tool_insert_node(args),
            "reddb_insert_edge" => self.tool_insert_edge(args),
            "reddb_insert_vector" => self.tool_insert_vector(args),
            "reddb_insert_document" => self.tool_insert_document(args),
            "reddb_kv_get" => self.tool_kv_get(args),
            "reddb_kv_set" => self.tool_kv_set(args),
            "reddb_kv_invalidate_tags" => self.tool_kv_invalidate_tags(args),
            "reddb_config_get" => self.tool_config_get(args),
            "reddb_config_put" => self.tool_config_put(args),
            "reddb_config_resolve" => self.tool_config_resolve(args),
            "reddb_vault_get" => self.tool_vault_get(args),
            "reddb_vault_put" => self.tool_vault_put(args),
            "reddb_vault_unseal" => self.tool_vault_unseal(args),
            "reddb_delete" => self.tool_delete(args),
            "reddb_search_vector" => self.tool_search_vector(args),
            "reddb_search_text" => self.tool_search_text(args),
            "reddb_health" => self.tool_health(),
            "reddb_graph_traverse" => self.tool_graph_traverse(args),
            "reddb_graph_shortest_path" => self.tool_graph_shortest_path(args),
            "reddb_update" => self.tool_update(args),
            "reddb_scan" => self.tool_scan(args),
            "reddb_graph_centrality" => self.tool_graph_centrality(args),
            "reddb_graph_community" => self.tool_graph_community(args),
            "reddb_graph_components" => self.tool_graph_components(args),
            "reddb_graph_cycles" => self.tool_graph_cycles(args),
            "reddb_graph_clustering" => self.tool_graph_clustering(args),
            "reddb_create_collection" => self.tool_create_collection(args),
            "reddb_drop_collection" => self.tool_drop_collection(args),
            "reddb_auth_bootstrap" => self.tool_auth_bootstrap(args),
            "reddb_auth_create_user" => self.tool_auth_create_user(args),
            "reddb_auth_login" => self.tool_auth_login(args),
            "reddb_auth_create_api_key" => self.tool_auth_create_api_key(args),
            "reddb_auth_list_users" => self.tool_auth_list_users(),
            crate::runtime::ai::mcp_ask_tool::TOOL_NAME => self.tool_ask(args),
            _ => Err(format!("unknown tool: {name}")),
        };

        match result {
            Ok(text) => {
                let mut content = Map::new();
                content.insert("type".to_string(), JsonValue::String("text".to_string()));
                content.insert("text".to_string(), JsonValue::String(text));

                let mut result_obj = Map::new();
                result_obj.insert(
                    "content".to_string(),
                    JsonValue::Array(vec![JsonValue::Object(content)]),
                );
                protocol::build_result_message(id, JsonValue::Object(result_obj))
            }
            Err(err) => {
                let mut content = Map::new();
                content.insert("type".to_string(), JsonValue::String("text".to_string()));
                content.insert("text".to_string(), JsonValue::String(err.clone()));

                let mut result_obj = Map::new();
                result_obj.insert(
                    "content".to_string(),
                    JsonValue::Array(vec![JsonValue::Object(content)]),
                );
                result_obj.insert("isError".to_string(), JsonValue::Bool(true));
                protocol::build_result_message(id, JsonValue::Object(result_obj))
            }
        }
    }

    // ------------------------------------------------------------------
    // Tool implementations
    // ------------------------------------------------------------------

    fn tool_query(&self, args: &JsonValue) -> Result<String, String> {
        let sql = args
            .get("sql")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'sql'")?;

        // Optional positional `$N` bind parameters. Decoded via the same
        // helper the JSON-RPC stdio path uses (#358), so MCP, embedded
        // stdio, and HTTP all bind via one codec.
        if let Some(raw_params) = args.get("params") {
            let arr = raw_params
                .as_array()
                .ok_or_else(|| "'params' must be an array".to_string())?;
            let binds: Vec<Value> = arr
                .iter()
                .map(crate::rpc_stdio::json_value_to_schema_value)
                .collect();

            use crate::storage::query::modes::parse_multi;
            use crate::storage::query::user_params;
            let parsed = parse_multi(sql).map_err(|e| format!("{}", e))?;
            let bound = user_params::bind(&parsed, &binds).map_err(|e| format!("{}", e))?;
            let result = self
                .runtime
                .execute_query_expr(bound)
                .map_err(|e| format!("{}", e))?;
            let json = runtime_query_json(&result, &None, &None);
            return json_to_string(&json).map_err(|e| format!("serialization error: {}", e));
        }

        let uc = QueryUseCases::new(&self.runtime);
        let result = uc
            .execute(ExecuteQueryInput {
                query: sql.to_string(),
            })
            .map_err(|e| format!("{}", e))?;

        let json = runtime_query_json(&result, &None, &None);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_ask(&self, args: &JsonValue) -> Result<String, String> {
        let invocation =
            crate::runtime::ai::mcp_ask_tool::parse(args).map_err(format_mcp_ask_parse_error)?;
        let ask = crate::storage::query::ast::AskQuery {
            explain: false,
            question: invocation.question,
            provider: invocation.using,
            model: invocation.model,
            depth: invocation.depth.map(|v| v as usize),
            limit: invocation.limit.map(|v| v as usize),
            min_score: invocation.min_score.map(|v| v as f32),
            collection: None,
            temperature: invocation.temperature.map(|v| v as f32),
            seed: invocation.seed,
            strict: invocation.strict.unwrap_or(true),
            stream: false,
            cache: if matches!(invocation.nocache, Some(true)) {
                crate::storage::query::ast::AskCacheClause::NoCache
            } else if let Some(ttl) = invocation.cache_ttl {
                crate::storage::query::ast::AskCacheClause::CacheTtl(ttl)
            } else {
                crate::storage::query::ast::AskCacheClause::Default
            },
        };
        let result = self
            .runtime
            .execute_ask("ASK <mcp>", &ask)
            .map_err(|e| format!("{}", e))?;
        let json = crate::rpc_stdio::query_result_to_json(&result);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_collections(&self) -> Result<String, String> {
        let uc = CatalogUseCases::new(&self.runtime);
        let collections = uc.collections();
        let json = JsonValue::Array(collections.into_iter().map(JsonValue::String).collect());
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_insert_row(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let data = args
            .get("data")
            .and_then(|v| v.as_object())
            .ok_or("missing required field 'data' (must be an object)")?;

        let mut fields = Vec::new();
        for (key, value) in data {
            let sv = crate::application::entity::json_to_storage_value(value)
                .map_err(|e| format!("{}", e))?;
            fields.push((key.clone(), sv));
        }

        let metadata = parse_metadata_arg(args)?;

        let uc = EntityUseCases::new(&self.runtime);
        let output = uc
            .create_row(CreateRowInput {
                collection: collection.to_string(),
                fields,
                metadata,
                node_links: vec![],
                vector_links: vec![],
            })
            .map_err(|e| format!("{}", e))?;

        let json = created_entity_output_json(&output);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_insert_node(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let label = args
            .get("label")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'label'")?;
        let node_type = args
            .get("node_type")
            .and_then(|v| v.as_str())
            .map(String::from);

        let mut properties = Vec::new();
        if let Some(props) = args.get("properties").and_then(|v| v.as_object()) {
            for (key, value) in props {
                let sv = crate::application::entity::json_to_storage_value(value)
                    .map_err(|e| format!("{}", e))?;
                properties.push((key.clone(), sv));
            }
        }

        let metadata = parse_metadata_arg(args)?;

        let uc = EntityUseCases::new(&self.runtime);
        let output = uc
            .create_node(CreateNodeInput {
                collection: collection.to_string(),
                label: label.to_string(),
                node_type,
                properties,
                metadata,
                embeddings: vec![],
                table_links: vec![],
                node_links: vec![],
            })
            .map_err(|e| format!("{}", e))?;

        let json = created_entity_output_json(&output);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_insert_edge(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let label = args
            .get("label")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'label'")?;
        let from_id = args
            .get("from")
            .and_then(|v| v.as_u64())
            .ok_or("missing required field 'from' (integer)")?;
        let to_id = args
            .get("to")
            .and_then(|v| v.as_u64())
            .ok_or("missing required field 'to' (integer)")?;
        let weight = args
            .get("weight")
            .and_then(|v| v.as_f64())
            .map(|w| w as f32);

        let mut properties = Vec::new();
        if let Some(props) = args.get("properties").and_then(|v| v.as_object()) {
            for (key, value) in props {
                let sv = crate::application::entity::json_to_storage_value(value)
                    .map_err(|e| format!("{}", e))?;
                properties.push((key.clone(), sv));
            }
        }

        let metadata = parse_metadata_arg(args)?;

        let uc = EntityUseCases::new(&self.runtime);
        let output = uc
            .create_edge(CreateEdgeInput {
                collection: collection.to_string(),
                label: label.to_string(),
                from: EntityId::new(from_id),
                to: EntityId::new(to_id),
                weight,
                properties,
                metadata,
            })
            .map_err(|e| format!("{}", e))?;

        let json = created_entity_output_json(&output);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_insert_vector(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let dense_arr = args
            .get("dense")
            .and_then(|v| v.as_array())
            .ok_or("missing required field 'dense' (array of numbers)")?;

        let mut dense = Vec::with_capacity(dense_arr.len());
        for v in dense_arr {
            dense.push(
                v.as_f64()
                    .ok_or("'dense' array must contain only numbers")? as f32,
            );
        }
        if dense.is_empty() {
            return Err("'dense' vector cannot be empty".to_string());
        }

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .map(String::from);
        let metadata = parse_metadata_arg(args)?;

        let uc = EntityUseCases::new(&self.runtime);
        let output = uc
            .create_vector(CreateVectorInput {
                collection: collection.to_string(),
                dense,
                content,
                metadata,
                link_row: None,
                link_node: None,
            })
            .map_err(|e| format!("{}", e))?;

        let json = created_entity_output_json(&output);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_insert_document(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let body = args.get("body").ok_or("missing required field 'body'")?;

        let metadata = parse_metadata_arg(args)?;

        let uc = EntityUseCases::new(&self.runtime);
        let output = uc
            .create_document(CreateDocumentInput {
                collection: collection.to_string(),
                body: body.clone(),
                metadata,
                node_links: vec![],
                vector_links: vec![],
            })
            .map_err(|e| format!("{}", e))?;

        let json = created_entity_output_json(&output);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_kv_get(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'key'")?;

        let uc = EntityUseCases::new(&self.runtime);
        match uc.get_kv(collection, key).map_err(|e| format!("{}", e))? {
            Some((value, entity_id)) => {
                let mut obj = Map::new();
                obj.insert("found".to_string(), JsonValue::Bool(true));
                obj.insert("key".to_string(), JsonValue::String(key.to_string()));
                obj.insert("value".to_string(), storage_value_to_json(&value));
                obj.insert(
                    "entity_id".to_string(),
                    JsonValue::Number(entity_id.raw() as f64),
                );
                json_to_string(&JsonValue::Object(obj))
                    .map_err(|e| format!("serialization error: {}", e))
            }
            None => {
                let mut obj = Map::new();
                obj.insert("found".to_string(), JsonValue::Bool(false));
                obj.insert("key".to_string(), JsonValue::String(key.to_string()));
                json_to_string(&JsonValue::Object(obj))
                    .map_err(|e| format!("serialization error: {}", e))
            }
        }
    }

    fn tool_kv_set(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'key'")?;
        let value_arg = args.get("value").ok_or("missing required field 'value'")?;

        let sv = crate::application::entity::json_to_storage_value(value_arg)
            .map_err(|e| format!("{}", e))?;

        let metadata = parse_metadata_arg(args)?;

        let tags = parse_string_array_arg(args, "tags")?;
        let ops = crate::runtime::impl_kv::KvAtomicOps::new(&self.runtime);
        let (_, id) = ops
            .set_with_tags_and_metadata(collection, key, sv, None, &tags, false, metadata)
            .map_err(|e| format!("{}", e))?;

        let mut obj = Map::new();
        obj.insert("ok".to_string(), JsonValue::Bool(true));
        obj.insert("entity_id".to_string(), JsonValue::Number(id.raw() as f64));
        obj.insert(
            "tags".to_string(),
            JsonValue::Array(tags.into_iter().map(JsonValue::String).collect()),
        );
        json_to_string(&JsonValue::Object(obj)).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_kv_invalidate_tags(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let tags = parse_string_array_arg(args, "tags")?;
        if tags.is_empty() {
            return Err("missing required field 'tags'".to_string());
        }
        let ops = crate::runtime::impl_kv::KvAtomicOps::new(&self.runtime);
        let count = ops
            .invalidate_tags(collection, &tags)
            .map_err(|e| format!("{}", e))?;

        let mut obj = Map::new();
        obj.insert("ok".to_string(), JsonValue::Bool(true));
        obj.insert("invalidated".to_string(), JsonValue::Number(count as f64));
        obj.insert(
            "tags".to_string(),
            JsonValue::Array(tags.into_iter().map(JsonValue::String).collect()),
        );
        json_to_string(&JsonValue::Object(obj)).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_config_get(&self, args: &JsonValue) -> Result<String, String> {
        let collection = mcp_keyed_ident(get_str_field(args, "collection")?)?;
        let key = mcp_keyed_ident(get_str_field(args, "key")?)?;
        self.tool_keyed_query(format!("GET CONFIG {collection} {key}"))
    }

    fn tool_config_put(&self, args: &JsonValue) -> Result<String, String> {
        reject_mcp_volatile_options(args, "CONFIG")?;
        let collection = mcp_keyed_ident(get_str_field(args, "collection")?)?;
        let key = mcp_keyed_ident(get_str_field(args, "key")?)?;
        let tags = parse_string_array_arg(args, "tags")?;
        let literal = if let Some(secret_ref) = args.get("secret_ref") {
            let object = secret_ref
                .as_object()
                .ok_or("field 'secret_ref' must be an object")?;
            let ref_collection = object
                .get("collection")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "secret_ref.collection is required".to_string())
                .and_then(mcp_keyed_ident)?;
            let ref_key = object
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "secret_ref.key is required".to_string())
                .and_then(mcp_keyed_ident)?;
            format!("SECRET_REF(vault, {ref_collection}.{ref_key})")
        } else {
            mcp_value_literal(args.get("value").ok_or("missing required field 'value'")?)?
        };
        let mut sql = format!("PUT CONFIG {collection} {key} = {literal}");
        append_mcp_tags_clause(&mut sql, &tags);
        self.tool_keyed_query(sql)
    }

    fn tool_config_resolve(&self, args: &JsonValue) -> Result<String, String> {
        let collection = mcp_keyed_ident(get_str_field(args, "collection")?)?;
        let key = mcp_keyed_ident(get_str_field(args, "key")?)?;
        self.tool_keyed_query(format!("RESOLVE CONFIG {collection} {key}"))
    }

    fn tool_vault_get(&self, args: &JsonValue) -> Result<String, String> {
        let collection = mcp_keyed_ident(get_str_field(args, "collection")?)?;
        let key = mcp_keyed_ident(get_str_field(args, "key")?)?;
        self.tool_keyed_query(format!("VAULT GET {collection}.{key}"))
    }

    fn tool_vault_put(&self, args: &JsonValue) -> Result<String, String> {
        reject_mcp_volatile_options(args, "VAULT")?;
        let collection = mcp_keyed_ident(get_str_field(args, "collection")?)?;
        let key = mcp_keyed_ident(get_str_field(args, "key")?)?;
        let value = mcp_value_literal(args.get("value").ok_or("missing required field 'value'")?)?;
        let tags = parse_string_array_arg(args, "tags")?;
        let mut sql = format!("VAULT PUT {collection}.{key} = {value}");
        append_mcp_tags_clause(&mut sql, &tags);
        self.tool_keyed_query(sql)
    }

    fn tool_vault_unseal(&self, args: &JsonValue) -> Result<String, String> {
        let collection = mcp_keyed_ident(get_str_field(args, "collection")?)?;
        let key = mcp_keyed_ident(get_str_field(args, "key")?)?;
        self.tool_keyed_query(format!("UNSEAL VAULT {collection}.{key}"))
    }

    fn tool_keyed_query(&self, sql: String) -> Result<String, String> {
        let uc = QueryUseCases::new(&self.runtime);
        let result = uc
            .execute(ExecuteQueryInput { query: sql })
            .map_err(|e| format!("{}", e))?;
        let json = runtime_query_json(&result, &None, &None);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_delete(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let id = args
            .get("id")
            .and_then(|v| v.as_u64())
            .ok_or("missing required field 'id' (integer)")?;

        let uc = EntityUseCases::new(&self.runtime);
        let output = uc
            .delete(DeleteEntityInput {
                collection: collection.to_string(),
                id: EntityId::new(id),
            })
            .map_err(|e| format!("{}", e))?;

        let mut obj = Map::new();
        obj.insert("deleted".to_string(), JsonValue::Bool(output.deleted));
        obj.insert("id".to_string(), JsonValue::Number(output.id.raw() as f64));
        json_to_string(&JsonValue::Object(obj)).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_search_vector(&self, args: &JsonValue) -> Result<String, String> {
        let collection = args
            .get("collection")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'collection'")?;
        let vector_arr = args
            .get("vector")
            .and_then(|v| v.as_array())
            .ok_or("missing required field 'vector' (array of numbers)")?;

        let mut vector = Vec::with_capacity(vector_arr.len());
        for v in vector_arr {
            vector.push(
                v.as_f64()
                    .ok_or("'vector' array must contain only numbers")? as f32,
            );
        }
        let k = args
            .get("k")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(10);
        let min_score = args
            .get("min_score")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(0.0);

        let uc = QueryUseCases::new(&self.runtime);
        let results = uc
            .search_similar(SearchSimilarInput {
                collection: collection.to_string(),
                vector,
                k,
                min_score,
                text: None,
                provider: None,
            })
            .map_err(|e| format!("{}", e))?;

        let items: Vec<JsonValue> = results
            .iter()
            .map(|r| {
                let mut obj = Map::new();
                obj.insert(
                    "entity_id".to_string(),
                    JsonValue::Number(r.entity_id.raw() as f64),
                );
                obj.insert("score".to_string(), JsonValue::Number(r.score as f64));
                obj.insert("distance".to_string(), JsonValue::Number(r.distance as f64));
                JsonValue::Object(obj)
            })
            .collect();

        let mut obj = Map::new();
        obj.insert("count".to_string(), JsonValue::Number(items.len() as f64));
        obj.insert("results".to_string(), JsonValue::Array(items));
        json_to_string(&JsonValue::Object(obj)).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_search_text(&self, args: &JsonValue) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'query'")?;

        let collections = args
            .get("collections")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            });
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let fuzzy = args.get("fuzzy").and_then(|v| v.as_bool()).unwrap_or(false);

        let uc = QueryUseCases::new(&self.runtime);
        let result = uc
            .search_text(SearchTextInput {
                query: query.to_string(),
                collections,
                entity_types: None,
                capabilities: None,
                fields: None,
                limit,
                fuzzy,
            })
            .map_err(|e| format!("{}", e))?;

        let items: Vec<JsonValue> = result
            .matches
            .iter()
            .map(|m| {
                let mut obj = Map::new();
                obj.insert(
                    "entity_id".to_string(),
                    JsonValue::Number(m.entity.id.raw() as f64),
                );
                obj.insert(
                    "kind".to_string(),
                    JsonValue::String(format!("{:?}", m.entity.kind)),
                );
                obj.insert("score".to_string(), JsonValue::Number(m.score as f64));
                JsonValue::Object(obj)
            })
            .collect();

        let mut obj = Map::new();
        obj.insert("count".to_string(), JsonValue::Number(items.len() as f64));
        obj.insert("results".to_string(), JsonValue::Array(items));
        json_to_string(&JsonValue::Object(obj)).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_health(&self) -> Result<String, String> {
        let uc = CatalogUseCases::new(&self.runtime);
        let stats = uc.stats();
        let json = runtime_stats_json(&stats);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_graph_traverse(&self, args: &JsonValue) -> Result<String, String> {
        let source = args
            .get("source")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'source'")?;
        let direction = parse_direction(args.get("direction").and_then(|v| v.as_str()));
        let max_depth = args
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(3);
        let strategy = match args.get("strategy").and_then(|v| v.as_str()) {
            Some("dfs") => RuntimeGraphTraversalStrategy::Dfs,
            _ => RuntimeGraphTraversalStrategy::Bfs,
        };

        let uc = GraphUseCases::new(&self.runtime);
        let result = uc
            .traverse(GraphTraversalInput {
                source: source.to_string(),
                direction,
                max_depth,
                strategy,
                edge_labels: None,
                projection: None,
            })
            .map_err(|e| format!("{}", e))?;

        let visits: Vec<JsonValue> = result
            .visits
            .iter()
            .map(|v| {
                let mut obj = Map::new();
                obj.insert("depth".to_string(), JsonValue::Number(v.depth as f64));
                obj.insert("node_id".to_string(), JsonValue::String(v.node.id.clone()));
                obj.insert("label".to_string(), JsonValue::String(v.node.label.clone()));
                obj.insert(
                    "node_type".to_string(),
                    JsonValue::String(v.node.node_type.clone()),
                );
                JsonValue::Object(obj)
            })
            .collect();

        let edges: Vec<JsonValue> = result
            .edges
            .iter()
            .map(|e| {
                let mut obj = Map::new();
                obj.insert("source".to_string(), JsonValue::String(e.source.clone()));
                obj.insert("target".to_string(), JsonValue::String(e.target.clone()));
                obj.insert(
                    "edge_type".to_string(),
                    JsonValue::String(e.edge_type.clone()),
                );
                obj.insert("weight".to_string(), JsonValue::Number(e.weight as f64));
                JsonValue::Object(obj)
            })
            .collect();

        let mut obj = Map::new();
        obj.insert(
            "source".to_string(),
            JsonValue::String(result.source.clone()),
        );
        obj.insert(
            "visit_count".to_string(),
            JsonValue::Number(visits.len() as f64),
        );
        obj.insert("visits".to_string(), JsonValue::Array(visits));
        obj.insert("edges".to_string(), JsonValue::Array(edges));
        json_to_string(&JsonValue::Object(obj)).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_graph_shortest_path(&self, args: &JsonValue) -> Result<String, String> {
        let source = args
            .get("source")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'source'")?;
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'target'")?;
        let direction = parse_direction(args.get("direction").and_then(|v| v.as_str()));
        let algorithm = match args.get("algorithm").and_then(|v| v.as_str()) {
            Some("astar") | Some("a*") => RuntimeGraphPathAlgorithm::AStar,
            Some("bellman_ford") | Some("bellmanford") => RuntimeGraphPathAlgorithm::BellmanFord,
            Some("dijkstra") => RuntimeGraphPathAlgorithm::Dijkstra,
            _ => RuntimeGraphPathAlgorithm::Bfs,
        };

        let uc = GraphUseCases::new(&self.runtime);
        let result = uc
            .shortest_path(GraphShortestPathInput {
                source: source.to_string(),
                target: target.to_string(),
                direction,
                algorithm,
                edge_labels: None,
                projection: None,
            })
            .map_err(|e| format!("{}", e))?;

        let mut obj = Map::new();
        obj.insert(
            "source".to_string(),
            JsonValue::String(result.source.clone()),
        );
        obj.insert(
            "target".to_string(),
            JsonValue::String(result.target.clone()),
        );
        obj.insert(
            "nodes_visited".to_string(),
            JsonValue::Number(result.nodes_visited as f64),
        );

        match &result.path {
            Some(path) => {
                obj.insert("found".to_string(), JsonValue::Bool(true));
                obj.insert(
                    "hop_count".to_string(),
                    JsonValue::Number(path.hop_count as f64),
                );
                obj.insert(
                    "total_weight".to_string(),
                    JsonValue::Number(path.total_weight),
                );
                let nodes_json: Vec<JsonValue> = path
                    .nodes
                    .iter()
                    .map(|n| {
                        let mut nobj = Map::new();
                        nobj.insert("id".to_string(), JsonValue::String(n.id.clone()));
                        nobj.insert("label".to_string(), JsonValue::String(n.label.clone()));
                        JsonValue::Object(nobj)
                    })
                    .collect();
                obj.insert("nodes".to_string(), JsonValue::Array(nodes_json));
            }
            None => {
                obj.insert("found".to_string(), JsonValue::Bool(false));
            }
        }

        json_to_string(&JsonValue::Object(obj)).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_update(&self, args: &JsonValue) -> Result<String, String> {
        let collection = get_str_field(args, "collection")?;
        let set_obj = args.get("set").ok_or("missing 'set'")?;
        let where_clause = args
            .get("where_filter")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Build UPDATE SQL and execute via runtime
        let mut sql = format!("UPDATE {} SET ", collection);
        if let Some(obj) = set_obj.as_object() {
            let assignments: Vec<String> = obj
                .iter()
                .map(|(k, v)| {
                    let val_str = match v {
                        JsonValue::String(s) => format!("'{}'", s),
                        JsonValue::Number(n) => n.to_string(),
                        JsonValue::Bool(b) => b.to_string(),
                        _ => format!("'{}'", v),
                    };
                    format!("{} = {}", k, val_str)
                })
                .collect();
            sql.push_str(&assignments.join(", "));
        } else {
            return Err("'set' must be a JSON object".to_string());
        }
        if !where_clause.is_empty() {
            sql.push_str(&format!(" WHERE {}", where_clause));
        }

        let uc = QueryUseCases::new(&self.runtime);
        let result = uc
            .execute(ExecuteQueryInput { query: sql })
            .map_err(|e| format!("{}", e))?;

        let mut resp = Map::new();
        resp.insert("ok".into(), JsonValue::Bool(true));
        resp.insert(
            "affected_rows".into(),
            JsonValue::Number(result.affected_rows as f64),
        );
        json_to_string(&JsonValue::Object(resp)).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_scan(&self, args: &JsonValue) -> Result<String, String> {
        let collection = get_str_field(args, "collection")?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(10);
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(0);

        let uc = QueryUseCases::new(&self.runtime);
        let page = uc
            .scan(ScanCollectionInput {
                collection: collection.to_string(),
                offset,
                limit,
            })
            .map_err(|e| format!("{}", e))?;

        let json = crate::presentation::entity_json::scan_page_json(&page);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_graph_centrality(&self, args: &JsonValue) -> Result<String, String> {
        let algorithm_str = get_str_field(args, "algorithm")?;
        let algo = match algorithm_str {
            "degree" => RuntimeGraphCentralityAlgorithm::Degree,
            "closeness" => RuntimeGraphCentralityAlgorithm::Closeness,
            "betweenness" => RuntimeGraphCentralityAlgorithm::Betweenness,
            "eigenvector" => RuntimeGraphCentralityAlgorithm::Eigenvector,
            "pagerank" => RuntimeGraphCentralityAlgorithm::PageRank,
            _ => return Err(format!("unknown algorithm: {algorithm_str}")),
        };

        let uc = GraphUseCases::new(&self.runtime);
        let result = uc
            .centrality(GraphCentralityInput {
                algorithm: algo,
                top_k: 100,
                normalize: true,
                max_iterations: None,
                epsilon: None,
                alpha: None,
                projection: None,
            })
            .map_err(|e| format!("{}", e))?;

        let json = crate::presentation::graph_json::graph_centrality_json(&result);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_graph_community(&self, args: &JsonValue) -> Result<String, String> {
        let algorithm_str = get_str_field(args, "algorithm")?;
        let algo = match algorithm_str {
            "label_propagation" => RuntimeGraphCommunityAlgorithm::LabelPropagation,
            "louvain" => RuntimeGraphCommunityAlgorithm::Louvain,
            _ => return Err(format!("unknown algorithm: {algorithm_str}")),
        };
        let max_iterations = args
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let uc = GraphUseCases::new(&self.runtime);
        let result = uc
            .communities(GraphCommunitiesInput {
                algorithm: algo,
                min_size: 1,
                max_iterations,
                resolution: None,
                projection: None,
            })
            .map_err(|e| format!("{}", e))?;

        let json = crate::presentation::graph_json::graph_community_json(&result);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_graph_components(&self, args: &JsonValue) -> Result<String, String> {
        let mode = match args.get("mode").and_then(|v| v.as_str()) {
            Some("strongly_connected") => RuntimeGraphComponentsMode::Strong,
            _ => RuntimeGraphComponentsMode::Weak,
        };

        let uc = GraphUseCases::new(&self.runtime);
        let result = uc
            .components(GraphComponentsInput {
                mode,
                min_size: 1,
                projection: None,
            })
            .map_err(|e| format!("{}", e))?;

        let json = crate::presentation::graph_json::graph_components_json(&result);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_graph_cycles(&self, args: &JsonValue) -> Result<String, String> {
        let max_length = args
            .get("max_length")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(10);
        let max_cycles = args
            .get("max_cycles")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(100);

        let uc = GraphUseCases::new(&self.runtime);
        let result = uc
            .cycles(GraphCyclesInput {
                max_length,
                max_cycles,
                projection: None,
            })
            .map_err(|e| format!("{}", e))?;

        let json = crate::presentation::graph_json::graph_cycles_json(&result);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_graph_clustering(&self, _args: &JsonValue) -> Result<String, String> {
        let uc = GraphUseCases::new(&self.runtime);
        let result = uc
            .clustering(GraphClusteringInput {
                top_k: 100,
                include_triangles: true,
                projection: None,
            })
            .map_err(|e| format!("{}", e))?;

        let json = crate::presentation::graph_json::graph_clustering_json(&result);
        json_to_string(&json).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_create_collection(&self, args: &JsonValue) -> Result<String, String> {
        let name = get_str_field(args, "name")?;
        self.runtime
            .db()
            .store()
            .create_collection(name)
            .map_err(|e| format!("{e:?}"))?;
        let mut resp = Map::new();
        resp.insert("ok".into(), JsonValue::Bool(true));
        resp.insert("collection".into(), JsonValue::String(name.to_string()));
        json_to_string(&JsonValue::Object(resp)).map_err(|e| format!("serialization error: {}", e))
    }

    fn tool_drop_collection(&self, args: &JsonValue) -> Result<String, String> {
        let name = get_str_field(args, "name")?;
        self.runtime
            .db()
            .store()
            .drop_collection(name)
            .map_err(|e| format!("{e:?}"))?;
        let mut resp = Map::new();
        resp.insert("ok".into(), JsonValue::Bool(true));
        resp.insert("dropped".into(), JsonValue::String(name.to_string()));
        json_to_string(&JsonValue::Object(resp)).map_err(|e| format!("serialization error: {}", e))
    }
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn format_mcp_ask_parse_error(err: crate::runtime::ai::mcp_ask_tool::ParseError) -> String {
    use crate::runtime::ai::mcp_ask_tool::ParseError;

    match err {
        ParseError::NotAnObject => "arguments must be an object".to_string(),
        ParseError::MissingQuestion => "missing required field 'question'".to_string(),
        ParseError::QuestionWrongType => "field 'question' must be a string".to_string(),
        ParseError::WrongType { path, expected } => {
            format!("{path} must be {expected}")
        }
        ParseError::OutOfRange { path, detail } => {
            format!("{path} out of range: {detail}")
        }
        ParseError::CacheAndNocache => {
            "options.cache and options.nocache are mutually exclusive".to_string()
        }
        ParseError::UnknownOption { path } => format!("unknown option {path}"),
    }
}

fn parse_direction(s: Option<&str>) -> RuntimeGraphDirection {
    match s {
        Some("incoming") => RuntimeGraphDirection::Incoming,
        Some("both") => RuntimeGraphDirection::Both,
        _ => RuntimeGraphDirection::Outgoing,
    }
}

/// Parse optional metadata from an `args` JSON object.
fn parse_metadata_arg(
    args: &JsonValue,
) -> Result<Vec<(String, crate::storage::unified::MetadataValue)>, String> {
    match args.get("metadata").and_then(|v| v.as_object()) {
        Some(obj) => {
            let mut out = Vec::with_capacity(obj.len());
            for (key, value) in obj {
                let mv = crate::application::entity::json_to_metadata_value(value)
                    .map_err(|e| format!("{}", e))?;
                out.push((key.clone(), mv));
            }
            Ok(out)
        }
        None => Ok(vec![]),
    }
}

fn parse_string_array_arg(args: &JsonValue, field: &str) -> Result<Vec<String>, String> {
    match args.get(field) {
        None | Some(JsonValue::Null) => Ok(Vec::new()),
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| format!("field '{field}' must be an array of strings"))
            })
            .collect(),
        _ => Err(format!("field '{field}' must be an array of strings")),
    }
}

fn mcp_keyed_ident(value: &str) -> Result<String, String> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
    {
        Ok(value.to_string())
    } else {
        Err(
            "keyed collection and key names must use letters, numbers, underscores, or dots"
                .to_string(),
        )
    }
}

fn mcp_value_literal(value: &JsonValue) -> Result<String, String> {
    match value {
        JsonValue::String(value) => Ok(format!("'{}'", value.replace('\'', "''"))),
        JsonValue::Number(value) => Ok(value.to_string()),
        JsonValue::Bool(value) => Ok(value.to_string()),
        JsonValue::Null => Ok("NULL".to_string()),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            json_to_string(value).map_err(|err| format!("serialization error: {err}"))
        }
    }
}

fn append_mcp_tags_clause(sql: &mut String, tags: &[String]) {
    if tags.is_empty() {
        return;
    }
    sql.push_str(" TAGS [");
    for (index, tag) in tags.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('\'');
        sql.push_str(&tag.replace('\'', "''"));
        sql.push('\'');
    }
    sql.push(']');
}

fn reject_mcp_volatile_options(args: &JsonValue, domain: &str) -> Result<(), String> {
    for field in ["ttl", "ttl_ms", "expire", "expire_ms", "expires_at"] {
        if args.get(field).is_some() {
            return Err(format!(
                "{domain} does not support TTL or expiration options"
            ));
        }
    }
    Ok(())
}

// Convert a storage Value to JSON (local helper to avoid visibility issues).
fn get_str_field<'a>(args: &'a JsonValue, field: &str) -> Result<&'a str, String> {
    args.get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing '{field}'"))
}

// Auth tool implementations
impl McpServer {
    fn tool_auth_bootstrap(&self, args: &JsonValue) -> Result<String, String> {
        let username = get_str_field(args, "username")?;
        let password = get_str_field(args, "password")?;

        let br = self
            .auth_store
            .bootstrap(username, password)
            .map_err(|e| e.to_string())?;

        let mut result = Map::new();
        result.insert("ok".into(), JsonValue::Bool(true));
        result.insert("username".into(), JsonValue::String(br.user.username));
        result.insert(
            "role".into(),
            JsonValue::String(br.user.role.as_str().into()),
        );
        result.insert("api_key".into(), JsonValue::String(br.api_key.key));
        result.insert("api_key_name".into(), JsonValue::String(br.api_key.name));
        if let Some(cert) = br.certificate {
            result.insert("certificate".into(), JsonValue::String(cert));
            result.insert(
                "message".into(),
                JsonValue::String(
                    "Save this certificate — it is the ONLY way to unseal the vault after restart."
                        .into(),
                ),
            );
        } else {
            result.insert(
                "message".into(),
                JsonValue::String(
                    "First admin user created. Save the API key — it won't be shown again.".into(),
                ),
            );
        }
        json_to_string(&JsonValue::Object(result))
    }

    fn tool_auth_create_user(&self, args: &JsonValue) -> Result<String, String> {
        let username = get_str_field(args, "username")?;
        let password = get_str_field(args, "password")?;
        let role_str = get_str_field(args, "role")?;
        let role = Role::from_str(role_str).ok_or_else(|| format!("invalid role: {role_str}"))?;

        self.auth_store
            .create_user(username, password, role)
            .map_err(|e| e.to_string())?;

        let mut result = Map::new();
        result.insert("ok".into(), JsonValue::Bool(true));
        result.insert("username".into(), JsonValue::String(username.into()));
        result.insert("role".into(), JsonValue::String(role.as_str().into()));
        json_to_string(&JsonValue::Object(result))
    }

    fn tool_auth_login(&self, args: &JsonValue) -> Result<String, String> {
        let username = get_str_field(args, "username")?;
        let password = get_str_field(args, "password")?;

        let session = self
            .auth_store
            .authenticate(username, password)
            .map_err(|e| e.to_string())?;

        let mut result = Map::new();
        result.insert("ok".into(), JsonValue::Bool(true));
        result.insert("token".into(), JsonValue::String(session.token));
        result.insert("username".into(), JsonValue::String(session.username));
        result.insert(
            "role".into(),
            JsonValue::String(session.role.as_str().into()),
        );
        result.insert(
            "expires_at".into(),
            JsonValue::Number(session.expires_at as f64),
        );
        json_to_string(&JsonValue::Object(result))
    }

    fn tool_auth_create_api_key(&self, args: &JsonValue) -> Result<String, String> {
        let username = get_str_field(args, "username")?;
        let name = get_str_field(args, "name")?;
        let role_str = get_str_field(args, "role")?;
        let role = Role::from_str(role_str).ok_or_else(|| format!("invalid role: {role_str}"))?;

        let key = self
            .auth_store
            .create_api_key(username, name, role)
            .map_err(|e| e.to_string())?;

        let mut result = Map::new();
        result.insert("ok".into(), JsonValue::Bool(true));
        result.insert("key".into(), JsonValue::String(key.key));
        result.insert("name".into(), JsonValue::String(key.name));
        result.insert("role".into(), JsonValue::String(key.role.as_str().into()));
        json_to_string(&JsonValue::Object(result))
    }

    fn tool_auth_list_users(&self) -> Result<String, String> {
        let users = self.auth_store.list_users();
        let arr: Vec<JsonValue> = users
            .into_iter()
            .map(|u| {
                let mut obj = Map::new();
                obj.insert("username".into(), JsonValue::String(u.username));
                obj.insert("role".into(), JsonValue::String(u.role.as_str().into()));
                obj.insert("enabled".into(), JsonValue::Bool(u.enabled));
                obj.insert(
                    "api_key_count".into(),
                    JsonValue::Number(u.api_keys.len() as f64),
                );
                JsonValue::Object(obj)
            })
            .collect();
        json_to_string(&JsonValue::Array(arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    static ASK_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn make_server() -> McpServer {
        let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
        McpServer::new(rt)
    }

    fn parse_json(s: &str) -> JsonValue {
        json_from_str(s).expect("valid json")
    }

    #[test]
    fn tools_list_registers_reddb_ask_descriptor() {
        let srv = make_server();
        let response = srv.handle_tools_list(Some(&JsonValue::Number(1.0)));
        let parsed = parse_json(&response);
        let tools = parsed
            .get("result")
            .and_then(|result| result.get("tools"))
            .and_then(JsonValue::as_array)
            .expect("tools array");

        let ask = tools
            .iter()
            .find(|tool| tool.get("name").and_then(JsonValue::as_str) == Some("reddb.ask"))
            .expect("reddb.ask registered");

        let desc = ask
            .get("description")
            .and_then(JsonValue::as_str)
            .expect("description");
        assert!(desc.contains("citations"), "description: {desc}");
        assert!(desc.contains("sources_flat"), "description: {desc}");
        assert!(desc.contains("URN"), "description: {desc}");

        let options = ask
            .get("inputSchema")
            .and_then(|schema| schema.get("properties"))
            .and_then(|props| props.get("options"))
            .and_then(|opts| opts.get("properties"))
            .and_then(JsonValue::as_object)
            .expect("options properties");
        for key in [
            "strict",
            "using",
            "model",
            "limit",
            "min_score",
            "depth",
            "temperature",
            "seed",
            "cache",
            "nocache",
        ] {
            assert!(
                options.contains_key(key),
                "missing option {key} in {options:?}"
            );
        }
    }

    #[test]
    fn tools_call_reddb_ask_uses_typed_argument_parser() {
        let srv = make_server();
        let params = parse_json(
            r#"{
                "name": "reddb.ask",
                "arguments": {
                    "question": "what cites this?",
                    "options": { "tempurature": 0.2 }
                }
            }"#,
        );

        let response = srv.handle_tools_call(Some(&JsonValue::Number(1.0)), Some(&params));
        let parsed = parse_json(&response);
        let result = parsed.get("result").expect("result");
        assert_eq!(
            result.get("isError").and_then(JsonValue::as_bool),
            Some(true)
        );
        let text = result
            .get("content")
            .and_then(JsonValue::as_array)
            .and_then(|content| content.first())
            .and_then(|item| item.get("text"))
            .and_then(JsonValue::as_str)
            .expect("error text");
        assert!(text.contains("options.tempurature"), "text: {text}");
    }

    #[test]
    fn tools_call_reddb_ask_returns_canonical_citation_envelope() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = AskStub::start();
        let _api_base = EnvVarGuard::set(
            "REDDB_OLLAMA_API_BASE",
            &format!("http://{}/v1", stub.addr()),
        );

        let srv = make_server();
        srv.tool_query(&parse_json(
            r#"{"sql":"CREATE TABLE travel (id INTEGER, passport TEXT, notes TEXT)"}"#,
        ))
        .expect("ddl ok");
        srv.tool_query(&parse_json(
            r#"{"sql":"INSERT INTO travel (id, passport, notes) VALUES (1, 'PT-002', 'incident FDD-12313 escalated')"}"#,
        ))
        .expect("insert ok");

        let params = parse_json(
            r#"{
                "name": "reddb.ask",
                "arguments": {
                    "question": "passport FDD-12313",
                    "options": {
                        "strict": false,
                        "using": "ollama",
                        "model": "mock-ask",
                        "limit": 1,
                        "min_score": 0,
                        "depth": 0,
                        "temperature": 0,
                        "seed": 0,
                        "cache": { "ttl": "5m" }
                    }
                }
            }"#,
        );

        let response = srv.handle_tools_call(Some(&JsonValue::Number(1.0)), Some(&params));
        let parsed = parse_json(&response);
        let result = parsed.get("result").expect("result");
        assert_ne!(
            result.get("isError").and_then(JsonValue::as_bool),
            Some(true),
            "response: {response}"
        );
        let text = result
            .get("content")
            .and_then(JsonValue::as_array)
            .and_then(|content| content.first())
            .and_then(|item| item.get("text"))
            .and_then(JsonValue::as_str)
            .expect("tool text");
        let envelope = parse_json(text);

        assert_eq!(
            envelope.get("answer").and_then(JsonValue::as_str),
            Some("FDD-12313 escalated [^1].")
        );
        assert_eq!(
            envelope.get("provider").and_then(JsonValue::as_str),
            Some("ollama")
        );
        assert_eq!(
            envelope.get("model").and_then(JsonValue::as_str),
            Some("mock-ask")
        );
        assert_eq!(
            envelope.get("cache_hit").and_then(JsonValue::as_bool),
            Some(false)
        );
        assert!(envelope
            .get("sources_flat")
            .and_then(JsonValue::as_array)
            .is_some());
        assert!(envelope
            .get("citations")
            .and_then(JsonValue::as_array)
            .is_some());
        assert!(envelope
            .get("validation")
            .and_then(JsonValue::as_object)
            .is_some());
        assert!(
            envelope.get("rows").is_none(),
            "ASK must not be row-wrapped: {text}"
        );
    }

    #[test]
    fn tool_query_without_params_keeps_legacy_path() {
        let srv = make_server();
        let args = parse_json(r#"{"sql":"SELECT 1 AS one"}"#);
        let out = srv.tool_query(&args).expect("query ok");
        assert!(out.contains("\"one\""), "expected 'one' column in {out}");
    }

    #[test]
    fn tool_query_binds_int_and_text_params() {
        let srv = make_server();
        srv.tool_query(&parse_json(
            r#"{"sql":"CREATE TABLE mcpp (id INTEGER, name TEXT)"}"#,
        ))
        .expect("ddl ok");
        srv.tool_query(&parse_json(
            r#"{"sql":"INSERT INTO mcpp (id, name) VALUES (1, 'Alice')"}"#,
        ))
        .expect("insert 1");
        srv.tool_query(&parse_json(
            r#"{"sql":"INSERT INTO mcpp (id, name) VALUES (2, 'Bob')"}"#,
        ))
        .expect("insert 2");

        let out = srv
            .tool_query(&parse_json(
                r#"{"sql":"SELECT * FROM mcpp WHERE id = $1 AND name = $2","params":[1,"Alice"]}"#,
            ))
            .expect("query with params ok");
        assert!(out.contains("Alice"), "expected Alice in {out}");
        assert!(!out.contains("Bob"), "Bob must not match: {out}");
    }

    #[test]
    fn tool_query_params_must_be_array() {
        let srv = make_server();
        let err = srv
            .tool_query(&parse_json(
                r#"{"sql":"SELECT 1","params":{"not":"array"}}"#,
            ))
            .expect_err("must reject non-array params");
        assert!(err.contains("array"), "got {err}");
    }

    #[test]
    fn tool_query_param_arity_mismatch_surfaces_error() {
        let srv = make_server();
        srv.tool_query(&parse_json(
            r#"{"sql":"CREATE TABLE mcpa (id INTEGER)"}"#,
        ))
        .expect("ddl ok");
        let err = srv
            .tool_query(&parse_json(
                r#"{"sql":"SELECT * FROM mcpa WHERE id = $1","params":[1,2]}"#,
            ))
            .expect_err("arity mismatch");
        assert!(
            err.contains("number of parameters") || err.contains("expects"),
            "got {err}"
        );
    }

    #[test]
    fn tool_query_vector_param_binds_into_search_similar() {
        let srv = make_server();
        let out = srv
            .tool_query(&parse_json(
                r#"{"sql":"SEARCH SIMILAR $1 COLLECTION mcpv LIMIT 5","params":[[0.1,0.2,0.3]]}"#,
            ));
        // The collection doesn't exist; we only need to confirm the
        // param-bind path runs (i.e. the error reflects runtime semantics,
        // not a `$N` placeholder being unresolved).
        if let Err(e) = out {
            assert!(
                !e.contains("placeholder") && !e.contains("Parameter"),
                "param did not bind: {e}"
            );
        }
    }

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.previous.take() {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    struct AskStub {
        addr: SocketAddr,
        shutdown: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl AskStub {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("stub bind");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let addr = listener.local_addr().expect("local addr");
            let shutdown = Arc::new(AtomicBool::new(false));
            let server_shutdown = Arc::clone(&shutdown);
            let handle = thread::spawn(move || {
                while !server_shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request = read_stub_request(&mut stream);
                            if request.contains("/embeddings") {
                                write_json_response(
                                    &mut stream,
                                    r#"{"model":"mock-embedding","data":[{"index":0,"embedding":[1,0,0]}],"usage":{"prompt_tokens":3,"total_tokens":3}}"#,
                                );
                            } else {
                                write_json_response(
                                    &mut stream,
                                    r#"{"model":"mock-ask","choices":[{"message":{"role":"assistant","content":"FDD-12313 escalated [^1]."},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}}"#,
                                );
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                addr,
                shutdown,
                handle: Some(handle),
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }
    }

    impl Drop for AskStub {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.addr);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn read_stub_request(stream: &mut TcpStream) -> String {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
        let mut buffer = [0_u8; 4096];
        let count = stream.read(&mut buffer).unwrap_or(0);
        String::from_utf8_lossy(&buffer[..count]).into_owned()
    }

    fn write_json_response(stream: &mut TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write stub response");
    }
}
