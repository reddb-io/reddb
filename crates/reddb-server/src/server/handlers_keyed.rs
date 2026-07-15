use super::*;

impl RedDBServer {
    pub(crate) fn handle_v1_keyed_route(
        &self,
        method: &str,
        path: &str,
        query: &BTreeMap<String, String>,
        body: &[u8],
    ) -> Option<HttpResponse> {
        if let Some(rest) = path.strip_prefix("/v1/kv/") {
            return Some(self.handle_v1_kv(method, rest, query, body));
        }
        if let Some(rest) = path.strip_prefix("/v1/config/") {
            return Some(self.handle_v1_config(method, rest, query, body));
        }
        if let Some(rest) = path.strip_prefix("/v1/vault/") {
            return Some(self.handle_v1_vault(method, rest, query, body));
        }
        None
    }

    fn handle_v1_kv(
        &self,
        method: &str,
        rest: &str,
        query: &BTreeMap<String, String>,
        body: &[u8],
    ) -> HttpResponse {
        let parts = path_parts(rest);
        match parts.as_slice() {
            [collection, "tags", "invalidate"] => match method {
                "POST" => self.handle_invalidate_kv_tags(collection, body.to_vec()),
                _ => json_error(405, "method not allowed for KV tag invalidation endpoint"),
            },
            [collection, key, "watch"] => match method {
                "GET" => match decoded_http_kv_target(collection, key) {
                    Ok((collection, key)) => self.handle_watch_kv(&collection, &key, query),
                    Err(response) => response,
                },
                _ => json_error(405, "method not allowed for KV watch endpoint"),
            },
            [collection, key, "incr"] => match method {
                "POST" => {
                    let payload = match parse_json_body_allow_empty(body) {
                        Ok(payload) => payload,
                        Err(response) => return response,
                    };
                    let by = payload.get("by").and_then(JsonValue::as_i64).unwrap_or(1);
                    let path = match keyed_path(collection, key) {
                        Ok(path) => path,
                        Err(response) => return response,
                    };
                    let mut sql = format!("KV INCR {path} BY {by}");
                    if let Some(expire_ms) = json_u64_field_any(&payload, &["expire_ms", "ttl_ms"])
                    {
                        sql.push_str(&format!(" EXPIRE {expire_ms} ms"));
                    }
                    self.execute_keyed_sql(sql)
                }
                _ => json_error(405, "method not allowed for KV counter endpoint"),
            },
            [collection, key] => match method {
                "GET" => match decoded_http_kv_target(collection, key) {
                    Ok((collection, key)) => self.handle_get_kv(&collection, &key),
                    Err(response) => response,
                },
                "PUT" => self.handle_v1_kv_put(collection, key, body),
                "DELETE" => match decoded_http_kv_target(collection, key) {
                    Ok((collection, key)) => self.handle_delete_kv(&collection, &key),
                    Err(response) => response,
                },
                _ => json_error(405, "method not allowed for KV endpoint"),
            },
            _ => json_error(404, "route not found under /v1/kv"),
        }
    }

    fn handle_v1_kv_put(&self, collection: &str, key: &str, body: &[u8]) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(value) = payload.get("value") else {
            return json_error(400, "field 'value' is required");
        };
        let tags = match json_string_array_field(&payload, "tags") {
            Ok(tags) => tags,
            Err(message) => return json_error(400, message),
        };
        let mut sql = format!(
            "KV PUT {} = {}",
            match keyed_path(collection, key) {
                Ok(path) => path,
                Err(response) => return response,
            },
            match keyed_value_literal(value) {
                Ok(literal) => literal,
                Err(response) => return response,
            }
        );
        if let Some(expire_ms) = json_u64_field_any(&payload, &["expire_ms", "ttl_ms"]) {
            sql.push_str(&format!(" EXPIRE {expire_ms} ms"));
        }
        append_tags_clause(&mut sql, &tags);
        self.execute_keyed_sql(sql)
    }

    fn handle_v1_config(
        &self,
        method: &str,
        rest: &str,
        query: &BTreeMap<String, String>,
        body: &[u8],
    ) -> HttpResponse {
        let parts = path_parts(rest);
        match parts.as_slice() {
            [collection] => match method {
                "GET" => self.execute_keyed_sql(list_sql("CONFIG", collection, query)),
                _ => json_error(405, "method not allowed for Config collection endpoint"),
            },
            [collection, key, "resolve"] => match method {
                "POST" => match keyed_target(collection, key) {
                    Ok((collection, key)) => {
                        self.execute_keyed_sql(format!("RESOLVE CONFIG {collection} {key}"))
                    }
                    Err(response) => response,
                },
                _ => json_error(405, "method not allowed for Config resolve endpoint"),
            },
            [collection, key, "history"] => match method {
                "GET" => match keyed_target(collection, key) {
                    Ok((collection, key)) => {
                        self.execute_keyed_sql(format!("HISTORY CONFIG {collection} {key}"))
                    }
                    Err(response) => response,
                },
                _ => json_error(405, "method not allowed for Config history endpoint"),
            },
            [collection, key, "watch"] => match method {
                "GET" => match keyed_target(collection, key) {
                    Ok((collection, key)) => {
                        let from_lsn = query.get("from_lsn").and_then(|v| v.parse::<u64>().ok());
                        let mut sql = format!("WATCH CONFIG {collection} {key}");
                        if let Some(from_lsn) = from_lsn {
                            sql.push_str(&format!(" FROM LSN {from_lsn}"));
                        }
                        self.execute_keyed_sql(sql)
                    }
                    Err(response) => response,
                },
                _ => json_error(405, "method not allowed for Config watch endpoint"),
            },
            [_, _, "incr"] | [_, _, "decr"] => {
                json_error(405, "CONFIG counter operations are not supported")
            }
            [collection, key] => match method {
                "GET" => match keyed_target(collection, key) {
                    Ok((collection, key)) => {
                        self.execute_keyed_sql(format!("GET CONFIG {collection} {key}"))
                    }
                    Err(response) => response,
                },
                "PUT" => self.handle_v1_config_put(collection, key, body),
                "DELETE" => match keyed_target(collection, key) {
                    Ok((collection, key)) => {
                        self.execute_keyed_sql(format!("DELETE CONFIG {collection} {key}"))
                    }
                    Err(response) => response,
                },
                _ => json_error(405, "method not allowed for Config endpoint"),
            },
            _ => json_error(404, "route not found under /v1/config"),
        }
    }

    fn handle_v1_config_put(&self, collection: &str, key: &str, body: &[u8]) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        if has_volatile_keyed_option(&payload) {
            return json_error(400, "CONFIG does not support TTL or expiration options");
        }
        let Some(value) = payload.get("secret_ref").or_else(|| payload.get("value")) else {
            return json_error(400, "field 'value' is required");
        };
        let tags = match json_string_array_field(&payload, "tags") {
            Ok(tags) => tags,
            Err(message) => return json_error(400, message),
        };
        let (collection, key) = match keyed_target(collection, key) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let literal = match config_value_literal(value, payload.get("secret_ref").is_some()) {
            Ok(literal) => literal,
            Err(response) => return response,
        };
        let mut sql = format!("PUT CONFIG {collection} {key} = {literal}");
        append_tags_clause(&mut sql, &tags);
        self.execute_keyed_sql(sql)
    }

    fn handle_v1_vault(
        &self,
        method: &str,
        rest: &str,
        query: &BTreeMap<String, String>,
        body: &[u8],
    ) -> HttpResponse {
        let parts = path_parts(rest);
        match parts.as_slice() {
            [collection] => match method {
                "GET" => self.execute_keyed_sql(list_sql("VAULT", collection, query)),
                _ => json_error(405, "method not allowed for Vault collection endpoint"),
            },
            [collection, key, "unseal"] => match method {
                "POST" => match keyed_path(collection, key) {
                    Ok(path) => self.execute_keyed_sql(format!("UNSEAL VAULT {path}")),
                    Err(response) => response,
                },
                _ => json_error(405, "method not allowed for Vault unseal endpoint"),
            },
            [collection, key, "history"] => match method {
                "GET" => match keyed_path(collection, key) {
                    Ok(path) => self.execute_keyed_sql(format!("HISTORY VAULT {path}")),
                    Err(response) => response,
                },
                _ => json_error(405, "method not allowed for Vault history endpoint"),
            },
            [_, _, "incr"] | [_, _, "decr"] => {
                json_error(405, "VAULT counter operations are not supported")
            }
            [collection, key] => match method {
                "GET" => match keyed_path(collection, key) {
                    Ok(path) => self.execute_keyed_sql(format!("VAULT GET {path}")),
                    Err(response) => response,
                },
                "PUT" => self.handle_v1_vault_put(collection, key, body, false),
                "DELETE" => match keyed_path(collection, key) {
                    Ok(path) => self.execute_keyed_sql(format!("DELETE VAULT {path}")),
                    Err(response) => response,
                },
                _ => json_error(405, "method not allowed for Vault endpoint"),
            },
            [collection, key, "rotate"] => match method {
                "POST" => self.handle_v1_vault_put(collection, key, body, true),
                _ => json_error(405, "method not allowed for Vault rotate endpoint"),
            },
            _ => json_error(404, "route not found under /v1/vault"),
        }
    }

    fn handle_v1_vault_put(
        &self,
        collection: &str,
        key: &str,
        body: &[u8],
        rotate: bool,
    ) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        if has_volatile_keyed_option(&payload) {
            return json_error(400, "VAULT does not support TTL or expiration options");
        }
        let Some(value) = payload.get("value") else {
            return json_error(400, "field 'value' is required");
        };
        let tags = match json_string_array_field(&payload, "tags") {
            Ok(tags) => tags,
            Err(message) => return json_error(400, message),
        };
        let path = match keyed_path(collection, key) {
            Ok(path) => path,
            Err(response) => return response,
        };
        let literal = match keyed_value_literal(value) {
            Ok(literal) => literal,
            Err(response) => return response,
        };
        let mut sql = if rotate {
            format!("ROTATE VAULT {path} = {literal}")
        } else {
            format!("VAULT PUT {path} = {literal}")
        };
        append_tags_clause(&mut sql, &tags);
        self.execute_keyed_sql(sql)
    }

    fn execute_keyed_sql(&self, sql: String) -> HttpResponse {
        match self
            .query_use_cases()
            .execute(ExecuteQueryInput { query: sql })
        {
            Ok(result) => json_response(
                200,
                crate::presentation::query_result_json::runtime_query_json(&result, &None, &None),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }
}

fn path_parts(rest: &str) -> Vec<&str> {
    rest.trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect()
}

fn keyed_path(collection: &str, key: &str) -> Result<String, HttpResponse> {
    let collection = decode_keyed_segment(collection, "collection")?;
    let key = decode_keyed_segment(key, "key")?;
    if !valid_keyed_ident(&collection) {
        return Err(json_error(
            400,
            "collection contains unsupported characters",
        ));
    }
    if key.is_empty() {
        return Err(json_error(400, "key contains unsupported characters"));
    }
    Ok(format!("{collection}.{}", keyed_key_sql_segment(&key)))
}

fn decoded_http_kv_target(collection: &str, key: &str) -> Result<(String, String), HttpResponse> {
    let collection = decode_keyed_segment(collection, "collection")?;
    let key = decode_keyed_segment(key, "key")?;
    if collection.is_empty() || key.is_empty() {
        return Err(json_error(400, "collection and key are required"));
    }
    Ok((collection, key))
}

fn keyed_target(collection: &str, key: &str) -> Result<(String, String), HttpResponse> {
    let collection = decode_keyed_segment(collection, "collection")?;
    let key = decode_keyed_segment(key, "key")?;
    if !valid_keyed_ident(&collection) {
        return Err(json_error(
            400,
            "collection contains unsupported characters",
        ));
    }
    if !valid_keyed_ident(&key) {
        return Err(json_error(400, "key contains unsupported characters"));
    }
    Ok((collection, key))
}

fn valid_keyed_ident(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

fn decode_keyed_segment(segment: &str, name: &str) -> Result<String, HttpResponse> {
    percent_decode_path_segment(segment)
        .map_err(|err| json_error(400, format!("invalid {name} path segment: {err}")))
}

fn keyed_key_sql_segment(key: &str) -> String {
    if valid_keyed_ident(key) {
        return key.to_string();
    }
    format!("'{}'", key.replace('\'', "''"))
}

fn list_sql(domain: &str, collection: &str, query: &BTreeMap<String, String>) -> String {
    let collection = if valid_keyed_ident(collection) {
        collection.to_string()
    } else {
        "_invalid_".to_string()
    };
    let mut sql = format!("LIST {domain} {collection}");
    if let Some(prefix) = query
        .get("prefix")
        .filter(|prefix| valid_keyed_ident(prefix))
    {
        sql.push_str(&format!(" PREFIX {prefix}"));
    }
    if let Some(limit) = query
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
    {
        sql.push_str(&format!(" LIMIT {limit}"));
    }
    if let Some(offset) = query
        .get("offset")
        .and_then(|value| value.parse::<usize>().ok())
    {
        sql.push_str(&format!(" OFFSET {offset}"));
    }
    sql
}

fn keyed_value_literal(value: &JsonValue) -> Result<String, HttpResponse> {
    match value {
        JsonValue::String(value) => Ok(format!("'{}'", value.replace('\'', "''"))),
        JsonValue::Integer(value) => Ok(value.to_string()),
        JsonValue::Number(value) => Ok(value.to_string()),
        JsonValue::Decimal(value) => Ok(value.clone()),
        JsonValue::Bool(value) => Ok(value.to_string()),
        JsonValue::Null => Ok("NULL".to_string()),
        JsonValue::Array(_) | JsonValue::Object(_) => crate::json::to_string(value)
            .map_err(|err| json_error(400, format!("failed to encode JSON value: {err}"))),
    }
}

fn config_value_literal(
    value: &JsonValue,
    explicit_secret_ref: bool,
) -> Result<String, HttpResponse> {
    if explicit_secret_ref {
        let Some(object) = value.as_object() else {
            return Err(json_error(400, "field 'secret_ref' must be an object"));
        };
        let collection = object
            .get("collection")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| json_error(400, "secret_ref.collection is required"))?;
        let key = object
            .get("key")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| json_error(400, "secret_ref.key is required"))?;
        let path = keyed_path(collection, key)?;
        return Ok(format!("SECRET_REF(vault, {path})"));
    }
    keyed_value_literal(value)
}

fn append_tags_clause(sql: &mut String, tags: &[String]) {
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

fn has_volatile_keyed_option(payload: &JsonValue) -> bool {
    payload.get("ttl").is_some()
        || payload.get("ttl_ms").is_some()
        || payload.get("expire").is_some()
        || payload.get("expire_ms").is_some()
        || payload.get("expires_at").is_some()
}

fn json_u64_field_any(payload: &JsonValue, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| payload.get(name).and_then(JsonValue::as_u64))
}

fn json_string_array_field(payload: &JsonValue, field: &str) -> Result<Vec<String>, String> {
    let Some(value) = payload.get(field) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(format!("field '{field}' must be an array of strings"));
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("field '{field}' must contain only strings"))
        })
        .collect()
}
