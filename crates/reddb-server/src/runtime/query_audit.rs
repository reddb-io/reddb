use std::collections::HashMap;
use std::sync::Arc;

use crate::crypto::uuid::Uuid;
use crate::storage::schema::types::Value;
use crate::storage::unified::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use crate::storage::UnifiedStore;
use crate::utils::now_unix_millis;

pub const QUERY_AUDIT_COLLECTION: &str = "red.query_audit";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryAuditRule {
    pub actor: Option<String>,
    pub tenant: Option<String>,
    pub collection: Option<String>,
    pub action: Option<String>,
}

impl QueryAuditRule {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }

    pub fn collection(mut self, collection: impl Into<String>) -> Self {
        self.collection = Some(collection.into());
        self
    }

    pub fn action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into());
        self
    }

    fn matches(&self, event: &QueryAuditEvent) -> bool {
        self.actor
            .as_deref()
            .is_none_or(|actor| event.actor.as_deref() == Some(actor))
            && self
                .tenant
                .as_deref()
                .is_none_or(|tenant| event.tenant.as_deref() == Some(tenant))
            && self
                .action
                .as_deref()
                .is_none_or(|action| event.statement_kind.eq_ignore_ascii_case(action))
            && self.collection.as_deref().is_none_or(|collection| {
                event
                    .touched_collections
                    .iter()
                    .any(|touched| touched == collection)
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryAuditConfig {
    pub enabled: bool,
    pub rules: Vec<QueryAuditRule>,
}

impl QueryAuditConfig {
    pub fn enabled_with_rules(rules: Vec<QueryAuditRule>) -> Self {
        Self {
            enabled: true,
            rules,
        }
    }

    pub fn regulated() -> Self {
        Self {
            enabled: true,
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct QueryAuditEvent {
    pub actor: Option<String>,
    pub tenant: Option<String>,
    pub statement_kind: &'static str,
    pub touched_collections: Vec<String>,
    pub duration_ms: u64,
    pub row_count: u64,
    pub request_id: Option<String>,
    pub query_hash: Option<String>,
}

pub struct QueryAuditStream {
    store: Arc<UnifiedStore>,
    config: parking_lot::RwLock<QueryAuditConfig>,
}

impl QueryAuditStream {
    pub fn new(store: Arc<UnifiedStore>, config: QueryAuditConfig) -> Self {
        if config.enabled {
            let _ = store.get_or_create_collection(QUERY_AUDIT_COLLECTION);
        }
        Self {
            store,
            config: parking_lot::RwLock::new(config),
        }
    }

    pub fn enable_infrastructure(&self) {
        self.config.write().enabled = true;
        let _ = self.store.get_or_create_collection(QUERY_AUDIT_COLLECTION);
    }

    pub fn is_enabled(&self) -> bool {
        self.config.read().enabled
    }

    pub fn has_rules(&self) -> bool {
        let cfg = self.config.read();
        cfg.enabled && !cfg.rules.is_empty()
    }

    pub fn rules(&self) -> Vec<QueryAuditRule> {
        self.config.read().rules.clone()
    }

    pub fn add_rule(&self, rule: QueryAuditRule) {
        let mut cfg = self.config.write();
        cfg.enabled = true;
        cfg.rules.push(rule);
        let _ = self.store.get_or_create_collection(QUERY_AUDIT_COLLECTION);
    }

    pub fn emit(&self, event: QueryAuditEvent) {
        let cfg = self.config.read();
        if !cfg.enabled || !cfg.rules.iter().any(|rule| rule.matches(&event)) {
            return;
        }
        drop(cfg);

        let _ = self.store.get_or_create_collection(QUERY_AUDIT_COLLECTION);
        let ts_ms = now_unix_millis();
        let ts_ns = (ts_ms as i128)
            .saturating_mul(1_000_000)
            .min(i64::MAX as i128) as i64;
        let id = Uuid::new_v7().to_string();

        let mut named = HashMap::with_capacity(11);
        named.insert("id".into(), Value::text(id));
        named.insert("ts".into(), Value::Integer(ts_ns));
        named.insert(
            "actor".into(),
            event.actor.map(Value::text).unwrap_or(Value::Null),
        );
        named.insert(
            "tenant".into(),
            event.tenant.map(Value::text).unwrap_or(Value::Null),
        );
        named.insert(
            "statement_kind".into(),
            Value::text(event.statement_kind.to_string()),
        );
        named.insert(
            "touched_collections".into(),
            Value::text(event.touched_collections.join(",")),
        );
        named.insert(
            "duration_ms".into(),
            Value::UnsignedInteger(event.duration_ms),
        );
        named.insert("row_count".into(), Value::UnsignedInteger(event.row_count));
        named.insert(
            "request_id".into(),
            event.request_id.map(Value::text).unwrap_or(Value::Null),
        );
        named.insert(
            "query_hash".into(),
            event.query_hash.map(Value::text).unwrap_or(Value::Null),
        );

        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from(QUERY_AUDIT_COLLECTION),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        );
        let _ = self.store.insert_auto(QUERY_AUDIT_COLLECTION, entity);
    }
}
