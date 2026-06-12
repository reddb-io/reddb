//! Catalog AST-leaf descriptors (ADR 0053, RQL Phase 2 S4b).
//!
//! These descriptor types are referenced directly by the canonical SQL AST
//! (`CreateTableQuery.{subscriptions, analytics_config}` and the `ALTER TABLE`
//! event/analytics operations). Re-homing them into the neutral keystone crate
//! removes a `QueryExpr -> reddb-server` leaf edge so the AST can later move to
//! `reddb-io-rql` with no server dependency.
//!
//! The server's `crate::catalog` module keeps a re-export shim so existing
//! call-sites stay untouched (byte-faithful move, same pattern as #1061/#1062).

/// The logical multi-structure model a collection presents (table, graph,
/// vector, queue, …). Referenced as a field type by the canonical SQL AST
/// (`CreateCollectionQuery`/`CreateTableQuery` and their builders), so it
/// is re-homed here (ADR 0053, RQL Phase 2) to keep the AST free of a
/// `reddb-server` leaf edge. The server's `crate::catalog` re-export shim
/// keeps existing call-sites untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionModel {
    Table,
    Document,
    Graph,
    Vector,
    Hll,
    Sketch,
    Filter,
    Kv,
    Config,
    Vault,
    Mixed,
    TimeSeries,
    Queue,
    Metrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionOperation {
    Insert,
    Update,
    Delete,
}

impl SubscriptionOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }

    // `from_str` returns `Option`, not `Result` — different semantics from the
    // `std::str::FromStr` trait method, so the trait is intentionally not used.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_uppercase().as_str() {
            "INSERT" => Some(Self::Insert),
            "UPDATE" => Some(Self::Update),
            "DELETE" => Some(Self::Delete),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionDescriptor {
    /// Logical name for this subscription. Empty string for legacy unnamed subscriptions.
    pub name: String,
    pub source: String,
    pub target_queue: String,
    pub ops_filter: Vec<SubscriptionOperation>,
    pub where_filter: Option<String>,
    pub redact_fields: Vec<String>,
    pub enabled: bool,
    /// When true, events are routed to the bare `target_queue` regardless of
    /// the current tenant — a cluster-wide subscription. When false (default),
    /// events are namespaced as `{tenant}/{target_queue}` whenever a tenant
    /// context is active, enforcing per-tenant isolation.
    pub all_tenants: bool,
}

/// A graph-analytics output declared by `CREATE GRAPH ... WITH ANALYTICS (...)`.
///
/// Each variant maps to a family of pure graph algorithms (issues #795-#797)
/// and resolves as a virtual `<graph>.<output>` view returning that family's
/// native row shape. The `using` option selects the concrete algorithm inside
/// the family (e.g. `centrality (using = pagerank)`); the remaining options are
/// algorithm parameters carried verbatim into the executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnalyticsOutput {
    Communities,
    Components,
    Centrality,
}

impl AnalyticsOutput {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Communities => "communities",
            Self::Components => "components",
            Self::Centrality => "centrality",
        }
    }

    // `from_str` returns `Option`, not `Result` — different semantics from the
    // `std::str::FromStr` trait method, so the trait is intentionally not used.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "communities" => Some(Self::Communities),
            "components" => Some(Self::Components),
            "centrality" => Some(Self::Centrality),
            _ => None,
        }
    }
}

/// One enabled analytics output plus its declared options. Persisted in the
/// parent graph's `CollectionContract` (WAL-backed) and surfaced on the
/// `CollectionDescriptor` so the resolver can recognise `<graph>.<output>`.
///
/// `SHOW COLLECTIONS` behaviour (issue #800 HITL decision): analytics outputs
/// resolve as virtual `<graph>.<output>` views and are deliberately **not**
/// registered as top-level collections. They therefore never appear in
/// `SHOW COLLECTIONS` — not by default and not under `SHOW COLLECTIONS
/// INCLUDING INTERNAL` — keeping the parent graph's listing clean. Only the
/// parent graph collection is listed; its enabled outputs are introspectable
/// through this `analytics_config` on the parent's descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyticsViewDescriptor {
    pub output: AnalyticsOutput,
    /// `using = <algorithm>` — concrete algorithm within the output family.
    /// `None` resolves to the family default (louvain / connected-components /
    /// pagerank).
    pub algorithm: Option<String>,
    /// `resolution = <f64>` — Louvain resolution (γ) for `communities`.
    pub resolution: Option<f64>,
    /// `max_iterations = <i64>` — iteration cap for iterative centralities.
    pub max_iterations: Option<i64>,
    /// `tolerance = <f64>` — convergence tolerance for iterative centralities.
    pub tolerance: Option<f64>,
}
