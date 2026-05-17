//! Issue #580 — DeclarativeRetention slice 1.
//!
//! Lazy-on-scan retention filter. When a collection carries a
//! `retention_duration_ms` policy on its `CollectionContract`, reads
//! drop rows whose timestamp-column value is older than `now -
//! retention_duration_ms` before they leave the runtime. The slice
//! never physically drops rows — `UNSET RETENTION` immediately
//! re-exposes the previously-hidden rows.
//!
//! The filter is intentionally a post-step on the assembled
//! `Vec<UnifiedRecord>` rather than a per-entity predicate hooked
//! into every scan path: there are 12+ early-return sites in
//! `query_exec::table` and centralising the gate at the single
//! `execute_runtime_table_query` chokepoint keeps the slice
//! surgical. Records whose projection elided the timestamp column
//! pass through unchanged — that is a documented limitation of
//! slice 1; the demoable `SELECT * FROM events` path keeps the
//! auto `created_at` / `updated_at` system columns and is fully
//! covered.
//!
//! Column resolution order:
//!   1. `WITH timestamps = true` → `created_at` (unix-ms).
//!   2. First declared column with a temporal `data_type`
//!      (`TIMESTAMP`, `TIMESTAMPMS`, `DATETIME`, `DATE`).

use crate::physical::CollectionContract;
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::Value;

/// Drop expired rows from `records` in-place based on the
/// collection contract's retention policy.
pub(crate) fn apply(records: &mut Vec<UnifiedRecord>, contract: Option<&CollectionContract>) {
    let Some(contract) = contract else {
        return;
    };
    let Some(retention_ms) = contract.retention_duration_ms else {
        return;
    };
    let Some(ts_column) = resolve_timestamp_column(contract) else {
        return;
    };

    let now_ms = current_unix_ms();
    let cutoff = now_ms.saturating_sub(retention_ms as u128);

    records.retain(|record| {
        // Records that don't carry the timestamp column (e.g.
        // because the SELECT projection elided it) are left in
        // place — slice 1 is intentionally permissive here.
        match record.get(&ts_column) {
            Some(value) => value_as_unix_ms(value)
                .map(|ts| (ts as u128) >= cutoff)
                .unwrap_or(true),
            None => true,
        }
    });
}

/// Resolve which column carries the row's timestamp for retention.
pub(crate) fn resolve_timestamp_column(contract: &CollectionContract) -> Option<String> {
    if contract.timestamps_enabled {
        return Some("created_at".to_string());
    }
    contract
        .declared_columns
        .iter()
        .find(|column| is_temporal_data_type(&column.data_type))
        .map(|column| column.name.clone())
}

fn is_temporal_data_type(data_type: &str) -> bool {
    matches!(
        data_type.to_ascii_uppercase().as_str(),
        "TIMESTAMP" | "TIMESTAMPMS" | "TIMESTAMP_MS" | "DATETIME" | "DATE"
    )
}

fn value_as_unix_ms(value: &Value) -> Option<i64> {
    match value {
        Value::TimestampMs(v) => Some(*v),
        // `Value::Timestamp` is seconds since epoch on this engine —
        // multiply up so the comparison is in the same unit as the
        // policy duration. Saturating because pre-1970 timestamps
        // would otherwise wrap.
        Value::Timestamp(v) => Some(v.saturating_mul(1_000)),
        Value::BigInt(v) => Some(*v),
        Value::UnsignedInteger(v) => i64::try_from(*v).ok(),
        Value::Integer(v) => Some(*v as i64),
        _ => None,
    }
}

fn current_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CollectionModel, SchemaMode};
    use crate::physical::{
        CollectionContract, ContractOrigin, DeclaredColumnContract,
    };
    use crate::storage::query::unified::{sys_key_created_at, UnifiedRecord};
    use std::sync::Arc;

    fn base_contract() -> CollectionContract {
        CollectionContract {
            name: "events".to_string(),
            declared_model: CollectionModel::Table,
            schema_mode: SchemaMode::SemiStructured,
            origin: ContractOrigin::Explicit,
            version: 1,
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
            default_ttl_ms: None,
            vector_dimension: None,
            vector_metric: None,
            context_index_fields: Vec::new(),
            declared_columns: Vec::new(),
            table_def: None,
            timestamps_enabled: true,
            context_index_enabled: false,
            metrics_raw_retention_ms: None,
            metrics_rollup_policies: Vec::new(),
            metrics_tenant_identity: None,
            metrics_namespace: None,
            append_only: false,
            subscriptions: Vec::new(),
            session_key: None,
            session_gap_ms: None,
            retention_duration_ms: Some(60_000), // 1 minute
        }
    }

    fn record_with_created_at(ts_ms: i64) -> UnifiedRecord {
        let schema = Arc::new(vec![sys_key_created_at()]);
        UnifiedRecord::with_schema(schema, vec![Value::BigInt(ts_ms)])
    }

    #[test]
    fn drops_expired_keeps_fresh() {
        let contract = base_contract();
        let now = current_unix_ms() as i64;
        let mut records = vec![
            record_with_created_at(now - 10_000),         // fresh
            record_with_created_at(now - 120_000),        // expired
        ];
        apply(&mut records, Some(&contract));
        assert_eq!(records.len(), 1, "exactly the fresh row should survive");
    }

    #[test]
    fn no_policy_leaves_records_alone() {
        let mut contract = base_contract();
        contract.retention_duration_ms = None;
        let mut records = vec![
            record_with_created_at(0),
            record_with_created_at(0),
        ];
        apply(&mut records, Some(&contract));
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn missing_column_does_not_filter() {
        // Custom timestamp column declared, but the projected record
        // has no such field — slice 1 leaves it alone rather than
        // hiding the row.
        let mut contract = base_contract();
        contract.timestamps_enabled = false;
        contract.declared_columns.push(DeclaredColumnContract {
            name: "ts".to_string(),
            data_type: "TIMESTAMP".to_string(),
            sql_type: None,
            not_null: false,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        });
        let schema = Arc::new(vec![Arc::<str>::from("other_col")]);
        let mut records =
            vec![UnifiedRecord::with_schema(schema, vec![Value::text("v")])];
        apply(&mut records, Some(&contract));
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn resolves_declared_temporal_column_name() {
        let mut contract = base_contract();
        contract.timestamps_enabled = false;
        contract.declared_columns.push(DeclaredColumnContract {
            name: "ts".to_string(),
            data_type: "TIMESTAMPMS".to_string(),
            sql_type: None,
            not_null: false,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        });
        assert_eq!(resolve_timestamp_column(&contract).as_deref(), Some("ts"));
    }
}
