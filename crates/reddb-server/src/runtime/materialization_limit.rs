//! Issue #769 (PRD #759 / S10) — materialization ceiling for the
//! aggregating executors.
//!
//! Scans became pull-based in S9, so they need no row ceiling. The
//! executors that must buffer by design — aggregation (GROUP BY),
//! sort (ORDER BY) and window functions — cannot stream, so they get
//! an explicit cap instead: `stream.executor.max_materialized_rows`
//! in `red_config`. When an executor's in-memory row count crosses
//! the cap the query terminates with
//! [`RedDBError::MaterializationLimitExceeded`] naming the executor
//! and the live count.
//!
//! The limit is read fresh from `red_config` per query, so a config
//! hot-reload affects new queries only — an in-flight query keeps the
//! ceiling it started with (the read happens once at the materialise
//! point, not on a back-reference). The same guard fires identically
//! in streaming and non-streaming delivery because both share the one
//! materialising execute path.

use crate::api::{RedDBError, RedDBResult};
use crate::storage::schema::Value;
use crate::RedDB;

/// Collection that backs `red.config` over the runtime KV store.
const RED_CONFIG_COLLECTION: &str = "red_config";

/// The single config key this guard consults.
const CONFIG_KEY: &str = "stream.executor.max_materialized_rows";

/// Default ceiling when the operator hasn't pinned one (issue #769
/// suggested 10⁶). High enough that ordinary analytical queries never
/// trip it, low enough that a runaway aggregation cannot exhaust
/// process memory before it fires.
pub(crate) const DEFAULT_MAX_MATERIALIZED_ROWS: usize = 1_000_000;

/// Read `stream.executor.max_materialized_rows` from `red_config`.
///
/// `red_config` is append-only — `SET CONFIG` inserts a fresh row each
/// time rather than updating in place — so a naive first-match read
/// returns a stale value after a hot-reload. We resolve the newest
/// value by highest entity id, the same rule the runtime's own
/// `latest_config_snapshot` uses; that is what makes a hot-reload take
/// effect for new queries.
///
/// Missing or unparseable values fall back to
/// [`DEFAULT_MAX_MATERIALIZED_ROWS`] — bad config never terminates a
/// query that would otherwise succeed. A configured `0` means
/// "explicitly unbounded" (mirrors the `RED_MAX_*` resource-limit
/// convention) and disables the guard by returning [`usize::MAX`].
pub(crate) fn max_materialized_rows(db: &RedDB) -> usize {
    // (entity_id, parsed_value) of the newest matching config row.
    let mut newest: Option<(u64, u64)> = None;
    if let Some(manager) = db.store().get_collection(RED_CONFIG_COLLECTION) {
        manager.for_each_entity(|entity| {
            let Some(row) = entity.data.as_row() else {
                return true;
            };
            let Some(Value::Text(key)) = row.get_field("key") else {
                return true;
            };
            if !key.eq_ignore_ascii_case(CONFIG_KEY) {
                return true;
            }
            let parsed: Option<u64> = match row.get_field("value") {
                Some(Value::Integer(v)) if *v >= 0 => Some(*v as u64),
                Some(Value::UnsignedInteger(v)) => Some(*v),
                Some(Value::Float(v)) if *v >= 0.0 => Some(*v as u64),
                Some(Value::Text(text)) => text.trim().parse().ok(),
                _ => None,
            };
            if let Some(v) = parsed {
                let id = entity.id.raw();
                if newest.is_none_or(|(best_id, _)| id >= best_id) {
                    newest = Some((id, v));
                }
            }
            true
        });
    }
    match newest {
        None => DEFAULT_MAX_MATERIALIZED_ROWS,
        Some((_, 0)) => usize::MAX, // explicit unbounded
        Some((_, v)) => v as usize,
    }
}

/// Terminate the query when `current` materialized rows have exceeded
/// `limit`. `executor` is one of `"aggregation"`, `"sort"`,
/// `"window"`. Pure and limit-injected so the threshold logic is unit
/// testable without a live store.
pub(crate) fn check(executor: &'static str, current: usize, limit: usize) -> RedDBResult<()> {
    if current > limit {
        return Err(RedDBError::MaterializationLimitExceeded {
            executor,
            limit,
            current,
        });
    }
    Ok(())
}

/// Read the live ceiling from `red_config` and enforce it against
/// `current`. Convenience wrapper over [`check`] for the executor call
/// sites that hold a `&RedDB`.
pub(crate) fn guard(db: &RedDB, executor: &'static str, current: usize) -> RedDBResult<()> {
    check(executor, current, max_materialized_rows(db))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBOptions, RedDBRuntime};

    fn mk_runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory())
            .expect("in-memory runtime should open")
    }

    #[test]
    fn check_passes_at_or_below_limit() {
        assert!(check("aggregation", 0, 10).is_ok());
        assert!(check("aggregation", 10, 10).is_ok());
    }

    #[test]
    fn check_fails_just_past_limit_and_names_executor() {
        let err = check("sort", 11, 10).unwrap_err();
        match err {
            RedDBError::MaterializationLimitExceeded {
                executor,
                limit,
                current,
            } => {
                assert_eq!(executor, "sort");
                assert_eq!(limit, 10);
                assert_eq!(current, 11);
            }
            other => panic!("expected MaterializationLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn default_applies_when_key_absent() {
        let rt = mk_runtime();
        assert_eq!(
            max_materialized_rows(&rt.db()),
            DEFAULT_MAX_MATERIALIZED_ROWS
        );
    }

    #[test]
    fn configured_value_is_read_back() {
        let rt = mk_runtime();
        rt.execute_query("SET CONFIG stream.executor.max_materialized_rows = 42")
            .expect("set config ok");
        assert_eq!(max_materialized_rows(&rt.db()), 42);
    }

    #[test]
    fn zero_means_unbounded() {
        let rt = mk_runtime();
        rt.execute_query("SET CONFIG stream.executor.max_materialized_rows = 0")
            .expect("set config ok");
        assert_eq!(max_materialized_rows(&rt.db()), usize::MAX);
    }
}
