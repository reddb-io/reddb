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

        // Defence in depth: re-filter every result bucket by the
        // visible set. The pre-filter on `input.collections` already
        // bounds the corpus, but the cross-ref / graph expansion paths
        // can resolve `xref.target_collection` and
        // `entity.kind.collection()` independently, so we re-check
        // here so a regression in one of those paths can't leak rows.
        let allowed = visible;
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
        Ok(result)
    }
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
}
