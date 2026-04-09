//! HTTP handlers for authentication and authorization endpoints.
//!
//! Provides REST endpoints for:
//! - Bootstrap (first admin user creation)
//! - Login (session token issuance)
//! - User CRUD (create, list, delete)
//! - API key management (create, revoke)
//! - Password change
//! - Whoami (token introspection)

use super::*;

impl RedDBServer {
    /// POST /auth/bootstrap
    ///
    /// Creates the first admin user. One-shot, irreversible.
    /// Body: `{ "username": "admin", "password": "secret" }`
    pub(crate) fn handle_auth_bootstrap(&self, body: Vec<u8>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let password = match json_string_field(&payload, "password") {
            Some(p) => p,
            None => return json_error(400, "missing 'password' field"),
        };

        match auth_store.bootstrap(&username, &password) {
            Ok(result) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "username".to_string(),
                    JsonValue::String(result.user.username.clone()),
                );
                object.insert(
                    "role".to_string(),
                    JsonValue::String(result.user.role.to_string()),
                );
                object.insert(
                    "api_key".to_string(),
                    JsonValue::String(result.api_key.key.clone()),
                );
                if let Some(ref cert) = result.certificate {
                    object.insert("certificate".to_string(), JsonValue::String(cert.clone()));
                }
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(403, err.to_string()),
        }
    }

    /// POST /auth/login
    ///
    /// Authenticates with username/password and returns a session token.
    /// Body: `{ "username": "admin", "password": "secret" }`
    pub(crate) fn handle_auth_login(&self, body: Vec<u8>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let password = match json_string_field(&payload, "password") {
            Some(p) => p,
            None => return json_error(400, "missing 'password' field"),
        };

        match auth_store.authenticate(&username, &password) {
            Ok(session) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "token".to_string(),
                    JsonValue::String(session.token.clone()),
                );
                object.insert(
                    "username".to_string(),
                    JsonValue::String(session.username.clone()),
                );
                object.insert(
                    "role".to_string(),
                    JsonValue::String(session.role.to_string()),
                );
                object.insert(
                    "expires_at".to_string(),
                    JsonValue::Number(session.expires_at as f64),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(401, err.to_string()),
        }
    }

    /// POST /auth/users
    ///
    /// Creates a new user with the specified role.
    /// Body: `{ "username": "alice", "password": "pass", "role": "write" }`
    pub(crate) fn handle_auth_create_user(&self, body: Vec<u8>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let password = match json_string_field(&payload, "password") {
            Some(p) => p,
            None => return json_error(400, "missing 'password' field"),
        };
        let role_str = json_string_field(&payload, "role").unwrap_or_else(|| "read".to_string());
        let role = match crate::auth::Role::from_str(&role_str) {
            Some(r) => r,
            None => {
                return json_error(
                    400,
                    format!("invalid role '{}': must be read, write, or admin", role_str),
                )
            }
        };

        match auth_store.create_user(&username, &password, role) {
            Ok(user) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "username".to_string(),
                    JsonValue::String(user.username.clone()),
                );
                object.insert("role".to_string(), JsonValue::String(user.role.to_string()));
                object.insert("enabled".to_string(), JsonValue::Bool(user.enabled));
                json_response(201, JsonValue::Object(object))
            }
            Err(err) => json_error(409, err.to_string()),
        }
    }

    /// GET /auth/users
    ///
    /// Lists all users (password hashes are redacted).
    pub(crate) fn handle_auth_list_users(&self) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let users = auth_store.list_users();
        let user_values: Vec<JsonValue> = users
            .iter()
            .map(|u| {
                let mut obj = Map::new();
                obj.insert(
                    "username".to_string(),
                    JsonValue::String(u.username.clone()),
                );
                obj.insert("role".to_string(), JsonValue::String(u.role.to_string()));
                obj.insert("enabled".to_string(), JsonValue::Bool(u.enabled));
                obj.insert(
                    "created_at".to_string(),
                    JsonValue::Number(u.created_at as f64),
                );
                obj.insert(
                    "updated_at".to_string(),
                    JsonValue::Number(u.updated_at as f64),
                );
                let keys: Vec<JsonValue> = u
                    .api_keys
                    .iter()
                    .map(|k| {
                        let mut key_obj = Map::new();
                        key_obj.insert("name".to_string(), JsonValue::String(k.name.clone()));
                        key_obj.insert("role".to_string(), JsonValue::String(k.role.to_string()));
                        key_obj.insert(
                            "created_at".to_string(),
                            JsonValue::Number(k.created_at as f64),
                        );
                        JsonValue::Object(key_obj)
                    })
                    .collect();
                obj.insert("api_keys".to_string(), JsonValue::Array(keys));
                JsonValue::Object(obj)
            })
            .collect();

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("users".to_string(), JsonValue::Array(user_values));
        json_response(200, JsonValue::Object(object))
    }

    /// DELETE /auth/users/:username
    ///
    /// Deletes a user and revokes all their API keys and sessions.
    pub(crate) fn handle_auth_delete_user(&self, username: &str) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        match auth_store.delete_user(username) {
            Ok(()) => json_ok(format!("user '{}' deleted", username)),
            Err(err) => json_error(404, err.to_string()),
        }
    }

    /// POST /auth/api-keys
    ///
    /// Creates a new API key for a user.
    /// Body: `{ "username": "alice", "name": "ci-deploy", "role": "write" }`
    pub(crate) fn handle_auth_create_api_key(&self, body: Vec<u8>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let name = json_string_field(&payload, "name").unwrap_or_else(|| "unnamed".to_string());
        let role_str = json_string_field(&payload, "role").unwrap_or_else(|| "read".to_string());
        let role = match crate::auth::Role::from_str(&role_str) {
            Some(r) => r,
            None => {
                return json_error(
                    400,
                    format!("invalid role '{}': must be read, write, or admin", role_str),
                )
            }
        };

        match auth_store.create_api_key(&username, &name, role) {
            Ok(api_key) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("key".to_string(), JsonValue::String(api_key.key.clone()));
                object.insert("name".to_string(), JsonValue::String(api_key.name.clone()));
                object.insert(
                    "role".to_string(),
                    JsonValue::String(api_key.role.to_string()),
                );
                json_response(201, JsonValue::Object(object))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    /// DELETE /auth/api-keys/:key
    ///
    /// Revokes an API key.
    pub(crate) fn handle_auth_revoke_api_key(&self, key: &str) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        match auth_store.revoke_api_key(key) {
            Ok(()) => json_ok("API key revoked"),
            Err(err) => json_error(404, err.to_string()),
        }
    }

    /// POST /auth/change-password
    ///
    /// Changes a user's password.
    /// Body: `{ "username": "alice", "old_password": "old", "new_password": "new" }`
    pub(crate) fn handle_auth_change_password(&self, body: Vec<u8>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let old_password = match json_string_field(&payload, "old_password") {
            Some(p) => p,
            None => return json_error(400, "missing 'old_password' field"),
        };
        let new_password = match json_string_field(&payload, "new_password") {
            Some(p) => p,
            None => return json_error(400, "missing 'new_password' field"),
        };

        match auth_store.change_password(&username, &old_password, &new_password) {
            Ok(()) => json_ok("password changed"),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    /// GET /auth/whoami
    ///
    /// Returns the current user's identity based on the auth token.
    pub(crate) fn handle_auth_whoami(&self, headers: &BTreeMap<String, String>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let token = headers
            .get("authorization")
            .and_then(|v| v.strip_prefix("Bearer "));

        match token {
            Some(tok) => match auth_store.validate_token(tok) {
                Some((username, role)) => {
                    let mut object = Map::new();
                    object.insert("ok".to_string(), JsonValue::Bool(true));
                    object.insert("username".to_string(), JsonValue::String(username));
                    object.insert("role".to_string(), JsonValue::String(role.to_string()));
                    object.insert("authenticated".to_string(), JsonValue::Bool(true));
                    json_response(200, JsonValue::Object(object))
                }
                None => json_error(401, "invalid or expired token"),
            },
            None => {
                if auth_store.is_enabled() {
                    json_error(401, "no authorization token provided")
                } else {
                    let mut object = Map::new();
                    object.insert("ok".to_string(), JsonValue::Bool(true));
                    object.insert(
                        "username".to_string(),
                        JsonValue::String("anonymous".to_string()),
                    );
                    object.insert("role".to_string(), JsonValue::String("admin".to_string()));
                    object.insert("authenticated".to_string(), JsonValue::Bool(false));
                    object.insert(
                        "note".to_string(),
                        JsonValue::String(
                            "auth is disabled; all requests have admin access".to_string(),
                        ),
                    );
                    json_response(200, JsonValue::Object(object))
                }
            }
        }
    }
}
