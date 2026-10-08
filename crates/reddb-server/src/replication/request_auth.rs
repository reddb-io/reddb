//! Credentials for the replica's outbound snapshot, WAL and acknowledgement RPCs.
//! These are distinct from the credentials used by clients of the local replica.

use tonic::metadata::{Ascii, MetadataValue};
use tonic::Request;

#[derive(Clone, Default)]
pub(crate) struct ReplicaRequestAuth {
    principal: Option<String>,
    authorization: Option<MetadataValue<Ascii>>,
}

impl ReplicaRequestAuth {
    pub(crate) fn from_env() -> Result<Self, &'static str> {
        Self::from_values(
            read_secret("REDDB_REPLICATION_USERNAME")?,
            read_secret("REDDB_REPLICATION_API_KEY")?,
        )
    }

    fn from_values(principal: Option<String>, key: Option<String>) -> Result<Self, &'static str> {
        match (principal, key) {
            (None, None) => Ok(Self::default()),
            (Some(principal), Some(key)) => {
                if principal.is_empty() || principal.trim() != principal || principal.contains('/')
                    || principal.chars().any(char::is_control)
                    || key.is_empty() || key.chars().any(char::is_whitespace)
                {
                    return Err("invalid replication credential configuration");
                }
                let mut authorization: MetadataValue<Ascii> = format!("Bearer {key}")
                    .parse().map_err(|_| "invalid replication credential configuration")?;
                authorization.set_sensitive(true);
                Ok(Self { principal: Some(principal), authorization: Some(authorization) })
            }
            _ => Err("replication requires both REDDB_REPLICATION_USERNAME and REDDB_REPLICATION_API_KEY"),
        }
    }

    pub(crate) fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    pub(crate) fn request<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        self.authorize(&mut request);
        request
    }

    pub(crate) fn authorize<T>(&self, request: &mut Request<T>) {
        if let Some(authorization) = &self.authorization {
            request
                .metadata_mut()
                .insert("authorization", authorization.clone());
        }
    }
}

fn read_secret(name: &str) -> Result<Option<String>, &'static str> {
    let inline = std::env::var(name).ok().filter(|value| !value.is_empty());
    let file = std::env::var(format!("{name}_FILE"))
        .ok()
        .filter(|value| !value.is_empty());
    match (inline, file) {
        (Some(_), Some(_)) => Err("replication secret configured both inline and by file"),
        (Some(value), None) => Ok(Some(value)),
        (None, Some(path)) => {
            let value =
                std::fs::read_to_string(path).map_err(|_| "cannot read replication secret file")?;
            let value = value.trim_end_matches(['\n', '\r']).to_string();
            if value.is_empty() {
                return Err("empty replication secret file");
            }
            Ok(Some(value))
        }
        (None, None) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_requests_carry_sensitive_bearer_metadata() {
        let auth =
            ReplicaRequestAuth::from_values(Some("replica_a".into()), Some("test-key".into()))
                .expect("valid test credentials");
        assert_eq!(auth.principal(), Some("replica_a"));
        let request = auth.request(());
        let header = request
            .metadata()
            .get("authorization")
            .expect("bearer header");
        assert_eq!(header.to_str().expect("ASCII header"), "Bearer test-key");
        assert!(header.is_sensitive());
    }

    #[test]
    fn partial_and_malformed_credentials_fail_without_disclosing_secrets() {
        for (principal, key) in [
            (Some("replica_a"), None),
            (None, Some("test-key")),
            (Some("replica_a"), Some("secret\nkey")),
            (Some("tenant/replica_a"), Some("test-key")),
            (Some(" replica_a"), Some("test-key")),
        ] {
            let error = ReplicaRequestAuth::from_values(
                principal.map(str::to_string),
                key.map(str::to_string),
            )
            .err()
            .expect("invalid credentials rejected");
            assert!(!error.contains("test-key") && !error.contains("secret"));
        }
    }

    #[test]
    fn absent_credentials_do_not_invent_an_identity() {
        let auth = ReplicaRequestAuth::from_values(None, None).expect("unconfigured");
        assert!(auth.principal().is_none());
        assert!(auth.request(()).metadata().get("authorization").is_none());
    }
}
