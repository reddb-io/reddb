use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::schema::Value;
use crate::storage::unified::entity::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use crate::storage::unified::store::UnifiedStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcOperation {
    Add,
    Sub,
    Set,
}

impl EcOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Set => "set",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "add" => Some(Self::Add),
            "sub" => Some(Self::Sub),
            "set" => Some(Self::Set),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EcTransaction {
    pub target_id: u64,
    pub field: String,
    pub value: f64,
    pub operation: EcOperation,
    pub timestamp: u64,
    pub cohort_hour: String,
    pub applied: bool,
    pub source: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn cohort_hour_from_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let hours = secs / 3600;
    let days = hours / 24;
    let remaining_hours = hours % 24;

    let epoch_days = days as i64;
    let year = 1970 + (epoch_days * 400 / 146097) as u32;
    let month = ((epoch_days % 365) / 30 + 1).min(12) as u32;
    let day = ((epoch_days % 365) % 30 + 1).min(28) as u32;

    format!("{:04}-{:02}-{:02}T{:02}", year, month, day, remaining_hours)
}

pub fn create_transaction(
    store: &UnifiedStore,
    tx_collection: &str,
    target_id: u64,
    field: &str,
    value: f64,
    operation: EcOperation,
    source: Option<&str>,
) -> Result<EntityId, String> {
    let _ = store.get_or_create_collection(tx_collection);

    let timestamp = now_ms();
    let cohort = cohort_hour_from_ms(timestamp);

    let mut named = std::collections::HashMap::new();
    named.insert("target_id".to_string(), Value::UnsignedInteger(target_id));
    named.insert("field".to_string(), Value::Text(field.to_string()));
    named.insert("value".to_string(), Value::Float(value));
    named.insert(
        "operation".to_string(),
        Value::Text(operation.as_str().to_string()),
    );
    named.insert("timestamp".to_string(), Value::UnsignedInteger(timestamp));
    named.insert("cohort_hour".to_string(), Value::Text(cohort));
    named.insert("applied".to_string(), Value::Boolean(false));
    if let Some(src) = source {
        named.insert("source".to_string(), Value::Text(src.to_string()));
    }

    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: Arc::from(tx_collection),
            row_id: 0,
        },
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(named),
            schema: None,
        }),
    );

    store
        .insert_auto(tx_collection, entity)
        .map_err(|e| format!("ec transaction insert failed: {:?}", e))
}

pub fn query_pending_transactions(
    store: &UnifiedStore,
    tx_collection: &str,
    target_id: Option<u64>,
) -> Vec<(EntityId, EcTransaction)> {
    let manager = match store.get_collection(tx_collection) {
        Some(m) => m,
        None => return Vec::new(),
    };

    let mut results = Vec::new();

    manager.for_each_entity(|entity| {
        let row = match entity.data.as_row() {
            Some(r) => r,
            None => return true,
        };

        let applied = row
            .get_field("applied")
            .and_then(|v| match v {
                Value::Boolean(b) => Some(*b),
                _ => None,
            })
            .unwrap_or(false);

        if applied {
            return true;
        }

        let tid = row
            .get_field("target_id")
            .and_then(|v| match v {
                Value::UnsignedInteger(n) => Some(*n),
                Value::Integer(n) => Some(*n as u64),
                _ => None,
            })
            .unwrap_or(0);

        if let Some(filter_id) = target_id {
            if tid != filter_id {
                return true;
            }
        }

        let field = row
            .get_field("field")
            .and_then(|v| match v {
                Value::Text(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let value = row
            .get_field("value")
            .and_then(|v| match v {
                Value::Float(f) => Some(*f),
                Value::Integer(n) => Some(*n as f64),
                Value::UnsignedInteger(n) => Some(*n as f64),
                _ => None,
            })
            .unwrap_or(0.0);

        let operation = row
            .get_field("operation")
            .and_then(|v| match v {
                Value::Text(s) => EcOperation::from_str(s),
                _ => None,
            })
            .unwrap_or(EcOperation::Add);

        let timestamp = row
            .get_field("timestamp")
            .and_then(|v| match v {
                Value::UnsignedInteger(n) => Some(*n),
                Value::Integer(n) => Some(*n as u64),
                _ => None,
            })
            .unwrap_or(0);

        let cohort_hour = row
            .get_field("cohort_hour")
            .and_then(|v| match v {
                Value::Text(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let source = row.get_field("source").and_then(|v| match v {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        });

        results.push((
            entity.id,
            EcTransaction {
                target_id: tid,
                field,
                value,
                operation,
                timestamp,
                cohort_hour,
                applied: false,
                source,
            },
        ));

        true
    });

    results.sort_by_key(|(_, tx)| tx.timestamp);
    results
}

pub fn mark_transactions_applied(
    store: &UnifiedStore,
    tx_collection: &str,
    entity_ids: &[EntityId],
) {
    let manager = match store.get_collection(tx_collection) {
        Some(m) => m,
        None => return,
    };

    for &eid in entity_ids {
        if let Some(mut entity) = manager.get(eid) {
            if let EntityData::Row(ref mut row) = entity.data {
                if let Some(ref mut named) = row.named {
                    named.insert("applied".to_string(), Value::Boolean(true));
                }
            }
            let _ = manager.update(entity);
        }
    }
}
