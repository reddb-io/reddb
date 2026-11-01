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
use crate::RedDB;
use reddb_types::Value;

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
/// query that would otherwise succeed. Physical read failures propagate rather
/// than silently relaxing admission. A configured `0` means
/// "explicitly unbounded" (mirrors the `RED_MAX_*` resource-limit
/// convention) and disables the guard by returning [`usize::MAX`].
pub(crate) fn max_materialized_rows(db: &RedDB) -> RedDBResult<usize> {
    // (entity_id, parsed_value) of the newest matching config row.
    let mut newest: Option<(u64, u64)> = None;
    if let Some(manager) = db.store().get_collection(RED_CONFIG_COLLECTION) {
        manager.try_for_each_entity(usize::MAX, |entity| {
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
        })?;
    }
    Ok(match newest {
        None => DEFAULT_MAX_MATERIALIZED_ROWS,
        Some((_, 0)) => usize::MAX, // explicit unbounded
        Some((_, v)) => v as usize,
    })
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
    check(executor, current, max_materialized_rows(db)?)
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
            max_materialized_rows(&rt.db()).expect("default limit"),
            DEFAULT_MAX_MATERIALIZED_ROWS
        );
    }

    #[test]
    fn configured_value_is_read_back() {
        let rt = mk_runtime();
        rt.execute_query("SET CONFIG stream.executor.max_materialized_rows = 42")
            .expect("set config ok");
        assert_eq!(
            max_materialized_rows(&rt.db()).expect("configured limit"),
            42
        );
    }

    #[test]
    fn zero_means_unbounded() {
        let rt = mk_runtime();
        rt.execute_query("SET CONFIG stream.executor.max_materialized_rows = 0")
            .expect("set config ok");
        assert_eq!(
            max_materialized_rows(&rt.db()).expect("unbounded config"),
            usize::MAX
        );
    }

    fn cold_limit_fixture() -> (tempfile::TempDir, crate::RedDBOptions, RedDBRuntime) {
        let directory = tempfile::tempdir().expect("cold limit fixture");
        let options = RedDBOptions::persistent(directory.path().join("limits.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let runtime = RedDBRuntime::with_options(options.clone()).expect("seed runtime");
        for sql in [
            "CREATE TABLE limit_groups (cohort INTEGER)",
            "INSERT INTO limit_groups (cohort) VALUES (1), (2), (3)",
            "SET CONFIG stream.executor.max_materialized_rows = 1",
            "SET CONFIG stream.executor.max_materialized_rows = 2",
        ] {
            runtime.execute_query(sql).expect("seed limit and groups");
        }
        runtime.flush().expect("persist configured limit");
        drop(runtime);
        let runtime = cold_open(&options);
        assert!(runtime
            .db()
            .store()
            .get_collection(RED_CONFIG_COLLECTION)
            .expect("config collection")
            .has_backing());
        (directory, options, runtime)
    }

    fn cold_open(options: &RedDBOptions) -> RedDBRuntime {
        crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options.clone())
        })
        .expect("cold runtime")
    }

    #[test]
    fn cold_group_limit_survives_reopen_and_overlay_update() {
        let (_directory, options, runtime) = cold_limit_fixture();
        let sql = "SELECT cohort, COUNT(*) FROM limit_groups GROUP BY cohort";
        let error = runtime
            .execute_query(sql)
            .expect_err("cold configured ceiling applies");
        assert!(
            matches!(
                error,
                RedDBError::MaterializationLimitExceeded {
                    executor: "aggregation",
                    limit: 2,
                    current: 3,
                }
            ),
            "{error:?}"
        );
        runtime
            .execute_query("SET CONFIG stream.executor.max_materialized_rows = 4")
            .expect("newest overlay limit");
        assert_eq!(
            runtime
                .execute_query(sql)
                .expect("raised limit")
                .result
                .records
                .len(),
            3
        );
        runtime.flush().expect("persist new limit");
        drop(runtime);
        let reopened = cold_open(&options);
        assert_eq!(
            reopened
                .execute_query(sql)
                .expect("persisted new limit")
                .result
                .records
                .len(),
            3
        );
    }

    #[test]
    fn cold_group_limit_page_failure_does_not_relax_admission() {
        use crate::storage::engine::{Page, PageType};
        let (_directory, _options, runtime) = cold_limit_fixture();
        let store = runtime.db().store();
        let root = store
            .get_collection(RED_CONFIG_COLLECTION)
            .expect("config")
            .backing_root_page()
            .expect("config root");
        let pager = store.pager().expect("fixture pager");
        let original = pager.read_page(root).expect("config page");
        pager
            .write_page(root, Page::new(PageType::Overflow, root))
            .expect("inject page failure");
        let result = guard(&runtime.db(), "aggregation", 1);
        pager
            .write_page(root, original)
            .expect("restore fixture before assertions");
        let error = result.expect_err("unreadable config must not select the default ceiling");
        assert!(
            !matches!(error, RedDBError::MaterializationLimitExceeded { .. }),
            "{error:?}"
        );
        guard(&runtime.db(), "aggregation", 1).expect("restored config read");
    }
}
