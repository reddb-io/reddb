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

/// Per-collection AI policy declared via DDL `WITH (...)` (PRD #1267,
/// issue #1271). Each modality block is optional; an absent block means
/// the collection opts out of that modality. Persisted in the
/// `CollectionContract` (versioned/migrated with the schema) and surfaced
/// via introspection.
///
/// This slice carries the *declaration* only — no enrichment/gating
/// behaviour. Provider/model capability validation against the matrix
/// (#1269) happens at DDL execution time in the server, where the
/// capability registry lives.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AiPolicy {
    /// `EMBED (...)` — which fields embed into which provider/model.
    pub embed: Option<EmbedPolicy>,
    /// `MODERATE (...)` — content moderation gate.
    pub moderate: Option<ModeratePolicy>,
    /// `VISION (...)` — image-reference understanding.
    pub vision: Option<VisionPolicy>,
}

impl AiPolicy {
    /// True when no modality block is declared. Callers treat an empty
    /// policy the same as an absent one (`None`).
    pub fn is_empty(&self) -> bool {
        self.embed.is_none() && self.moderate.is_none() && self.vision.is_none()
    }
}

/// `EMBED (fields = (...), provider = '..', model = '..')`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedPolicy {
    /// Source fields whose text is embedded.
    pub fields: Vec<String>,
    /// Provider token (e.g. `openai`).
    pub provider: String,
    /// Model name as written in the policy.
    pub model: String,
}

/// `MODERATE (fields = (...), provider, model, sync, degraded, on_reject,
/// hard_delete)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeratePolicy {
    /// Source fields screened by the moderation provider.
    pub fields: Vec<String>,
    pub provider: String,
    pub model: String,
    /// When true the moderation check is a synchronous gate on the write
    /// (`sync = true`); when false it runs out-of-band.
    pub sync_gate: bool,
    /// Behaviour when the moderation provider is unavailable.
    pub degraded_mode: ModerateDegradedMode,
    /// What happens to content that fails moderation.
    pub reject_action: ModerateRejectAction,
    /// When true (`hard_delete = true`), a quarantined row that
    /// re-moderates to a reject is hard-deleted instead of being
    /// tombstoned-and-retained for audit/appeal (the default). Opt-in
    /// per-collection because hard-delete forfeits the audit trail.
    pub hard_delete_on_reject: bool,
}

/// Behaviour when the moderation provider can't be reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModerateDegradedMode {
    /// Fail open — let the write through unmoderated (default).
    #[default]
    Open,
    /// Fail closed — reject the write when moderation can't run.
    Closed,
}

impl ModerateDegradedMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }

    // `from_str` returns `Option`, not `Result` — different semantics from the
    // `std::str::FromStr` trait method, so the trait is intentionally not used.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// Disposition applied to content that fails moderation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModerateRejectAction {
    /// Reject the write outright (default).
    #[default]
    Reject,
    /// Accept the write but mark it as flagged.
    Flag,
    /// Accept the write with the offending content redacted.
    Redact,
}

impl ModerateRejectAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reject => "reject",
            Self::Flag => "flag",
            Self::Redact => "redact",
        }
    }

    // `from_str` returns `Option`, not `Result` — different semantics from the
    // `std::str::FromStr` trait method, so the trait is intentionally not used.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "reject" => Some(Self::Reject),
            "flag" => Some(Self::Flag),
            "redact" => Some(Self::Redact),
            _ => None,
        }
    }
}

/// `VISION (image_field = '..', outputs = (...), provider, model)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisionPolicy {
    /// Field holding the image reference (URL / blob id).
    pub image_field: String,
    /// Output kinds requested (e.g. `caption`, `tags`, `objects`).
    pub output_kinds: Vec<String>,
    pub provider: String,
    pub model: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moderate_degraded_mode_round_trips() {
        for (mode, name) in [
            (ModerateDegradedMode::Open, "open"),
            (ModerateDegradedMode::Closed, "closed"),
        ] {
            assert_eq!(mode.as_str(), name);
            assert_eq!(
                ModerateDegradedMode::from_str(&name.to_ascii_uppercase()),
                Some(mode)
            );
        }
        assert_eq!(ModerateDegradedMode::from_str("ajar"), None);
        assert_eq!(ModerateDegradedMode::default(), ModerateDegradedMode::Open);
    }

    #[test]
    fn moderate_reject_action_round_trips() {
        for (action, name) in [
            (ModerateRejectAction::Reject, "reject"),
            (ModerateRejectAction::Flag, "flag"),
            (ModerateRejectAction::Redact, "redact"),
        ] {
            assert_eq!(action.as_str(), name);
            assert_eq!(
                ModerateRejectAction::from_str(&name.to_ascii_uppercase()),
                Some(action)
            );
        }
        assert_eq!(ModerateRejectAction::from_str("ban"), None);
        assert_eq!(
            ModerateRejectAction::default(),
            ModerateRejectAction::Reject
        );
    }

    #[test]
    fn ai_policy_empty_detection() {
        assert!(AiPolicy::default().is_empty());
        let with_embed = AiPolicy {
            embed: Some(EmbedPolicy {
                fields: vec!["body".to_string()],
                provider: "openai".to_string(),
                model: "text-embedding-3-small".to_string(),
            }),
            ..AiPolicy::default()
        };
        assert!(!with_embed.is_empty());
    }

    #[test]
    fn subscription_operations_round_trip_canonical_names() {
        for (op, name) in [
            (SubscriptionOperation::Insert, "INSERT"),
            (SubscriptionOperation::Update, "UPDATE"),
            (SubscriptionOperation::Delete, "DELETE"),
        ] {
            assert_eq!(op.as_str(), name);
            assert_eq!(
                SubscriptionOperation::from_str(&name.to_ascii_lowercase()),
                Some(op)
            );
        }
        assert_eq!(SubscriptionOperation::from_str("UPSERT"), None);
    }

    #[test]
    fn analytics_outputs_round_trip_lowercase_names() {
        for (output, name) in [
            (AnalyticsOutput::Communities, "communities"),
            (AnalyticsOutput::Components, "components"),
            (AnalyticsOutput::Centrality, "centrality"),
        ] {
            assert_eq!(output.as_str(), name);
            assert_eq!(
                AnalyticsOutput::from_str(&name.to_ascii_uppercase()),
                Some(output)
            );
        }
        assert_eq!(AnalyticsOutput::from_str("pagerank"), None);
    }

    #[test]
    fn descriptors_are_plain_data_carriers() {
        let subscription = SubscriptionDescriptor {
            name: "audit".to_string(),
            source: "orders".to_string(),
            target_queue: "events".to_string(),
            ops_filter: vec![SubscriptionOperation::Insert, SubscriptionOperation::Delete],
            where_filter: Some("amount > 0".to_string()),
            redact_fields: vec!["secret".to_string()],
            enabled: true,
            all_tenants: false,
        };
        assert_eq!(subscription.ops_filter[1].as_str(), "DELETE");

        let view = AnalyticsViewDescriptor {
            output: AnalyticsOutput::Centrality,
            algorithm: Some("pagerank".to_string()),
            resolution: Some(1.0),
            max_iterations: Some(20),
            tolerance: Some(0.001),
        };
        assert_eq!(view.output.as_str(), "centrality");
        assert_eq!(view.algorithm.as_deref(), Some("pagerank"));
    }
}
