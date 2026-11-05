//! Remote, tenant-scoped access to the existing native Blob Cache.
//! Application namespaces never address the runtime's internal result cache.
use super::*;
use crate::auth::policies::{EvalContext, ResourceRef};
use crate::serde_json::JsonDecode;
use crate::server::handlers_auth::resolve_auth_caller;
use crate::server::route_catalog::RouteRequest;
use crate::storage::cache::blob::{
    BlobCachePolicy, BlobCachePut, CacheError, CachePresence, L1Admission,
};
use crate::storage::cache::sweeper::BlobCacheSweeper;
use base64::{engine::general_purpose::STANDARD, Engine};

const VALUE_BYTES_MAX: usize = 1024 * 1024;
const KEY_BYTES_MAX: usize = 4096;
const NAMESPACE_BYTES_MAX: usize = 256;
const TAG_COUNT_MAX: usize = 64;

#[derive(Default)]
struct CachePolicyInput {
    ttl_ms: Option<u64>,
    expires_at_unix_ms: Option<u64>,
    max_blob_bytes: Option<usize>,
    l1_admission: Option<String>,
    priority: Option<u8>,
    version: Option<u64>,
}

struct CachePutInput {
    value: String,
    ttl_ms: Option<u64>,
    tags: Vec<String>,
    policy: CachePolicyInput,
}

struct CacheTagsInput {
    tags: Vec<String>,
}

fn cache_fields(value: &JsonValue, allowed: &[&str]) -> Result<(), String> {
    let fields = value.as_object().ok_or("cache body must be an object")?;
    if fields.keys().any(|name| !allowed.contains(&name.as_str())) {
        return Err("unknown cache field".into());
    }
    Ok(())
}

fn cache_field<T: JsonDecode>(value: &JsonValue, name: &str) -> Result<Option<T>, String> {
    value.get(name).cloned().map(T::from_json_value).transpose()
}

fn cache_integer(value: &JsonValue, name: &str) -> Result<Option<u64>, String> {
    value
        .get(name)
        .map(|number| match number {
            JsonValue::Integer(number) => {
                u64::try_from(*number).map_err(|_| "negative cache integer".into())
            }
            JsonValue::Number(number)
                if number.is_finite()
                    && *number >= 0.0
                    && number.fract() == 0.0
                    && *number <= 9_007_199_254_740_991.0 =>
            {
                number
                    .to_string()
                    .parse::<u64>()
                    .map_err(|_| "invalid cache integer".into())
            }
            _ => Err("invalid cache integer".into()),
        })
        .transpose()
}

impl JsonDecode for CachePutInput {
    fn from_json_value(value: JsonValue) -> Result<Self, String> {
        cache_fields(&value, &["value", "ttl_ms", "tags", "policy"])?;
        Ok(Self {
            value: cache_field(&value, "value")?.ok_or("cache value is required")?,
            ttl_ms: cache_integer(&value, "ttl_ms")?,
            tags: cache_field(&value, "tags")?.unwrap_or_default(),
            policy: cache_field(&value, "policy")?.unwrap_or_default(),
        })
    }
}

impl JsonDecode for CachePolicyInput {
    fn from_json_value(value: JsonValue) -> Result<Self, String> {
        cache_fields(
            &value,
            &[
                "ttl_ms",
                "expires_at_unix_ms",
                "max_blob_bytes",
                "l1_admission",
                "priority",
                "version",
            ],
        )?;
        Ok(Self {
            ttl_ms: cache_integer(&value, "ttl_ms")?,
            expires_at_unix_ms: cache_integer(&value, "expires_at_unix_ms")?,
            max_blob_bytes: cache_integer(&value, "max_blob_bytes")?
                .map(usize::try_from)
                .transpose()
                .map_err(|_| "cache size is too large")?,
            l1_admission: cache_field(&value, "l1_admission")?,
            priority: cache_integer(&value, "priority")?
                .map(u8::try_from)
                .transpose()
                .map_err(|_| "cache priority is too large")?,
            version: cache_integer(&value, "version")?,
        })
    }
}

impl JsonDecode for CacheTagsInput {
    fn from_json_value(value: JsonValue) -> Result<Self, String> {
        cache_fields(&value, &["tags"])?;
        Ok(Self {
            tags: cache_field(&value, "tags")?.ok_or("cache tags are required")?,
        })
    }
}

impl RedDBServer {
    pub(crate) fn handle_cache(
        &self,
        request: &RouteRequest<'_>,
        operation: &str,
        action: &str,
    ) -> HttpResponse {
        let mut response = self
            .handle_cache_result(request, operation, action)
            .unwrap_or_else(|response| response);
        response.extra_headers.push((
            "Cache-Control",
            header_escape_guard::HeaderEscapeGuard::header_value("no-store")
                .expect("static cache-control header is safe"),
        ));
        response
    }

    fn handle_cache_result(
        &self,
        request: &RouteRequest<'_>,
        operation: &str,
        action: &str,
    ) -> Result<HttpResponse, HttpResponse> {
        let namespace =
            cache_segment(request.matched.params.get("namespace"), NAMESPACE_BYTES_MAX)?;
        let namespace = self.cache_namespace(request.headers, &namespace, action)?;
        let cache = self.runtime.result_blob_cache();
        let result = match operation {
            "put" => {
                // Bound base64 before decoding or allocating the raw payload.
                if request.body.len() > VALUE_BYTES_MAX * 2 {
                    return Err(json_error(413, "cache request is too large"));
                }
                let input: CachePutInput = cache_json(request.body)?;
                validate_tags(&input.tags)?;
                if input.value.len() > VALUE_BYTES_MAX.div_ceil(3) * 4 {
                    return Err(json_error(413, "cache value is too large"));
                }
                let value = STANDARD
                    .decode(&input.value)
                    .map_err(|_| json_error(400, "cache value must be standard base64"))?;
                if value.len() > VALUE_BYTES_MAX {
                    return Err(json_error(413, "cache value is too large"));
                }
                let policy = cache_policy(input.ttl_ms, input.policy)?;
                let key = cache_segment(request.matched.params.get("key"), KEY_BYTES_MAX)?;
                cache
                    .put(
                        namespace,
                        key,
                        BlobCachePut::new(value)
                            .with_tags(input.tags)
                            .with_policy(policy),
                    )
                    .map_err(cache_error)?;
                crate::json!({ "ok": true })
            }
            "get" => {
                let key = cache_segment(request.matched.params.get("key"), KEY_BYTES_MAX)?;
                match cache.get(&namespace, &key) {
                    Some(hit) => crate::json!({ "value": STANDARD.encode(hit.value()) }),
                    None => crate::json!({ "value": JsonValue::Null }),
                }
            }
            "exists" => {
                let key = cache_segment(request.matched.params.get("key"), KEY_BYTES_MAX)?;
                let presence = match cache.exists(&namespace, &key) {
                    CachePresence::Present => "present",
                    CachePresence::Absent => "absent",
                    CachePresence::MaybePresent => "maybe",
                };
                crate::json!({ "status": presence })
            }
            "invalidate" => {
                let key = cache_segment(request.matched.params.get("key"), KEY_BYTES_MAX)?;
                crate::json!({ "removed": cache.invalidate_key(&namespace, &key) })
            }
            "invalidate_prefix" => {
                let prefix = request
                    .query
                    .get("prefix")
                    .ok_or_else(|| json_error(400, "prefix is required"))?;
                // The transport keeps query values encoded; decode exactly once,
                // including form-style spaces, before matching literal key bytes.
                let prefix = percent_decode_path_segment(&prefix.replace('+', " "))
                    .map_err(|_| json_error(400, "invalid cache prefix encoding"))?;
                validate_cache_text(&prefix, KEY_BYTES_MAX, true)?;
                crate::json!({ "removed": cache.invalidate_prefix(&namespace, &prefix) })
            }
            "invalidate_tags" => {
                let input: CacheTagsInput = cache_json(request.body)?;
                validate_tags(&input.tags)?;
                let tags: Vec<&str> = input.tags.iter().map(String::as_str).collect();
                crate::json!({ "removed": cache.invalidate_tags(&namespace, &tags) })
            }
            "flush" => {
                BlobCacheSweeper::flush_namespace(cache, &namespace);
                crate::json!({ "ok": true })
            }
            _ => return Err(json_error(404, "unknown cache operation")),
        };
        Ok(json_response(200, result))
    }

    fn cache_namespace(
        &self,
        headers: &BTreeMap<String, String>,
        namespace: &str,
        action: &str,
    ) -> Result<String, HttpResponse> {
        let caller = resolve_auth_caller(self, headers).or_else(|| {
            let validator = self.runtime.oauth_validator()?;
            let token = headers.get("authorization")?.strip_prefix("Bearer ")?;
            let (tenant, username, role) =
                crate::wire::redwire::auth::validate_oauth_jwt_full(&validator, token).ok()?;
            Some(crate::server::handlers_auth::AuthCaller {
                id: crate::auth::UserId::from_parts(tenant.as_deref(), &username),
                role,
            })
        });
        if (self
            .auth_store
            .as_ref()
            .is_some_and(|store| store.is_enabled())
            || self.runtime.oauth_validator().is_some())
            && caller.is_none()
        {
            return Err(json_error(
                401,
                "cache access requires an authenticated user",
            ));
        }
        let tenant = caller
            .as_ref()
            .and_then(|caller| caller.id.tenant.as_deref());
        if let Some(caller) = &caller {
            let allowed = if action == "cache:read" {
                caller.role.can_read()
            } else {
                caller.role.can_write()
            };
            if !allowed {
                return Err(json_error(
                    403,
                    "cache operation is not allowed for this role",
                ));
            }
            if let Some(store) = self
                .auth_store
                .as_ref()
                .filter(|store| store.iam_authorization_enabled())
            {
                let resource = match tenant {
                    Some(tenant) => ResourceRef::new("cache", namespace).with_tenant(tenant),
                    None => ResourceRef::new("cache", namespace),
                };
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |time| time.as_millis());
                let context = EvalContext {
                    principal_tenant: caller.id.tenant.clone(),
                    current_tenant: caller.id.tenant.clone(),
                    peer_ip: None,
                    mfa_present: false,
                    now_ms,
                    principal_is_admin_role: caller.role.can_admin(),
                    principal_is_platform_scoped: tenant.is_none(),
                };
                if !store.check_policy_authz_with_role(
                    &caller.id,
                    action,
                    &resource,
                    &context,
                    caller.role,
                ) {
                    return Err(json_error(403, "cache operation denied by IAM policy"));
                }
            }
        }
        // A length-delimited tenant prefix distinguishes platform scope, empty
        // tenant IDs and caller-controlled namespaces, including separator bytes.
        Ok(match tenant {
            Some(tenant) => format!("http-cache:t{}:{tenant}:{namespace}", tenant.len()),
            None => format!("http-cache:p:{namespace}"),
        })
    }
}

fn cache_segment(value: Option<&String>, limit: usize) -> Result<String, HttpResponse> {
    let value = value.ok_or_else(|| json_error(400, "cache path segment is required"))?;
    let value = percent_decode_path_segment(value)
        .map_err(|_| json_error(400, "invalid cache path encoding"))?;
    validate_cache_text(&value, limit, false)?;
    Ok(value)
}

fn validate_cache_text(value: &str, limit: usize, allow_empty: bool) -> Result<(), HttpResponse> {
    if (!allow_empty && value.is_empty())
        || value.len() > limit
        || value.chars().any(char::is_control)
    {
        return Err(json_error(400, "invalid cache key, namespace or tag"));
    }
    Ok(())
}

fn validate_tags(tags: &[String]) -> Result<(), HttpResponse> {
    if tags.len() > TAG_COUNT_MAX {
        return Err(json_error(400, "too many cache tags"));
    }
    for tag in tags {
        validate_cache_text(tag, NAMESPACE_BYTES_MAX, false)?;
    }
    Ok(())
}

fn cache_json<T: JsonDecode>(body: &[u8]) -> Result<T, HttpResponse> {
    crate::serde_json::from_slice(body).map_err(|_| json_error(400, "invalid cache JSON body"))
}

fn cache_policy(
    ttl_ms: Option<u64>,
    input: CachePolicyInput,
) -> Result<BlobCachePolicy, HttpResponse> {
    if ttl_ms.is_some() && input.ttl_ms.is_some() && ttl_ms != input.ttl_ms {
        return Err(json_error(400, "conflicting cache TTL values"));
    }
    let mut policy = BlobCachePolicy::default().max_blob_bytes(VALUE_BYTES_MAX);
    if let Some(ttl) = ttl_ms.or(input.ttl_ms) {
        policy = policy.ttl_ms(ttl);
    }
    if let Some(expires) = input.expires_at_unix_ms {
        policy = policy.expires_at_unix_ms(expires);
    }
    if let Some(size) = input.max_blob_bytes {
        policy = policy.max_blob_bytes(size.min(VALUE_BYTES_MAX));
    }
    if let Some(priority) = input.priority {
        policy = policy.priority(priority);
    }
    if let Some(version) = input.version {
        policy = policy.version(version);
    }
    if let Some(admission) = input.l1_admission {
        policy = policy.l1_admission(match admission.as_str() {
            "auto" => L1Admission::Auto,
            "always" => L1Admission::Always,
            "never" => L1Admission::Never,
            _ => return Err(json_error(400, "invalid cache L1 admission")),
        });
    }
    Ok(policy)
}

fn cache_error(error: CacheError) -> HttpResponse {
    match error {
        CacheError::BlobTooLarge { .. } | CacheError::MetadataTooLarge { .. } => {
            json_error(413, "cache value or metadata is too large")
        }
        CacheError::VersionMismatch { .. } => json_error(409, "cache version conflict"),
        CacheError::TooManyNamespaces { .. } => {
            json_error(429, "cache namespace capacity exceeded")
        }
        CacheError::L2Full { .. } | CacheError::L2Io(_) => {
            json_error(503, "cache storage is unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::policies::Policy;
    use crate::auth::store::PrincipalRef;
    use crate::auth::{AuthConfig, AuthStore, Role, UserId};

    fn server() -> RedDBServer {
        RedDBServer::new(
            RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("test runtime"),
        )
    }

    fn request(
        server: &RedDBServer,
        method: &str,
        path: &str,
        body: &str,
        token: Option<&str>,
    ) -> HttpResponse {
        let (path, prefix) = path
            .split_once("?prefix=")
            .map_or((path, None), |(path, prefix)| (path, Some(prefix)));
        let mut headers = BTreeMap::new();
        if let Some(token) = token {
            headers.insert("authorization".into(), format!("Bearer {token}"));
        }
        // Caller-controlled tenant headers cannot select another cache partition.
        headers.insert("x-reddb-tenant".into(), "foreign".into());
        let query = prefix
            .map(|prefix| BTreeMap::from([("prefix".into(), prefix.into())]))
            .unwrap_or_default();
        server.route(HttpRequest {
            method: method.into(),
            path: path.into(),
            query,
            headers,
            body: body.as_bytes().to_vec(),
        })
    }

    fn value(response: HttpResponse) -> ::serde_json::Value {
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        ::serde_json::from_slice(&response.body).expect("JSON cache response")
    }

    fn put(server: &RedDBServer, namespace: &str, key: &str, bytes: &[u8], token: Option<&str>) {
        let body = ::serde_json::json!({ "value": STANDARD.encode(bytes), "ttl_ms": 600_000, "tags": ["daily"] }).to_string();
        value(request(
            server,
            "PUT",
            &format!("/cache/ns/{namespace}/{key}"),
            &body,
            token,
        ));
    }

    #[test]
    fn native_cache_http_binary_roundtrip_ttl_and_reserved_key_names() {
        let server = server();
        for key in [
            "tokens",
            "tags",
            "flush",
            "invalidate-tags",
            "a%2Fb%3Ac%25%20%E2%98%83",
        ] {
            put(&server, "bigquery", key, &[0, 255, 128, 13, 10], None);
            let response = request(
                &server,
                "GET",
                &format!("/cache/ns/bigquery/{key}"),
                "",
                None,
            );
            assert!(response
                .extra_headers
                .iter()
                .any(|(name, value)| *name == "Cache-Control" && value == "no-store"));
            assert_eq!(
                value(response)["value"],
                STANDARD.encode([0, 255, 128, 13, 10])
            );
            value(request(
                &server,
                "DELETE",
                &format!("/cache/ns/bigquery/{key}"),
                "",
                None,
            ));
            assert!(value(request(
                &server,
                "GET",
                &format!("/cache/ns/bigquery/{key}"),
                "",
                None
            ))["value"]
                .is_null());
        }
        value(request(
            &server,
            "PUT",
            "/cache/ns/bigquery/short",
            r#"{"value":"YQ==","ttl_ms":1}"#,
            None,
        ));
        std::thread::sleep(Duration::from_millis(10));
        assert!(value(request(
            &server,
            "GET",
            "/cache/ns/bigquery/short",
            "",
            None
        ))["value"]
            .is_null());
        assert_eq!(
            value(request(
                &server,
                "GET",
                "/cache/ns/bigquery/short/exists",
                "",
                None
            ))["status"],
            "absent"
        );
    }

    #[test]
    fn native_cache_http_invalidations_never_address_internal_or_other_namespaces() {
        let server = server();
        server
            .runtime
            .result_blob_cache()
            .put(
                "runtime.result_cache",
                "secret",
                BlobCachePut::new(b"internal".to_vec()),
            )
            .expect("internal cache value");
        assert!(value(request(
            &server,
            "GET",
            "/cache/ns/runtime.result_cache/secret",
            "",
            None
        ))["value"]
            .is_null());
        for namespace in ["reports", "other"] {
            for key in ["daily:1", "daily:2", "monthly"] {
                put(&server, namespace, key, b"ok", None);
            }
        }
        assert_eq!(
            value(request(
                &server,
                "DELETE",
                "/cache/ns/reports?prefix=daily%3A",
                "",
                None
            ))["removed"],
            2
        );
        assert_eq!(
            value(request(
                &server,
                "POST",
                "/cache/ns/reports/invalidate-tags",
                r#"{"tags":["daily"]}"#,
                None
            ))["removed"],
            1
        );
        assert!(value(request(
            &server,
            "GET",
            "/cache/ns/reports/monthly",
            "",
            None
        ))["value"]
            .is_null());
        put(&server, "reports", "again", b"ok", None);
        value(request(
            &server,
            "POST",
            "/cache/ns/reports/flush",
            "",
            None,
        ));
        assert!(
            value(request(&server, "GET", "/cache/ns/reports/again", "", None))["value"].is_null()
        );
        assert_eq!(
            value(request(&server, "GET", "/cache/ns/other/monthly", "", None))["value"],
            "b2s="
        );
        value(request(
            &server,
            "POST",
            "/cache/ns/runtime.result_cache/flush",
            "",
            None,
        ));
        assert_eq!(
            server
                .runtime
                .result_blob_cache()
                .get("runtime.result_cache", "secret")
                .expect("internal value survived")
                .value(),
            b"internal"
        );
    }

    #[test]
    fn native_cache_http_validates_input_before_touching_cache() {
        let server = server();
        for body in [
            "not JSON",
            r#"{"value":"??"}"#,
            r#"{"value":"YQ==","ttl_ms":-1}"#,
            r#"{"value":"YQ==","ttl_ms":10,"policy":{"ttl_ms":20}}"#,
            r#"{"value":"YQ==","tags":["\u0000"]}"#,
            r#"{"value":"YQ==","policy":{"unknown":true}}"#,
        ] {
            assert_eq!(
                request(&server, "PUT", "/cache/ns/reports/key", body, None).status,
                400,
                "{body}"
            );
        }
        for path in [
            "/cache/ns/bad%00/key",
            "/cache/ns/reports/%FF",
            "/cache/ns/reports/%0A",
        ] {
            assert_eq!(
                request(&server, "PUT", path, r#"{"value":"YQ=="}"#, None).status,
                400
            );
        }
        let large = ::serde_json::json!({ "value": STANDARD.encode(vec![0; VALUE_BYTES_MAX + 1]) })
            .to_string();
        assert_eq!(
            request(&server, "PUT", "/cache/ns/reports/large", &large, None).status,
            413
        );
        assert_eq!(
            request(&server, "DELETE", "/cache/ns/reports", "", None).status,
            400
        );
        for prefix in ["%FF", "%00", "%GG"] {
            assert_eq!(
                request(
                    &server,
                    "DELETE",
                    &format!("/cache/ns/reports?prefix={prefix}"),
                    "",
                    None,
                )
                .status,
                400
            );
        }
        assert!(
            value(request(&server, "GET", "/cache/ns/reports/key", "", None))["value"].is_null()
        );
    }

    #[test]
    fn native_cache_http_scopes_tenants_and_honors_roles_and_iam_denies() {
        let store = Arc::new(AuthStore::new(AuthConfig {
            enabled: true,
            require_auth: true,
            ..AuthConfig::default()
        }));
        let mut keys = Vec::new();
        for (tenant, role) in [
            (Some("acme"), Role::Write),
            (Some("globex"), Role::Write),
            (None, Role::Write),
            (Some("acme"), Role::Read),
        ] {
            let username = if role == Role::Read {
                "reader"
            } else {
                "writer"
            };
            store
                .create_user_in_tenant(tenant, username, "password", role)
                .expect("test user");
            keys.push(
                store
                    .create_api_key_in_tenant(tenant, username, "test", role)
                    .expect("test key")
                    .key,
            );
        }
        let server = server().with_auth(Arc::clone(&store));
        assert_eq!(
            request(&server, "GET", "/cache/ns/reports/key", "", None).status,
            401
        );
        for (key, bytes) in keys.iter().take(3).zip([
            b"acme".as_slice(),
            b"globex".as_slice(),
            b"platform".as_slice(),
        ]) {
            put(&server, "reports", "key", bytes, Some(key));
        }
        assert_eq!(
            value(request(
                &server,
                "GET",
                "/cache/ns/reports/key",
                "",
                Some(&keys[0])
            ))["value"],
            STANDARD.encode(b"acme")
        );
        assert_eq!(
            value(request(
                &server,
                "GET",
                "/cache/ns/reports/key",
                "",
                Some(&keys[1])
            ))["value"],
            STANDARD.encode(b"globex")
        );
        assert_eq!(
            value(request(
                &server,
                "GET",
                "/cache/ns/reports/key",
                "",
                Some(&keys[3])
            ))["value"],
            STANDARD.encode(b"acme")
        );
        assert!(matches!(
            request(
                &server,
                "PUT",
                "/cache/ns/reports/key",
                r#"{"value":"YQ=="}"#,
                Some(&keys[3])
            )
            .status,
            401 | 403
        ));
        value(request(
            &server,
            "POST",
            "/cache/ns/reports/flush",
            "",
            Some(&keys[0]),
        ));
        assert_eq!(
            value(request(
                &server,
                "GET",
                "/cache/ns/reports/key",
                "",
                Some(&keys[1])
            ))["value"],
            STANDARD.encode(b"globex")
        );
        let policy = Policy::from_json_str(r#"{"id":"cache-deny","version":1,"statements":[{"effect":"deny","actions":["cache:read","cache:write","cache:invalidate"],"resources":["cache:reports"]}]}"#).expect("cache policy");
        store.put_policy(policy).expect("put policy");
        store
            .attach_policy(
                PrincipalRef::User(UserId::from_parts(Some("globex"), "writer")),
                "cache-deny",
            )
            .expect("attach policy");
        for (method, path, body) in [
            ("GET", "/cache/ns/reports/key", ""),
            ("PUT", "/cache/ns/reports/key", r#"{"value":"YQ=="}"#),
            ("POST", "/cache/ns/reports/flush", ""),
        ] {
            assert_eq!(
                request(&server, method, path, body, Some(&keys[1])).status,
                403
            );
        }
    }
}
