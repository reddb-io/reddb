//! Issue #582 — Analytics slice 4: `BatchInsertEndpoint` HTTP support.
//!
//! Owns the config + durable idempotency receipts that the
//! `POST /collections/:name/batch` handler uses to:
//!
//! * reject batches exceeding `red.batch.max_rows` with `413
//!   BatchTooLarge` before any storage work,
//! * deduplicate requests carrying the same `Idempotency-Key` within
//!   `red.batch.idempotency_window_secs`, returning the previously
//!   committed response body verbatim across process failover,
//! * report row-level validation failures by index so the caller can
//!   pinpoint the row that broke the all-or-nothing commit.
//!
//! The actual commit + AnalyticsSchemaRegistry validation runs in the
//! handler (`server::handlers_entity::handle_batch_insert`). This
//! module only owns the pieces that need to be unit-testable without
//! booting a full HTTP server.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reddb_types::Value;

use crate::storage::unified::entity::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use crate::storage::unified::{Metadata, MetadataValue};
use crate::{RedDBError, RedDBResult, RedDBRuntime};

const DEFAULT_MAX_ROWS: usize = 10_000;
const DEFAULT_IDEMPOTENCY_WINDOW_SECS: u64 = 300;
const ENV_MAX_ROWS: &str = "RED_BATCH_MAX_ROWS";
const ENV_IDEMPOTENCY_WINDOW_SECS: &str = "RED_BATCH_IDEMPOTENCY_WINDOW_SECS";
const RECEIPT_COLLECTION: &str = "red.batch.idempotency";

/// Knobs for the batch endpoint. Read from `RED_BATCH_*` env vars at
/// process start; the `red.batch.*` config keys in the brief route to
/// the same defaults until the broader config-overlay binding lands.
#[derive(Debug, Clone, Copy)]
pub struct BatchInsertConfig {
    pub max_rows: usize,
    pub idempotency_window: Duration,
}

impl BatchInsertConfig {
    pub fn from_env() -> Self {
        let max_rows = std::env::var(ENV_MAX_ROWS)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_ROWS);
        let window_secs = std::env::var(ENV_IDEMPOTENCY_WINDOW_SECS)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_IDEMPOTENCY_WINDOW_SECS);
        Self {
            max_rows,
            idempotency_window: Duration::from_secs(window_secs),
        }
    }
}

/// A committed response for a `(collection, idempotency-key)` pair.
#[derive(Debug, Clone)]
pub struct DurableReceipt {
    pub status: u16,
    pub body: Vec<u8>,
}

pub fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn receipt_key(collection: &str, idempotency_key: &str) -> String {
    let principal = crate::runtime::execution_context::current_auth_principal()
        .map(|principal| principal.to_string())
        .unwrap_or_default();
    let tenant = crate::runtime::execution_context::current_tenant().unwrap_or_default();
    let parts = [
        principal.as_str(),
        tenant.as_str(),
        collection,
        idempotency_key,
    ];
    let mut material = Vec::new();
    for part in parts {
        material.extend_from_slice(&(part.len() as u64).to_be_bytes());
        material.extend_from_slice(part.as_bytes());
    }
    hex::encode(crate::crypto::sha256(&material))
}

pub fn lock_durable_commit(collection: &str, idempotency_key: &str) -> MutexGuard<'static, ()> {
    const STRIPES: usize = 64;
    static LOCKS: OnceLock<Vec<Mutex<()>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| (0..STRIPES).map(|_| Mutex::new(())).collect());
    let digest = crate::crypto::sha256(receipt_key(collection, idempotency_key).as_bytes());
    let stripe = usize::from(digest[0]) % STRIPES;
    locks[stripe]
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn row_u64(row: &RowData, field: &str) -> Option<u64> {
    match row.get_field(field)? {
        Value::UnsignedInteger(value) => Some(*value),
        Value::Integer(value) if *value >= 0 => Some(*value as u64),
        _ => None,
    }
}

fn row_text(row: &RowData, field: &str) -> Option<String> {
    match row.get_field(field)? {
        Value::Text(value) => Some(value.to_string()),
        _ => None,
    }
}

fn receipt_is_committed(runtime: &RedDBRuntime, entity: &UnifiedEntity) -> bool {
    let snapshots = runtime.snapshot_manager();
    let inserted = entity.xmin == 0
        || (!snapshots.is_active(entity.xmin) && !snapshots.is_aborted(entity.xmin));
    let deleted =
        entity.xmax != 0 && !snapshots.is_active(entity.xmax) && !snapshots.is_aborted(entity.xmax);
    inserted && !deleted
}

pub fn lookup_durable_receipt(
    runtime: &RedDBRuntime,
    collection: &str,
    idempotency_key: &str,
    now_ms: u64,
) -> RedDBResult<Option<DurableReceipt>> {
    let store = runtime.db().store();
    let Some(manager) = store.get_collection(RECEIPT_COLLECTION) else {
        return Ok(None);
    };
    let _publication = manager
        .has_backing()
        .then(|| store.vault_publication_lock.lock());
    let key = receipt_key(collection, idempotency_key);
    let mut latest = None;
    manager
        .try_for_each_key_entity(&key, |entity| {
            if !receipt_is_committed(runtime, entity) {
                return true;
            }
            let Some(row) = entity.data.as_row() else {
                return true;
            };
            let Some(expires_at_ms) = row_u64(row, "expires_at_ms") else {
                return true;
            };
            if expires_at_ms <= now_ms {
                return true;
            }
            let Some(status) = row_u64(row, "status").and_then(|value| u16::try_from(value).ok())
            else {
                return true;
            };
            let Some(body) = row_text(row, "body") else {
                return true;
            };
            if latest
                .as_ref()
                .is_none_or(|(latest_id, _): &(EntityId, DurableReceipt)| entity.id > *latest_id)
            {
                latest = Some((
                    entity.id,
                    DurableReceipt {
                        status,
                        body: body.into_bytes(),
                    },
                ));
            }
            true
        })
        .map_err(|error| RedDBError::Internal(error.to_string()))?;
    Ok(latest.map(|(_, receipt)| receipt))
}

pub fn insert_durable_receipt(
    runtime: &RedDBRuntime,
    collection: &str,
    idempotency_key: &str,
    status: u16,
    body: &[u8],
    expires_at_ms: u64,
) -> RedDBResult<()> {
    let body = std::str::from_utf8(body)
        .map_err(|_| RedDBError::Internal("batch response is not UTF-8".to_string()))?;
    let store = runtime.db().store();
    let key = receipt_key(collection, idempotency_key);
    let fields = [
        ("key".to_string(), Value::text(key)),
        ("collection".to_string(), Value::text(collection)),
        (
            "status".to_string(),
            Value::UnsignedInteger(u64::from(status)),
        ),
        ("body".to_string(), Value::text(body)),
        (
            "expires_at_ms".to_string(),
            Value::UnsignedInteger(expires_at_ms),
        ),
    ]
    .into_iter()
    .collect();
    let mut entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: RECEIPT_COLLECTION.into(),
            row_id: 0,
        },
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(fields),
            schema: None,
        }),
    );
    if let Some(xid) = runtime.current_xid() {
        entity.set_xmin(xid);
    }
    let id = store
        .insert_auto(RECEIPT_COLLECTION, entity)
        .map_err(|error| RedDBError::Internal(error.to_string()))?;
    store
        .set_metadata(
            RECEIPT_COLLECTION,
            id,
            Metadata::with_fields(
                [(
                    "_expires_at".to_string(),
                    MetadataValue::Timestamp(expires_at_ms),
                )]
                .into_iter()
                .collect(),
            ),
        )
        .map_err(|error| RedDBError::Internal(error.to_string()))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum BatchInsertError {
    /// Body did not deserialize to a JSON array.
    BodyNotJsonArray,
    /// `rows.len() > config.max_rows`.
    BatchTooLarge { limit: usize, got: usize },
    /// Parse / shape failure for row `index`.
    RowParseFailure { index: usize, reason: String },
    /// AnalyticsSchemaRegistry rejected the row at `index`.
    RowSchemaRejected { index: usize, reason: String },
}

impl BatchInsertError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::BatchTooLarge { .. } => 413,
            _ => 400,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::BodyNotJsonArray => "BadRequest",
            Self::BatchTooLarge { .. } => "BatchTooLarge",
            Self::RowParseFailure { .. } => "RowParseFailure",
            Self::RowSchemaRejected { .. } => "RowSchemaRejected",
        }
    }

    pub fn row_index(&self) -> Option<usize> {
        match self {
            Self::RowParseFailure { index, .. } | Self::RowSchemaRejected { index, .. } => {
                Some(*index)
            }
            _ => None,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::BodyNotJsonArray => "request body must be a JSON array of rows".to_string(),
            Self::BatchTooLarge { limit, got } => {
                format!("batch size {got} exceeds red.batch.max_rows={limit}")
            }
            Self::RowParseFailure { index, reason } => {
                format!("row {index} failed validation: {reason}")
            }
            Self::RowSchemaRejected { index, reason } => {
                format!("row {index} rejected by AnalyticsSchemaRegistry: {reason}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_brief() {
        let cfg = BatchInsertConfig {
            max_rows: DEFAULT_MAX_ROWS,
            idempotency_window: Duration::from_secs(DEFAULT_IDEMPOTENCY_WINDOW_SECS),
        };
        assert_eq!(cfg.max_rows, 10_000);
        assert_eq!(cfg.idempotency_window, Duration::from_secs(300));
    }

    #[test]
    fn batch_too_large_maps_to_413() {
        let err = BatchInsertError::BatchTooLarge { limit: 10, got: 11 };
        assert_eq!(err.http_status(), 413);
        assert_eq!(err.code(), "BatchTooLarge");
        assert!(err.row_index().is_none());
        assert!(err.message().contains("10"));
        assert!(err.message().contains("11"));
    }

    #[test]
    fn row_schema_rejected_carries_index() {
        let err = BatchInsertError::RowSchemaRejected {
            index: 3,
            reason: "AnalyticsSchemaError:TypeMismatch:click:v1:user_id".to_string(),
        };
        assert_eq!(err.http_status(), 400);
        assert_eq!(err.code(), "RowSchemaRejected");
        assert_eq!(err.row_index(), Some(3));
        assert!(err.message().contains("row 3"));
        assert!(err.message().contains("TypeMismatch"));
    }

    #[test]
    fn row_parse_failure_carries_index() {
        let err = BatchInsertError::RowParseFailure {
            index: 7,
            reason: "missing fields".to_string(),
        };
        assert_eq!(err.http_status(), 400);
        assert_eq!(err.row_index(), Some(7));
        assert!(err.message().contains("row 7"));
    }

    #[test]
    fn body_not_json_array_is_400() {
        let err = BatchInsertError::BodyNotJsonArray;
        assert_eq!(err.http_status(), 400);
        assert!(err.row_index().is_none());
    }
}
