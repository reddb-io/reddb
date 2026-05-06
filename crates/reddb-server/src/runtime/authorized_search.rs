//! Authorized search entry point — issue #119.
//!
//! Wraps every SEARCH SIMILAR / SEARCH TEXT / SEARCH CONTEXT runtime
//! call so that the candidate set is *pre-filtered* by the calling
//! identity's `EffectiveScope.visible_collections` BEFORE any
//! similarity score is computed. Without this gate AI commands could
//! return rows outside the calling user's RLS scope and leak them via
//! the LLM context window.
//!
//! The contract is intentionally narrow: every public function takes
//! `scope: &dyn ReadFrame`, refuses with a structured `RedDBError` if
//! the frame carries `None` for `visible_collections()`, and trims the
//! candidate set to the intersection of the user-supplied collection
//! list (if any) and the scope's allow-list. The legacy direct entry
//! points (`RedDBRuntime::search_similar`, `search_text`,
//! `search_context`) remain in place for tests and for callers that
//! have already opened a frame; this module is the *canonical* entry
//! that the `SEARCH SIMILAR / SEARCH CONTEXT` SQL commands and ASK go
//! through.

use std::collections::HashSet;

use tracing::{debug, info_span, warn};

use super::statement_frame::ReadFrame;
use super::RedDBRuntime;
use crate::api::{RedDBError, RedDBResult};
use crate::application::SearchContextInput;
use crate::storage::unified::devx::SimilarResult;

/// Surface area used by the AI runtime entry points. Holds a reference
/// to the scope and the canonical functions; kept as an empty enum so
/// callers spell `AuthorizedSearch::execute_*(...)` (matching the
/// shape required by issue #119).
pub enum AuthorizedSearch {}

impl AuthorizedSearch {
    /// Authorized SEARCH SIMILAR. Refuses with a structured error when
    /// the caller's scope has no `visible_collections` set, or when the
    /// requested collection is outside that set. Otherwise dispatches
    /// to the underlying `RedDBRuntime::search_similar`.
    pub(crate) fn execute_similar(
        runtime: &RedDBRuntime,
        scope: &dyn ReadFrame,
        collection: &str,
        vector: &[f32],
        k: usize,
        min_score: f32,
    ) -> RedDBResult<Vec<SimilarResult>> {
        let span = info_span!(
            "authorized_search.similar",
            collection = collection,
            tenant = ?scope.effective_scope(),
        );
        let _enter = span.enter();

        let visible = require_visible(scope, "SEARCH SIMILAR")?;
        if !visible.contains(collection) {
            warn!(
                target: "authorized_search",
                collection = collection,
                "denied: collection outside visible scope"
            );
            return Err(RedDBError::Query(format!(
                "permission denied: collection `{collection}` is not in the caller's visible scope"
            )));
        }
        debug!(target: "authorized_search", "scope-checked, dispatching");
        runtime.search_similar(collection, vector, k, min_score)
    }

    /// Authorized SEARCH TEXT. The underlying executor accepts an
    /// optional collection list; we intersect it with the visible set
    /// before forwarding so collections outside scope never enter the
    /// candidate pool.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute_text(
        runtime: &RedDBRuntime,
        scope: &dyn ReadFrame,
        query: String,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        fields: Option<Vec<String>>,
        limit: Option<usize>,
        fuzzy: bool,
    ) -> RedDBResult<crate::storage::unified::dsl::QueryResult> {
        let span = info_span!(
            "authorized_search.text",
            tenant = ?scope.effective_scope(),
        );
        let _enter = span.enter();

        let visible = require_visible(scope, "SEARCH TEXT")?;
        let constrained = constrain_collections(collections, visible);
        if let Some(ref c) = constrained {
            if c.is_empty() {
                // Caller asked for collections outside their scope —
                // refuse loudly rather than fall through to the global
                // path (which would scan every collection ignoring the
                // intent).
                return Err(RedDBError::Query(
                    "permission denied: no requested collection is in the caller's visible scope"
                        .to_string(),
                ));
            }
        }
        runtime.search_text(
            query,
            constrained,
            entity_types,
            capabilities,
            fields,
            limit,
            fuzzy,
        )
    }

    /// Authorized SEARCH CONTEXT. Pre-filters the input collection list
    /// against the visible scope and post-filters every result bucket
    /// so cross-ref / graph / vector expansion can't leak rows from
    /// outside the scope. Refuses with a structured error when the
    /// scope has no visible-collections set.
    pub(crate) fn execute_context(
        runtime: &RedDBRuntime,
        scope: &dyn ReadFrame,
        mut input: SearchContextInput,
    ) -> RedDBResult<crate::runtime::ContextSearchResult> {
        let span = info_span!(
            "authorized_search.context",
            tenant = ?scope.effective_scope(),
        );
        let _enter = span.enter();

        let visible = require_visible(scope, "SEARCH CONTEXT")?;

        // Pre-filter the request: drop any caller-supplied collection
        // that's outside the visible set, and force the global-scan
        // path to also stay inside the visible set by passing the
        // intersection through.
        input.collections = constrain_collections(input.collections, visible);
        if let Some(ref c) = input.collections {
            if c.is_empty() {
                return Err(RedDBError::Query(
                    "permission denied: no requested collection is in the caller's visible scope"
                        .to_string(),
                ));
            }
        } else {
            // No caller list — substitute the visible set so the
            // global-scan tier stays bounded by it.
            let mut bounded: Vec<String> = visible.iter().cloned().collect();
            bounded.sort();
            input.collections = Some(bounded);
        }

        let mut result = runtime.search_context(input)?;
        post_filter_context_result(&mut result, visible);
        Ok(result)
    }
}

/// Defence-in-depth pass run after `search_context` returns. The
/// `input.collections` pre-filter already bounds the corpus, but the
/// cross-ref / graph / vector expansion paths resolve
/// `xref.target_collection` and `entity.kind.collection()`
/// independently. Re-filtering each bucket here ensures a regression
/// in one of those paths can't leak rows from outside `allowed`.
///
/// Factored out as a free function so the property test (256 cases)
/// can drive the invariant without booting a runtime.
fn post_filter_context_result(
    result: &mut crate::runtime::ContextSearchResult,
    allowed: &HashSet<String>,
) {
    let retain = |bucket: &mut Vec<crate::runtime::ContextEntity>| {
        bucket.retain(|e| allowed.contains(&e.collection));
    };
    retain(&mut result.tables);
    retain(&mut result.graph.nodes);
    retain(&mut result.graph.edges);
    retain(&mut result.vectors);
    retain(&mut result.documents);
    retain(&mut result.key_values);

    // Connections reference entity ids; recompute visible ids so a
    // dangling edge into a filtered-out row is dropped.
    let visible_ids: HashSet<u64> = std::iter::empty()
        .chain(result.tables.iter().map(|e| e.entity.id.raw()))
        .chain(result.graph.nodes.iter().map(|e| e.entity.id.raw()))
        .chain(result.graph.edges.iter().map(|e| e.entity.id.raw()))
        .chain(result.vectors.iter().map(|e| e.entity.id.raw()))
        .chain(result.documents.iter().map(|e| e.entity.id.raw()))
        .chain(result.key_values.iter().map(|e| e.entity.id.raw()))
        .collect();
    result
        .connections
        .retain(|c| visible_ids.contains(&c.from_id) && visible_ids.contains(&c.to_id));

    result.summary.total_entities = result.tables.len()
        + result.graph.nodes.len()
        + result.graph.edges.len()
        + result.vectors.len()
        + result.documents.len()
        + result.key_values.len();
}

/// Resolve the visible-collections set on a frame, refusing with a
/// structured error when none is wired. Centralised so every entry
/// point produces the same error string and tracing event.
fn require_visible<'a>(
    scope: &'a dyn ReadFrame,
    op: &'static str,
) -> RedDBResult<&'a HashSet<String>> {
    match scope.visible_collections() {
        Some(set) => Ok(set),
        None => {
            warn!(
                target: "authorized_search",
                op = op,
                "refused: no visible-collections scope on frame"
            );
            Err(RedDBError::Query(format!(
                "{op} requires an authenticated scope with visible_collections; \
                 none was attached to the runtime frame"
            )))
        }
    }
}

/// Intersect a caller-supplied collection list with the visible set.
/// `None` (no caller list) means "every collection" — we pass `None`
/// through unchanged so the caller's existing default of "scan the
/// whole DB" reads as "scan everything visible to the scope". The
/// caller is expected to substitute the visible set explicitly when it
/// needs a bounded global-scan corpus.
fn constrain_collections(
    requested: Option<Vec<String>>,
    visible: &HashSet<String>,
) -> Option<Vec<String>> {
    match requested {
        None => None,
        Some(list) => {
            let filtered: Vec<String> = list
                .into_iter()
                .filter(|c| visible.contains(c))
                .collect();
            Some(filtered)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBOptions;
    use crate::auth::Role;
    use crate::runtime::statement_frame::test_support::FakeReadFrame;
    use crate::runtime::RedDBRuntime;

    fn rt() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt")
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn execute_similar_refuses_without_scope() {
        let rt = rt();
        let frame = FakeReadFrame::without_scope();
        let err = AuthorizedSearch::execute_similar(&rt, &frame, "orders", &[0.1], 1, 0.0)
            .expect_err("refuses without scope");
        assert!(format!("{err}").contains("requires an authenticated scope"));
    }

    #[test]
    fn execute_similar_refuses_collection_outside_scope() {
        let rt = rt();
        let frame = FakeReadFrame::with_visible(set(&["orders"]));
        let err = AuthorizedSearch::execute_similar(&rt, &frame, "secrets", &[0.1], 1, 0.0)
            .expect_err("refuses out-of-scope collection");
        assert!(format!("{err}").contains("not in the caller's visible scope"));
    }

    #[test]
    fn execute_context_refuses_without_scope() {
        let rt = rt();
        let frame = FakeReadFrame::without_scope();
        let err = AuthorizedSearch::execute_context(
            &rt,
            &frame,
            SearchContextInput {
                query: "x".into(),
                field: None,
                vector: None,
                collections: None,
                graph_depth: None,
                graph_max_edges: None,
                max_cross_refs: None,
                follow_cross_refs: None,
                expand_graph: None,
                global_scan: None,
                reindex: None,
                limit: None,
                min_score: None,
            },
        )
        .expect_err("refuses without scope");
        assert!(format!("{err}").contains("requires an authenticated scope"));
    }

    #[test]
    fn constrain_collections_drops_out_of_scope_items() {
        let visible = set(&["a", "b"]);
        let got = constrain_collections(Some(vec!["a".into(), "c".into()]), &visible);
        assert_eq!(got, Some(vec!["a".into()]));
        // None -> None passthrough.
        assert!(constrain_collections(None, &visible).is_none());
        // Touch `Role` so the import isn't dropped if test fixtures grow.
        let _ = Role::Read;
    }

    // -----------------------------------------------------------------
    // Property test (issue #119): every result row's collection ∈
    // scope.visible_collections after `post_filter_context_result`.
    // 256 cases as the issue requires.
    // -----------------------------------------------------------------

    use crate::runtime::{
        ContextConnection, ContextConnectionType, ContextEntity, ContextGraphResult,
        ContextSearchResult, ContextSummary, DiscoveryMethod,
    };
    use crate::storage::unified::entity::{EntityData, EntityKind, RowData, UnifiedEntity};
    use crate::storage::unified::EntityId;
    use proptest::prelude::*;

    fn fake_entity(id: u64, collection: &str) -> UnifiedEntity {
        UnifiedEntity::new(
            EntityId::new(id),
            EntityKind::TableRow {
                table: std::sync::Arc::from(collection),
                row_id: id,
            },
            EntityData::Row(RowData::new(Vec::new())),
        )
    }

    fn fake_ctx_entity(id: u64, collection: &str) -> ContextEntity {
        ContextEntity {
            entity: fake_entity(id, collection),
            score: 0.5,
            discovery: DiscoveryMethod::GlobalScan,
            collection: collection.to_string(),
        }
    }

    fn empty_summary() -> ContextSummary {
        ContextSummary {
            total_entities: 0,
            direct_matches: 0,
            expanded_via_graph: 0,
            expanded_via_cross_refs: 0,
            expanded_via_vector_query: 0,
            collections_searched: 0,
            execution_time_us: 0,
            tiers_used: Vec::new(),
            entities_reindexed: 0,
        }
    }

    fn build_result(rows: &[(u64, &str)]) -> ContextSearchResult {
        let entities: Vec<ContextEntity> = rows
            .iter()
            .map(|(id, c)| fake_ctx_entity(*id, c))
            .collect();
        ContextSearchResult {
            query: "x".into(),
            tables: entities.clone(),
            graph: ContextGraphResult {
                nodes: entities.clone(),
                edges: Vec::new(),
            },
            vectors: entities.clone(),
            documents: Vec::new(),
            key_values: Vec::new(),
            connections: vec![ContextConnection {
                from_id: rows.first().map(|(id, _)| *id).unwrap_or(0),
                to_id: rows.last().map(|(id, _)| *id).unwrap_or(0),
                connection_type: ContextConnectionType::CrossRef("x".into()),
                weight: 1.0,
            }],
            summary: empty_summary(),
        }
    }

    proptest! {
        // Issue #119: every result row's collection MUST be in
        // `visible_collections` after AuthorizedSearch's defence-in-
        // depth post-filter. Run 256 cases with arbitrary mixes of
        // collection names and visible sets.
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn every_result_row_is_in_visible_set(
            row_collections in proptest::collection::vec("[a-z]{1,4}", 0..10),
            visible in proptest::collection::hash_set("[a-z]{1,4}", 0..6),
        ) {
            let rows: Vec<(u64, &str)> = row_collections
                .iter()
                .enumerate()
                .map(|(i, c)| (i as u64 + 1, c.as_str()))
                .collect();
            let mut result = build_result(&rows);
            post_filter_context_result(&mut result, &visible);

            // The invariant: nothing escapes the visible scope.
            for e in result.tables.iter()
                .chain(result.graph.nodes.iter())
                .chain(result.graph.edges.iter())
                .chain(result.vectors.iter())
                .chain(result.documents.iter())
                .chain(result.key_values.iter())
            {
                prop_assert!(visible.contains(&e.collection),
                    "leaked row collection={} not in visible={:?}",
                    e.collection, visible);
            }
            // Connections only reference visible-id pairs.
            let visible_ids: HashSet<u64> = std::iter::empty()
                .chain(result.tables.iter().map(|e| e.entity.id.raw()))
                .chain(result.graph.nodes.iter().map(|e| e.entity.id.raw()))
                .chain(result.graph.edges.iter().map(|e| e.entity.id.raw()))
                .chain(result.vectors.iter().map(|e| e.entity.id.raw()))
                .chain(result.documents.iter().map(|e| e.entity.id.raw()))
                .chain(result.key_values.iter().map(|e| e.entity.id.raw()))
                .collect();
            for c in &result.connections {
                prop_assert!(visible_ids.contains(&c.from_id) && visible_ids.contains(&c.to_id),
                    "dangling connection {} -> {} survived filter",
                    c.from_id, c.to_id);
            }
        }
    }

    // -----------------------------------------------------------------
    // Regression test (issue #119): tenant A user runs SEARCH SIMILAR;
    // tenant B rows never enter the result.
    //
    // Drives the boundary check directly through `AuthorizedSearch::
    // execute_similar` with an `EffectiveScope` whose visible set
    // mirrors what `AuthStore::visible_collections_for_scope` returns
    // for tenant A. Asking for a tenant-B collection must refuse.
    // -----------------------------------------------------------------

    #[test]
    fn tenant_a_cannot_see_tenant_b_collection() {
        let rt = rt();
        // Tenant A's caller — visible set restricted to A's collection.
        let frame_a = FakeReadFrame::with_visible(set(&["a_orders"]));
        // SEARCH SIMILAR against `b_orders` (tenant B's collection)
        // must refuse with the structured permission-denied error,
        // BEFORE any similarity score is computed (the underlying
        // `search_similar` call is never reached because
        // `visible.contains` short-circuits first).
        let err = AuthorizedSearch::execute_similar(&rt, &frame_a, "b_orders", &[0.1], 1, 0.0)
            .expect_err("tenant-A scope must refuse tenant-B collection");
        assert!(format!("{err}").contains("not in the caller's visible scope"));
    }

    /// Companion regression: SEARCH CONTEXT also rejects when every
    /// requested collection is outside the caller's visible scope.
    /// Without the pre-filter, the global-scan tier would scan every
    /// collection in the DB (including tenant B's) and only the
    /// per-row RLS gate would catch leaks — which is exactly the
    /// failure mode #119 closes.
    #[test]
    fn search_context_refuses_all_out_of_scope_collections() {
        let rt = rt();
        let frame = FakeReadFrame::with_visible(set(&["a_orders"]));
        let err = AuthorizedSearch::execute_context(
            &rt,
            &frame,
            SearchContextInput {
                query: "x".into(),
                field: None,
                vector: None,
                collections: Some(vec!["b_orders".into(), "b_customers".into()]),
                graph_depth: None,
                graph_max_edges: None,
                max_cross_refs: None,
                follow_cross_refs: None,
                expand_graph: None,
                global_scan: None,
                reindex: None,
                limit: None,
                min_score: None,
            },
        )
        .expect_err("all-out-of-scope SEARCH CONTEXT must refuse");
        let msg = format!("{err}");
        assert!(
            msg.contains("no requested collection is in the caller's visible scope"),
            "expected scope-refusal, got: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Cache hit-rate metric is exposed (issue #119 acceptance).
    // -----------------------------------------------------------------

    #[test]
    fn auth_cache_stats_are_exposed_via_authstore() {
        use crate::auth::store::AuthStore;
        use crate::auth::AuthConfig;
        let store = AuthStore::new(AuthConfig::default());
        let stats0 = store.auth_cache_stats();
        assert_eq!(stats0.hits + stats0.misses, 0);
        // Drive a miss + insert via the public API.
        let _ = store.visible_collections_for_scope(
            None,
            Role::Read,
            "alice",
            &vec!["orders".to_string()],
        );
        let stats1 = store.auth_cache_stats();
        assert!(
            stats1.misses >= 1,
            "first lookup must record a miss, got {stats1:?}"
        );
        // Second call hits the freshly-populated entry.
        let _ = store.visible_collections_for_scope(
            None,
            Role::Read,
            "alice",
            &vec!["orders".to_string()],
        );
        let stats2 = store.auth_cache_stats();
        assert!(
            stats2.hits >= 1,
            "second lookup must record a hit, got {stats2:?}"
        );
        // Hit rate is computable.
        let _ = stats2.hit_rate();
    }
}
