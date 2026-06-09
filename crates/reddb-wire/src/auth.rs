//! Shared auth payload/header vocabulary for RedDB transports.
//!
//! The concrete HTTP/gRPC clients own their transport libraries, but the
//! field names and bearer/login payload shape are wire contracts.

pub const AUTHORIZATION_HEADER: &str = "authorization";
pub const BEARER_AUTH_SCHEME: &str = "Bearer";

pub fn bearer_authorization_value(token: &str) -> String {
    format!("{BEARER_AUTH_SCHEME} {token}")
}

pub fn login_payload_json(username: &str, password: &str) -> String {
    serde_json::json!({
        "username": username,
        "password": password,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_authorization_value_uses_canonical_scheme() {
        assert_eq!(bearer_authorization_value("token-1"), "Bearer token-1");
    }

    #[test]
    fn login_payload_json_escapes_credentials() {
        let payload = login_payload_json("ada\"lovelace", "p\\ass\nword");
        let value: serde_json::Value = serde_json::from_str(&payload).expect("json");

        assert_eq!(value["username"], "ada\"lovelace");
        assert_eq!(value["password"], "p\\ass\nword");
    }
}
