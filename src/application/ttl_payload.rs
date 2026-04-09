use crate::application::entity::{PatchEntityOperation, PatchEntityOperationType};
use crate::json::Value as JsonValue;
use crate::storage::unified::MetadataValue;
use crate::{RedDBError, RedDBResult};

pub(crate) const INTERNAL_TTL_SECONDS_KEY: &str = "_ttl";
pub(crate) const INTERNAL_TTL_MILLIS_KEY: &str = "_ttl_ms";
pub(crate) const INTERNAL_EXPIRES_AT_KEY: &str = "_expires_at";
pub(crate) const INTERNAL_TTL_METADATA_KEYS: [&str; 3] = [
    INTERNAL_TTL_SECONDS_KEY,
    INTERNAL_TTL_MILLIS_KEY,
    INTERNAL_EXPIRES_AT_KEY,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TtlFieldKind {
    Duration,
    ExpiresAt,
}

pub(crate) fn has_internal_ttl_metadata(metadata: &[(String, MetadataValue)]) -> bool {
    metadata.iter().any(|(key, _)| {
        INTERNAL_TTL_METADATA_KEYS
            .iter()
            .any(|reserved| reserved == &key.as_str())
    })
}

pub(crate) fn parse_collection_default_ttl_ms(payload: &JsonValue) -> RedDBResult<Option<u64>> {
    let ttl = payload.get("ttl");
    let ttl_ms = payload.get("ttl_ms");

    if ttl.is_some() && ttl_ms.is_some() {
        return Err(RedDBError::Query(
            "collection TTL accepts either 'ttl' or 'ttl_ms', not both".to_string(),
        ));
    }

    if let Some(value) = ttl {
        return parse_duration_field_ms("ttl", value, 1_000).map(Some);
    }

    if let Some(value) = ttl_ms {
        return parse_duration_field_ms("ttl_ms", value, 1).map(Some);
    }

    Ok(None)
}

pub(crate) fn format_ttl_ms(ms: u64) -> String {
    for (unit_ms, suffix) in [
        (86_400_000, "d"),
        (3_600_000, "h"),
        (60_000, "m"),
        (1_000, "s"),
    ] {
        if ms >= unit_ms && ms % unit_ms == 0 {
            return format!("{}{}", ms / unit_ms, suffix);
        }
    }
    format!("{ms}ms")
}

pub(crate) fn parse_top_level_ttl_metadata_entries(
    payload: &JsonValue,
) -> RedDBResult<Vec<(String, MetadataValue)>> {
    let metadata = payload.get("metadata").and_then(JsonValue::as_object);
    let mut out = Vec::new();
    let mut seen_duration = false;
    let mut seen_expires_at = false;

    for field in [
        "ttl",
        "ttl_ms",
        "expires_at",
        INTERNAL_TTL_SECONDS_KEY,
        INTERNAL_TTL_MILLIS_KEY,
        INTERNAL_EXPIRES_AT_KEY,
    ] {
        let Some(value) = payload.get(field) else {
            continue;
        };

        let kind = ttl_field_kind(field);
        match kind {
            TtlFieldKind::Duration if seen_duration => {
                return Err(RedDBError::Query(
                    "TTL duration cannot be defined multiple times in the same payload".to_string(),
                ))
            }
            TtlFieldKind::ExpiresAt if seen_expires_at => {
                return Err(RedDBError::Query(
                    "TTL expiration cannot be defined multiple times in the same payload"
                        .to_string(),
                ))
            }
            TtlFieldKind::Duration => seen_duration = true,
            TtlFieldKind::ExpiresAt => seen_expires_at = true,
        }

        if metadata
            .map(|metadata| metadata_contains_conflicting_ttl(metadata, kind))
            .unwrap_or(false)
        {
            return Err(RedDBError::Query(format!(
                "ttl field '{field}' cannot be defined both at the top level and inside metadata"
            )));
        }

        out.push(parse_ttl_metadata_entry(field, value)?);
    }

    Ok(out)
}

pub(crate) fn normalize_ttl_patch_operations(
    operations: Vec<PatchEntityOperation>,
) -> RedDBResult<Vec<PatchEntityOperation>> {
    let mut out = Vec::with_capacity(operations.len());
    for mut op in operations {
        match op.path.as_slice() {
            [field]
                if matches!(
                    field.as_str(),
                    "ttl"
                        | "ttl_ms"
                        | "expires_at"
                        | INTERNAL_TTL_SECONDS_KEY
                        | INTERNAL_TTL_MILLIS_KEY
                        | INTERNAL_EXPIRES_AT_KEY
                ) =>
            {
                let field = field.clone();
                rewrite_ttl_patch_operation(&mut op, &field)?;
            }
            [root, field]
                if root == "metadata"
                    && matches!(
                        field.as_str(),
                        "ttl"
                            | "ttl_ms"
                            | "expires_at"
                            | INTERNAL_TTL_SECONDS_KEY
                            | INTERNAL_TTL_MILLIS_KEY
                            | INTERNAL_EXPIRES_AT_KEY
                    ) =>
            {
                let field = field.clone();
                rewrite_ttl_patch_operation(&mut op, &field)?;
            }
            _ => {}
        }
        out.push(op);
    }
    Ok(out)
}

fn rewrite_ttl_patch_operation(
    operation: &mut PatchEntityOperation,
    field: &str,
) -> RedDBResult<()> {
    let internal_key = internal_ttl_key(field).to_string();
    operation.path = vec!["metadata".to_string(), internal_key];
    if matches!(
        operation.op,
        PatchEntityOperationType::Set | PatchEntityOperationType::Replace
    ) {
        let value = operation.value.as_ref().ok_or_else(|| {
            RedDBError::Query(format!(
                "patch operation for ttl field '{field}' requires a value"
            ))
        })?;
        operation.value = Some(parse_ttl_json_value(field, value)?);
    }
    Ok(())
}

fn parse_ttl_metadata_entry(
    field: &str,
    value: &JsonValue,
) -> RedDBResult<(String, MetadataValue)> {
    let internal_key = internal_ttl_key(field).to_string();
    match value {
        JsonValue::Null => Ok((internal_key, MetadataValue::Null)),
        _ => Ok((internal_key, ttl_json_value_to_metadata(field, value)?)),
    }
}

fn parse_ttl_json_value(field: &str, value: &JsonValue) -> RedDBResult<JsonValue> {
    match value {
        JsonValue::Null => Ok(JsonValue::Null),
        _ => match ttl_field_kind(field) {
            TtlFieldKind::Duration => Ok(JsonValue::Number(parse_duration_field_ms(
                field,
                value,
                duration_field_default_unit_ms(field),
            )? as f64)),
            TtlFieldKind::ExpiresAt => Ok(JsonValue::Number(parse_epoch_ms(field, value)? as f64)),
        },
    }
}

fn ttl_json_value_to_metadata(field: &str, value: &JsonValue) -> RedDBResult<MetadataValue> {
    match ttl_field_kind(field) {
        TtlFieldKind::Duration => Ok(ttl_u64_to_metadata(parse_duration_field_ms(
            field,
            value,
            duration_field_default_unit_ms(field),
        )?)),
        TtlFieldKind::ExpiresAt => Ok(ttl_u64_to_metadata(parse_epoch_ms(field, value)?)),
    }
}

fn parse_duration_field_ms(
    field: &str,
    value: &JsonValue,
    default_unit_ms: u64,
) -> RedDBResult<u64> {
    match value {
        JsonValue::Number(value) => parse_duration_number_ms(field, *value, default_unit_ms),
        JsonValue::String(value) => parse_duration_text_ms(field, value, default_unit_ms),
        JsonValue::Null => Err(RedDBError::Query(format!(
            "field '{field}' cannot be null in this context"
        ))),
        _ => Err(RedDBError::Query(format!(
            "field '{field}' expects a numeric value or duration string"
        ))),
    }
}

fn parse_duration_text_ms(field: &str, value: &str, default_unit_ms: u64) -> RedDBResult<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RedDBError::Query(format!(
            "field '{field}' cannot be empty"
        )));
    }

    let split_at = trimmed
        .find(|ch: char| ch.is_ascii_alphabetic())
        .unwrap_or(trimmed.len());
    let (number_part, unit_part) = trimmed.split_at(split_at);
    let number_part = number_part.trim();
    let unit_part = unit_part.trim();

    let numeric = number_part.parse::<f64>().map_err(|_| {
        RedDBError::Query(format!(
            "field '{field}' expects a numeric value or duration string"
        ))
    })?;
    let multiplier_ms = if unit_part.is_empty() {
        default_unit_ms as f64
    } else {
        ttl_unit_multiplier_ms(field, unit_part)?
    };

    resolve_duration_ms(field, numeric, multiplier_ms)
}

fn parse_duration_number_ms(field: &str, value: f64, default_unit_ms: u64) -> RedDBResult<u64> {
    resolve_duration_ms(field, value, default_unit_ms as f64)
}

fn resolve_duration_ms(field: &str, value: f64, multiplier_ms: f64) -> RedDBResult<u64> {
    if !value.is_finite() {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be a finite number"
        )));
    }
    if value < 0.0 {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be non-negative"
        )));
    }

    let ttl_ms = value * multiplier_ms;
    if ttl_ms > u64::MAX as f64 {
        return Err(RedDBError::Query(format!(
            "field '{field}' value is too large"
        )));
    }
    if ttl_ms.fract().abs() >= f64::EPSILON {
        return Err(RedDBError::Query(format!(
            "field '{field}' must resolve to a whole number of milliseconds"
        )));
    }

    Ok(ttl_ms as u64)
}

fn parse_epoch_ms(field: &str, value: &JsonValue) -> RedDBResult<u64> {
    match value {
        JsonValue::Number(value) => parse_epoch_ms_number(field, *value),
        JsonValue::String(value) => {
            let numeric = value.trim().parse::<f64>().map_err(|_| {
                RedDBError::Query(format!(
                    "field '{field}' expects an epoch timestamp in milliseconds"
                ))
            })?;
            parse_epoch_ms_number(field, numeric)
        }
        JsonValue::Null => Err(RedDBError::Query(format!(
            "field '{field}' cannot be null in this context"
        ))),
        _ => Err(RedDBError::Query(format!(
            "field '{field}' expects an epoch timestamp in milliseconds"
        ))),
    }
}

fn parse_epoch_ms_number(field: &str, value: f64) -> RedDBResult<u64> {
    if !value.is_finite() {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be a finite number"
        )));
    }
    if value.fract().abs() >= f64::EPSILON {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be an integer"
        )));
    }
    if value < 0.0 {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be non-negative"
        )));
    }
    if value > u64::MAX as f64 {
        return Err(RedDBError::Query(format!(
            "field '{field}' value is too large"
        )));
    }
    Ok(value as u64)
}

fn ttl_unit_multiplier_ms(field: &str, unit: &str) -> RedDBResult<f64> {
    match unit.to_ascii_lowercase().as_str() {
        "ms" | "msec" | "millisecond" | "milliseconds" => Ok(1.0),
        "s" | "sec" | "secs" | "second" | "seconds" => Ok(1_000.0),
        "m" | "min" | "mins" | "minute" | "minutes" => Ok(60_000.0),
        "h" | "hr" | "hrs" | "hour" | "hours" => Ok(3_600_000.0),
        "d" | "day" | "days" => Ok(86_400_000.0),
        other => Err(RedDBError::Query(format!(
            "field '{field}' uses unsupported TTL unit '{other}'"
        ))),
    }
}

fn ttl_field_kind(field: &str) -> TtlFieldKind {
    match field {
        "expires_at" | INTERNAL_EXPIRES_AT_KEY => TtlFieldKind::ExpiresAt,
        _ => TtlFieldKind::Duration,
    }
}

fn internal_ttl_key(field: &str) -> &'static str {
    match field {
        "ttl" | "ttl_ms" | INTERNAL_TTL_SECONDS_KEY | INTERNAL_TTL_MILLIS_KEY => {
            INTERNAL_TTL_MILLIS_KEY
        }
        "expires_at" | INTERNAL_EXPIRES_AT_KEY => INTERNAL_EXPIRES_AT_KEY,
        _ => INTERNAL_TTL_MILLIS_KEY,
    }
}

fn duration_field_default_unit_ms(field: &str) -> u64 {
    match field {
        "ttl_ms" | INTERNAL_TTL_MILLIS_KEY => 1,
        _ => 1_000,
    }
}

fn metadata_contains_conflicting_ttl(
    metadata: &crate::json::Map<String, JsonValue>,
    kind: TtlFieldKind,
) -> bool {
    match kind {
        TtlFieldKind::Duration => {
            metadata.contains_key(INTERNAL_TTL_SECONDS_KEY)
                || metadata.contains_key(INTERNAL_TTL_MILLIS_KEY)
        }
        TtlFieldKind::ExpiresAt => metadata.contains_key(INTERNAL_EXPIRES_AT_KEY),
    }
}

fn ttl_u64_to_metadata(value: u64) -> MetadataValue {
    if value <= i64::MAX as u64 {
        MetadataValue::Int(value as i64)
    } else {
        MetadataValue::Timestamp(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::Map;

    fn object(entries: Vec<(&str, JsonValue)>) -> JsonValue {
        JsonValue::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect::<Map<_, _>>(),
        )
    }

    #[test]
    fn parse_public_ttl_duration_string_to_internal_metadata() {
        let payload = object(vec![("ttl", JsonValue::String("1.5s".to_string()))]);
        let metadata = parse_top_level_ttl_metadata_entries(&payload)
            .expect("ttl string should parse into metadata");

        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].0, INTERNAL_TTL_MILLIS_KEY);
        assert!(matches!(metadata[0].1, MetadataValue::Int(1500)));
    }

    #[test]
    fn parse_collection_ttl_prefers_single_field() {
        let payload = object(vec![
            ("name", JsonValue::String("sessions".to_string())),
            ("ttl", JsonValue::String("60s".to_string())),
            ("ttl_ms", JsonValue::Number(60000.0)),
        ]);

        let err = parse_collection_default_ttl_ms(&payload)
            .expect_err("duplicate collection ttl fields must fail");

        assert!(
            err.to_string().contains("either 'ttl' or 'ttl_ms'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn normalize_patch_operation_for_public_ttl_field() {
        let normalized = normalize_ttl_patch_operations(vec![PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["ttl".to_string()],
            value: Some(JsonValue::String("250ms".to_string())),
        }])
        .expect("ttl patch operation should normalize");

        assert_eq!(
            normalized[0].path,
            vec!["metadata".to_string(), INTERNAL_TTL_MILLIS_KEY.to_string()]
        );
        assert_eq!(normalized[0].value, Some(JsonValue::Number(250.0)));
    }
}
