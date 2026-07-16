//! Runtime value comparison and coercion helpers.
use super::*;

pub(in crate::runtime) fn compare_runtime_values(
    left: &Value,
    right: &Value,
    op: CompareOp,
) -> bool {
    match op {
        CompareOp::Eq => runtime_values_equal(left, right),
        CompareOp::Ne => !runtime_values_equal(left, right),
        CompareOp::Lt => runtime_partial_cmp(left, right).is_some_and(|ord| ord == Ordering::Less),
        CompareOp::Le => runtime_partial_cmp(left, right)
            .is_some_and(|ord| matches!(ord, Ordering::Less | Ordering::Equal)),
        CompareOp::Gt => {
            runtime_partial_cmp(left, right).is_some_and(|ord| ord == Ordering::Greater)
        }
        CompareOp::Ge => runtime_partial_cmp(left, right)
            .is_some_and(|ord| matches!(ord, Ordering::Greater | Ordering::Equal)),
    }
}

pub(in crate::runtime) fn runtime_values_equal(left: &Value, right: &Value) -> bool {
    if matches!(
        (left, right),
        (Value::DecimalText(_), _) | (_, Value::DecimalText(_))
    ) {
        return crate::storage::query::value_compare::partial_compare_values(left, right)
            == Some(Ordering::Equal);
    }

    if let Some(ordering) = runtime_exact_integer_cmp(left, right) {
        return ordering == Ordering::Equal;
    }

    if let (Some(left), Some(right)) = (runtime_value_number(left), runtime_value_number(right)) {
        return left == right;
    }

    // Equality: prefer borrow path to avoid two String clones.
    if let (Some(ls), Some(rs)) = (runtime_value_text_str(left), runtime_value_text_str(right)) {
        return ls == rs;
    }
    if let (Some(left), Some(right)) = (runtime_value_text(left), runtime_value_text(right)) {
        return left == right;
    }

    if let (Value::Boolean(left), Value::Boolean(right)) = (left, right) {
        return left == right;
    }

    left == right
}

pub(in crate::runtime) fn runtime_partial_cmp(left: &Value, right: &Value) -> Option<Ordering> {
    if matches!(
        (left, right),
        (Value::DecimalText(_), _) | (_, Value::DecimalText(_))
    ) {
        return crate::storage::query::value_compare::partial_compare_values(left, right);
    }

    if let Some(ordering) = runtime_exact_integer_cmp(left, right) {
        return Some(ordering);
    }

    if let (Some(left), Some(right)) = (runtime_value_number(left), runtime_value_number(right)) {
        return left.partial_cmp(&right);
    }

    match (left, right) {
        (Value::Timestamp(left), Value::Timestamp(right)) => return Some(left.cmp(right)),
        (Value::TimestampMs(left), Value::TimestampMs(right)) => return Some(left.cmp(right)),
        (Value::Date(left), Value::Date(right)) => return Some(left.cmp(right)),
        (Value::Time(left), Value::Time(right)) => return Some(left.cmp(right)),
        (Value::Duration(left), Value::Duration(right)) => return Some(left.cmp(right)),
        _ => {}
    }

    // Fast text path: borrow the string slice when possible (avoids two
    // String clones), then compare abbreviated 8-byte keys first — full
    // str::cmp only if the first 8 bytes are equal.
    if let (Some(ls), Some(rs)) = (runtime_value_text_str(left), runtime_value_text_str(right)) {
        let l_abbrev = text_abbrev_key(ls);
        let r_abbrev = text_abbrev_key(rs);
        return Some(match l_abbrev.cmp(&r_abbrev) {
            Ordering::Equal => ls.cmp(rs),
            other => other,
        });
    }
    // Slower path for non-String text variants (RowRef, VectorRef, formatted values).
    if let (Some(left), Some(right)) = (runtime_value_text(left), runtime_value_text(right)) {
        return Some(left.as_str().cmp(right.as_str()));
    }

    match (left, right) {
        (Value::Boolean(left), Value::Boolean(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

pub(in crate::runtime::join_filter) fn runtime_exact_integer_cmp(
    left: &Value,
    right: &Value,
) -> Option<Ordering> {
    match (left, right) {
        (Value::Integer(left), Value::Integer(right)) => Some(left.cmp(right)),
        (Value::UnsignedInteger(left), Value::UnsignedInteger(right)) => Some(left.cmp(right)),
        (Value::Integer(left), Value::UnsignedInteger(right)) => Some(if *left < 0 {
            Ordering::Less
        } else {
            (*left as u64).cmp(right)
        }),
        (Value::UnsignedInteger(left), Value::Integer(right)) => Some(if *right < 0 {
            Ordering::Greater
        } else {
            left.cmp(&(*right as u64))
        }),
        _ => None,
    }
}

pub(in crate::runtime) fn runtime_value_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        Value::BigInt(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        Value::Timestamp(value) => Some(*value as f64),
        Value::Duration(value) => Some(*value as f64),
        _ => None,
    }
}

pub(in crate::runtime::join_filter) fn value_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) | Value::BigInt(value) => Some(*value),
        Value::UnsignedInteger(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

/// Coerce a value to `u64` — used by the H3 scalars whose cell ids are
/// full 64-bit unsigned (#1575).
pub(in crate::runtime::join_filter) fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(value) => Some(*value),
        Value::Integer(value) | Value::BigInt(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

pub(in crate::runtime) fn runtime_value_text(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.to_string()),
        Value::NodeRef(value) => Some(value.clone()),
        Value::EdgeRef(value) => Some(value.clone()),
        Value::RowRef(table, row_id) => Some(format!("{table}:{row_id}")),
        Value::VectorRef(collection, vector_id) => Some(format!("{collection}:{vector_id}")),
        Value::IpAddr(value) => Some(value.to_string()),
        Value::MacAddr(value) => Some(format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            value[0], value[1], value[2], value[3], value[4], value[5]
        )),
        Value::Uuid(value) => Some(
            value
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        ),
        Value::Boolean(value) => Some(value.to_string()),
        Value::Integer(value) => Some(value.to_string()),
        Value::UnsignedInteger(value) => Some(value.to_string()),
        Value::DecimalText(value) => Some(value.clone()),
        Value::Float(value) => Some(value.to_string()),
        Value::Timestamp(value) => Some(value.to_string()),
        Value::Duration(value) => Some(value.to_string()),
        Value::Null => None,
        Value::Json(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::Blob(_) | Value::Vector(_) => None,
        Value::Color([r, g, b]) => Some(format!("#{:02X}{:02X}{:02X}", r, g, b)),
        Value::Email(s) => Some(s.clone()),
        Value::Url(s) => Some(s.clone()),
        Value::Phone(n) => Some(format!("+{}", n)),
        Value::Semver(packed) => Some(format!(
            "{}.{}.{}",
            packed / 1_000_000,
            (packed / 1_000) % 1_000,
            packed % 1_000
        )),
        Value::Cidr(ip, prefix) => Some(format!(
            "{}.{}.{}.{}/{}",
            (ip >> 24) & 0xFF,
            (ip >> 16) & 0xFF,
            (ip >> 8) & 0xFF,
            ip & 0xFF,
            prefix
        )),
        Value::Date(days) => Some(days.to_string()),
        Value::Time(ms) => {
            let total_secs = ms / 1000;
            Some(format!(
                "{:02}:{:02}:{:02}",
                total_secs / 3600,
                (total_secs / 60) % 60,
                total_secs % 60
            ))
        }
        Value::Decimal(v) => Some(Value::Decimal(*v).display_string()),
        Value::EnumValue(i) => Some(format!("enum({})", i)),
        Value::Array(_) => None,
        Value::TimestampMs(ms) => Some(ms.to_string()),
        Value::Ipv4(ip) => Some(format!(
            "{}.{}.{}.{}",
            (ip >> 24) & 0xFF,
            (ip >> 16) & 0xFF,
            (ip >> 8) & 0xFF,
            ip & 0xFF
        )),
        Value::Ipv6(bytes) => Some(format!("{}", std::net::Ipv6Addr::from(*bytes))),
        Value::Subnet(ip, mask) => {
            let prefix = mask.leading_ones();
            Some(format!(
                "{}.{}.{}.{}/{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF,
                prefix
            ))
        }
        Value::Port(p) => Some(p.to_string()),
        Value::Latitude(micro) => Some(format!("{:.6}", *micro as f64 / 1_000_000.0)),
        Value::Longitude(micro) => Some(format!("{:.6}", *micro as f64 / 1_000_000.0)),
        Value::GeoPoint(lat, lon) => Some(format!(
            "{:.6},{:.6}",
            *lat as f64 / 1_000_000.0,
            *lon as f64 / 1_000_000.0
        )),
        Value::Country2(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::Country3(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::Lang2(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::Lang5(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::Currency(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::AssetCode(code) => Some(code.clone()),
        Value::Money { .. } => Some(value.display_string()),
        Value::ColorAlpha([r, g, b, a]) => Some(format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a)),
        Value::BigInt(v) => Some(v.to_string()),
        Value::KeyRef(col, key) => Some(format!("{}:{}", col, key)),
        Value::DocRef(col, id) => Some(format!("{}#{}", col, id)),
        Value::TableRef(name) => Some(name.clone()),
        Value::PageRef(page_id) => Some(format!("page:{}", page_id)),
        Value::Secret(_) | Value::Password(_) => Some("***".to_string()),
    }
}

/// Borrow-only text view — only covers variants whose value is already
/// a `String` field (no allocations). Used by `runtime_partial_cmp` to
/// avoid cloning text values when comparing.
pub(in crate::runtime) fn runtime_value_text_str(value: &Value) -> Option<&str> {
    match value {
        Value::Text(s) => Some(s.as_ref()),
        Value::NodeRef(s) | Value::EdgeRef(s) | Value::TableRef(s) => Some(s.as_str()),
        Value::Email(s) | Value::Url(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Like `runtime_value_text` but returns `Cow::Borrowed` when the value
/// already holds a `String` (Text, Email, Url, ref types) — zero alloc for
/// the common case in Like/StartsWith/EndsWith/Contains hot-path filters.
pub(in crate::runtime) fn runtime_value_text_cow(
    value: &Value,
) -> Option<std::borrow::Cow<'_, str>> {
    if let Some(s) = runtime_value_text_str(value) {
        return Some(std::borrow::Cow::Borrowed(s));
    }
    runtime_value_text(value).map(std::borrow::Cow::Owned)
}

/// Abbreviated sort key for a text slice: first 8 bytes in big-endian as a
/// `u64`. Shorter strings are zero-padded. Comparing this key first avoids
/// a full `str::cmp` in the typical case where the first 8 bytes differ —
/// mirrors PostgreSQL varlena abbreviated key optimisation (varlena.c:98-130).
#[inline]
pub(in crate::runtime) fn text_abbrev_key(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let len = bytes.len().min(8);
    let mut key = [0u8; 8];
    key[..len].copy_from_slice(&bytes[..len]);
    u64::from_be_bytes(key)
}

pub(in crate::runtime) fn table_column_name(field: &FieldRef) -> Option<&str> {
    match field {
        FieldRef::TableColumn { column, .. } => Some(column.as_str()),
        _ => None,
    }
}
