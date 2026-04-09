//! Type coercion and validation for rich types.
//!
//! Converts human-readable input strings into compact native Value representations.
//! Each parser validates format, enforces constraints, and produces the most compact
//! binary encoding possible for the target type.

use super::types::{DataType, Value};

/// Coerce a string input into the target type. Returns error description on failure.
pub fn coerce(
    input: &str,
    target: DataType,
    enum_variants: Option<&[String]>,
) -> Result<Value, String> {
    match target {
        DataType::Color => parse_color(input),
        DataType::Email => parse_email(input),
        DataType::Url => parse_url(input),
        DataType::Phone => parse_phone(input),
        DataType::Semver => parse_semver(input),
        DataType::Cidr => parse_cidr(input),
        DataType::Date => parse_date(input),
        DataType::Time => parse_time(input),
        DataType::Decimal => parse_decimal(input),
        DataType::Enum => parse_enum(input, enum_variants.unwrap_or(&[])),
        DataType::Integer => input
            .parse::<i64>()
            .map(Value::Integer)
            .map_err(|e| e.to_string()),
        DataType::UnsignedInteger => input
            .parse::<u64>()
            .map(Value::UnsignedInteger)
            .map_err(|e| e.to_string()),
        DataType::Float => input
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|e| e.to_string()),
        DataType::Boolean => parse_boolean(input),
        DataType::Text => Ok(Value::Text(input.to_string())),
        DataType::TimestampMs => parse_timestamp_ms(input),
        DataType::Ipv4 => parse_ipv4(input),
        DataType::Ipv6 => parse_ipv6(input),
        DataType::Subnet => parse_subnet(input),
        DataType::Port => parse_port(input),
        DataType::Latitude => parse_latitude(input),
        DataType::Longitude => parse_longitude(input),
        DataType::GeoPoint => parse_geopoint(input),
        DataType::Country2 => parse_country2(input),
        DataType::Country3 => parse_country3(input),
        DataType::Lang2 => parse_lang2(input),
        DataType::Lang5 => parse_lang5(input),
        DataType::Currency => parse_currency(input),
        DataType::ColorAlpha => parse_color_alpha(input),
        DataType::BigInt => input
            .parse::<i64>()
            .map(Value::BigInt)
            .map_err(|e| e.to_string()),
        DataType::KeyRef => parse_key_ref(input),
        DataType::DocRef => parse_doc_ref(input),
        DataType::TableRef => Ok(Value::TableRef(input.to_string())),
        DataType::PageRef => input
            .parse::<u32>()
            .map(Value::PageRef)
            .map_err(|e| e.to_string()),
        _ => Ok(Value::Text(input.to_string())), // fallback for unsupported coercions
    }
}

fn parse_color(input: &str) -> Result<Value, String> {
    let hex = input.trim_start_matches('#');
    if hex.len() != 6 {
        return Err("color must be 6 hex digits (e.g., #FF5733)".into());
    }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| "invalid red component")?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| "invalid green component")?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| "invalid blue component")?;
    Ok(Value::Color([r, g, b]))
}

fn parse_email(input: &str) -> Result<Value, String> {
    let lower = input.trim().to_lowercase();
    if !lower.contains('@') || !lower.contains('.') {
        return Err("invalid email format".into());
    }
    let parts: Vec<&str> = lower.split('@').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err("invalid email".into());
    }
    if !parts[1].contains('.') {
        return Err("email domain must have a dot".into());
    }
    Ok(Value::Email(lower))
}

fn parse_url(input: &str) -> Result<Value, String> {
    let trimmed = input.trim();
    if !trimmed.starts_with("http://")
        && !trimmed.starts_with("https://")
        && !trimmed.starts_with("ftp://")
    {
        return Err("URL must start with http://, https://, or ftp://".into());
    }
    Ok(Value::Url(trimmed.to_string()))
}

fn parse_phone(input: &str) -> Result<Value, String> {
    let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 7 || digits.len() > 15 {
        return Err("phone must have 7-15 digits".into());
    }
    let num = digits.parse::<u64>().map_err(|e| e.to_string())?;
    Ok(Value::Phone(num))
}

fn parse_semver(input: &str) -> Result<Value, String> {
    let parts: Vec<&str> = input.split('.').collect();
    if parts.len() != 3 {
        return Err("semver must be X.Y.Z".into());
    }
    let major: u32 = parts[0].parse().map_err(|_| "invalid major")?;
    let minor: u32 = parts[1].parse().map_err(|_| "invalid minor")?;
    let patch: u32 = parts[2].parse().map_err(|_| "invalid patch")?;
    if major > 999 || minor > 999 || patch > 999 {
        return Err("version components must be 0-999".into());
    }
    Ok(Value::Semver(major * 1_000_000 + minor * 1_000 + patch))
}

fn parse_cidr(input: &str) -> Result<Value, String> {
    let parts: Vec<&str> = input.split('/').collect();
    if parts.len() != 2 {
        return Err("CIDR must be IP/prefix (e.g., 10.0.0.0/8)".into());
    }
    let ip_parts: Vec<u8> = parts[0]
        .split('.')
        .map(|s| s.parse::<u8>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "invalid IP")?;
    if ip_parts.len() != 4 {
        return Err("IPv4 must have 4 octets".into());
    }
    let ip = ((ip_parts[0] as u32) << 24)
        | ((ip_parts[1] as u32) << 16)
        | ((ip_parts[2] as u32) << 8)
        | (ip_parts[3] as u32);
    let prefix: u8 = parts[1].parse().map_err(|_| "invalid prefix")?;
    if prefix > 32 {
        return Err("prefix must be 0-32".into());
    }
    Ok(Value::Cidr(ip, prefix))
}

fn parse_date(input: &str) -> Result<Value, String> {
    let parts: Vec<&str> = input.split('-').collect();
    if parts.len() != 3 {
        return Err("date must be YYYY-MM-DD".into());
    }
    let year: i32 = parts[0].parse().map_err(|_| "invalid year")?;
    let month: u32 = parts[1].parse().map_err(|_| "invalid month")?;
    let day: u32 = parts[2].parse().map_err(|_| "invalid day")?;
    if !(1..=12).contains(&month) {
        return Err("month must be 1-12".into());
    }
    if !(1..=31).contains(&day) {
        return Err("day must be 1-31".into());
    }
    let days = civil_days(year, month, day);
    Ok(Value::Date(days))
}

fn parse_time(input: &str) -> Result<Value, String> {
    let parts: Vec<&str> = input.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err("time must be HH:MM or HH:MM:SS".into());
    }
    let h: u32 = parts[0].parse().map_err(|_| "invalid hour")?;
    let m: u32 = parts[1].parse().map_err(|_| "invalid minute")?;
    let s: u32 = if parts.len() == 3 {
        parts[2].parse().map_err(|_| "invalid second")?
    } else {
        0
    };
    if h > 23 || m > 59 || s > 59 {
        return Err("invalid time".into());
    }
    Ok(Value::Time((h * 3600 + m * 60 + s) * 1000))
}

fn parse_decimal(input: &str) -> Result<Value, String> {
    let f: f64 = input.parse().map_err(|_| "invalid decimal")?;
    Ok(Value::Decimal((f * 10_000.0) as i64))
}

fn parse_enum(input: &str, variants: &[String]) -> Result<Value, String> {
    let lower = input.to_lowercase();
    variants
        .iter()
        .position(|v| v.to_lowercase() == lower)
        .map(|i| Value::EnumValue(i as u8))
        .ok_or_else(|| {
            format!(
                "'{}' is not a valid variant. Expected one of: {}",
                input,
                variants.join(", ")
            )
        })
}

fn parse_boolean(input: &str) -> Result<Value, String> {
    match input.to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(Value::Boolean(true)),
        "false" | "0" | "no" | "off" => Ok(Value::Boolean(false)),
        _ => Err(format!("invalid boolean: '{}'", input)),
    }
}

fn parse_timestamp_ms(input: &str) -> Result<Value, String> {
    // Try epoch ms first, then ISO datetime
    if let Ok(ms) = input.parse::<i64>() {
        return Ok(Value::TimestampMs(ms));
    }
    parse_iso_datetime_ms(input).map(Value::TimestampMs)
}

fn parse_iso_datetime_ms(input: &str) -> Result<i64, String> {
    // Parse "2024-03-15T14:30:00.123Z" or "2024-03-15T14:30:00Z"
    let trimmed = input.trim().trim_end_matches('Z');
    let (date_part, time_part) = trimmed
        .split_once('T')
        .ok_or("ISO datetime must contain 'T' separator")?;

    let date_parts: Vec<&str> = date_part.split('-').collect();
    if date_parts.len() != 3 {
        return Err("date part must be YYYY-MM-DD".into());
    }
    let year: i32 = date_parts[0].parse().map_err(|_| "invalid year")?;
    let month: u32 = date_parts[1].parse().map_err(|_| "invalid month")?;
    let day: u32 = date_parts[2].parse().map_err(|_| "invalid day")?;
    if !(1..=12).contains(&month) {
        return Err("month must be 1-12".into());
    }
    if !(1..=31).contains(&day) {
        return Err("day must be 1-31".into());
    }

    let (time_hms, millis) = if let Some((hms, ms_str)) = time_part.split_once('.') {
        let ms: u32 = ms_str.parse().map_err(|_| "invalid milliseconds")?;
        (hms, ms)
    } else {
        (time_part, 0)
    };

    let time_parts: Vec<&str> = time_hms.split(':').collect();
    if time_parts.len() < 2 || time_parts.len() > 3 {
        return Err("time must be HH:MM or HH:MM:SS".into());
    }
    let h: u32 = time_parts[0].parse().map_err(|_| "invalid hour")?;
    let m: u32 = time_parts[1].parse().map_err(|_| "invalid minute")?;
    let s: u32 = if time_parts.len() == 3 {
        time_parts[2].parse().map_err(|_| "invalid second")?
    } else {
        0
    };
    if h > 23 || m > 59 || s > 59 {
        return Err("invalid time".into());
    }

    let days = civil_days(year, month, day) as i64;
    let total_ms = days * 86_400_000
        + (h as i64) * 3_600_000
        + (m as i64) * 60_000
        + (s as i64) * 1000
        + millis as i64;
    Ok(total_ms)
}

fn parse_ipv4(input: &str) -> Result<Value, String> {
    let ip = parse_ipv4_to_u32(input)?;
    Ok(Value::Ipv4(ip))
}

fn parse_ipv6(input: &str) -> Result<Value, String> {
    let addr: std::net::Ipv6Addr = input
        .trim()
        .parse()
        .map_err(|_| "invalid IPv6 address".to_string())?;
    Ok(Value::Ipv6(addr.octets()))
}

fn parse_subnet(input: &str) -> Result<Value, String> {
    let parts: Vec<&str> = input.split('/').collect();
    if parts.len() != 2 {
        return Err("subnet must be IP/MASK or IP/PREFIX".into());
    }
    let ip = parse_ipv4_to_u32(parts[0])?;
    let mask = if parts[1].contains('.') {
        parse_ipv4_to_u32(parts[1])?
    } else {
        let prefix: u8 = parts[1].parse().map_err(|_| "invalid prefix")?;
        if prefix > 32 {
            return Err("prefix must be 0-32".into());
        }
        if prefix == 0 {
            0u32
        } else {
            !0u32 << (32 - prefix)
        }
    };
    Ok(Value::Subnet(ip, mask))
}

fn parse_port(input: &str) -> Result<Value, String> {
    let port: u16 = input
        .trim()
        .parse()
        .map_err(|_| "port must be 0-65535".to_string())?;
    Ok(Value::Port(port))
}

fn parse_latitude(input: &str) -> Result<Value, String> {
    let lat: f64 = input
        .trim()
        .parse()
        .map_err(|_| "invalid latitude".to_string())?;
    if !(-90.0..=90.0).contains(&lat) {
        return Err("latitude must be -90 to 90".into());
    }
    Ok(Value::Latitude((lat * 1_000_000.0) as i32))
}

fn parse_longitude(input: &str) -> Result<Value, String> {
    let lon: f64 = input
        .trim()
        .parse()
        .map_err(|_| "invalid longitude".to_string())?;
    if !(-180.0..=180.0).contains(&lon) {
        return Err("longitude must be -180 to 180".into());
    }
    Ok(Value::Longitude((lon * 1_000_000.0) as i32))
}

fn parse_geopoint(input: &str) -> Result<Value, String> {
    let parts: Vec<&str> = input.split(',').collect();
    if parts.len() != 2 {
        return Err("geopoint must be 'lat,lon'".into());
    }
    let lat: f64 = parts[0]
        .trim()
        .parse()
        .map_err(|_| "invalid latitude".to_string())?;
    let lon: f64 = parts[1]
        .trim()
        .parse()
        .map_err(|_| "invalid longitude".to_string())?;
    if !(-90.0..=90.0).contains(&lat) {
        return Err("latitude must be -90 to 90".into());
    }
    if !(-180.0..=180.0).contains(&lon) {
        return Err("longitude must be -180 to 180".into());
    }
    Ok(Value::GeoPoint(
        (lat * 1_000_000.0) as i32,
        (lon * 1_000_000.0) as i32,
    ))
}

fn parse_country2(input: &str) -> Result<Value, String> {
    let upper = input.trim().to_uppercase();
    if upper.len() != 2 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err("country code must be 2 uppercase letters (ISO 3166-1 alpha-2)".into());
    }
    let bytes = upper.as_bytes();
    Ok(Value::Country2([bytes[0], bytes[1]]))
}

fn parse_country3(input: &str) -> Result<Value, String> {
    let upper = input.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err("country code must be 3 uppercase letters (ISO 3166-1 alpha-3)".into());
    }
    let bytes = upper.as_bytes();
    Ok(Value::Country3([bytes[0], bytes[1], bytes[2]]))
}

fn parse_lang2(input: &str) -> Result<Value, String> {
    let lower = input.trim().to_lowercase();
    if lower.len() != 2 || !lower.chars().all(|c| c.is_ascii_lowercase()) {
        return Err("language code must be 2 lowercase letters (ISO 639-1)".into());
    }
    let bytes = lower.as_bytes();
    Ok(Value::Lang2([bytes[0], bytes[1]]))
}

fn parse_lang5(input: &str) -> Result<Value, String> {
    let trimmed = input.trim();
    if trimmed.len() != 5 {
        return Err("language tag must be 5 chars (e.g., pt-BR)".into());
    }
    let bytes = trimmed.as_bytes();
    if bytes[2] != b'-' {
        return Err("language tag format: xx-XX".into());
    }
    if !bytes[0].is_ascii_lowercase() || !bytes[1].is_ascii_lowercase() {
        return Err("language subtag must be lowercase".into());
    }
    if !bytes[3].is_ascii_uppercase() || !bytes[4].is_ascii_uppercase() {
        return Err("region subtag must be uppercase".into());
    }
    Ok(Value::Lang5([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4],
    ]))
}

fn parse_currency(input: &str) -> Result<Value, String> {
    let upper = input.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err("currency must be 3 uppercase letters (ISO 4217)".into());
    }
    let bytes = upper.as_bytes();
    Ok(Value::Currency([bytes[0], bytes[1], bytes[2]]))
}

fn parse_color_alpha(input: &str) -> Result<Value, String> {
    let hex = input.trim().trim_start_matches('#');
    if hex.len() != 8 {
        return Err("color+alpha must be 8 hex digits (#RRGGBBAA)".into());
    }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| "invalid red")?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| "invalid green")?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| "invalid blue")?;
    let a = u8::from_str_radix(&hex[6..8], 16).map_err(|_| "invalid alpha")?;
    Ok(Value::ColorAlpha([r, g, b, a]))
}

fn parse_key_ref(input: &str) -> Result<Value, String> {
    let parts: Vec<&str> = input.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err("key ref must be 'collection:key'".into());
    }
    Ok(Value::KeyRef(parts[0].to_string(), parts[1].to_string()))
}

fn parse_doc_ref(input: &str) -> Result<Value, String> {
    let parts: Vec<&str> = input.splitn(2, '#').collect();
    if parts.len() != 2 {
        return Err("doc ref must be 'collection#id'".into());
    }
    let id: u64 = parts[1].parse().map_err(|_| "invalid doc id")?;
    Ok(Value::DocRef(parts[0].to_string(), id))
}

fn parse_ipv4_to_u32(input: &str) -> Result<u32, String> {
    let parts: Vec<u8> = input
        .trim()
        .split('.')
        .map(|s| s.parse::<u8>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "invalid IPv4".to_string())?;
    if parts.len() != 4 {
        return Err("IPv4 must have 4 octets".into());
    }
    Ok(((parts[0] as u32) << 24)
        | ((parts[1] as u32) << 16)
        | ((parts[2] as u32) << 8)
        | (parts[3] as u32))
}

/// Days since Unix epoch from civil date (Howard Hinnant's algorithm)
fn civil_days(year: i32, month: u32, day: u32) -> i32 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m = month;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe as i64 - 719468) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Color ---

    #[test]
    fn test_coerce_color_valid_hash() {
        let val = coerce("#FF5733", DataType::Color, None).unwrap();
        assert_eq!(val, Value::Color([0xFF, 0x57, 0x33]));
    }

    #[test]
    fn test_coerce_color_valid_no_hash() {
        let val = coerce("00AABB", DataType::Color, None).unwrap();
        assert_eq!(val, Value::Color([0x00, 0xAA, 0xBB]));
    }

    #[test]
    fn test_coerce_color_lowercase() {
        let val = coerce("#ff5733", DataType::Color, None).unwrap();
        assert_eq!(val, Value::Color([0xFF, 0x57, 0x33]));
    }

    #[test]
    fn test_coerce_color_invalid_length() {
        assert!(coerce("#FFF", DataType::Color, None).is_err());
    }

    #[test]
    fn test_coerce_color_invalid_hex() {
        assert!(coerce("#GGHHII", DataType::Color, None).is_err());
    }

    // --- Email ---

    #[test]
    fn test_coerce_email_valid() {
        let val = coerce("User@Example.COM", DataType::Email, None).unwrap();
        assert_eq!(val, Value::Email("user@example.com".to_string()));
    }

    #[test]
    fn test_coerce_email_valid_with_plus() {
        let val = coerce("user+tag@example.com", DataType::Email, None).unwrap();
        assert_eq!(val, Value::Email("user+tag@example.com".to_string()));
    }

    #[test]
    fn test_coerce_email_missing_at() {
        assert!(coerce("notanemail.com", DataType::Email, None).is_err());
    }

    #[test]
    fn test_coerce_email_missing_dot_in_domain() {
        assert!(coerce("user@localhost", DataType::Email, None).is_err());
    }

    #[test]
    fn test_coerce_email_empty_local() {
        assert!(coerce("@example.com", DataType::Email, None).is_err());
    }

    #[test]
    fn test_coerce_email_double_at() {
        assert!(coerce("user@@example.com", DataType::Email, None).is_err());
    }

    // --- URL ---

    #[test]
    fn test_coerce_url_valid_https() {
        let val = coerce("https://example.com/path", DataType::Url, None).unwrap();
        assert_eq!(val, Value::Url("https://example.com/path".to_string()));
    }

    #[test]
    fn test_coerce_url_valid_http() {
        let val = coerce("http://example.com", DataType::Url, None).unwrap();
        assert_eq!(val, Value::Url("http://example.com".to_string()));
    }

    #[test]
    fn test_coerce_url_valid_ftp() {
        let val = coerce("ftp://files.example.com/data", DataType::Url, None).unwrap();
        assert_eq!(val, Value::Url("ftp://files.example.com/data".to_string()));
    }

    #[test]
    fn test_coerce_url_invalid_no_scheme() {
        assert!(coerce("example.com", DataType::Url, None).is_err());
    }

    #[test]
    fn test_coerce_url_invalid_scheme() {
        assert!(coerce("ssh://example.com", DataType::Url, None).is_err());
    }

    // --- Phone ---

    #[test]
    fn test_coerce_phone_valid_with_plus() {
        let val = coerce("+55 11 99988-7766", DataType::Phone, None).unwrap();
        assert_eq!(val, Value::Phone(5511999887766));
    }

    #[test]
    fn test_coerce_phone_valid_digits_only() {
        let val = coerce("1234567890", DataType::Phone, None).unwrap();
        assert_eq!(val, Value::Phone(1234567890));
    }

    #[test]
    fn test_coerce_phone_valid_parens() {
        let val = coerce("(11) 99999-0000", DataType::Phone, None).unwrap();
        assert_eq!(val, Value::Phone(11999990000));
    }

    #[test]
    fn test_coerce_phone_too_short() {
        assert!(coerce("123", DataType::Phone, None).is_err());
    }

    #[test]
    fn test_coerce_phone_too_long() {
        assert!(coerce("1234567890123456", DataType::Phone, None).is_err());
    }

    // --- Semver ---

    #[test]
    fn test_coerce_semver_valid() {
        let val = coerce("1.23.456", DataType::Semver, None).unwrap();
        assert_eq!(val, Value::Semver(1_023_456));
    }

    #[test]
    fn test_coerce_semver_zero() {
        let val = coerce("0.0.0", DataType::Semver, None).unwrap();
        assert_eq!(val, Value::Semver(0));
    }

    #[test]
    fn test_coerce_semver_max() {
        let val = coerce("999.999.999", DataType::Semver, None).unwrap();
        assert_eq!(val, Value::Semver(999_999_999));
    }

    #[test]
    fn test_coerce_semver_invalid_format() {
        assert!(coerce("1.2", DataType::Semver, None).is_err());
    }

    #[test]
    fn test_coerce_semver_overflow() {
        assert!(coerce("1000.0.0", DataType::Semver, None).is_err());
    }

    #[test]
    fn test_coerce_semver_non_numeric() {
        assert!(coerce("1.2.beta", DataType::Semver, None).is_err());
    }

    // --- CIDR ---

    #[test]
    fn test_coerce_cidr_valid() {
        let val = coerce("10.0.0.0/8", DataType::Cidr, None).unwrap();
        let expected_ip = (10u32 << 24) | 0;
        assert_eq!(val, Value::Cidr(expected_ip, 8));
    }

    #[test]
    fn test_coerce_cidr_host_route() {
        let val = coerce("192.168.1.1/32", DataType::Cidr, None).unwrap();
        let expected_ip = (192u32 << 24) | (168u32 << 16) | (1u32 << 8) | 1;
        assert_eq!(val, Value::Cidr(expected_ip, 32));
    }

    #[test]
    fn test_coerce_cidr_default_route() {
        let val = coerce("0.0.0.0/0", DataType::Cidr, None).unwrap();
        assert_eq!(val, Value::Cidr(0, 0));
    }

    #[test]
    fn test_coerce_cidr_invalid_prefix() {
        assert!(coerce("10.0.0.0/33", DataType::Cidr, None).is_err());
    }

    #[test]
    fn test_coerce_cidr_no_prefix() {
        assert!(coerce("10.0.0.0", DataType::Cidr, None).is_err());
    }

    #[test]
    fn test_coerce_cidr_bad_ip() {
        assert!(coerce("999.0.0.0/8", DataType::Cidr, None).is_err());
    }

    // --- Date ---

    #[test]
    fn test_coerce_date_epoch() {
        let val = coerce("1970-01-01", DataType::Date, None).unwrap();
        assert_eq!(val, Value::Date(0));
    }

    #[test]
    fn test_coerce_date_valid() {
        let val = coerce("2024-06-15", DataType::Date, None).unwrap();
        // 2024-06-15 is 19889 days since epoch
        if let Value::Date(days) = val {
            assert!(days > 19000 && days < 20000, "days={}", days);
        } else {
            panic!("expected Date");
        }
    }

    #[test]
    fn test_coerce_date_invalid_month() {
        assert!(coerce("2024-13-01", DataType::Date, None).is_err());
    }

    #[test]
    fn test_coerce_date_invalid_day() {
        assert!(coerce("2024-01-00", DataType::Date, None).is_err());
    }

    #[test]
    fn test_coerce_date_invalid_format() {
        assert!(coerce("01/15/2024", DataType::Date, None).is_err());
    }

    // --- Time ---

    #[test]
    fn test_coerce_time_hms() {
        let val = coerce("14:30:00", DataType::Time, None).unwrap();
        assert_eq!(val, Value::Time(52_200_000));
    }

    #[test]
    fn test_coerce_time_hm() {
        let val = coerce("08:15", DataType::Time, None).unwrap();
        assert_eq!(val, Value::Time((8 * 3600 + 15 * 60) * 1000));
    }

    #[test]
    fn test_coerce_time_midnight() {
        let val = coerce("00:00:00", DataType::Time, None).unwrap();
        assert_eq!(val, Value::Time(0));
    }

    #[test]
    fn test_coerce_time_end_of_day() {
        let val = coerce("23:59:59", DataType::Time, None).unwrap();
        assert_eq!(val, Value::Time((23 * 3600 + 59 * 60 + 59) * 1000));
    }

    #[test]
    fn test_coerce_time_invalid_hour() {
        assert!(coerce("25:00:00", DataType::Time, None).is_err());
    }

    #[test]
    fn test_coerce_time_invalid_minute() {
        assert!(coerce("12:60:00", DataType::Time, None).is_err());
    }

    #[test]
    fn test_coerce_time_invalid_format() {
        assert!(coerce("noon", DataType::Time, None).is_err());
    }

    // --- Decimal ---

    #[test]
    fn test_coerce_decimal_valid() {
        let val = coerce("123.4567", DataType::Decimal, None).unwrap();
        assert_eq!(val, Value::Decimal(1_234_567));
    }

    #[test]
    fn test_coerce_decimal_integer_input() {
        let val = coerce("42", DataType::Decimal, None).unwrap();
        assert_eq!(val, Value::Decimal(420_000));
    }

    #[test]
    fn test_coerce_decimal_negative() {
        let val = coerce("-9.99", DataType::Decimal, None).unwrap();
        assert_eq!(val, Value::Decimal(-99_900));
    }

    #[test]
    fn test_coerce_decimal_zero() {
        let val = coerce("0.0", DataType::Decimal, None).unwrap();
        assert_eq!(val, Value::Decimal(0));
    }

    #[test]
    fn test_coerce_decimal_invalid() {
        assert!(coerce("not_a_number", DataType::Decimal, None).is_err());
    }

    // --- Enum ---

    #[test]
    fn test_coerce_enum_valid() {
        let variants = vec![
            "Active".to_string(),
            "Inactive".to_string(),
            "Pending".to_string(),
        ];
        let val = coerce("active", DataType::Enum, Some(&variants)).unwrap();
        assert_eq!(val, Value::EnumValue(0));
    }

    #[test]
    fn test_coerce_enum_case_insensitive() {
        let variants = vec!["Red".to_string(), "Green".to_string(), "Blue".to_string()];
        let val = coerce("GREEN", DataType::Enum, Some(&variants)).unwrap();
        assert_eq!(val, Value::EnumValue(1));
    }

    #[test]
    fn test_coerce_enum_last_variant() {
        let variants = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let val = coerce("C", DataType::Enum, Some(&variants)).unwrap();
        assert_eq!(val, Value::EnumValue(2));
    }

    #[test]
    fn test_coerce_enum_invalid_variant() {
        let variants = vec!["Active".to_string(), "Inactive".to_string()];
        let err = coerce("Unknown", DataType::Enum, Some(&variants)).unwrap_err();
        assert!(err.contains("not a valid variant"));
        assert!(err.contains("Active"));
    }

    #[test]
    fn test_coerce_enum_empty_variants() {
        assert!(coerce("anything", DataType::Enum, Some(&[])).is_err());
    }

    // --- Boolean ---

    #[test]
    fn test_coerce_boolean_true_variants() {
        for input in &["true", "1", "yes", "on", "TRUE", "Yes", "ON"] {
            let val = coerce(input, DataType::Boolean, None).unwrap();
            assert_eq!(val, Value::Boolean(true), "failed for input: {}", input);
        }
    }

    #[test]
    fn test_coerce_boolean_false_variants() {
        for input in &["false", "0", "no", "off", "FALSE", "No", "OFF"] {
            let val = coerce(input, DataType::Boolean, None).unwrap();
            assert_eq!(val, Value::Boolean(false), "failed for input: {}", input);
        }
    }

    #[test]
    fn test_coerce_boolean_invalid() {
        assert!(coerce("maybe", DataType::Boolean, None).is_err());
    }

    // --- Numeric passthrough ---

    #[test]
    fn test_coerce_integer() {
        let val = coerce("-42", DataType::Integer, None).unwrap();
        assert_eq!(val, Value::Integer(-42));
    }

    #[test]
    fn test_coerce_unsigned_integer() {
        let val = coerce("18446744073709551615", DataType::UnsignedInteger, None).unwrap();
        assert_eq!(val, Value::UnsignedInteger(u64::MAX));
    }

    #[test]
    fn test_coerce_float() {
        let val = coerce("3.14159", DataType::Float, None).unwrap();
        assert_eq!(val, Value::Float(3.14159));
    }

    #[test]
    fn test_coerce_text() {
        let val = coerce("anything goes", DataType::Text, None).unwrap();
        assert_eq!(val, Value::Text("anything goes".to_string()));
    }

    // --- Roundtrip: coerce then serialize then deserialize ---

    #[test]
    fn test_coerce_roundtrip_color() {
        let val = coerce("#AABBCC", DataType::Color, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "#AABBCC");
    }

    #[test]
    fn test_coerce_roundtrip_email() {
        let val = coerce("Admin@Example.COM", DataType::Email, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "admin@example.com");
    }

    #[test]
    fn test_coerce_roundtrip_semver() {
        let val = coerce("2.10.3", DataType::Semver, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "2.10.3");
    }

    #[test]
    fn test_coerce_roundtrip_cidr() {
        let val = coerce("192.168.1.0/24", DataType::Cidr, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "192.168.1.0/24");
    }

    #[test]
    fn test_coerce_roundtrip_date() {
        let val = coerce("1970-01-01", DataType::Date, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "1970-01-01");
    }

    #[test]
    fn test_coerce_roundtrip_time() {
        let val = coerce("14:30:00", DataType::Time, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "14:30:00");
    }

    #[test]
    fn test_coerce_roundtrip_decimal() {
        let val = coerce("99.99", DataType::Decimal, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
    }

    #[test]
    fn test_civil_days_known_dates() {
        // 1970-01-01 should be day 0
        assert_eq!(civil_days(1970, 1, 1), 0);
        // 2000-01-01 should be day 10957
        assert_eq!(civil_days(2000, 1, 1), 10957);
        // 1969-12-31 should be day -1
        assert_eq!(civil_days(1969, 12, 31), -1);
    }

    // --- TimestampMs ---

    #[test]
    fn test_coerce_timestamp_ms_epoch() {
        let val = coerce("1710510600123", DataType::TimestampMs, None).unwrap();
        assert_eq!(val, Value::TimestampMs(1710510600123));
    }

    #[test]
    fn test_coerce_timestamp_ms_iso() {
        let val = coerce("2024-03-15T14:30:00.123Z", DataType::TimestampMs, None).unwrap();
        if let Value::TimestampMs(ms) = val {
            assert!(ms > 0, "expected positive timestamp, got {}", ms);
        } else {
            panic!("expected TimestampMs");
        }
    }

    #[test]
    fn test_coerce_timestamp_ms_invalid() {
        assert!(coerce("not-a-timestamp", DataType::TimestampMs, None).is_err());
    }

    // --- IPv4 ---

    #[test]
    fn test_coerce_ipv4_valid() {
        let val = coerce("192.168.1.1", DataType::Ipv4, None).unwrap();
        let expected = (192u32 << 24) | (168 << 16) | (1 << 8) | 1;
        assert_eq!(val, Value::Ipv4(expected));
    }

    #[test]
    fn test_coerce_ipv4_invalid() {
        assert!(coerce("999.0.0.1", DataType::Ipv4, None).is_err());
    }

    #[test]
    fn test_coerce_ipv4_too_few_octets() {
        assert!(coerce("192.168.1", DataType::Ipv4, None).is_err());
    }

    // --- IPv6 ---

    #[test]
    fn test_coerce_ipv6_valid() {
        let val = coerce("::1", DataType::Ipv6, None).unwrap();
        let mut expected = [0u8; 16];
        expected[15] = 1;
        assert_eq!(val, Value::Ipv6(expected));
    }

    #[test]
    fn test_coerce_ipv6_full() {
        let val = coerce(
            "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
            DataType::Ipv6,
            None,
        )
        .unwrap();
        if let Value::Ipv6(bytes) = val {
            assert_eq!(bytes[0], 0x20);
            assert_eq!(bytes[1], 0x01);
        } else {
            panic!("expected Ipv6");
        }
    }

    #[test]
    fn test_coerce_ipv6_invalid() {
        assert!(coerce("not-an-ipv6", DataType::Ipv6, None).is_err());
    }

    // --- Subnet ---

    #[test]
    fn test_coerce_subnet_cidr() {
        let val = coerce("10.0.0.0/16", DataType::Subnet, None).unwrap();
        let expected_ip = 10u32 << 24;
        let expected_mask = !0u32 << 16;
        assert_eq!(val, Value::Subnet(expected_ip, expected_mask));
    }

    #[test]
    fn test_coerce_subnet_dotted_mask() {
        let val = coerce("10.0.0.0/255.255.0.0", DataType::Subnet, None).unwrap();
        let expected_ip = 10u32 << 24;
        let expected_mask = (255u32 << 24) | (255 << 16);
        assert_eq!(val, Value::Subnet(expected_ip, expected_mask));
    }

    #[test]
    fn test_coerce_subnet_invalid() {
        assert!(coerce("10.0.0.0", DataType::Subnet, None).is_err());
    }

    // --- Port ---

    #[test]
    fn test_coerce_port_valid() {
        let val = coerce("8080", DataType::Port, None).unwrap();
        assert_eq!(val, Value::Port(8080));
    }

    #[test]
    fn test_coerce_port_max() {
        let val = coerce("65535", DataType::Port, None).unwrap();
        assert_eq!(val, Value::Port(65535));
    }

    #[test]
    fn test_coerce_port_invalid() {
        assert!(coerce("70000", DataType::Port, None).is_err());
    }

    // --- Latitude ---

    #[test]
    fn test_coerce_latitude_valid() {
        let val = coerce("-23.550520", DataType::Latitude, None).unwrap();
        assert_eq!(val, Value::Latitude(-23550520));
    }

    #[test]
    fn test_coerce_latitude_out_of_range() {
        assert!(coerce("91.0", DataType::Latitude, None).is_err());
    }

    // --- Longitude ---

    #[test]
    fn test_coerce_longitude_valid() {
        let val = coerce("-46.633308", DataType::Longitude, None).unwrap();
        assert_eq!(val, Value::Longitude(-46633308));
    }

    #[test]
    fn test_coerce_longitude_out_of_range() {
        assert!(coerce("181.0", DataType::Longitude, None).is_err());
    }

    // --- GeoPoint ---

    #[test]
    fn test_coerce_geopoint_valid() {
        let val = coerce("-23.550520,-46.633308", DataType::GeoPoint, None).unwrap();
        assert_eq!(val, Value::GeoPoint(-23550520, -46633308));
    }

    #[test]
    fn test_coerce_geopoint_with_spaces() {
        let val = coerce("-23.550520, -46.633308", DataType::GeoPoint, None).unwrap();
        assert_eq!(val, Value::GeoPoint(-23550520, -46633308));
    }

    #[test]
    fn test_coerce_geopoint_invalid() {
        assert!(coerce("not,valid", DataType::GeoPoint, None).is_err());
    }

    #[test]
    fn test_coerce_geopoint_out_of_range() {
        assert!(coerce("91.0,0.0", DataType::GeoPoint, None).is_err());
    }

    // --- Country2 ---

    #[test]
    fn test_coerce_country2_valid() {
        let val = coerce("br", DataType::Country2, None).unwrap();
        assert_eq!(val, Value::Country2([b'B', b'R']));
    }

    #[test]
    fn test_coerce_country2_uppercase() {
        let val = coerce("US", DataType::Country2, None).unwrap();
        assert_eq!(val, Value::Country2([b'U', b'S']));
    }

    #[test]
    fn test_coerce_country2_invalid_length() {
        assert!(coerce("BRA", DataType::Country2, None).is_err());
    }

    #[test]
    fn test_coerce_country2_invalid_chars() {
        assert!(coerce("12", DataType::Country2, None).is_err());
    }

    // --- Country3 ---

    #[test]
    fn test_coerce_country3_valid() {
        let val = coerce("bra", DataType::Country3, None).unwrap();
        assert_eq!(val, Value::Country3([b'B', b'R', b'A']));
    }

    #[test]
    fn test_coerce_country3_invalid() {
        assert!(coerce("BR", DataType::Country3, None).is_err());
    }

    // --- Lang2 ---

    #[test]
    fn test_coerce_lang2_valid() {
        let val = coerce("pt", DataType::Lang2, None).unwrap();
        assert_eq!(val, Value::Lang2([b'p', b't']));
    }

    #[test]
    fn test_coerce_lang2_uppercase_normalized() {
        let val = coerce("PT", DataType::Lang2, None).unwrap();
        assert_eq!(val, Value::Lang2([b'p', b't']));
    }

    #[test]
    fn test_coerce_lang2_invalid() {
        assert!(coerce("por", DataType::Lang2, None).is_err());
    }

    // --- Lang5 ---

    #[test]
    fn test_coerce_lang5_valid() {
        let val = coerce("pt-BR", DataType::Lang5, None).unwrap();
        assert_eq!(val, Value::Lang5([b'p', b't', b'-', b'B', b'R']));
    }

    #[test]
    fn test_coerce_lang5_en_us() {
        let val = coerce("en-US", DataType::Lang5, None).unwrap();
        assert_eq!(val, Value::Lang5([b'e', b'n', b'-', b'U', b'S']));
    }

    #[test]
    fn test_coerce_lang5_invalid_format() {
        assert!(coerce("pt_BR", DataType::Lang5, None).is_err());
    }

    #[test]
    fn test_coerce_lang5_wrong_case() {
        assert!(coerce("PT-br", DataType::Lang5, None).is_err());
    }

    // --- Currency ---

    #[test]
    fn test_coerce_currency_valid() {
        let val = coerce("brl", DataType::Currency, None).unwrap();
        assert_eq!(val, Value::Currency([b'B', b'R', b'L']));
    }

    #[test]
    fn test_coerce_currency_usd() {
        let val = coerce("USD", DataType::Currency, None).unwrap();
        assert_eq!(val, Value::Currency([b'U', b'S', b'D']));
    }

    #[test]
    fn test_coerce_currency_invalid() {
        assert!(coerce("US", DataType::Currency, None).is_err());
    }

    // --- ColorAlpha ---

    #[test]
    fn test_coerce_color_alpha_valid() {
        let val = coerce("#FF573380", DataType::ColorAlpha, None).unwrap();
        assert_eq!(val, Value::ColorAlpha([0xFF, 0x57, 0x33, 0x80]));
    }

    #[test]
    fn test_coerce_color_alpha_no_hash() {
        let val = coerce("AABBCCDD", DataType::ColorAlpha, None).unwrap();
        assert_eq!(val, Value::ColorAlpha([0xAA, 0xBB, 0xCC, 0xDD]));
    }

    #[test]
    fn test_coerce_color_alpha_invalid_length() {
        assert!(coerce("#FF5733", DataType::ColorAlpha, None).is_err());
    }

    // --- BigInt ---

    #[test]
    fn test_coerce_bigint_valid() {
        let val = coerce("9223372036854775807", DataType::BigInt, None).unwrap();
        assert_eq!(val, Value::BigInt(i64::MAX));
    }

    #[test]
    fn test_coerce_bigint_negative() {
        let val = coerce("-9223372036854775808", DataType::BigInt, None).unwrap();
        assert_eq!(val, Value::BigInt(i64::MIN));
    }

    #[test]
    fn test_coerce_bigint_invalid() {
        assert!(coerce("not_a_number", DataType::BigInt, None).is_err());
    }

    // --- Roundtrip tests for new types ---

    #[test]
    fn test_coerce_roundtrip_ipv4() {
        let val = coerce("192.168.1.1", DataType::Ipv4, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "192.168.1.1");
    }

    #[test]
    fn test_coerce_roundtrip_ipv6() {
        let val = coerce("::1", DataType::Ipv6, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "::1");
    }

    #[test]
    fn test_coerce_roundtrip_subnet() {
        let val = coerce("10.0.0.0/16", DataType::Subnet, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "10.0.0.0/16");
    }

    #[test]
    fn test_coerce_roundtrip_port() {
        let val = coerce("443", DataType::Port, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "443");
    }

    #[test]
    fn test_coerce_roundtrip_geopoint() {
        let val = coerce("-23.550520,-46.633308", DataType::GeoPoint, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
    }

    #[test]
    fn test_coerce_roundtrip_country2() {
        let val = coerce("BR", DataType::Country2, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "BR");
    }

    #[test]
    fn test_coerce_roundtrip_lang5() {
        let val = coerce("pt-BR", DataType::Lang5, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "pt-BR");
    }

    #[test]
    fn test_coerce_roundtrip_currency() {
        let val = coerce("USD", DataType::Currency, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "USD");
    }

    #[test]
    fn test_coerce_roundtrip_color_alpha() {
        let val = coerce("#FF573380", DataType::ColorAlpha, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "#FF573380");
    }

    #[test]
    fn test_coerce_roundtrip_bigint() {
        let val = coerce("123456789012345", DataType::BigInt, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "123456789012345");
    }

    // --- KeyRef ---

    #[test]
    fn test_coerce_key_ref_valid() {
        let val = coerce("users:alice", DataType::KeyRef, None).unwrap();
        assert_eq!(val, Value::KeyRef("users".to_string(), "alice".to_string()));
    }

    #[test]
    fn test_coerce_key_ref_with_colon_in_key() {
        let val = coerce("cache:prefix:item", DataType::KeyRef, None).unwrap();
        assert_eq!(
            val,
            Value::KeyRef("cache".to_string(), "prefix:item".to_string())
        );
    }

    #[test]
    fn test_coerce_key_ref_invalid_no_colon() {
        assert!(coerce("usersalice", DataType::KeyRef, None).is_err());
    }

    // --- DocRef ---

    #[test]
    fn test_coerce_doc_ref_valid() {
        let val = coerce("orders#42", DataType::DocRef, None).unwrap();
        assert_eq!(val, Value::DocRef("orders".to_string(), 42));
    }

    #[test]
    fn test_coerce_doc_ref_invalid_no_hash() {
        assert!(coerce("orders42", DataType::DocRef, None).is_err());
    }

    #[test]
    fn test_coerce_doc_ref_invalid_id() {
        assert!(coerce("orders#abc", DataType::DocRef, None).is_err());
    }

    // --- TableRef ---

    #[test]
    fn test_coerce_table_ref_valid() {
        let val = coerce("my_table", DataType::TableRef, None).unwrap();
        assert_eq!(val, Value::TableRef("my_table".to_string()));
    }

    // --- PageRef ---

    #[test]
    fn test_coerce_page_ref_valid() {
        let val = coerce("12345", DataType::PageRef, None).unwrap();
        assert_eq!(val, Value::PageRef(12345));
    }

    #[test]
    fn test_coerce_page_ref_invalid() {
        assert!(coerce("not_a_number", DataType::PageRef, None).is_err());
    }

    // --- Roundtrip tests ---

    #[test]
    fn test_coerce_roundtrip_key_ref() {
        let val = coerce("sessions:tok123", DataType::KeyRef, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "sessions:tok123");
    }

    #[test]
    fn test_coerce_roundtrip_doc_ref() {
        let val = coerce("products#999", DataType::DocRef, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "products#999");
    }

    #[test]
    fn test_coerce_roundtrip_table_ref() {
        let val = coerce("inventory", DataType::TableRef, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "inventory");
    }

    #[test]
    fn test_coerce_roundtrip_page_ref() {
        let val = coerce("42", DataType::PageRef, None).unwrap();
        let bytes = val.to_bytes();
        let (recovered, _) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(val, recovered);
        assert_eq!(recovered.display_string(), "page:42");
    }
}
