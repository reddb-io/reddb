//! Legacy and canonical names of runtime-owned collections.
//!
//! These names are persisted. This table alone does not migrate a store: a
//! caller must reconcile its catalogue, WAL and manifest before publishing it.

pub const LEGACY_NAMES: &[(&str, &str)] = &[
    ("red_config", "red.config"),
    ("red_stats", "red.query.stats_catalog"),
    ("red_index_registry", "red.index.registry"),
    ("red_queue_meta", "red.queue.meta"),
    ("red_timeseries_meta", "red.timeseries.meta"),
    ("red_timeseries_series", "red.timeseries.series"),
    ("red_timeseries_tag_index", "red.timeseries.tag_index"),
    (
        "red_materialized_view_defs",
        "red.materialized_view.definitions",
    ),
    ("red_ask_audit", "red.ask.audit"),
    ("red_internal_result_cache", "red.query.result_cache"),
    ("red_topology_meta", "red.topology.meta"),
    ("red_migrations", "red.migration.definitions"),
    ("red_migration_deps", "red.migration.dependencies"),
    ("red_commits", "red.vcs.commits"),
    ("red_refs", "red.vcs.refs"),
    ("red_worksets", "red.vcs.worksets"),
    ("red_closure", "red.vcs.closure"),
    ("red_conflicts", "red.vcs.conflicts"),
    ("red_merge_state", "red.vcs.merge_state"),
    ("red_remotes", "red.vcs.remotes"),
    ("red_vcs_settings", "red.vcs.settings"),
];

/// Find the canonical identity of an exact legacy collection name.
pub fn canonical_name(name: &str) -> Option<&'static str> {
    LEGACY_NAMES
        .iter()
        .find_map(|(legacy, canonical)| (*legacy == name).then_some(*canonical))
}

/// Return the durable identity for a collection name. Exact legacy system
/// names remain accepted at API and WAL boundaries, but are never published
/// into a new catalog or snapshot.
pub fn canonical_or_self(name: &str) -> &str {
    canonical_name(name).unwrap_or(name)
}

pub fn canonical_owned(name: String) -> String {
    canonical_name(&name).map_or(name, str::to_owned)
}

/// Plan migrations without changing the store. A destination already present
/// alongside its legacy name is ambiguous and must never be merged implicitly.
pub fn migration_plan<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<(&'static str, &'static str)>, (&'static str, &'static str)> {
    let existing: std::collections::HashSet<&str> = names.into_iter().collect();
    let mut plan = Vec::new();
    for &(legacy, canonical) in LEGACY_NAMES {
        if existing.contains(legacy) {
            if existing.contains(canonical) {
                return Err((legacy, canonical));
            }
            plan.push((legacy, canonical));
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn mappings_are_unique_and_do_not_match_other_names() {
        let mut legacy_names = HashSet::new();
        let mut canonical_names = HashSet::new();
        for &(legacy, canonical) in LEGACY_NAMES {
            assert!(legacy_names.insert(legacy));
            assert!(canonical_names.insert(canonical));
            assert!(legacy.starts_with("red_"));
            assert!(canonical.starts_with("red."));
            assert_eq!(canonical_name(legacy), Some(canonical));
            assert_eq!(canonical_name(canonical), None);
        }
        assert_eq!(canonical_name("red_queue_meta_user"), None);
        assert_eq!(canonical_name("users"), None);
    }

    #[test]
    fn migration_plan_rejects_collisions_and_is_idempotent() {
        let plan = migration_plan(["users", "red_config", "red_queue_meta"])
            .expect("distinct destinations");
        assert_eq!(
            plan,
            vec![
                ("red_config", "red.config"),
                ("red_queue_meta", "red.queue.meta")
            ]
        );
        assert_eq!(
            migration_plan(["red.config", "red.queue.meta"]),
            Ok(Vec::new())
        );
        assert_eq!(
            migration_plan(["red_config", "red.config", "red_queue_meta"]),
            Err(("red_config", "red.config")),
        );
    }
}
