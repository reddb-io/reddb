//! Resolution of the three HTTP handler-pool knobs (issue #574 slice 5).
//!
//! The values are configurable through the standard precedence chain
//! used elsewhere in the boot path:
//!
//!   flag > red_config > env > built-in default
//!
//! Built-in defaults reproduce the hard-coded values from slices 1+2:
//!   - max_handlers      = (2 * num_cpus).clamp(8, 256)
//!   - handler_timeout   = 30_000 ms
//!   - retry_after_secs  = 5
//!
//! Each knob is validated at parse time and at resolution time so a
//! stale red_config value cannot corrupt the running server.

/// Lower bound for `handler_timeout_ms`. Anything below this is so
/// short the deadline trips on healthy requests; we reject the value.
pub const MIN_HANDLER_TIMEOUT_MS: u64 = 100;
/// Inclusive bounds for `retry_after_secs`. Below 1s means clients
/// hammer the server; above 30s means a transient overload looks like
/// a permanent outage to load balancers.
pub const MIN_RETRY_AFTER_SECS: u64 = 1;
pub const MAX_RETRY_AFTER_SECS: u64 = 30;

/// Built-in default for `max_handlers`. Matches
/// `HttpConnectionLimiter::with_default_cap`.
pub fn default_max_handlers() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    (2 * cores).clamp(8, 256)
}

pub const DEFAULT_HANDLER_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_RETRY_AFTER_SECS: u64 = 5;

/// Built-in default for the per-principal in-flight cap (issue #934).
/// `0` disables enforcement: all unauthenticated callers share the single
/// `anon` principal, so a finite default would throttle aggregate
/// anonymous traffic — mirrors the QPS quota, which also defaults off.
/// Operators opt in by setting `red.http.max_conns_per_principal`.
pub const DEFAULT_MAX_CONNS_PER_PRINCIPAL: usize = 0;

/// Validate a `max_handlers` candidate from any source. Returns the
/// value unchanged on success.
pub fn validate_max_handlers(value: usize) -> Result<usize, String> {
    if value == 0 {
        return Err("http max_handlers must be >= 1".to_string());
    }
    Ok(value)
}

/// Validate a `max_conns_per_principal` candidate (issue #934). Every
/// `usize` is accepted: `0` is the sentinel that disables enforcement,
/// any positive value is a real cap. The validator exists so the parse
/// path is symmetric with the other knobs (a non-numeric red_config value
/// is still rejected by the `parse::<usize>()` upstream).
pub fn validate_max_conns_per_principal(value: usize) -> Result<usize, String> {
    Ok(value)
}

pub fn validate_handler_timeout_ms(value: u64) -> Result<u64, String> {
    if value < MIN_HANDLER_TIMEOUT_MS {
        return Err(format!(
            "http handler_timeout_ms must be >= {MIN_HANDLER_TIMEOUT_MS}"
        ));
    }
    Ok(value)
}

pub fn validate_retry_after_secs(value: u64) -> Result<u64, String> {
    if !(MIN_RETRY_AFTER_SECS..=MAX_RETRY_AFTER_SECS).contains(&value) {
        return Err(format!(
            "http retry_after_secs must be in [{MIN_RETRY_AFTER_SECS}, {MAX_RETRY_AFTER_SECS}]"
        ));
    }
    Ok(value)
}

/// CLI-layer input. Each pair holds the already-validated value coming
/// from a flag and from an env var, respectively. The resolver applies
/// the `flag > red_config > env > default` precedence using these
/// inputs plus a config-store lookup.
#[derive(Debug, Default, Clone)]
pub struct HttpLimitsCliInput {
    pub max_handlers_flag: Option<usize>,
    pub max_handlers_env: Option<usize>,
    pub handler_timeout_ms_flag: Option<u64>,
    pub handler_timeout_ms_env: Option<u64>,
    pub retry_after_secs_flag: Option<u64>,
    pub retry_after_secs_env: Option<u64>,
    pub max_conns_per_principal_flag: Option<usize>,
    pub max_conns_per_principal_env: Option<usize>,
}

/// Resolved values after applying the full precedence chain. Stamped
/// into both the `RedDBServer` and the startup log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpLimitsResolved {
    pub max_handlers: usize,
    pub handler_timeout_ms: u64,
    pub retry_after_secs: u64,
    /// Issue #934 — per-principal concurrent in-flight-request ceiling.
    /// `0` disables enforcement.
    pub max_conns_per_principal: usize,
}

impl HttpLimitsResolved {
    pub fn builtin_defaults() -> Self {
        Self {
            max_handlers: default_max_handlers(),
            handler_timeout_ms: DEFAULT_HANDLER_TIMEOUT_MS,
            retry_after_secs: DEFAULT_RETRY_AFTER_SECS,
            max_conns_per_principal: DEFAULT_MAX_CONNS_PER_PRINCIPAL,
        }
    }
}

/// Apply the `flag > red_config > env > default` chain.
///
/// `config_lookup` is a closure so this function is independent of the
/// runtime/config-store type — keeps the resolver pure and testable.
/// Each lookup returns the raw text value stored under the given key,
/// matching how `set_config_tree` persists scalars.
pub fn resolve_http_limits<F>(input: &HttpLimitsCliInput, config_lookup: F) -> HttpLimitsResolved
where
    F: Fn(&str) -> Option<String>,
{
    let defaults = HttpLimitsResolved::builtin_defaults();

    let max_handlers = input
        .max_handlers_flag
        .or_else(|| {
            config_lookup("red.http.max_handlers")
                .and_then(|raw| raw.parse::<usize>().ok())
                .and_then(|v| validate_max_handlers(v).ok())
        })
        .or(input.max_handlers_env)
        .unwrap_or(defaults.max_handlers);

    let handler_timeout_ms = input
        .handler_timeout_ms_flag
        .or_else(|| {
            config_lookup("red.http.handler_timeout_ms")
                .and_then(|raw| raw.parse::<u64>().ok())
                .and_then(|v| validate_handler_timeout_ms(v).ok())
        })
        .or(input.handler_timeout_ms_env)
        .unwrap_or(defaults.handler_timeout_ms);

    let retry_after_secs = input
        .retry_after_secs_flag
        .or_else(|| {
            config_lookup("red.http.retry_after_secs")
                .and_then(|raw| raw.parse::<u64>().ok())
                .and_then(|v| validate_retry_after_secs(v).ok())
        })
        .or(input.retry_after_secs_env)
        .unwrap_or(defaults.retry_after_secs);

    let max_conns_per_principal = input
        .max_conns_per_principal_flag
        .or_else(|| {
            config_lookup("red.http.max_conns_per_principal")
                .and_then(|raw| raw.parse::<usize>().ok())
                .and_then(|v| validate_max_conns_per_principal(v).ok())
        })
        .or(input.max_conns_per_principal_env)
        .unwrap_or(defaults.max_conns_per_principal);

    HttpLimitsResolved {
        max_handlers,
        handler_timeout_ms,
        retry_after_secs,
        max_conns_per_principal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn no_config() -> impl Fn(&str) -> Option<String> {
        |_| None
    }

    fn map_lookup(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |key| map.get(key).map(|v| v.to_string())
    }

    #[test]
    fn defaults_when_nothing_set() {
        let resolved = resolve_http_limits(&HttpLimitsCliInput::default(), no_config());
        assert_eq!(resolved, HttpLimitsResolved::builtin_defaults());
    }

    #[test]
    fn flag_wins_over_env_and_default() {
        let input = HttpLimitsCliInput {
            max_handlers_flag: Some(16),
            max_handlers_env: Some(99),
            handler_timeout_ms_flag: Some(5_000),
            handler_timeout_ms_env: Some(7_000),
            retry_after_secs_flag: Some(3),
            retry_after_secs_env: Some(7),
            ..Default::default()
        };
        let resolved = resolve_http_limits(&input, no_config());
        assert_eq!(resolved.max_handlers, 16);
        assert_eq!(resolved.handler_timeout_ms, 5_000);
        assert_eq!(resolved.retry_after_secs, 3);
    }

    #[test]
    fn flag_wins_over_red_config() {
        let input = HttpLimitsCliInput {
            max_handlers_flag: Some(16),
            handler_timeout_ms_flag: Some(5_000),
            retry_after_secs_flag: Some(3),
            ..Default::default()
        };
        let lookup = map_lookup(HashMap::from([
            ("red.http.max_handlers", "64"),
            ("red.http.handler_timeout_ms", "9000"),
            ("red.http.retry_after_secs", "9"),
        ]));
        let resolved = resolve_http_limits(&input, lookup);
        assert_eq!(resolved.max_handlers, 16);
        assert_eq!(resolved.handler_timeout_ms, 5_000);
        assert_eq!(resolved.retry_after_secs, 3);
    }

    #[test]
    fn red_config_wins_over_env() {
        let input = HttpLimitsCliInput {
            max_handlers_env: Some(99),
            handler_timeout_ms_env: Some(7_000),
            retry_after_secs_env: Some(7),
            ..Default::default()
        };
        let lookup = map_lookup(HashMap::from([
            ("red.http.max_handlers", "64"),
            ("red.http.handler_timeout_ms", "9000"),
            ("red.http.retry_after_secs", "9"),
        ]));
        let resolved = resolve_http_limits(&input, lookup);
        assert_eq!(resolved.max_handlers, 64);
        assert_eq!(resolved.handler_timeout_ms, 9_000);
        assert_eq!(resolved.retry_after_secs, 9);
    }

    #[test]
    fn env_wins_over_default() {
        let input = HttpLimitsCliInput {
            max_handlers_env: Some(11),
            handler_timeout_ms_env: Some(1_500),
            retry_after_secs_env: Some(2),
            ..Default::default()
        };
        let resolved = resolve_http_limits(&input, no_config());
        assert_eq!(resolved.max_handlers, 11);
        assert_eq!(resolved.handler_timeout_ms, 1_500);
        assert_eq!(resolved.retry_after_secs, 2);
    }

    #[test]
    fn invalid_red_config_is_ignored_in_favor_of_lower_layers() {
        // Garbage in red_config — must not break boot. Env value wins;
        // if env is absent, default wins.
        let input = HttpLimitsCliInput {
            max_handlers_env: Some(11),
            ..Default::default()
        };
        let lookup = map_lookup(HashMap::from([
            ("red.http.max_handlers", "0"),        // rejected by validate
            ("red.http.handler_timeout_ms", "5"),  // rejected by validate
            ("red.http.retry_after_secs", "9999"), // rejected by validate
        ]));
        let resolved = resolve_http_limits(&input, lookup);
        // max_handlers: red_config invalid -> env (11)
        assert_eq!(resolved.max_handlers, 11);
        // handler_timeout_ms: red_config invalid, no env -> default
        assert_eq!(resolved.handler_timeout_ms, DEFAULT_HANDLER_TIMEOUT_MS);
        // retry_after_secs: red_config invalid, no env -> default
        assert_eq!(resolved.retry_after_secs, DEFAULT_RETRY_AFTER_SECS);
    }

    #[test]
    fn validators_reject_zero_equivalent_values() {
        assert!(validate_max_handlers(0).is_err());
        assert!(validate_max_handlers(1).is_ok());

        assert!(validate_handler_timeout_ms(0).is_err());
        assert!(validate_handler_timeout_ms(MIN_HANDLER_TIMEOUT_MS - 1).is_err());
        assert!(validate_handler_timeout_ms(MIN_HANDLER_TIMEOUT_MS).is_ok());

        assert!(validate_retry_after_secs(0).is_err());
        assert!(validate_retry_after_secs(MIN_RETRY_AFTER_SECS).is_ok());
        assert!(validate_retry_after_secs(MAX_RETRY_AFTER_SECS).is_ok());
        assert!(validate_retry_after_secs(MAX_RETRY_AFTER_SECS + 1).is_err());
    }

    #[test]
    fn default_max_handlers_in_bounds() {
        let cap = default_max_handlers();
        assert!((8..=256).contains(&cap));
    }

    #[test]
    fn max_conns_per_principal_defaults_to_disabled() {
        let resolved = resolve_http_limits(&HttpLimitsCliInput::default(), no_config());
        assert_eq!(resolved.max_conns_per_principal, 0);
        assert_eq!(
            HttpLimitsResolved::builtin_defaults().max_conns_per_principal,
            DEFAULT_MAX_CONNS_PER_PRINCIPAL
        );
    }

    #[test]
    fn max_conns_per_principal_precedence_flag_over_config_over_env() {
        let input = HttpLimitsCliInput {
            max_conns_per_principal_flag: Some(4),
            max_conns_per_principal_env: Some(9),
            ..Default::default()
        };
        let lookup = map_lookup(HashMap::from([("red.http.max_conns_per_principal", "7")]));
        // Flag wins.
        assert_eq!(
            resolve_http_limits(&input, lookup).max_conns_per_principal,
            4
        );

        // Without a flag, red_config wins over env.
        let input = HttpLimitsCliInput {
            max_conns_per_principal_env: Some(9),
            ..Default::default()
        };
        let lookup = map_lookup(HashMap::from([("red.http.max_conns_per_principal", "7")]));
        assert_eq!(
            resolve_http_limits(&input, lookup).max_conns_per_principal,
            7
        );

        // Env wins over the built-in default.
        let input = HttpLimitsCliInput {
            max_conns_per_principal_env: Some(9),
            ..Default::default()
        };
        assert_eq!(
            resolve_http_limits(&input, no_config()).max_conns_per_principal,
            9
        );
    }

    #[test]
    fn max_conns_per_principal_zero_in_config_is_honored_as_disabled() {
        // Unlike max_handlers, 0 is a valid value here (it disables the
        // cap), so a `red_config` 0 must take effect rather than fall
        // through to a lower layer.
        let lookup = map_lookup(HashMap::from([("red.http.max_conns_per_principal", "0")]));
        let input = HttpLimitsCliInput {
            max_conns_per_principal_env: Some(5),
            ..Default::default()
        };
        assert_eq!(
            resolve_http_limits(&input, lookup).max_conns_per_principal,
            0
        );
    }

    #[test]
    fn non_numeric_max_conns_per_principal_config_falls_through() {
        let lookup = map_lookup(HashMap::from([(
            "red.http.max_conns_per_principal",
            "not-a-number",
        )]));
        let input = HttpLimitsCliInput {
            max_conns_per_principal_env: Some(5),
            ..Default::default()
        };
        assert_eq!(
            resolve_http_limits(&input, lookup).max_conns_per_principal,
            5
        );
    }
}
