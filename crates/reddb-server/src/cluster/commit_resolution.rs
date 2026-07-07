//! Commit policy resolution for multi-writer clusters (issue #1001, PRD #987).
//!
//! A cluster has one global default [`CommitPolicy`], and a collection may
//! declare a stricter or looser override when its model semantics justify it
//! (see the [clustering glossary](../../../.red/context/clustering.md) entries
//! *Commit policy* and *Ephemeral-local commit*). This module is the single
//! deterministic place that combines those two inputs into the **effective**
//! policy a write actually commits under, and enforces the one safety rule that
//! the raw [`CommitPolicy`] type cannot express on its own:
//!
//! > Durable transactional, queue, audit, config, and vault collections must not
//! > *silently* use local-only acknowledgement once HA intent is declared.
//! > Only collections explicitly declared ephemeral/cache-like may opt into
//! > `local` commit, and they do so with documented failover semantics.
//!
//! ## Why a resolver rather than a field on the collection
//!
//! The effective policy is a function of three independent inputs — the cluster
//! default, the per-collection override, and whether the deployment has declared
//! HA intent — and the guardrail couples all three. Resolving them ad hoc at each
//! call site (write admission *and* failover eligibility both need the answer)
//! would let the two paths drift, so a misconfigured durable collection could be
//! admitted with `local` on the write path while failover still believed it was
//! quorum-durable. A single pure resolver keeps both paths reading the same
//! decision and makes the guardrail testable in isolation.
//!
//! ## Resolution
//!
//! 1. The effective policy is the collection override if present, otherwise the
//!    cluster default ([`ResolutionSource`] records which won).
//! 2. If the effective policy is local-only acknowledgement (`Local`, or the
//!    degenerate `AckN(0)` which [the policy docs](super::super::replication::commit_policy)
//!    define as equivalent to `Local`) **and** HA intent is declared:
//!    - a **durable** model ([`CollectionDataModel::is_durable`]) is rejected with
//!      [`CommitPolicyViolation::DurableLocalUnderHa`] — fail closed, the caller
//!      must not admit writes under a silently-degraded policy.
//!    - an **ephemeral/cache-like** model is allowed, tagged
//!      [`GuardrailDisposition::EphemeralLocalAllowed`] so the decision is
//!      explicit in the audit trail.
//! 3. Otherwise the resolution succeeds; the guardrail is
//!    [`GuardrailDisposition::Satisfied`] for a durable model under declared HA
//!    intent (the effective policy is genuinely durable), or
//!    [`GuardrailDisposition::NotApplicable`] when HA intent is not declared.
//!
//! The resolved policy also reports its **failover eligibility**
//! ([`CommitPolicyResolution::failover_eligibility`]): a durable policy means a
//! candidate may be promoted only if its log covers the range commit watermark,
//! while a local-ack policy carries an explicit data-loss window — the documented
//! failover semantics ephemeral/cache collections accept in exchange for `local`.

use crate::replication::CommitPolicy;

/// The durability model a collection declares for itself. The first five are
/// **durable** models whose data must survive a single-node loss; the last two
/// are explicitly **local-eligible** — losing their most recent unreplicated
/// writes on failover is an accepted trade for lower write latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionDataModel {
    /// Durable transactional records — the default model for user data.
    Transactional,
    /// Durable work-queue collection (at-least-once delivery semantics).
    Queue,
    /// Append-only audit log.
    Audit,
    /// Cluster/application configuration.
    Config,
    /// Secret/credential material.
    Vault,
    /// Explicitly ephemeral data with no durability expectation.
    Ephemeral,
    /// Cache-like data that can be rebuilt from a source of truth.
    Cache,
}

impl CollectionDataModel {
    /// `true` for models whose data must survive a single-node loss and so may
    /// never silently acknowledge a write locally under declared HA intent.
    pub fn is_durable(self) -> bool {
        match self {
            Self::Transactional | Self::Queue | Self::Audit | Self::Config | Self::Vault => true,
            Self::Ephemeral | Self::Cache => false,
        }
    }

    /// `true` for the explicitly local-eligible models (`Ephemeral`, `Cache`)
    /// that may opt into local commit even under declared HA intent.
    pub fn allows_ephemeral_local(self) -> bool {
        !self.is_durable()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Transactional => "transactional",
            Self::Queue => "queue",
            Self::Audit => "audit",
            Self::Config => "config",
            Self::Vault => "vault",
            Self::Ephemeral => "ephemeral",
            Self::Cache => "cache",
        }
    }
}

/// Whether the deployment has declared HA intent. The guardrail only restricts
/// local-only acknowledgement once intent is [`Declared`](Self::Declared); a
/// single-writer / non-HA deployment resolves policies without restriction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HaIntent {
    /// Multi-writer HA mode: durable models may not silently use `local`.
    Declared,
    /// No HA intent declared — the guardrail does not apply.
    #[default]
    None,
}

impl HaIntent {
    pub fn is_declared(self) -> bool {
        matches!(self, Self::Declared)
    }

    /// Parse from `RED_CLUSTER_HA_INTENT`. Truthy (`true`/`1`/`yes`/`declared`)
    /// means [`Declared`](Self::Declared); anything else (including unset) means
    /// [`None`](Self::None) so the guardrail stays off unless opted into.
    pub fn from_env() -> Self {
        match std::env::var("RED_CLUSTER_HA_INTENT") {
            Ok(raw) => Self::parse(raw.trim()),
            Err(_) => Self::None,
        }
    }

    pub fn parse(raw: &str) -> Self {
        let t = raw.trim();
        if t.eq_ignore_ascii_case("true")
            || t == "1"
            || t.eq_ignore_ascii_case("yes")
            || t.eq_ignore_ascii_case("declared")
        {
            Self::Declared
        } else {
            Self::None
        }
    }
}

/// Which input supplied the effective policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionSource {
    /// No collection override; the cluster global default applied.
    ClusterDefault,
    /// The collection's own override applied.
    CollectionOverride,
    /// A per-request override strengthened the already-resolved floor.
    RequestOverride,
}

impl ResolutionSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::ClusterDefault => "cluster_default",
            Self::CollectionOverride => "collection_override",
            Self::RequestOverride => "request_override",
        }
    }
}

/// How the ephemeral-local guardrail dispositioned a successful resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardrailDisposition {
    /// HA intent not declared — the guardrail did not run.
    NotApplicable,
    /// Durable model under declared HA intent with a genuinely durable effective
    /// policy: the guardrail ran and was satisfied.
    Satisfied,
    /// Ephemeral/cache-like model explicitly permitted to use local commit under
    /// declared HA intent (documented failover semantics apply).
    EphemeralLocalAllowed,
}

/// Failover implication of a resolved commit policy. Consumed by failover
/// eligibility: a durable policy gates promotion on watermark coverage, while a
/// local-ack policy admits an explicit data-loss window on the promoted node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverEligibility {
    /// The effective policy is durable: a candidate may be promoted only if its
    /// applied log covers the range commit watermark.
    RequiresWatermarkCoverage,
    /// The effective policy is local-only: a promoted candidate may not have the
    /// failed owner's most recent local-only writes — an accepted, documented
    /// loss window for ephemeral/cache-like data.
    LocalAckDataLossWindow,
}

/// The deterministic outcome of resolving a cluster default + collection
/// override + HA intent against a collection's data model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitPolicyResolution {
    /// The policy the write actually commits under.
    pub effective: CommitPolicy,
    /// Which input supplied [`effective`](Self::effective).
    pub source: ResolutionSource,
    /// How the guardrail dispositioned this resolution.
    pub guardrail: GuardrailDisposition,
}

impl CommitPolicyResolution {
    /// `true` when the effective policy requires durability beyond the local WAL,
    /// i.e. failover must gate promotion on range-commit-watermark coverage.
    pub fn requires_durable_watermark(&self) -> bool {
        !is_local_ack(self.effective)
    }

    /// Failover implication of the resolved policy. See [`FailoverEligibility`].
    pub fn failover_eligibility(&self) -> FailoverEligibility {
        if self.requires_durable_watermark() {
            FailoverEligibility::RequiresWatermarkCoverage
        } else {
            FailoverEligibility::LocalAckDataLossWindow
        }
    }
}

/// Rejection raised when resolution would silently degrade a durable model to
/// local-only acknowledgement under declared HA intent. The caller must fail
/// closed rather than admit writes under the degraded policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitPolicyViolation {
    /// A durable model resolved to local-only acknowledgement under declared HA
    /// intent. `source` records whether the offending policy came from the
    /// cluster default or the collection's own override.
    DurableLocalUnderHa {
        model: CollectionDataModel,
        source: ResolutionSource,
    },
    /// A per-request override attempted to weaken the already-resolved floor.
    RequestBelowResolvedFloor {
        floor: CommitPolicy,
        requested: CommitPolicy,
    },
}

impl CommitPolicyViolation {
    pub fn message(&self) -> String {
        match self {
            Self::DurableLocalUnderHa { model, source } => format!(
                "durable collection model '{}' may not use local-only commit acknowledgement \
                 under declared HA intent (policy source: {})",
                model.label(),
                source.label()
            ),
            Self::RequestBelowResolvedFloor { floor, requested } => format!(
                "per-request commit policy '{}' is weaker than resolved floor '{}'",
                requested.detail_label(),
                floor.detail_label()
            ),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::DurableLocalUnderHa { .. } => "DURABLE_LOCAL_UNDER_HA",
            Self::RequestBelowResolvedFloor { .. } => "COMMIT_POLICY_BELOW_FLOOR",
        }
    }
}

impl std::fmt::Display for CommitPolicyViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for CommitPolicyViolation {}

/// `true` when `policy` acknowledges a commit on local WAL durability alone:
/// `Local`, or the degenerate `AckN(0)` the policy docs define as equivalent.
pub fn is_local_ack(policy: CommitPolicy) -> bool {
    matches!(policy, CommitPolicy::Local | CommitPolicy::AckN(0))
}

fn durability_rank(policy: CommitPolicy) -> u64 {
    match policy {
        CommitPolicy::Local | CommitPolicy::AckN(0) => 0,
        CommitPolicy::RemoteWal => 10,
        CommitPolicy::AckN(n) => 100 + u64::from(n),
        CommitPolicy::Quorum => 10_000,
    }
}

/// Apply an optional per-request override to an already-resolved floor.
///
/// The request may strengthen durability for one write, but it may not weaken
/// the floor chosen by collection/HA resolution. Weakening is rejected rather
/// than clamped so callers can surface a typed client error.
pub fn resolve_request_commit_policy(
    floor: CommitPolicyResolution,
    request_override: Option<CommitPolicy>,
) -> Result<CommitPolicyResolution, CommitPolicyViolation> {
    let Some(requested) = request_override else {
        return Ok(floor);
    };

    if durability_rank(requested) < durability_rank(floor.effective) {
        return Err(CommitPolicyViolation::RequestBelowResolvedFloor {
            floor: floor.effective,
            requested,
        });
    }

    Ok(CommitPolicyResolution {
        effective: requested,
        source: ResolutionSource::RequestOverride,
        guardrail: floor.guardrail,
    })
}

/// Deterministically resolve the effective commit policy for one collection.
///
/// `cluster_default` is the global default; `collection_override` is the
/// collection's declared override (if any); `model` is the collection's
/// durability model; `ha_intent` is whether the deployment declared HA intent.
///
/// Returns the resolved policy, or [`CommitPolicyViolation`] when the guardrail
/// rejects a durable model degraded to local-only acknowledgement under HA
/// intent. The function is pure and side-effect free.
pub fn resolve_commit_policy(
    cluster_default: CommitPolicy,
    collection_override: Option<CommitPolicy>,
    model: CollectionDataModel,
    ha_intent: HaIntent,
) -> Result<CommitPolicyResolution, CommitPolicyViolation> {
    let (effective, source) = match collection_override {
        Some(p) => (p, ResolutionSource::CollectionOverride),
        None => (cluster_default, ResolutionSource::ClusterDefault),
    };

    let guardrail = if !ha_intent.is_declared() {
        // No HA intent: the guardrail does not constrain the resolution.
        GuardrailDisposition::NotApplicable
    } else if is_local_ack(effective) {
        if model.is_durable() {
            return Err(CommitPolicyViolation::DurableLocalUnderHa { model, source });
        }
        // Ephemeral/cache-like: explicitly permitted to opt into local commit.
        GuardrailDisposition::EphemeralLocalAllowed
    } else {
        // Durable model under declared HA intent with a genuinely durable policy.
        GuardrailDisposition::Satisfied
    };

    Ok(CommitPolicyResolution {
        effective,
        source,
        guardrail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DURABLE: [CollectionDataModel; 5] = [
        CollectionDataModel::Transactional,
        CollectionDataModel::Queue,
        CollectionDataModel::Audit,
        CollectionDataModel::Config,
        CollectionDataModel::Vault,
    ];
    const LOCAL_ELIGIBLE: [CollectionDataModel; 2] =
        [CollectionDataModel::Ephemeral, CollectionDataModel::Cache];

    #[test]
    fn data_model_durability_partition() {
        for m in DURABLE {
            assert!(m.is_durable(), "{} should be durable", m.label());
            assert!(!m.allows_ephemeral_local());
        }
        for m in LOCAL_ELIGIBLE {
            assert!(!m.is_durable(), "{} should not be durable", m.label());
            assert!(m.allows_ephemeral_local());
        }
    }

    #[test]
    fn is_local_ack_treats_ack0_as_local() {
        assert!(is_local_ack(CommitPolicy::Local));
        assert!(is_local_ack(CommitPolicy::AckN(0)));
        assert!(!is_local_ack(CommitPolicy::AckN(1)));
        assert!(!is_local_ack(CommitPolicy::Quorum));
        assert!(!is_local_ack(CommitPolicy::RemoteWal));
    }

    // AC: default quorum behavior — cluster default applies with no override.
    #[test]
    fn cluster_default_quorum_applies_without_override() {
        let r = resolve_commit_policy(
            CommitPolicy::Quorum,
            None,
            CollectionDataModel::Transactional,
            HaIntent::Declared,
        )
        .expect("quorum default is durable under HA");
        assert_eq!(r.effective, CommitPolicy::Quorum);
        assert_eq!(r.source, ResolutionSource::ClusterDefault);
        assert_eq!(r.guardrail, GuardrailDisposition::Satisfied);
        assert_eq!(
            r.failover_eligibility(),
            FailoverEligibility::RequiresWatermarkCoverage
        );
    }

    // AC: collection override — a stricter/looser override beats the default.
    #[test]
    fn collection_override_beats_cluster_default() {
        let r = resolve_commit_policy(
            CommitPolicy::AckN(1),
            Some(CommitPolicy::Quorum),
            CollectionDataModel::Audit,
            HaIntent::Declared,
        )
        .expect("override quorum is durable");
        assert_eq!(r.effective, CommitPolicy::Quorum);
        assert_eq!(r.source, ResolutionSource::CollectionOverride);
        assert_eq!(r.guardrail, GuardrailDisposition::Satisfied);
    }

    #[test]
    fn request_override_can_strengthen_above_resolved_floor() {
        let floor = resolve_commit_policy(
            CommitPolicy::Local,
            None,
            CollectionDataModel::Transactional,
            HaIntent::None,
        )
        .expect("non-HA local floor resolves");

        let r = resolve_request_commit_policy(floor, Some(CommitPolicy::Quorum))
            .expect("request may strengthen local floor to quorum");
        assert_eq!(r.effective, CommitPolicy::Quorum);
        assert_eq!(r.source, ResolutionSource::RequestOverride);
    }

    #[test]
    fn request_override_rejects_weaker_than_resolved_floor() {
        let floor = resolve_commit_policy(
            CommitPolicy::Quorum,
            None,
            CollectionDataModel::Transactional,
            HaIntent::Declared,
        )
        .expect("quorum floor resolves");

        let err = resolve_request_commit_policy(floor, Some(CommitPolicy::AckN(1)))
            .expect_err("request may not weaken quorum floor");
        assert_eq!(
            err,
            CommitPolicyViolation::RequestBelowResolvedFloor {
                floor: CommitPolicy::Quorum,
                requested: CommitPolicy::AckN(1),
            }
        );
        assert_eq!(err.code(), "COMMIT_POLICY_BELOW_FLOOR");
    }

    // AC: local commit allowed for ephemeral/cache-like data under HA intent.
    #[test]
    fn local_commit_allowed_for_ephemeral_cache_under_ha() {
        for m in LOCAL_ELIGIBLE {
            // via cluster default
            let r = resolve_commit_policy(CommitPolicy::Local, None, m, HaIntent::Declared)
                .unwrap_or_else(|e| panic!("{} local should be allowed: {e}", m.label()));
            assert_eq!(r.effective, CommitPolicy::Local);
            assert_eq!(r.guardrail, GuardrailDisposition::EphemeralLocalAllowed);
            assert_eq!(
                r.failover_eligibility(),
                FailoverEligibility::LocalAckDataLossWindow
            );
            assert!(!r.requires_durable_watermark());

            // via explicit override, and the AckN(0) degenerate form
            let r = resolve_commit_policy(
                CommitPolicy::Quorum,
                Some(CommitPolicy::AckN(0)),
                m,
                HaIntent::Declared,
            )
            .expect("ack_n=0 is local-eligible for ephemeral/cache");
            assert_eq!(r.guardrail, GuardrailDisposition::EphemeralLocalAllowed);
        }
    }

    // AC: local commit rejected for durable models under HA intent.
    #[test]
    fn local_commit_rejected_for_durable_models_under_ha() {
        for m in DURABLE {
            // via cluster default
            let err = resolve_commit_policy(CommitPolicy::Local, None, m, HaIntent::Declared)
                .expect_err("durable local must be rejected under HA");
            assert_eq!(
                err,
                CommitPolicyViolation::DurableLocalUnderHa {
                    model: m,
                    source: ResolutionSource::ClusterDefault,
                }
            );
            assert!(err.message().contains(m.label()));

            // via override, including the AckN(0) degenerate form
            let err = resolve_commit_policy(
                CommitPolicy::Quorum,
                Some(CommitPolicy::AckN(0)),
                m,
                HaIntent::Declared,
            )
            .expect_err("durable ack_n=0 override must be rejected under HA");
            assert_eq!(
                err,
                CommitPolicyViolation::DurableLocalUnderHa {
                    model: m,
                    source: ResolutionSource::CollectionOverride,
                }
            );
        }
    }

    // Guardrail only bites under declared HA intent: a non-HA deployment may use
    // local commit for any model.
    #[test]
    fn local_commit_allowed_for_durable_when_ha_not_declared() {
        for m in DURABLE {
            let r = resolve_commit_policy(CommitPolicy::Local, None, m, HaIntent::None)
                .expect("guardrail off without HA intent");
            assert_eq!(r.effective, CommitPolicy::Local);
            assert_eq!(r.guardrail, GuardrailDisposition::NotApplicable);
        }
    }

    // AC: failover watermark implications follow the resolved policy.
    #[test]
    fn failover_watermark_implications_track_resolved_policy() {
        // Durable resolved policy → promotion gated on watermark coverage.
        let durable = resolve_commit_policy(
            CommitPolicy::AckN(2),
            None,
            CollectionDataModel::Queue,
            HaIntent::Declared,
        )
        .unwrap();
        assert!(durable.requires_durable_watermark());
        assert_eq!(
            durable.failover_eligibility(),
            FailoverEligibility::RequiresWatermarkCoverage
        );

        // Local resolved policy (ephemeral) → explicit data-loss window.
        let local = resolve_commit_policy(
            CommitPolicy::Local,
            None,
            CollectionDataModel::Cache,
            HaIntent::Declared,
        )
        .unwrap();
        assert!(!local.requires_durable_watermark());
        assert_eq!(
            local.failover_eligibility(),
            FailoverEligibility::LocalAckDataLossWindow
        );
    }

    #[test]
    fn resolution_is_deterministic() {
        let inputs = (
            CommitPolicy::AckN(1),
            Some(CommitPolicy::Quorum),
            CollectionDataModel::Vault,
            HaIntent::Declared,
        );
        let a = resolve_commit_policy(inputs.0, inputs.1, inputs.2, inputs.3);
        let b = resolve_commit_policy(inputs.0, inputs.1, inputs.2, inputs.3);
        assert_eq!(a, b);
    }

    #[test]
    fn ha_intent_parse() {
        assert_eq!(HaIntent::parse("true"), HaIntent::Declared);
        assert_eq!(HaIntent::parse("1"), HaIntent::Declared);
        assert_eq!(HaIntent::parse("YES"), HaIntent::Declared);
        assert_eq!(HaIntent::parse("declared"), HaIntent::Declared);
        assert_eq!(HaIntent::parse("false"), HaIntent::None);
        assert_eq!(HaIntent::parse(""), HaIntent::None);
        assert_eq!(HaIntent::parse("nonsense"), HaIntent::None);
        assert_eq!(HaIntent::default(), HaIntent::None);
    }

    #[test]
    fn source_and_disposition_labels() {
        assert_eq!(ResolutionSource::ClusterDefault.label(), "cluster_default");
        assert_eq!(
            ResolutionSource::CollectionOverride.label(),
            "collection_override"
        );
    }
}
