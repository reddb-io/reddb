use std::sync::Arc;

use reddb::auth::policies::Policy;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::evidence_export::{EvidenceExportFilter, EvidenceExportRequest};
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
    set_current_tenant,
};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn cleanup_scope() {
    clear_current_auth_identity();
    clear_current_tenant();
}

fn evidence_export_policy(id: &str) -> Policy {
    Policy::from_json_str(&format!(
        r#"{{
            "id": "{id}",
            "version": 1,
            "statements": [{{
                "effect": "allow",
                "actions": ["evidence:export"],
                "resources": ["evidence:control_events"]
            }}, {{
                "effect": "allow",
                "actions": ["select"],
                "resources": ["table:__red_schema_control_events"]
            }}]
        }}"#
    ))
    .unwrap()
}

#[test]
fn evidence_export_filters_metadata_hashes_and_records_control_events() {
    cleanup_scope();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open");
    let auth = Arc::new(AuthStore::new(AuthConfig::default()));
    auth.create_user_in_tenant(Some("acme"), "ops", "ops-password", Role::Admin)
        .unwrap();
    auth.put_policy(evidence_export_policy("evidence-exporter"))
        .unwrap();
    auth.attach_policy(
        reddb::auth::store::PrincipalRef::User(UserId::scoped("acme", "ops")),
        "evidence-exporter",
    )
    .unwrap();
    rt.set_auth_store(Arc::clone(&auth));

    set_current_tenant("acme".to_string());
    set_current_auth_identity("ops".to_string(), Role::Admin);
    rt.execute_query("CREATE TABLE export_docs (id INT)")
        .expect("seed matching control event");
    rt.execute_query("CREATE TABLE other_docs (id INT)")
        .expect("seed non-matching control event");

    let export = rt
        .export_evidence(EvidenceExportRequest {
            filter: EvidenceExportFilter {
                start_ts: Some(0),
                end_ts: Some(i64::MAX),
                actor_user_id: Some("acme/ops".to_string()),
                scope: Some("acme".to_string()),
                resource: Some("table:export_docs".to_string()),
                evidence_type: Some("schema.ddl".to_string()),
            },
        })
        .expect("evidence export should be authorized");

    assert_eq!(export.filters.scope.as_deref(), Some("acme"));
    assert_eq!(export.event_count, 1);
    assert_eq!(export.counts_by_type.get("schema.ddl"), Some(&1));
    assert!(export.export_started_at_ms <= export.export_completed_at_ms);
    assert!(export.high_water_ts.is_some());
    assert!(export.high_water_event_id.is_some());
    assert!(export.integrity_hash.starts_with("blake3:"));
    assert_eq!(export.events.len(), 1);
    assert_eq!(
        export.events[0].resource.as_deref(),
        Some("table:export_docs")
    );
    assert!(export.events[0].integrity_hash.starts_with("blake3:"));

    let body = format!("{export:?}");
    assert!(!body.contains("ops-password"), "{body}");

    let implicit_scope = rt
        .export_evidence(EvidenceExportRequest {
            filter: EvidenceExportFilter {
                start_ts: Some(0),
                end_ts: Some(i64::MAX),
                actor_user_id: Some("acme/ops".to_string()),
                scope: None,
                resource: Some("table:export_docs".to_string()),
                evidence_type: Some("schema.ddl".to_string()),
            },
        })
        .expect("active tenant should become the export scope");
    assert_eq!(implicit_scope.filters.scope.as_deref(), Some("acme"));
    assert_eq!(implicit_scope.event_count, 1);

    let cross_scope = rt
        .export_evidence(EvidenceExportRequest {
            filter: EvidenceExportFilter {
                start_ts: None,
                end_ts: None,
                actor_user_id: None,
                scope: Some("other".to_string()),
                resource: None,
                evidence_type: None,
            },
        })
        .expect_err("tenant-scoped export must not cross scopes");
    assert!(cross_scope.to_string().contains("outside active tenant"));

    let exported = rt
        .execute_query(
            "SELECT kind, action, resource, outcome FROM red.control_events \
             WHERE kind = 'evidence.export'",
        )
        .expect("export control event should be queryable");
    assert!(
        exported.result.records.iter().any(|row| {
            row.get("action") == Some(&Value::text("evidence_export"))
                && row.get("resource") == Some(&Value::text("evidence:control_events"))
                && row.get("outcome") == Some(&Value::text("allowed"))
        }),
        "{:?}",
        exported.result.records
    );

    cleanup_scope();
}

#[test]
fn evidence_export_denial_is_policy_checked_and_recorded() {
    cleanup_scope();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open");
    let auth = Arc::new(AuthStore::new(AuthConfig::default()));
    auth.create_user_in_tenant(Some("acme"), "reader", "reader-password", Role::Read)
        .unwrap();
    rt.set_auth_store(auth);

    set_current_tenant("acme".to_string());
    set_current_auth_identity("reader".to_string(), Role::Read);
    let err = rt
        .export_evidence(EvidenceExportRequest::default())
        .expect_err("export should require evidence policy");
    assert!(
        err.to_string().contains("evidence:export")
            && err.to_string().contains("evidence:control_events"),
        "{err}"
    );

    let denied = rt
        .execute_query(
            "SELECT kind, action, resource, outcome FROM red.control_events \
             WHERE kind = 'evidence.export'",
        )
        .expect("denied export control event should be queryable");
    assert!(
        denied.result.records.iter().any(|row| {
            row.get("action") == Some(&Value::text("evidence_export"))
                && row.get("resource") == Some(&Value::text("evidence:control_events"))
                && row.get("outcome") == Some(&Value::text("denied"))
        }),
        "{:?}",
        denied.result.records
    );

    cleanup_scope();
}
