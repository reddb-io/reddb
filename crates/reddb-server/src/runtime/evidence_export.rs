use std::collections::BTreeMap;

use crate::api::{RedDBError, RedDBResult};
use crate::auth::policies::{EvalContext, ResourceRef};
use crate::auth::UserId;
use crate::runtime::control_events::{EventKind, Outcome, Sensitivity, CONTROL_EVENTS_COLLECTION};
use crate::runtime::impl_core::{current_auth_identity, current_tenant};
use crate::storage::schema::Value;
use crate::storage::EntityData;
use crate::RedDBRuntime;

pub const EVIDENCE_EXPORT_ACTION: &str = "evidence:export";
pub const EVIDENCE_EXPORT_RESOURCE_KIND: &str = "evidence";
pub const EVIDENCE_EXPORT_RESOURCE_NAME: &str = "control_events";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvidenceExportRequest {
    pub filter: EvidenceExportFilter,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvidenceExportFilter {
    pub start_ts: Option<i64>,
    pub end_ts: Option<i64>,
    pub actor_user_id: Option<String>,
    pub scope: Option<String>,
    pub resource: Option<String>,
    pub evidence_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceExportReport {
    pub filters: EvidenceExportFilter,
    pub export_started_at_ms: u64,
    pub export_completed_at_ms: u64,
    pub event_count: usize,
    pub counts_by_type: BTreeMap<String, usize>,
    pub high_water_ts: Option<i64>,
    pub high_water_event_id: Option<String>,
    pub integrity_hash: String,
    pub events: Vec<EvidenceExportEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceExportEvent {
    pub id: String,
    pub ts: i64,
    pub kind: String,
    pub outcome: String,
    pub actor_kind: String,
    pub actor_user_id: Option<String>,
    pub scope: Option<String>,
    pub action: String,
    pub resource: Option<String>,
    pub reason: Option<String>,
    pub matched_policy_id: Option<String>,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub fields_json: String,
    pub integrity_hash: String,
}

impl RedDBRuntime {
    pub fn export_evidence(
        &self,
        request: EvidenceExportRequest,
    ) -> RedDBResult<EvidenceExportReport> {
        let export_started_at_ms = crate::utils::now_unix_millis();
        let mut filter = request.filter;
        if let Err(err) = self.check_evidence_export_policy() {
            let reason = err.to_string();
            self.emit_control_event(
                EventKind::EvidenceExport,
                Outcome::Denied,
                "evidence_export",
                Some("evidence:control_events".to_string()),
                Some(reason.clone()),
                export_filter_fields(&filter),
            )?;
            return Err(RedDBError::Query(reason));
        }
        if let Err(err) = apply_scope_guard(&mut filter) {
            let reason = err.to_string();
            self.emit_control_event(
                EventKind::EvidenceExport,
                Outcome::Denied,
                "evidence_export",
                Some("evidence:control_events".to_string()),
                Some(reason.clone()),
                export_filter_fields(&filter),
            )?;
            return Err(RedDBError::Query(reason));
        }

        let mut events = self.filtered_control_events(&filter);
        events.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
        let mut counts_by_type = BTreeMap::new();
        let mut high_water_ts = None;
        let mut high_water_event_id = None;
        for event in &events {
            *counts_by_type.entry(event.kind.clone()).or_insert(0) += 1;
            high_water_ts = Some(event.ts);
            high_water_event_id = Some(event.id.clone());
        }
        let integrity_hash = export_integrity_hash(&filter, &events);
        let event_count = events.len();
        let export_completed_at_ms = crate::utils::now_unix_millis();

        self.emit_control_event(
            EventKind::EvidenceExport,
            Outcome::Allowed,
            "evidence_export",
            Some("evidence:control_events".to_string()),
            None,
            allowed_export_fields(&filter, event_count, high_water_ts, &integrity_hash),
        )?;

        Ok(EvidenceExportReport {
            filters: filter,
            export_started_at_ms,
            export_completed_at_ms,
            event_count,
            counts_by_type,
            high_water_ts,
            high_water_event_id,
            integrity_hash,
            events,
        })
    }

    fn check_evidence_export_policy(&self) -> RedDBResult<()> {
        let tenant = current_tenant();
        let Some((username, role)) = current_auth_identity() else {
            return Err(RedDBError::Query(format!(
                "{EVIDENCE_EXPORT_ACTION} requires an authenticated principal"
            )));
        };
        let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
            RedDBError::Query(format!("{EVIDENCE_EXPORT_ACTION} requires an auth store"))
        })?;
        let principal = UserId::from_parts(tenant.as_deref(), &username);
        let mut resource =
            ResourceRef::new(EVIDENCE_EXPORT_RESOURCE_KIND, EVIDENCE_EXPORT_RESOURCE_NAME);
        if let Some(tenant) = tenant.as_deref() {
            resource = resource.with_tenant(tenant.to_string());
        }
        let ctx = EvalContext {
            principal_tenant: tenant.clone(),
            current_tenant: tenant,
            peer_ip: None,
            mfa_present: false,
            now_ms: crate::auth::now_ms(),
            principal_is_admin_role: role == crate::auth::Role::Admin,
            principal_is_platform_scoped: principal.tenant.is_none(),
        };
        if auth_store.check_policy_authz_with_role(
            &principal,
            EVIDENCE_EXPORT_ACTION,
            &resource,
            &ctx,
            role,
        ) {
            Ok(())
        } else {
            Err(RedDBError::Query(format!(
                "principal=`{principal}` action=`{EVIDENCE_EXPORT_ACTION}` resource=`{}:{}` denied by IAM policy",
                resource.kind, resource.name
            )))
        }
    }

    fn filtered_control_events(&self, filter: &EvidenceExportFilter) -> Vec<EvidenceExportEvent> {
        let Some(manager) = self.db().store().get_collection(CONTROL_EVENTS_COLLECTION) else {
            return Vec::new();
        };
        manager
            .query_all(|_| true)
            .into_iter()
            .filter_map(|entity| {
                let EntityData::Row(row) = entity.data else {
                    return None;
                };
                let event = EvidenceExportEvent::from_row(&row.named?)?;
                if filter.matches(&event) {
                    Some(event)
                } else {
                    None
                }
            })
            .collect()
    }
}

fn apply_scope_guard(filter: &mut EvidenceExportFilter) -> RedDBResult<()> {
    let Some(active_scope) = current_tenant() else {
        return Ok(());
    };
    match filter.scope.as_deref() {
        Some(scope) if scope != active_scope => Err(RedDBError::Query(format!(
            "evidence export scope `{scope}` is outside active tenant `{active_scope}`"
        ))),
        Some(_) => Ok(()),
        None => {
            filter.scope = Some(active_scope);
            Ok(())
        }
    }
}

impl EvidenceExportFilter {
    fn matches(&self, event: &EvidenceExportEvent) -> bool {
        if self.start_ts.is_some_and(|start| event.ts < start) {
            return false;
        }
        if self.end_ts.is_some_and(|end| event.ts > end) {
            return false;
        }
        if self
            .actor_user_id
            .as_ref()
            .is_some_and(|actor| event.actor_user_id.as_ref() != Some(actor))
        {
            return false;
        }
        if self
            .scope
            .as_ref()
            .is_some_and(|scope| event.scope.as_ref() != Some(scope))
        {
            return false;
        }
        if self
            .resource
            .as_ref()
            .is_some_and(|resource| event.resource.as_ref() != Some(resource))
        {
            return false;
        }
        if self
            .evidence_type
            .as_ref()
            .is_some_and(|kind| &event.kind != kind)
        {
            return false;
        }
        true
    }
}

impl EvidenceExportEvent {
    fn from_row(row: &std::collections::HashMap<String, Value>) -> Option<Self> {
        let mut event = Self {
            id: text(row, "id")?,
            ts: integer(row, "ts")?,
            kind: text(row, "kind")?,
            outcome: text(row, "outcome")?,
            actor_kind: text(row, "actor_kind")?,
            actor_user_id: nullable_text(row, "actor_user_id"),
            scope: nullable_text(row, "scope"),
            action: text(row, "action")?,
            resource: nullable_text(row, "resource"),
            reason: nullable_text(row, "reason"),
            matched_policy_id: nullable_text(row, "matched_policy_id"),
            request_id: nullable_text(row, "request_id"),
            trace_id: nullable_text(row, "trace_id"),
            fields_json: text(row, "fields_json").unwrap_or_else(|| "{}".to_string()),
            integrity_hash: String::new(),
        };
        event.integrity_hash = event_integrity_hash(&event);
        Some(event)
    }
}

fn allowed_export_fields(
    filter: &EvidenceExportFilter,
    event_count: usize,
    high_water_ts: Option<i64>,
    integrity_hash: &str,
) -> Vec<(String, Sensitivity)> {
    let mut fields = export_filter_fields(filter);
    fields.push((
        "event_count".to_string(),
        Sensitivity::raw(event_count.to_string()),
    ));
    if let Some(ts) = high_water_ts {
        fields.push((
            "high_water_ts".to_string(),
            Sensitivity::raw(ts.to_string()),
        ));
    }
    fields.push((
        "integrity_hash".to_string(),
        Sensitivity::raw(integrity_hash.to_string()),
    ));
    fields
}

fn export_filter_fields(filter: &EvidenceExportFilter) -> Vec<(String, Sensitivity)> {
    let mut fields = Vec::new();
    if let Some(ts) = filter.start_ts {
        fields.push((
            "filter_start_ts".to_string(),
            Sensitivity::raw(ts.to_string()),
        ));
    }
    if let Some(ts) = filter.end_ts {
        fields.push((
            "filter_end_ts".to_string(),
            Sensitivity::raw(ts.to_string()),
        ));
    }
    if let Some(actor) = &filter.actor_user_id {
        fields.push(("filter_actor_user_id".to_string(), Sensitivity::raw(actor)));
    }
    if let Some(scope) = &filter.scope {
        fields.push(("filter_scope".to_string(), Sensitivity::raw(scope)));
    }
    if let Some(resource) = &filter.resource {
        fields.push(("filter_resource".to_string(), Sensitivity::raw(resource)));
    }
    if let Some(kind) = &filter.evidence_type {
        fields.push(("filter_evidence_type".to_string(), Sensitivity::raw(kind)));
    }
    fields
}

fn text(row: &std::collections::HashMap<String, Value>, field: &str) -> Option<String> {
    match row.get(field) {
        Some(Value::Text(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn nullable_text(row: &std::collections::HashMap<String, Value>, field: &str) -> Option<String> {
    match row.get(field) {
        Some(Value::Text(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn integer(row: &std::collections::HashMap<String, Value>, field: &str) -> Option<i64> {
    match row.get(field) {
        Some(Value::Integer(value)) | Some(Value::TimestampMs(value)) => Some(*value),
        _ => None,
    }
}

fn export_integrity_hash(filter: &EvidenceExportFilter, events: &[EvidenceExportEvent]) -> String {
    let mut body = String::new();
    push_filter_canonical(filter, &mut body);
    for event in events {
        body.push('\n');
        body.push_str(&event.integrity_hash);
    }
    format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex())
}

fn event_integrity_hash(event: &EvidenceExportEvent) -> String {
    let mut body = String::new();
    push_json_field("id", Some(&event.id), &mut body);
    push_json_field("ts", Some(&event.ts.to_string()), &mut body);
    push_json_field("kind", Some(&event.kind), &mut body);
    push_json_field("outcome", Some(&event.outcome), &mut body);
    push_json_field("actor_kind", Some(&event.actor_kind), &mut body);
    push_json_field("actor_user_id", event.actor_user_id.as_deref(), &mut body);
    push_json_field("scope", event.scope.as_deref(), &mut body);
    push_json_field("action", Some(&event.action), &mut body);
    push_json_field("resource", event.resource.as_deref(), &mut body);
    push_json_field("reason", event.reason.as_deref(), &mut body);
    push_json_field(
        "matched_policy_id",
        event.matched_policy_id.as_deref(),
        &mut body,
    );
    push_json_field("request_id", event.request_id.as_deref(), &mut body);
    push_json_field("trace_id", event.trace_id.as_deref(), &mut body);
    push_json_field("fields_json", Some(&event.fields_json), &mut body);
    format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex())
}

fn push_filter_canonical(filter: &EvidenceExportFilter, out: &mut String) {
    push_json_field(
        "start_ts",
        filter.start_ts.as_ref().map(|v| v.to_string()).as_deref(),
        out,
    );
    push_json_field(
        "end_ts",
        filter.end_ts.as_ref().map(|v| v.to_string()).as_deref(),
        out,
    );
    push_json_field("actor_user_id", filter.actor_user_id.as_deref(), out);
    push_json_field("scope", filter.scope.as_deref(), out);
    push_json_field("resource", filter.resource.as_deref(), out);
    push_json_field("evidence_type", filter.evidence_type.as_deref(), out);
}

fn push_json_field(name: &str, value: Option<&str>, out: &mut String) {
    out.push('"');
    json_escape_into(name, out);
    out.push_str("\":");
    match value {
        Some(value) => {
            out.push('"');
            json_escape_into(value, out);
            out.push('"');
        }
        None => out.push_str("null"),
    }
    out.push(';');
}

fn json_escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}
