//! Vector + TurboQuant introspection — issue #743.
//!
//! Red UI vector toolbars need to render, for every vector collection:
//!
//!   - "what is this collection?" — source column / payload field,
//!     dimensions, metric, index type, row count, whether SEARCH is
//!     currently answerable;
//!   - "what is the artifact?" — build state, whether an encoded
//!     artifact is on disk, the stable TurboQuant / TurboVec
//!     parameters, whether we are serving searches off a scalar
//!     fallback, rebuild progress (when one is in flight), and the
//!     last error if the most recent build failed.
//!
//! The frontend must answer those questions **without** depending on
//! the layout of `engine::vector_store`, `engine::turboquant::*`,
//! segment-state enums, or any on-disk binary shape — those internals
//! churn faster than the UI contract can. This module is the stable
//! surface that mediates between them: a small set of plain-data types
//! (`VectorMetadata`, `ArtifactMetadata`, the `ArtifactState` enum and
//! its companions) plus an in-memory registry the runtime publishes to
//! whenever a collection's vector or artifact state changes.
//!
//! Lifecycle model — the five states Red UI distinguishes:
//!
//! - [`ArtifactState::Unavailable`] — no artifact has ever been built
//!   for this collection (e.g. it was just created, or the artifact
//!   was explicitly dropped). SEARCH against it falls through to the
//!   row store or returns NOT_READY depending on the kind.
//! - [`ArtifactState::Building`] — a background rebuild is in flight.
//!   `rebuild_progress_pct` may be populated when the builder can
//!   estimate it. SEARCH callers see NOT_READY until the build
//!   completes.
//! - [`ArtifactState::Ready`] — the artifact is loaded, current with
//!   the row store, and SEARCH is served from it.
//! - [`ArtifactState::Failed`] — the last build attempt errored;
//!   `last_error` carries the operator-facing message. SEARCH may
//!   fall through to the scalar path if `scalar_fallback_active` is
//!   true, otherwise it returns NOT_READY.
//! - [`ArtifactState::Fallback`] — the artifact is intentionally not
//!   the primary search path right now (e.g. dimensions changed and
//!   we are serving scalar until the rebuild finishes). Distinct from
//!   `Building` because the artifact on disk may itself be `Ready`
//!   for *some* shape — the runtime has just decided not to use it.
//!
//! Independence from internal storage modules is the load-bearing
//! property here. The registry stores `String` enum tags for index
//! type / metric / param family rather than re-exporting the engine
//! enums, precisely so a future internal rename in
//! `engine::turboquant` does not force a Red UI release. The follow-up
//! slice that wires concrete publish points from the engine into this
//! registry is tracked in PRD #735; the public Rust surface here is
//! the contract those publish points target and does not change when
//! they land.

use std::collections::HashMap;
use std::sync::Mutex;

/// Stable lifecycle bucket for a vector collection's search artifact.
/// Snapshot consumers (Red UI, virtual tables) read this flag and
/// never re-derive the rule from internal engine state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactState {
    /// No artifact has ever been built for this collection.
    Unavailable,
    /// A background rebuild is in flight.
    Building,
    /// Artifact is loaded and serving SEARCH.
    Ready,
    /// The most recent build attempt failed; see `last_error`.
    Failed,
    /// The runtime is intentionally serving SEARCH off a scalar
    /// fallback path rather than the artifact (e.g. dimension drift,
    /// codebook mismatch). Distinct from `Building` and `Failed`.
    Fallback,
}

impl ArtifactState {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactState::Unavailable => "unavailable",
            ArtifactState::Building => "building",
            ArtifactState::Ready => "ready",
            ArtifactState::Failed => "failed",
            ArtifactState::Fallback => "fallback",
        }
    }
}

/// Where a vector collection's vectors come from. Red UI surfaces this
/// so an operator can tell "this is a typed `VECTOR` column" apart
/// from "this is a JSON payload field we lift at ingest time".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VectorSource {
    /// Typed `VECTOR(<dim>)` column on a SQL collection. The string is
    /// the column name.
    Column(String),
    /// Embedded payload field on a document/blob collection. The
    /// string is the dotted path.
    Payload(String),
}

impl VectorSource {
    pub fn as_str(&self) -> &str {
        match self {
            VectorSource::Column(s) | VectorSource::Payload(s) => s.as_str(),
        }
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            VectorSource::Column(_) => "column",
            VectorSource::Payload(_) => "payload",
        }
    }
}

/// Operator-facing index kind tag. We deliberately ship this as a
/// small string-backed enum rather than re-exporting
/// `engine::IndexType`, so an internal rename of the engine enum does
/// not break the UI contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorIndexType {
    Hnsw,
    Ivf,
    TurboQuant,
    TurboVec,
    /// Scalar / brute-force scan. Stable name even though "no index"
    /// is the underlying truth.
    Scalar,
}

impl VectorIndexType {
    pub fn as_str(self) -> &'static str {
        match self {
            VectorIndexType::Hnsw => "hnsw",
            VectorIndexType::Ivf => "ivf",
            VectorIndexType::TurboQuant => "turboquant",
            VectorIndexType::TurboVec => "turbovec",
            VectorIndexType::Scalar => "scalar",
        }
    }
}

/// Distance metric, as a stable string contract independent of
/// `engine::DistanceMetric`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    Cosine,
    InnerProduct,
    L2,
}

impl DistanceMetric {
    pub fn as_str(self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "cosine",
            DistanceMetric::InnerProduct => "inner_product",
            DistanceMetric::L2 => "l2",
        }
    }
}

/// Stable subset of the TurboQuant / TurboVec parameters Red UI is
/// allowed to display. Anything that is not yet load-bearing or that
/// the engine considers internal is intentionally omitted — adding a
/// field here is a contract change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurboArtifactParams {
    /// Family tag — `"turboquant"` or `"turbovec"`. Free string so
    /// future variants don't force an enum migration on the UI.
    pub family: String,
    /// Number of codebook subspaces (`M` in PQ-style schemes).
    pub subspaces: Option<u32>,
    /// Bits per code symbol.
    pub bits_per_code: Option<u32>,
    /// Codebook entry count per subspace (typically `1 << bits_per_code`).
    pub codebook_size: Option<u32>,
}

/// Per-collection metadata about the vector data itself. Independent
/// of artifact state — a collection can have meaningful metadata even
/// when its artifact is `Unavailable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorMetadata {
    pub collection: String,
    pub source: VectorSource,
    pub dimensions: u32,
    pub metric: DistanceMetric,
    pub index_type: VectorIndexType,
    pub row_count: u64,
    /// Whether SEARCH against this collection can return rows right
    /// now. Convenience flag derived from artifact state + fallback
    /// availability at publish time, so the UI does not have to
    /// re-derive it from the artifact row.
    pub search_capable: bool,
}

/// Per-collection metadata about the on-disk / in-memory artifact
/// (TurboQuant / TurboVec / scalar fallback). Carries enough for Red
/// UI to render the toolbar without inspecting the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMetadata {
    pub collection: String,
    pub state: ArtifactState,
    /// True when an encoded artifact (e.g. a `.tv` snapshot) is
    /// present and loadable; orthogonal to `state` because an
    /// artifact can be present-but-`Fallback` or
    /// present-but-`Building` (a newer one is being rebuilt over it).
    pub encoded_artifact_present: bool,
    /// Stable, operator-facing slice of the TurboQuant / TurboVec
    /// parameters. `None` for scalar-only collections.
    pub params: Option<TurboArtifactParams>,
    /// Whether SEARCH is currently being answered (or could be) from
    /// the scalar fallback path rather than the artifact.
    pub scalar_fallback_active: bool,
    /// 0..=100, when the builder can estimate it; `None` otherwise
    /// (including outside `Building`).
    pub rebuild_progress_pct: Option<u8>,
    /// Operator-facing message from the most recent build failure.
    /// Populated in `Failed`; may also be populated in `Fallback` to
    /// explain *why* we are falling back. Cleared when the next build
    /// succeeds.
    pub last_error: Option<String>,
}

/// One row of vector + artifact introspection. Snapshot consumers get
/// the two halves bundled so the UI does not have to join on
/// `collection` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorIntrospection {
    pub vector: VectorMetadata,
    pub artifact: ArtifactMetadata,
}

#[derive(Debug, Clone)]
struct Entry {
    vector: VectorMetadata,
    artifact: ArtifactMetadata,
}

/// Process-local registry the runtime publishes vector/artifact state
/// into. The shape mirrors `storage::queue::presence::ConsumerPresenceRegistry`
/// from issue #742: cheap mutex + small hashmap is the right fit
/// because the cardinality is bounded by the operator's collection
/// count (dozens, not millions) and reads are dominated by snapshot.
#[derive(Debug, Default)]
pub struct VectorIntrospectionRegistry {
    entries: Mutex<HashMap<String, Entry>>,
}

impl VectorIntrospectionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish (or replace) the full introspection row for a
    /// collection. Callers in the engine compute the typed shape once
    /// at the right moment (collection create, artifact build start /
    /// finish, fallback toggle) and hand it over; the registry does
    /// not try to derive anything.
    pub fn publish(&self, vector: VectorMetadata, artifact: ArtifactMetadata) {
        debug_assert_eq!(
            vector.collection, artifact.collection,
            "vector and artifact metadata must agree on collection name"
        );
        let key = vector.collection.clone();
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        map.insert(key, Entry { vector, artifact });
    }

    /// Replace only the artifact half (build start/finish, fallback
    /// toggle, error). No-op if the collection has not been published
    /// yet, because the artifact row alone has no useful meaning
    /// without the vector row it sits next to.
    pub fn update_artifact(&self, artifact: ArtifactMetadata) -> bool {
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        match map.get_mut(&artifact.collection) {
            Some(entry) => {
                // Keep `search_capable` consistent with the new
                // artifact state. Specifically: a Ready artifact, or a
                // Fallback / Failed with the scalar fallback active,
                // can answer SEARCH; everything else cannot.
                let capable = match artifact.state {
                    ArtifactState::Ready => true,
                    ArtifactState::Fallback | ArtifactState::Failed => {
                        artifact.scalar_fallback_active
                    }
                    ArtifactState::Building | ArtifactState::Unavailable => {
                        artifact.scalar_fallback_active
                    }
                };
                entry.vector.search_capable = capable;
                entry.artifact = artifact;
                true
            }
            None => false,
        }
    }

    /// Drop a collection's introspection row (e.g. on `DROP COLLECTION`).
    pub fn forget(&self, collection: &str) -> bool {
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        map.remove(collection).is_some()
    }

    /// Snapshot of every tracked collection, deterministically ordered
    /// by `collection` so test assertions and Red UI tables both see
    /// a stable shape.
    pub fn snapshot(&self) -> Vec<VectorIntrospection> {
        let map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let mut rows: Vec<VectorIntrospection> = map
            .values()
            .map(|e| VectorIntrospection {
                vector: e.vector.clone(),
                artifact: e.artifact.clone(),
            })
            .collect();
        rows.sort_by(|a, b| a.vector.collection.cmp(&b.vector.collection));
        rows
    }

    /// Single-collection lookup, for the per-collection metadata
    /// endpoint Red UI hits when it opens one vector's toolbar.
    pub fn get(&self, collection: &str) -> Option<VectorIntrospection> {
        let map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        map.get(collection).map(|e| VectorIntrospection {
            vector: e.vector.clone(),
            artifact: e.artifact.clone(),
        })
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_vector(collection: &str, dim: u32) -> VectorMetadata {
        VectorMetadata {
            collection: collection.into(),
            source: VectorSource::Column("embedding".into()),
            dimensions: dim,
            metric: DistanceMetric::Cosine,
            index_type: VectorIndexType::TurboQuant,
            row_count: 1_024,
            search_capable: true,
        }
    }

    fn ready_artifact(collection: &str) -> ArtifactMetadata {
        ArtifactMetadata {
            collection: collection.into(),
            state: ArtifactState::Ready,
            encoded_artifact_present: true,
            params: Some(TurboArtifactParams {
                family: "turboquant".into(),
                subspaces: Some(8),
                bits_per_code: Some(8),
                codebook_size: Some(256),
            }),
            scalar_fallback_active: false,
            rebuild_progress_pct: None,
            last_error: None,
        }
    }

    /// Acceptance: "Tests cover a basic vector collection".
    ///
    /// A ready TurboQuant collection round-trips through the registry
    /// with every field intact, surfaces as `search_capable`, and is
    /// reachable by both `snapshot()` and `get()`.
    #[test]
    fn ready_collection_round_trips_through_registry() {
        let reg = VectorIntrospectionRegistry::new();
        reg.publish(ready_vector("docs", 384), ready_artifact("docs"));

        assert_eq!(reg.len(), 1);
        let row = reg.get("docs").expect("collection was published");
        assert_eq!(row.vector.collection, "docs");
        assert_eq!(row.vector.dimensions, 384);
        assert_eq!(row.vector.metric, DistanceMetric::Cosine);
        assert_eq!(row.vector.index_type, VectorIndexType::TurboQuant);
        assert_eq!(row.vector.row_count, 1_024);
        assert!(row.vector.search_capable);
        assert!(matches!(row.vector.source, VectorSource::Column(ref c) if c == "embedding"));

        assert_eq!(row.artifact.state, ArtifactState::Ready);
        assert!(row.artifact.encoded_artifact_present);
        assert!(!row.artifact.scalar_fallback_active);
        assert!(row.artifact.last_error.is_none());
        let params = row.artifact.params.expect("turbo params present");
        assert_eq!(params.family, "turboquant");
        assert_eq!(params.subspaces, Some(8));
        assert_eq!(params.bits_per_code, Some(8));
        assert_eq!(params.codebook_size, Some(256));

        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].vector.collection, "docs");
    }

    /// Acceptance: "Tests cover ... at least one unavailable or
    /// fallback artifact state."
    ///
    /// Encodes both: a freshly-created collection lands as
    /// `Unavailable` and not search-capable; switching it to
    /// `Fallback` with `scalar_fallback_active=true` flips
    /// `search_capable` back on without losing the artifact row.
    #[test]
    fn unavailable_then_fallback_states_are_distinguishable() {
        let reg = VectorIntrospectionRegistry::new();

        let mut vector = ready_vector("embeddings", 128);
        vector.row_count = 0;
        vector.search_capable = false;
        let unavailable = ArtifactMetadata {
            collection: "embeddings".into(),
            state: ArtifactState::Unavailable,
            encoded_artifact_present: false,
            params: None,
            scalar_fallback_active: false,
            rebuild_progress_pct: None,
            last_error: None,
        };
        reg.publish(vector, unavailable);

        let row = reg.get("embeddings").unwrap();
        assert_eq!(row.artifact.state, ArtifactState::Unavailable);
        assert!(!row.artifact.encoded_artifact_present);
        assert!(row.artifact.params.is_none());
        assert!(!row.vector.search_capable);

        let fallback = ArtifactMetadata {
            collection: "embeddings".into(),
            state: ArtifactState::Fallback,
            encoded_artifact_present: true,
            params: Some(TurboArtifactParams {
                family: "turbovec".into(),
                subspaces: Some(4),
                bits_per_code: Some(4),
                codebook_size: Some(16),
            }),
            scalar_fallback_active: true,
            rebuild_progress_pct: None,
            last_error: Some("dimension drift; serving scalar until rebuild".into()),
        };
        assert!(reg.update_artifact(fallback));

        let row = reg.get("embeddings").unwrap();
        assert_eq!(row.artifact.state, ArtifactState::Fallback);
        assert!(row.artifact.scalar_fallback_active);
        assert!(row
            .artifact
            .last_error
            .as_deref()
            .unwrap()
            .contains("scalar"));
        assert!(
            row.vector.search_capable,
            "scalar fallback keeps SEARCH alive even when the artifact is in Fallback"
        );
    }

    /// Acceptance: "The contract distinguishes unavailable, building,
    /// ready, failed, and fallback states."
    ///
    /// Walks the artifact through Building → Failed → Ready and
    /// verifies `search_capable` tracks the rules in
    /// `update_artifact`: only Ready (or a state with the scalar
    /// fallback active) keeps SEARCH alive.
    #[test]
    fn artifact_states_distinct_and_search_capability_tracks_them() {
        let reg = VectorIntrospectionRegistry::new();
        reg.publish(ready_vector("k", 64), ready_artifact("k"));

        let building = ArtifactMetadata {
            collection: "k".into(),
            state: ArtifactState::Building,
            encoded_artifact_present: false,
            params: None,
            scalar_fallback_active: false,
            rebuild_progress_pct: Some(42),
            last_error: None,
        };
        assert!(reg.update_artifact(building));
        let row = reg.get("k").unwrap();
        assert_eq!(row.artifact.state, ArtifactState::Building);
        assert_eq!(row.artifact.rebuild_progress_pct, Some(42));
        assert!(
            !row.vector.search_capable,
            "Building without fallback is not search-capable"
        );

        let failed = ArtifactMetadata {
            collection: "k".into(),
            state: ArtifactState::Failed,
            encoded_artifact_present: false,
            params: None,
            scalar_fallback_active: false,
            rebuild_progress_pct: None,
            last_error: Some("codec error: subspace=3 page=12".into()),
        };
        assert!(reg.update_artifact(failed));
        let row = reg.get("k").unwrap();
        assert_eq!(row.artifact.state, ArtifactState::Failed);
        assert!(!row.vector.search_capable);
        assert_eq!(
            row.artifact.last_error.as_deref(),
            Some("codec error: subspace=3 page=12")
        );

        // Recover to Ready — search_capable must flip back on and the
        // stale error must be cleared by the caller (the registry
        // stores what it is handed, by design).
        assert!(reg.update_artifact(ready_artifact("k")));
        let row = reg.get("k").unwrap();
        assert_eq!(row.artifact.state, ArtifactState::Ready);
        assert!(row.vector.search_capable);
        assert!(row.artifact.last_error.is_none());
    }

    #[test]
    fn update_artifact_no_ops_for_unpublished_collection() {
        let reg = VectorIntrospectionRegistry::new();
        let orphan = ArtifactMetadata {
            collection: "ghost".into(),
            state: ArtifactState::Building,
            encoded_artifact_present: false,
            params: None,
            scalar_fallback_active: false,
            rebuild_progress_pct: None,
            last_error: None,
        };
        assert!(!reg.update_artifact(orphan));
        assert!(reg.is_empty());
    }

    #[test]
    fn forget_drops_collection() {
        let reg = VectorIntrospectionRegistry::new();
        reg.publish(ready_vector("a", 8), ready_artifact("a"));
        reg.publish(ready_vector("b", 8), ready_artifact("b"));
        assert!(reg.forget("a"));
        assert!(!reg.forget("a"), "second forget no-ops");
        let names: Vec<_> = reg
            .snapshot()
            .into_iter()
            .map(|r| r.vector.collection)
            .collect();
        assert_eq!(names, vec!["b".to_string()]);
    }

    #[test]
    fn snapshot_is_deterministically_ordered() {
        let reg = VectorIntrospectionRegistry::new();
        // Insert shuffled.
        reg.publish(ready_vector("zeta", 8), ready_artifact("zeta"));
        reg.publish(ready_vector("alpha", 8), ready_artifact("alpha"));
        reg.publish(ready_vector("mu", 8), ready_artifact("mu"));

        let names: Vec<_> = reg
            .snapshot()
            .into_iter()
            .map(|r| r.vector.collection)
            .collect();
        assert_eq!(
            names,
            vec!["alpha".to_string(), "mu".to_string(), "zeta".to_string()]
        );
    }

    /// The five public `ArtifactState` variants must serialize to the
    /// stable string tags Red UI relies on. Treat this as the contract
    /// pin — changing any of these strings is a breaking change.
    #[test]
    fn artifact_state_strings_are_stable() {
        assert_eq!(ArtifactState::Unavailable.as_str(), "unavailable");
        assert_eq!(ArtifactState::Building.as_str(), "building");
        assert_eq!(ArtifactState::Ready.as_str(), "ready");
        assert_eq!(ArtifactState::Failed.as_str(), "failed");
        assert_eq!(ArtifactState::Fallback.as_str(), "fallback");
    }
}
