//! Audit log query / replay helpers.
//!
//! Backs the `GET /admin/audit` endpoint. Reads the active
//! `.audit.log` plus rotated siblings (`.audit.log.<ms>.zst`),
//! parses each line into [`AuditEvent`], and applies the request
//! filters in memory. The audit volume on a typical RedDB deploy is
//! orders of magnitude smaller than the data plane (admin actions,
//! auth events, lease transitions) so a linear scan over the rotated
//! tail is acceptable. If the volume ever justifies it, a real index
//! lives one refactor away — slot a sled / parquet sidecar in here
//! without touching the public surface.

use std::path::Path;

use crate::runtime::audit_log::{AuditEvent, Outcome};

/// Query filters. All fields are optional; an empty `Query` returns
/// the entire window up to `limit`.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    pub since_ms: Option<u128>,
    pub until_ms: Option<u128>,
    pub principal: Option<String>,
    pub tenant: Option<String>,
    /// Prefix match on `action` (e.g. `auth/`, `admin/`,
    /// `lease/acquire`). Empty string matches everything.
    pub action_prefix: Option<String>,
    pub outcome: Option<Outcome>,
    /// Hard cap on the number of returned events. Server should clamp
    /// this to a sensible maximum (e.g. 1000) before passing through.
    pub limit: usize,
}

impl AuditQuery {
    pub fn new() -> Self {
        Self {
            limit: 100,
            ..Default::default()
        }
    }

    fn matches(&self, ev: &AuditEvent) -> bool {
        if let Some(since) = self.since_ms {
            if ev.ts < since {
                return false;
            }
        }
        if let Some(until) = self.until_ms {
            if ev.ts > until {
                return false;
            }
        }
        if let Some(principal) = &self.principal {
            match &ev.principal {
                Some(p) if p == principal => {}
                _ => return false,
            }
        }
        if let Some(tenant) = &self.tenant {
            match &ev.tenant {
                Some(t) if t == tenant => {}
                _ => return false,
            }
        }
        if let Some(prefix) = &self.action_prefix {
            if !ev.action.starts_with(prefix) {
                return false;
            }
        }
        if let Some(outcome) = self.outcome {
            if ev.outcome != outcome {
                return false;
            }
        }
        true
    }
}

/// Run `query` against the audit log rooted at `active_path` (the
/// current `.audit.log`). Walks the active file plus every sibling
/// rotated archive (`.audit.log.<ms>.zst`), oldest-first by filename.
/// Returns the matching events in chronological order, capped at
/// `query.limit`.
pub fn run_query(active_path: &Path, query: &AuditQuery) -> Vec<AuditEvent> {
    let mut events: Vec<AuditEvent> = Vec::new();
    let parent = active_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = active_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(".audit.log");

    let mut rotated: Vec<(u128, std::path::PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(parent) {
        for entry in rd.flatten() {
            let name = entry.file_name();
            let Some(name_s) = name.to_str() else {
                continue;
            };
            if !name_s.starts_with(&format!("{stem}.")) {
                continue;
            }
            // Two recognized shapes: `<stem>.<ms>` (uncompressed
            // fallback) and `<stem>.<ms>.zst`.
            let after = &name_s[stem.len() + 1..];
            let ts_part = after.trim_end_matches(".zst");
            if let Ok(ts) = ts_part.parse::<u128>() {
                rotated.push((ts, entry.path()));
            }
        }
    }
    rotated.sort_by_key(|(ts, _)| *ts);

    for (_, path) in &rotated {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let plain = if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e == "zst")
            .unwrap_or(false)
        {
            match zstd::bulk::decompress(&bytes, 256 * 1024 * 1024) {
                Ok(p) => p,
                Err(_) => continue,
            }
        } else {
            bytes
        };
        ingest_buffer(&plain, query, &mut events);
    }

    if let Ok(active_bytes) = std::fs::read(active_path) {
        ingest_buffer(&active_bytes, query, &mut events);
    }

    if events.len() > query.limit {
        let take = query.limit;
        let drop = events.len() - take;
        events.drain(0..drop);
    }
    events
}

fn ingest_buffer(bytes: &[u8], query: &AuditQuery, out: &mut Vec<AuditEvent>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Some(ev) = AuditEvent::parse_line(line) else {
            continue;
        };
        if query.matches(&ev) {
            out.push(ev);
        }
    }
}

/// Render a list of events as a JSON array (returned by the HTTP
/// query handler). Stable field set so dashboards stay locked.
pub fn events_to_json_array(events: &[AuditEvent]) -> crate::json::Value {
    use crate::json::{Map, Value};
    let mut arr: Vec<Value> = Vec::with_capacity(events.len());
    for ev in events {
        let line = ev.to_json_line(None);
        if let Ok(v) = crate::json::from_str::<Value>(&line) {
            arr.push(v);
        }
    }
    let mut obj = Map::new();
    obj.insert("count".to_string(), Value::Number(events.len() as f64));
    obj.insert("events".to_string(), Value::Array(arr));
    Value::Object(obj)
}

// ---------------------------------------------------------------------------
// Param parsing
// ---------------------------------------------------------------------------

/// Parse RFC-3339 with second precision OR an integer ms epoch. The
/// query endpoint accepts either form per the spec; we keep the
/// parser tiny so we don't pull `chrono`.
pub fn parse_time_arg(raw: &str) -> Option<u128> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(ms) = trimmed.parse::<u128>() {
        return Some(ms);
    }
    parse_rfc3339_ms(trimmed)
}

/// Tiny RFC 3339 -> ms parser. Accepts `YYYY-MM-DDTHH:MM:SSZ` and
/// `YYYY-MM-DDTHH:MM:SS.mmmZ`. Rejects anything with a non-Z offset
/// — the audit log writes UTC and we want callers to pass UTC too.
pub fn parse_rfc3339_ms(s: &str) -> Option<u128> {
    let bytes = s.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    if !s.ends_with('Z') {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    if &bytes[4..5] != b"-" || &bytes[7..8] != b"-" || &bytes[10..11] != b"T" {
        return None;
    }
    let hour: u64 = s.get(11..13)?.parse().ok()?;
    let minute: u64 = s.get(14..16)?.parse().ok()?;
    let second: u64 = s.get(17..19)?.parse().ok()?;
    if &bytes[13..14] != b":" || &bytes[16..17] != b":" {
        return None;
    }
    let mut ms_extra: u64 = 0;
    if bytes.len() > 20 {
        // Either `.mmm` then `Z`, or some other suffix.
        if &bytes[19..20] == b"." {
            // Up to the `Z`.
            let dot_end = s.len() - 1; // skip Z
            let frac = s.get(20..dot_end)?;
            if frac.len() > 9 || frac.is_empty() {
                return None;
            }
            // pad / truncate to 3 digits for ms.
            let mut digits = String::with_capacity(3);
            for c in frac.chars().take(3) {
                if !c.is_ascii_digit() {
                    return None;
                }
                digits.push(c);
            }
            while digits.len() < 3 {
                digits.push('0');
            }
            ms_extra = digits.parse().ok()?;
        } else if &bytes[19..20] != b"Z" {
            return None;
        }
    }
    let days = days_from_civil(year, month, day);
    let secs =
        (days as i128) * 86_400 + (hour as i128) * 3600 + (minute as i128) * 60 + second as i128;
    let ms = secs * 1000 + ms_extra as i128;
    if ms < 0 {
        return None;
    }
    Some(ms as u128)
}

/// Howard Hinnant's days_from_civil algorithm. Mirrors the inverse
/// `civil_from_days` already in `audit_log.rs`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = m as u64;
    let d = d as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::audit_log::{AuditAuthSource, AuditEvent, AuditLogger, Outcome};
    use std::path::PathBuf;
    use std::time::Duration;

    fn temp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "reddb-audit-query-{}-{}-{}",
            tag,
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p.push("data.rdb");
        p
    }

    #[test]
    fn filters_by_principal_and_action_prefix() {
        let data = temp_path("filter");
        let logger = AuditLogger::for_data_path(&data);
        for who in ["alice", "bob", "alice", "carol"] {
            logger.record_event(
                AuditEvent::builder("auth/login.ok")
                    .principal(who)
                    .source(AuditAuthSource::Password)
                    .build(),
            );
        }
        logger.record_event(
            AuditEvent::builder("admin/shutdown")
                .principal("alice")
                .source(AuditAuthSource::Session)
                .outcome(Outcome::Success)
                .build(),
        );
        assert!(logger.wait_idle(Duration::from_secs(2)));

        let q = AuditQuery {
            principal: Some("alice".to_string()),
            action_prefix: Some("auth/".to_string()),
            limit: 100,
            ..Default::default()
        };
        let hits = run_query(logger.path(), &q);
        assert_eq!(hits.len(), 2, "expected two alice/auth lines");
        assert!(hits.iter().all(|e| e.principal.as_deref() == Some("alice")));
        assert!(hits.iter().all(|e| e.action.starts_with("auth/")));
    }

    #[test]
    fn parse_time_accepts_rfc3339_and_ms() {
        assert_eq!(
            parse_time_arg("2024-02-29T12:34:56.789Z"),
            Some(1_709_210_096_789)
        );
        assert_eq!(parse_time_arg("1709210096789"), Some(1_709_210_096_789));
        assert_eq!(parse_time_arg("not a time"), None);
    }

    #[test]
    fn limit_caps_oldest_off() {
        let data = temp_path("limit");
        let logger = AuditLogger::for_data_path(&data);
        for i in 0..10 {
            logger.record_event(AuditEvent::builder(format!("test/n/{i}")).build());
        }
        assert!(logger.wait_idle(Duration::from_secs(2)));
        let q = AuditQuery {
            limit: 3,
            ..Default::default()
        };
        let hits = run_query(logger.path(), &q);
        assert_eq!(hits.len(), 3);
        // Newest 3 are kept.
        assert_eq!(hits[0].action, "test/n/7");
        assert_eq!(hits[2].action, "test/n/9");
    }
}
