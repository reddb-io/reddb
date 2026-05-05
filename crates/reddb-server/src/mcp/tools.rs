//! MCP tool definitions for RedDB.
//!
//! Each tool exposes a specific RedDB capability to AI agents with a
//! typed JSON Schema input specification.

use crate::json::{Map, Value as JsonValue};

/// Definition of an MCP tool exposed by the RedDB server.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: JsonValue,
}

/// Build a JSON Schema object from a list of field descriptors.
fn schema(properties: Vec<(&str, &str, &str)>, required: Vec<&str>) -> JsonValue {
    let mut props = Map::new();
    for (name, field_type, description) in properties {
        let mut field = Map::new();
        field.insert(
            "type".to_string(),
            JsonValue::String(field_type.to_string()),
        );
        if !description.is_empty() {
            field.insert(
                "description".to_string(),
                JsonValue::String(description.to_string()),
            );
        }
        props.insert(name.to_string(), JsonValue::Object(field));
    }

    let mut obj = Map::new();
    obj.insert("type".to_string(), JsonValue::String("object".to_string()));
    obj.insert("properties".to_string(), JsonValue::Object(props));
    obj.insert(
        "required".to_string(),
        JsonValue::Array(
            required
                .into_iter()
                .map(|s| JsonValue::String(s.to_string()))
                .collect(),
        ),
    );
    obj.insert("additionalProperties".to_string(), JsonValue::Bool(false));
    JsonValue::Object(obj)
}

/// Build a JSON Schema object that accepts items with flexible inner types.
fn schema_with_nested(properties: Vec<(&str, JsonValue)>, required: Vec<&str>) -> JsonValue {
    let mut props = Map::new();
    for (name, descriptor) in properties {
        props.insert(name.to_string(), descriptor);
    }

    let mut obj = Map::new();
    obj.insert("type".to_string(), JsonValue::String("object".to_string()));
    obj.insert("properties".to_string(), JsonValue::Object(props));
    obj.insert(
        "required".to_string(),
        JsonValue::Array(
            required
                .into_iter()
                .map(|s| JsonValue::String(s.to_string()))
                .collect(),
        ),
    );
    obj.insert("additionalProperties".to_string(), JsonValue::Bool(false));
    JsonValue::Object(obj)
}

/// Simple string field descriptor.
fn string_field(description: &str) -> JsonValue {
    let mut f = Map::new();
    f.insert("type".to_string(), JsonValue::String("string".to_string()));
    f.insert(
        "description".to_string(),
        JsonValue::String(description.to_string()),
    );
    JsonValue::Object(f)
}

/// Simple number field descriptor.
fn number_field(description: &str) -> JsonValue {
    let mut f = Map::new();
    f.insert("type".to_string(), JsonValue::String("number".to_string()));
    f.insert(
        "description".to_string(),
        JsonValue::String(description.to_string()),
    );
    JsonValue::Object(f)
}

/// Simple integer field descriptor.
fn integer_field(description: &str) -> JsonValue {
    let mut f = Map::new();
    f.insert("type".to_string(), JsonValue::String("integer".to_string()));
    f.insert(
        "description".to_string(),
        JsonValue::String(description.to_string()),
    );
    JsonValue::Object(f)
}

/// Simple boolean field descriptor.
fn boolean_field(description: &str) -> JsonValue {
    let mut f = Map::new();
    f.insert("type".to_string(), JsonValue::String("boolean".to_string()));
    f.insert(
        "description".to_string(),
        JsonValue::String(description.to_string()),
    );
    JsonValue::Object(f)
}

/// Object field descriptor (accepts arbitrary JSON object).
fn object_field(description: &str) -> JsonValue {
    let mut f = Map::new();
    f.insert("type".to_string(), JsonValue::String("object".to_string()));
    f.insert(
        "description".to_string(),
        JsonValue::String(description.to_string()),
    );
    JsonValue::Object(f)
}

/// Array-of-numbers field descriptor.
fn number_array_field(description: &str) -> JsonValue {
    let mut items = Map::new();
    items.insert("type".to_string(), JsonValue::String("number".to_string()));

    let mut f = Map::new();
    f.insert("type".to_string(), JsonValue::String("array".to_string()));
    f.insert("items".to_string(), JsonValue::Object(items));
    f.insert(
        "description".to_string(),
        JsonValue::String(description.to_string()),
    );
    JsonValue::Object(f)
}

/// Return all tool definitions exposed by the RedDB MCP server.
pub fn all_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "reddb_query",
            description: "Execute a SQL or universal query against RedDB. Supports SELECT, INSERT, UPDATE, DELETE, and graph queries (Gremlin, Cypher, SPARQL).",
            input_schema: schema(
                vec![("sql", "string", "SQL or universal query to execute")],
                vec!["sql"],
            ),
        },
        ToolDef {
            name: "reddb_collections",
            description: "List all collections in the database.",
            input_schema: schema(vec![], vec![]),
        },
        ToolDef {
            name: "reddb_insert_row",
            description: "Insert a table row into a collection.",
            input_schema: schema_with_nested(
                vec![
                    ("collection", string_field("Target collection name")),
                    ("data", object_field("Object with field name/value pairs to insert")),
                    ("metadata", object_field("Optional metadata key/value pairs")),
                ],
                vec!["collection", "data"],
            ),
        },
        ToolDef {
            name: "reddb_insert_node",
            description: "Insert a graph node into a collection.",
            input_schema: schema_with_nested(
                vec![
                    ("collection", string_field("Target collection name")),
                    ("label", string_field("Node label (identifier)")),
                    ("node_type", string_field("Optional node type classification")),
                    ("properties", object_field("Optional node properties as key/value pairs")),
                    ("metadata", object_field("Optional metadata key/value pairs")),
                ],
                vec!["collection", "label"],
            ),
        },
        ToolDef {
            name: "reddb_insert_edge",
            description: "Insert a graph edge between two nodes.",
            input_schema: schema_with_nested(
                vec![
                    ("collection", string_field("Target collection name")),
                    ("label", string_field("Edge label (relationship type)")),
                    ("from", integer_field("Source node entity ID")),
                    ("to", integer_field("Target node entity ID")),
                    ("weight", number_field("Optional edge weight (default 1.0)")),
                    ("properties", object_field("Optional edge properties")),
                    ("metadata", object_field("Optional metadata key/value pairs")),
                ],
                vec!["collection", "label", "from", "to"],
            ),
        },
        ToolDef {
            name: "reddb_insert_vector",
            description: "Insert a vector embedding into a collection.",
            input_schema: schema_with_nested(
                vec![
                    ("collection", string_field("Target collection name")),
                    ("dense", number_array_field("Dense vector (array of floats)")),
                    ("content", string_field("Optional text content associated with the vector")),
                    ("metadata", object_field("Optional metadata key/value pairs")),
                ],
                vec!["collection", "dense"],
            ),
        },
        ToolDef {
            name: "reddb_insert_document",
            description: "Insert a JSON document into a collection.",
            input_schema: schema_with_nested(
                vec![
                    ("collection", string_field("Target collection name")),
                    ("body", object_field("JSON document body")),
                    ("metadata", object_field("Optional metadata key/value pairs")),
                ],
                vec!["collection", "body"],
            ),
        },
        ToolDef {
            name: "reddb_kv_get",
            description: "Get a value by key from a key-value collection.",
            input_schema: schema(
                vec![
                    ("collection", "string", "Collection name"),
                    ("key", "string", "Key to retrieve"),
                ],
                vec!["collection", "key"],
            ),
        },
        ToolDef {
            name: "reddb_kv_set",
            description: "Set a key-value pair in a collection.",
            input_schema: schema_with_nested(
                vec![
                    ("collection", string_field("Collection name")),
                    ("key", string_field("Key to set")),
                    ("value", {
                        let mut f = Map::new();
                        f.insert("description".to_string(), JsonValue::String("Value to store (string, number, boolean, or null)".to_string()));
                        JsonValue::Object(f)
                    }),
                    ("metadata", object_field("Optional metadata key/value pairs")),
                ],
                vec!["collection", "key", "value"],
            ),
        },
        ToolDef {
            name: "reddb_delete",
            description: "Delete an entity by ID from a collection.",
            input_schema: schema(
                vec![
                    ("collection", "string", "Collection name"),
                    ("id", "integer", "Entity ID to delete"),
                ],
                vec!["collection", "id"],
            ),
        },
        ToolDef {
            name: "reddb_search_vector",
            description: "Search for similar vectors using cosine similarity.",
            input_schema: schema_with_nested(
                vec![
                    ("collection", string_field("Collection to search in")),
                    ("vector", number_array_field("Query vector (array of floats)")),
                    ("k", integer_field("Number of results to return (default 10)")),
                    ("min_score", number_field("Minimum similarity score threshold (default 0.0)")),
                ],
                vec!["collection", "vector"],
            ),
        },
        ToolDef {
            name: "reddb_search_text",
            description: "Full-text search across collections.",
            input_schema: schema_with_nested(
                vec![
                    ("query", string_field("Search query string")),
                    ("collections", {
                        let mut items = Map::new();
                        items.insert("type".to_string(), JsonValue::String("string".to_string()));
                        let mut f = Map::new();
                        f.insert("type".to_string(), JsonValue::String("array".to_string()));
                        f.insert("items".to_string(), JsonValue::Object(items));
                        f.insert("description".to_string(), JsonValue::String("Optional list of collections to search".to_string()));
                        JsonValue::Object(f)
                    }),
                    ("limit", integer_field("Maximum number of results (default 10)")),
                    ("fuzzy", boolean_field("Enable fuzzy matching (default false)")),
                ],
                vec!["query"],
            ),
        },
        ToolDef {
            name: "reddb_health",
            description: "Check database health and return runtime statistics.",
            input_schema: schema(vec![], vec![]),
        },
        ToolDef {
            name: "reddb_graph_traverse",
            description: "Traverse the graph from a source node using BFS or DFS.",
            input_schema: schema_with_nested(
                vec![
                    ("source", string_field("Source node label to start traversal from")),
                    ("direction", string_field("Traversal direction: 'outgoing', 'incoming', or 'both' (default 'outgoing')")),
                    ("max_depth", integer_field("Maximum traversal depth (default 3)")),
                    ("strategy", string_field("Traversal strategy: 'bfs' or 'dfs' (default 'bfs')")),
                ],
                vec!["source"],
            ),
        },
        ToolDef {
            name: "reddb_graph_shortest_path",
            description: "Find the shortest path between two graph nodes.",
            input_schema: schema_with_nested(
                vec![
                    ("source", string_field("Source node label")),
                    ("target", string_field("Target node label")),
                    ("direction", string_field("Edge direction: 'outgoing', 'incoming', or 'both' (default 'outgoing')")),
                    (
                        "algorithm",
                        string_field(
                            "Path algorithm: 'bfs', 'dijkstra', 'astar', or 'bellman_ford' (default 'bfs')",
                        ),
                    ),
                ],
                vec!["source", "target"],
            ),
        },
        // Auth tools
        ToolDef {
            name: "reddb_auth_bootstrap",
            description: "Bootstrap the first admin user. Only works when no users exist yet. Returns the admin user and an API key.",
            input_schema: schema(
                vec![
                    ("username", "string", "Admin username"),
                    ("password", "string", "Admin password"),
                ],
                vec!["username", "password"],
            ),
        },
        ToolDef {
            name: "reddb_auth_create_user",
            description: "Create a new database user with a role (admin, write, or read).",
            input_schema: schema(
                vec![
                    ("username", "string", "Username for the new user"),
                    ("password", "string", "Password for the new user"),
                    ("role", "string", "Role: 'admin', 'write', or 'read'"),
                ],
                vec!["username", "password", "role"],
            ),
        },
        ToolDef {
            name: "reddb_auth_login",
            description: "Login with username and password. Returns a session token.",
            input_schema: schema(
                vec![
                    ("username", "string", "Username"),
                    ("password", "string", "Password"),
                ],
                vec!["username", "password"],
            ),
        },
        ToolDef {
            name: "reddb_auth_create_api_key",
            description: "Create a persistent API key for a user.",
            input_schema: schema(
                vec![
                    ("username", "string", "Username to create the key for"),
                    ("name", "string", "Human-readable label for the key"),
                    ("role", "string", "Role for this key: 'admin', 'write', or 'read'"),
                ],
                vec!["username", "name", "role"],
            ),
        },
        ToolDef {
            name: "reddb_auth_list_users",
            description: "List all database users and their roles.",
            input_schema: schema(vec![], vec![]),
        },
        // Update / Scan tools
        ToolDef {
            name: "reddb_update",
            description: "Update entities in a collection matching a filter.",
            input_schema: schema_with_nested(
                vec![
                    ("collection", string_field("Collection name")),
                    ("set", object_field("Key-value pairs to update")),
                    (
                        "where_filter",
                        string_field(
                            "Optional SQL WHERE clause (e.g., \"age > 21\")",
                        ),
                    ),
                ],
                vec!["collection", "set"],
            ),
        },
        ToolDef {
            name: "reddb_scan",
            description: "Scan entities from a collection with pagination.",
            input_schema: schema_with_nested(
                vec![
                    ("collection", string_field("Collection to scan")),
                    ("limit", integer_field("Maximum number of results (default 10)")),
                    ("offset", integer_field("Number of records to skip (default 0)")),
                ],
                vec!["collection"],
            ),
        },
        // Graph analytics tools
        ToolDef {
            name: "reddb_graph_centrality",
            description: "Compute centrality scores for graph nodes. Algorithms: degree, closeness, betweenness, eigenvector, pagerank.",
            input_schema: schema_with_nested(
                vec![(
                    "algorithm",
                    string_field(
                        "Centrality algorithm: 'degree', 'closeness', 'betweenness', 'eigenvector', 'pagerank'",
                    ),
                )],
                vec!["algorithm"],
            ),
        },
        ToolDef {
            name: "reddb_graph_community",
            description: "Detect communities in the graph. Algorithms: label_propagation, louvain.",
            input_schema: schema_with_nested(
                vec![
                    (
                        "algorithm",
                        string_field(
                            "Community detection algorithm: 'label_propagation' or 'louvain'",
                        ),
                    ),
                    (
                        "max_iterations",
                        integer_field("Maximum iterations (default 100)"),
                    ),
                ],
                vec!["algorithm"],
            ),
        },
        ToolDef {
            name: "reddb_graph_components",
            description: "Find connected components in the graph.",
            input_schema: schema_with_nested(
                vec![(
                    "mode",
                    string_field(
                        "Component mode: 'weakly_connected' or 'strongly_connected' (default 'weakly_connected')",
                    ),
                )],
                vec![],
            ),
        },
        ToolDef {
            name: "reddb_graph_cycles",
            description: "Detect cycles in the graph.",
            input_schema: schema_with_nested(
                vec![
                    (
                        "max_length",
                        integer_field("Maximum cycle length (default 10)"),
                    ),
                    (
                        "max_cycles",
                        integer_field("Maximum number of cycles to return (default 100)"),
                    ),
                ],
                vec![],
            ),
        },
        ToolDef {
            name: "reddb_graph_clustering",
            description: "Compute clustering coefficient for the graph.",
            input_schema: schema(vec![], vec![]),
        },
        // DDL tools
        ToolDef {
            name: "reddb_create_collection",
            description: "Create a new collection (table) in the database.",
            input_schema: schema(
                vec![("name", "string", "Collection name to create")],
                vec!["name"],
            ),
        },
        ToolDef {
            name: "reddb_drop_collection",
            description: "Drop (delete) a collection from the database.",
            input_schema: schema(
                vec![("name", "string", "Collection name to drop")],
                vec!["name"],
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_tools_defined() {
        let tools = all_tools();
        assert!(tools.len() >= 24);
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert!(names.contains(&"reddb_query"));
        assert!(names.contains(&"reddb_collections"));
        assert!(names.contains(&"reddb_insert_row"));
        assert!(names.contains(&"reddb_insert_node"));
        assert!(names.contains(&"reddb_insert_edge"));
        assert!(names.contains(&"reddb_insert_vector"));
        assert!(names.contains(&"reddb_insert_document"));
        assert!(names.contains(&"reddb_kv_get"));
        assert!(names.contains(&"reddb_kv_set"));
        assert!(names.contains(&"reddb_delete"));
        assert!(names.contains(&"reddb_search_vector"));
        assert!(names.contains(&"reddb_search_text"));
        assert!(names.contains(&"reddb_health"));
        assert!(names.contains(&"reddb_graph_traverse"));
        assert!(names.contains(&"reddb_graph_shortest_path"));
        // New tools
        assert!(names.contains(&"reddb_update"));
        assert!(names.contains(&"reddb_scan"));
        assert!(names.contains(&"reddb_graph_centrality"));
        assert!(names.contains(&"reddb_graph_community"));
        assert!(names.contains(&"reddb_graph_components"));
        assert!(names.contains(&"reddb_graph_cycles"));
        assert!(names.contains(&"reddb_graph_clustering"));
        assert!(names.contains(&"reddb_create_collection"));
        assert!(names.contains(&"reddb_drop_collection"));
    }

    #[test]
    fn test_tool_schemas_have_type() {
        for tool in all_tools() {
            assert_eq!(
                tool.input_schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "tool '{}' schema must have type=object",
                tool.name,
            );
        }
    }

    #[test]
    fn test_update_tool_schema() {
        let tools = all_tools();
        let tool = tools.iter().find(|t| t.name == "reddb_update").unwrap();
        assert_eq!(tool.name, "reddb_update");
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap();
        assert!(props.contains_key("collection"));
        assert!(props.contains_key("set"));
        assert!(props.contains_key("where_filter"));
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap();
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.contains(&"collection"));
        assert!(required_strs.contains(&"set"));
        assert!(!required_strs.contains(&"where_filter"));
    }

    #[test]
    fn test_scan_tool_schema() {
        let tools = all_tools();
        let tool = tools.iter().find(|t| t.name == "reddb_scan").unwrap();
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap();
        assert!(props.contains_key("collection"));
        assert!(props.contains_key("limit"));
        assert!(props.contains_key("offset"));
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap();
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.contains(&"collection"));
        // limit and offset are optional
        assert!(!required_strs.contains(&"limit"));
        assert!(!required_strs.contains(&"offset"));
    }

    #[test]
    fn test_graph_centrality_tool_schema() {
        let tools = all_tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "reddb_graph_centrality")
            .unwrap();
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap();
        assert!(props.contains_key("algorithm"));
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap();
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.contains(&"algorithm"));
    }

    #[test]
    fn test_graph_community_tool_schema() {
        let tools = all_tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "reddb_graph_community")
            .unwrap();
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap();
        assert!(props.contains_key("algorithm"));
        assert!(props.contains_key("max_iterations"));
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap();
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.contains(&"algorithm"));
        assert!(!required_strs.contains(&"max_iterations"));
    }

    #[test]
    fn test_graph_components_tool_schema() {
        let tools = all_tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "reddb_graph_components")
            .unwrap();
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap();
        assert!(props.contains_key("mode"));
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap();
        // mode is optional (has default)
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.is_empty());
    }

    #[test]
    fn test_graph_cycles_tool_schema() {
        let tools = all_tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "reddb_graph_cycles")
            .unwrap();
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap();
        assert!(props.contains_key("max_length"));
        assert!(props.contains_key("max_cycles"));
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap();
        // All optional
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.is_empty());
    }

    #[test]
    fn test_graph_clustering_tool_schema() {
        let tools = all_tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "reddb_graph_clustering")
            .unwrap();
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap();
        // No required properties - takes no arguments
        assert!(props.is_empty());
    }

    #[test]
    fn test_create_collection_tool_schema() {
        let tools = all_tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "reddb_create_collection")
            .unwrap();
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap();
        assert!(props.contains_key("name"));
        let name_type = props
            .get("name")
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str());
        assert_eq!(name_type, Some("string"));
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap();
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.contains(&"name"));
    }

    #[test]
    fn test_drop_collection_tool_schema() {
        let tools = all_tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "reddb_drop_collection")
            .unwrap();
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap();
        assert!(props.contains_key("name"));
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap();
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.contains(&"name"));
    }
}
