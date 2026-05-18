//! Env-driven `BackupBootstrap` (issue #517).
//!
//! Parses the canonical `REDDB_BACKUP_*` env contract into a
//! [`BackupConfig`]. Pure function — env access is injected as a
//! closure so unit tests need no real process env. The `red` binary
//! calls [`from_env`] at boot; the returned `Option<BackupConfig>`
//! drives `Options::with_remote_backend` + `with_atomic_remote_backend`
//! wiring and the archiver / checkpointer task intervals.
//!
//! Contract:
//!   * `REDDB_BACKUP_S3_ENDPOINT`   (required)
//!   * `REDDB_BACKUP_S3_BUCKET`     (required)
//!   * `REDDB_BACKUP_S3_PREFIX`     (required)
//!   * `REDDB_BACKUP_S3_ACCESS_KEY_ID`     (required)
//!   * `REDDB_BACKUP_S3_SECRET_ACCESS_KEY` (required)
//!   * `REDDB_BACKUP_S3_REGION`     (default `auto`)
//!   * `REDDB_BACKUP_CHECKPOINT_INTERVAL_SECS` (default 3600, must be > 0)
//!   * `REDDB_BACKUP_WAL_FLUSH_INTERVAL_SECS`  (default 30,   must be > 0)
//!   * `REDDB_BACKUP_PAUSE_ON_LAG_SECS`        (default 0 = disabled; > 0 enables
//!     graceful read-only mode when WAL archive lag exceeds the threshold —
//!     issue #519)
//!
//! Resolution:
//!   * All required vars absent → `Ok(None)` (standalone; identical to
//!     today's behaviour).
//!   * All required vars present → `Ok(Some(BackupConfig))`.
//!   * Partial config (at least one required present, at least one
//!     missing) → `Err` naming the missing var.
//!   * Non-numeric / zero interval → `Err`.

/// Parsed configuration produced by [`from_env`]. Carries everything
/// the `red` binary needs to construct an `S3Backend` and the two
/// background tasks (archiver + checkpointer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupConfig {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub prefix: String,
    pub checkpoint_interval_secs: u64,
    pub wal_flush_interval_secs: u64,
    /// Issue #519 — when > 0, the engine monitors archive lag (`now -
    /// last_successful_archive_at`) and transitions to a graceful
    /// read-only mode when the lag exceeds this threshold. `0` keeps
    /// the legacy behaviour (writes always accepted while local volume
    /// has room, regardless of remote backend health).
    pub pause_on_lag_secs: u64,
}

const REQUIRED_VARS: &[&str] = &[
    "REDDB_BACKUP_S3_ENDPOINT",
    "REDDB_BACKUP_S3_BUCKET",
    "REDDB_BACKUP_S3_PREFIX",
    "REDDB_BACKUP_S3_ACCESS_KEY_ID",
    "REDDB_BACKUP_S3_SECRET_ACCESS_KEY",
];

const REGION_VAR: &str = "REDDB_BACKUP_S3_REGION";
const CHECKPOINT_VAR: &str = "REDDB_BACKUP_CHECKPOINT_INTERVAL_SECS";
const WAL_FLUSH_VAR: &str = "REDDB_BACKUP_WAL_FLUSH_INTERVAL_SECS";
const PAUSE_ON_LAG_VAR: &str = "REDDB_BACKUP_PAUSE_ON_LAG_SECS";

const DEFAULT_REGION: &str = "auto";
const DEFAULT_CHECKPOINT_SECS: u64 = 3600;
const DEFAULT_WAL_FLUSH_SECS: u64 = 30;
const DEFAULT_PAUSE_ON_LAG_SECS: u64 = 0;

/// Parse the `REDDB_BACKUP_*` env contract using the supplied
/// env-var lookup. See module docs for the contract.
pub fn from_env<F>(env: F) -> Result<Option<BackupConfig>, String>
where
    F: Fn(&str) -> Option<String>,
{
    let presence: Vec<(&str, Option<String>)> = REQUIRED_VARS
        .iter()
        .map(|name| (*name, env(name).filter(|v| !v.trim().is_empty())))
        .collect();

    let present_count = presence.iter().filter(|(_, v)| v.is_some()).count();

    if present_count == 0 {
        return Ok(None);
    }

    if present_count < REQUIRED_VARS.len() {
        let missing: Vec<&str> = presence
            .iter()
            .filter_map(|(n, v)| v.is_none().then_some(*n))
            .collect();
        return Err(format!(
            "partial REDDB_BACKUP_S3_* config; missing: {}",
            missing.join(", ")
        ));
    }

    let mut required = presence.into_iter().map(|(_, v)| v.unwrap());
    let endpoint = required.next().unwrap();
    let bucket = required.next().unwrap();
    let prefix = required.next().unwrap();
    let access_key_id = required.next().unwrap();
    let secret_access_key = required.next().unwrap();

    let region = env(REGION_VAR)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REGION.to_string());

    let checkpoint_interval_secs =
        parse_interval(&env, CHECKPOINT_VAR, DEFAULT_CHECKPOINT_SECS)?;
    let wal_flush_interval_secs = parse_interval(&env, WAL_FLUSH_VAR, DEFAULT_WAL_FLUSH_SECS)?;
    let pause_on_lag_secs = parse_pause_on_lag(&env, DEFAULT_PAUSE_ON_LAG_SECS)?;

    Ok(Some(BackupConfig {
        endpoint,
        bucket,
        region,
        access_key_id,
        secret_access_key,
        prefix,
        checkpoint_interval_secs,
        wal_flush_interval_secs,
        pause_on_lag_secs,
    }))
}

fn parse_pause_on_lag<F>(env: &F, default: u64) -> Result<u64, String>
where
    F: Fn(&str) -> Option<String>,
{
    let Some(raw) = env(PAUSE_ON_LAG_VAR).filter(|v| !v.trim().is_empty()) else {
        return Ok(default);
    };
    let trimmed = raw.trim();
    let parsed: i128 = trimmed.parse().map_err(|_| {
        format!("{PAUSE_ON_LAG_VAR} must be a non-negative integer; got {raw:?}")
    })?;
    if parsed < 0 {
        return Err(format!(
            "{PAUSE_ON_LAG_VAR} must be >= 0; got {parsed} (negative not allowed)"
        ));
    }
    let as_u64 = u64::try_from(parsed)
        .map_err(|_| format!("{PAUSE_ON_LAG_VAR} exceeds u64 range; got {parsed}"))?;
    Ok(as_u64)
}

fn parse_interval<F>(env: &F, name: &str, default: u64) -> Result<u64, String>
where
    F: Fn(&str) -> Option<String>,
{
    let Some(raw) = env(name).filter(|v| !v.trim().is_empty()) else {
        return Ok(default);
    };
    let trimmed = raw.trim();
    let parsed: i128 = trimmed
        .parse()
        .map_err(|_| format!("{name} must be a positive integer; got {raw:?}"))?;
    if parsed <= 0 {
        return Err(format!(
            "{name} must be > 0; got {parsed} (zero/negative not allowed)"
        ));
    }
    let as_u64 = u64::try_from(parsed)
        .map_err(|_| format!("{name} exceeds u64 range; got {parsed}"))?;
    Ok(as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup<'a>(
        map: &'a HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| map.get(k).map(|s| s.to_string())
    }

    #[test]
    fn none_present_yields_none() {
        let map: HashMap<&'static str, &'static str> = HashMap::new();
        let got = from_env(lookup(&map)).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn all_required_present_yields_config_with_defaults() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://s3.example.com"),
            ("REDDB_BACKUP_S3_BUCKET", "buck"),
            ("REDDB_BACKUP_S3_PREFIX", "clusters/dev/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
        ]
        .into_iter()
        .collect();
        let cfg = from_env(lookup(&map)).unwrap().expect("Some");
        assert_eq!(cfg.endpoint, "https://s3.example.com");
        assert_eq!(cfg.bucket, "buck");
        assert_eq!(cfg.prefix, "clusters/dev/");
        assert_eq!(cfg.access_key_id, "AK");
        assert_eq!(cfg.secret_access_key, "SK");
        assert_eq!(cfg.region, DEFAULT_REGION);
        assert_eq!(cfg.checkpoint_interval_secs, DEFAULT_CHECKPOINT_SECS);
        assert_eq!(cfg.wal_flush_interval_secs, DEFAULT_WAL_FLUSH_SECS);
        assert_eq!(cfg.pause_on_lag_secs, DEFAULT_PAUSE_ON_LAG_SECS);
    }

    #[test]
    fn pause_on_lag_is_parsed_when_present() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://x"),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
            ("REDDB_BACKUP_S3_PREFIX", "p/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
            ("REDDB_BACKUP_PAUSE_ON_LAG_SECS", "300"),
        ]
        .into_iter()
        .collect();
        let cfg = from_env(lookup(&map)).unwrap().expect("Some");
        assert_eq!(cfg.pause_on_lag_secs, 300);
    }

    #[test]
    fn pause_on_lag_zero_is_disabled() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://x"),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
            ("REDDB_BACKUP_S3_PREFIX", "p/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
            ("REDDB_BACKUP_PAUSE_ON_LAG_SECS", "0"),
        ]
        .into_iter()
        .collect();
        let cfg = from_env(lookup(&map)).unwrap().expect("Some");
        assert_eq!(cfg.pause_on_lag_secs, 0);
    }

    #[test]
    fn pause_on_lag_negative_is_error() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://x"),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
            ("REDDB_BACKUP_S3_PREFIX", "p/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
            ("REDDB_BACKUP_PAUSE_ON_LAG_SECS", "-1"),
        ]
        .into_iter()
        .collect();
        let err = from_env(lookup(&map)).unwrap_err();
        assert!(err.contains("REDDB_BACKUP_PAUSE_ON_LAG_SECS"), "{err}");
    }

    #[test]
    fn pause_on_lag_non_numeric_is_error() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://x"),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
            ("REDDB_BACKUP_S3_PREFIX", "p/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
            ("REDDB_BACKUP_PAUSE_ON_LAG_SECS", "soon"),
        ]
        .into_iter()
        .collect();
        let err = from_env(lookup(&map)).unwrap_err();
        assert!(err.contains("REDDB_BACKUP_PAUSE_ON_LAG_SECS"), "{err}");
        assert!(err.contains("non-negative"), "{err}");
    }

    #[test]
    fn all_required_present_with_explicit_overrides() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://s3.example.com"),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
            ("REDDB_BACKUP_S3_PREFIX", "p/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
            ("REDDB_BACKUP_S3_REGION", "us-east-1"),
            ("REDDB_BACKUP_CHECKPOINT_INTERVAL_SECS", "60"),
            ("REDDB_BACKUP_WAL_FLUSH_INTERVAL_SECS", "5"),
        ]
        .into_iter()
        .collect();
        let cfg = from_env(lookup(&map)).unwrap().expect("Some");
        assert_eq!(cfg.region, "us-east-1");
        assert_eq!(cfg.checkpoint_interval_secs, 60);
        assert_eq!(cfg.wal_flush_interval_secs, 5);
    }

    #[test]
    fn partial_config_names_missing_var() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://s3.example.com"),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
        ]
        .into_iter()
        .collect();
        let err = from_env(lookup(&map)).unwrap_err();
        assert!(err.contains("REDDB_BACKUP_S3_PREFIX"), "{err}");
        assert!(err.contains("REDDB_BACKUP_S3_ACCESS_KEY_ID"), "{err}");
        assert!(err.contains("REDDB_BACKUP_S3_SECRET_ACCESS_KEY"), "{err}");
    }

    #[test]
    fn whitespace_only_required_treated_as_missing() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "   "),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
            ("REDDB_BACKUP_S3_PREFIX", "p/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
        ]
        .into_iter()
        .collect();
        let err = from_env(lookup(&map)).unwrap_err();
        assert!(err.contains("REDDB_BACKUP_S3_ENDPOINT"), "{err}");
    }

    #[test]
    fn non_numeric_interval_is_error() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://x"),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
            ("REDDB_BACKUP_S3_PREFIX", "p/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
            ("REDDB_BACKUP_CHECKPOINT_INTERVAL_SECS", "abc"),
        ]
        .into_iter()
        .collect();
        let err = from_env(lookup(&map)).unwrap_err();
        assert!(err.contains("REDDB_BACKUP_CHECKPOINT_INTERVAL_SECS"), "{err}");
        assert!(err.contains("positive integer"), "{err}");
    }

    #[test]
    fn zero_interval_is_error() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://x"),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
            ("REDDB_BACKUP_S3_PREFIX", "p/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
            ("REDDB_BACKUP_WAL_FLUSH_INTERVAL_SECS", "0"),
        ]
        .into_iter()
        .collect();
        let err = from_env(lookup(&map)).unwrap_err();
        assert!(err.contains("REDDB_BACKUP_WAL_FLUSH_INTERVAL_SECS"), "{err}");
        assert!(err.contains("> 0"), "{err}");
    }

    #[test]
    fn negative_interval_is_error() {
        let map: HashMap<&'static str, &'static str> = [
            ("REDDB_BACKUP_S3_ENDPOINT", "https://x"),
            ("REDDB_BACKUP_S3_BUCKET", "b"),
            ("REDDB_BACKUP_S3_PREFIX", "p/"),
            ("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK"),
            ("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK"),
            ("REDDB_BACKUP_CHECKPOINT_INTERVAL_SECS", "-10"),
        ]
        .into_iter()
        .collect();
        let err = from_env(lookup(&map)).unwrap_err();
        assert!(err.contains("REDDB_BACKUP_CHECKPOINT_INTERVAL_SECS"), "{err}");
    }
}
