//! Browser credential layer — HTTP endpoints (issue #936, PRD #930).
//!
//! The hybrid-token flow a browser SPA uses to reach the
//! RedWire-over-WSS endpoint (ADR 0036) without ever holding a long-lived
//! credential in JavaScript:
//!
//!   * `POST /auth/browser/login` — username/password → a short-lived
//!     **access JWT** in the JSON body (the SPA keeps it in memory) plus
//!     a **refresh token** in an `HttpOnly; Secure; SameSite` cookie the
//!     SPA can never read.
//!   * `POST /auth/browser/refresh` — the browser replays the refresh
//!     cookie (automatically, by virtue of `credentials: 'include'`) to
//!     mint a fresh access JWT. The refresh cookie is rotated on every
//!     call.
//!   * `POST /auth/browser/logout` — clears the refresh cookie.
//!
//! The access JWT is what the browser presents in the RedWire handshake
//! (`oauth-jwt` method, `{"jwt": "<access>"}`). Because ADR 0029 binds an
//! accepted stream to a snapshot lease rather than the bearer token,
//! rotating the access token never tears down an in-flight stream
//! (AC #3) — the new token is simply used for the next handshake.
//!
//! ## CORS / cross-origin note
//!
//! The HTTP edge's CORS posture is a wildcard `Access-Control-Allow-Origin:
//! *` with no `Allow-Credentials` (see `transport::CORS_HEADER_PAIRS`),
//! which is the correct posture for a header-authenticated API. The
//! refresh **cookie** therefore only flows on **same-origin** requests:
//! the hybrid-token flow assumes the SPA is served from the same origin as
//! the API (or behind a proxy presenting one origin), which is exactly the
//! "100%-in-browser SPA talks directly to the server" driver of ADR 0036.
//! A credentialed cross-origin cookie flow would require reflecting the
//! `Origin` and emitting `Allow-Credentials: true` — a separate decision
//! that is deliberately out of scope here.

use super::*;
use crate::auth::browser_token::{cookie_value, BrowserIdentity, BrowserTokenAuthority};
use crate::server::header_escape_guard::HeaderEscapeGuard;
use std::sync::Arc;

/// Header name for the refresh cookie. `&'static` so it satisfies the
/// `HttpResponse::with_header` name contract (ADR 0010 §"Out of scope":
/// names are source literals, only values pass the escape guard).
const SET_COOKIE: &str = "Set-Cookie";

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Attach a guard-validated `Set-Cookie` to a response. A cookie value
/// that somehow failed the CRLF/control-byte guard (it never should — we
/// build it ourselves from a JWT + fixed attributes) degrades to a 500
/// rather than risk header smuggling (ADR 0010).
fn with_set_cookie(response: HttpResponse, cookie: &str) -> HttpResponse {
    match HeaderEscapeGuard::header_value(cookie) {
        Ok(value) => response.with_header(SET_COOKIE, value),
        Err(_) => json_error(500, "failed to construct refresh cookie"),
    }
}

impl RedDBServer {
    /// Resolve the browser-token authority or a 501 if the browser
    /// credential layer is not enabled on this server.
    fn browser_authority(&self) -> Result<Arc<BrowserTokenAuthority>, HttpResponse> {
        self.runtime
            .browser_token_authority()
            .ok_or_else(|| json_error(501, "browser credential layer is not enabled"))
    }

    /// POST /auth/browser/login
    ///
    /// Body: `{ "username": "...", "password": "...", "tenant_id": "..."? }`
    /// On success: 200 with `{ access_token, expires_in, token_type,
    /// username, role, tenant_id? }` and a `Set-Cookie` refresh cookie.
    pub(crate) fn handle_browser_login(&self, body: Vec<u8>) -> HttpResponse {
        let authority = match self.browser_authority() {
            Ok(a) => a,
            Err(resp) => return resp,
        };
        // Resolve the auth store from the runtime — the same source of
        // truth the RedWire handshake reads — so the browser flow works
        // whenever an `AuthStore` is wired, not only when the server was
        // built through `with_auth`.
        let auth_store = match self.runtime.auth_store() {
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
        let tenant_id = json_string_field(&payload, "tenant_id");

        let caller_id = crate::auth::UserId::from_parts(tenant_id.as_deref(), &username);
        let session =
            match auth_store.authenticate_in_tenant(tenant_id.as_deref(), &username, &password) {
                Ok(session) => session,
                Err(err) => {
                    tracing::warn!(
                        target: "reddb::http_auth",
                        principal = %caller_id,
                        "browser login refused"
                    );
                    return json_error(401, err.to_string());
                }
            };

        let identity = BrowserIdentity {
            username: session.username.clone(),
            tenant: session.tenant_id.clone(),
            role: session.role,
        };
        let tokens = match authority.issue(&identity, now_unix_secs()) {
            Ok(t) => t,
            Err(e) => return json_error(500, format!("failed to issue tokens: {e}")),
        };

        tracing::info!(
            target: "reddb::http_auth",
            principal = %caller_id,
            "browser login ok"
        );

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "access_token".to_string(),
            JsonValue::String(tokens.access_token),
        );
        object.insert("token_type".to_string(), JsonValue::String("Bearer".into()));
        object.insert(
            "expires_in".to_string(),
            JsonValue::Number(tokens.access_expires_in as f64),
        );
        object.insert(
            "username".to_string(),
            JsonValue::String(identity.username.clone()),
        );
        object.insert(
            "role".to_string(),
            JsonValue::String(identity.role.to_string()),
        );
        if let Some(t) = &identity.tenant {
            object.insert("tenant_id".to_string(), JsonValue::String(t.clone()));
        }

        let response = json_response(200, JsonValue::Object(object));
        with_set_cookie(response, &authority.refresh_cookie(&tokens.refresh_token))
    }

    /// POST /auth/browser/refresh
    ///
    /// Reads the refresh cookie, validates it, and returns a fresh access
    /// JWT. The refresh cookie is **rotated** on every call (a new
    /// refresh token is issued and re-set), so a captured cookie's useful
    /// lifetime is bounded by the next refresh.
    pub(crate) fn handle_browser_refresh(
        &self,
        headers: &BTreeMap<String, String>,
    ) -> HttpResponse {
        let authority = match self.browser_authority() {
            Ok(a) => a,
            Err(resp) => return resp,
        };

        let cookie_header = match headers.get("cookie") {
            Some(c) => c,
            None => return json_error(401, "no refresh cookie"),
        };
        let refresh = match cookie_value(cookie_header, authority.cookie_name()) {
            Some(v) if !v.is_empty() => v,
            _ => return json_error(401, "no refresh cookie"),
        };

        let now = now_unix_secs();
        let identity = match authority.validate_refresh(refresh, now) {
            Ok(id) => id,
            Err(err) => {
                tracing::warn!(target: "reddb::http_auth", "browser refresh rejected: {err}");
                // Clear the now-useless cookie so the browser stops
                // replaying a token we will never accept.
                let response = json_error(401, "invalid or expired refresh token");
                return with_set_cookie(response, &authority.clear_cookie());
            }
        };

        let access_token = match authority.issue_access(&identity, now) {
            Ok(t) => t,
            Err(e) => return json_error(500, format!("failed to issue access token: {e}")),
        };
        // Rotate the refresh cookie too — limits how long a leaked cookie
        // stays valid and re-arms the sliding refresh window.
        let new_refresh = match authority.issue(&identity, now) {
            Ok(t) => t.refresh_token,
            Err(e) => return json_error(500, format!("failed to rotate refresh token: {e}")),
        };

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("access_token".to_string(), JsonValue::String(access_token));
        object.insert("token_type".to_string(), JsonValue::String("Bearer".into()));
        object.insert(
            "expires_in".to_string(),
            JsonValue::Number(authority.access_ttl_secs() as f64),
        );
        if let Some(t) = &identity.tenant {
            object.insert("tenant_id".to_string(), JsonValue::String(t.clone()));
        }
        object.insert(
            "username".to_string(),
            JsonValue::String(identity.username.clone()),
        );
        object.insert(
            "role".to_string(),
            JsonValue::String(identity.role.to_string()),
        );

        let response = json_response(200, JsonValue::Object(object));
        with_set_cookie(response, &authority.refresh_cookie(&new_refresh))
    }

    /// POST /auth/browser/logout
    ///
    /// Clears the refresh cookie. Stateless: the access JWT the SPA holds
    /// in memory simply expires on its own short TTL.
    pub(crate) fn handle_browser_logout(&self) -> HttpResponse {
        let authority = match self.browser_authority() {
            Ok(a) => a,
            Err(resp) => return resp,
        };
        let response = json_ok("logged out");
        with_set_cookie(response, &authority.clear_cookie())
    }
}
