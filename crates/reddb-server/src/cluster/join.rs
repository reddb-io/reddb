//! Authenticated cluster join through seed members (issue #988, ADR 0030).
//!
//! Join is the explicit admission flow a candidate runs to *become* an
//! authorized cluster member. The glossary fixes the steps: *"a candidate
//! member authenticates against seed members, verifies cluster identity,
//! downloads global control-plane state, and only then becomes an authorized
//! cluster member."* Until that completes, a node is just a reachable network
//! peer — not a member, and not something autodetect will adopt.
//!
//! ## The handshake, structurally
//!
//! A seed member holds a [`SeedAuthority`]: the cluster's [`ClusterId`], an
//! operator-provisioned **allowlist** of which identities may join (and as what
//! kind), and the current [`MembershipCatalog`]. A candidate sends a
//! [`JoinRequest`] carrying the cluster id it *believes* it is joining, its
//! authenticated [`NodeIdentity`], and the kind it intends to be. The seed:
//!
//! 1. **Verifies cluster identity.** A request that names a different cluster
//!    is [`JoinRejection::WrongCluster`] — authenticating correctly to the
//!    wrong cluster is still a rejection.
//! 2. **Authorizes the peer.** An identity absent from the allowlist is an
//!    unknown/unauthorized peer: [`JoinRejection::UnauthorizedPeer`]. This is
//!    what stops "anyone who can open a connection" from joining.
//! 3. **Checks the declared kind.** A peer allow-listed as a witness that asks
//!    to join as a data member (or vice versa) is
//!    [`JoinRejection::KindMismatch`].
//! 4. **Admits and snapshots.** The candidate is added to the catalog as a
//!    [joined-empty member](super::membership::ClusterMember::joined_empty) —
//!    no user ranges — and the seed returns a [`ControlPlaneSnapshot`] of the
//!    now-current membership for the candidate to adopt.
//!
//! Authentication itself is mTLS: the [`NodeIdentity`] in a request is the
//! validated X.509 subject of the peer certificate. [`JoinRequest::authenticated`]
//! is the only constructor, so a request cannot exist without a proven
//! identity — there is no "anonymous join" shape to defend against.

use super::identity::NodeIdentity;
use super::membership::{
    AdmissionOutcome, ClusterId, ClusterMember, MemberKind, MembershipCatalog,
};
use std::collections::BTreeMap;

/// A candidate's request to join a cluster through a seed member.
///
/// The only way to build one is [`JoinRequest::authenticated`], whose
/// `identity` is the validated certificate subject of the peer — so a request
/// always carries a proven identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRequest {
    /// The cluster the candidate believes it is joining. Checked against the
    /// seed's own cluster id.
    pub target_cluster: ClusterId,
    /// The candidate's authenticated cluster member identity.
    pub identity: NodeIdentity,
    /// The kind the candidate intends to join as.
    pub kind: MemberKind,
}

impl JoinRequest {
    /// Build a join request from an already-authenticated peer identity (the
    /// validated mTLS certificate subject).
    pub fn authenticated(
        target_cluster: ClusterId,
        identity: NodeIdentity,
        kind: MemberKind,
    ) -> Self {
        Self {
            target_cluster,
            identity,
            kind,
        }
    }
}

/// Why a join was refused. Each variant maps to one of the seed's checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinRejection {
    /// The request named a different cluster than this seed serves.
    WrongCluster {
        expected: ClusterId,
        presented: ClusterId,
    },
    /// The authenticated peer is not on the operator allowlist — an unknown or
    /// unauthorized peer.
    UnauthorizedPeer(NodeIdentity),
    /// The peer is allow-listed but asked to join as the wrong kind.
    KindMismatch {
        identity: NodeIdentity,
        allowed: MemberKind,
        requested: MemberKind,
    },
}

impl std::fmt::Display for JoinRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongCluster {
                expected,
                presented,
            } => write!(
                f,
                "join targets cluster {presented}, but this seed serves {expected}"
            ),
            Self::UnauthorizedPeer(id) => {
                write!(f, "peer {id} is not an authorized cluster member")
            }
            Self::KindMismatch {
                identity,
                allowed,
                requested,
            } => write!(
                f,
                "peer {identity} is allowed as {allowed:?} but requested {requested:?}"
            ),
        }
    }
}

impl std::error::Error for JoinRejection {}

/// The global control-plane state a freshly admitted member downloads — the
/// authorized membership it should adopt as its starting view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlaneSnapshot {
    pub cluster_id: ClusterId,
    pub members: Vec<ClusterMember>,
}

/// A successful admission: the outcome (newly admitted vs. already a member)
/// and the control-plane snapshot the candidate adopts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinGrant {
    pub outcome: AdmissionOutcome,
    pub snapshot: ControlPlaneSnapshot,
}

/// A seed member's authority to admit join candidates.
///
/// It owns the cluster's [`ClusterId`], the operator-provisioned allowlist of
/// who may join, and the live [`MembershipCatalog`]. [`evaluate_join`] runs the
/// full handshake and, on success, mutates the catalog to include the new
/// member.
///
/// [`evaluate_join`]: SeedAuthority::evaluate_join
#[derive(Debug, Clone)]
pub struct SeedAuthority {
    allowlist: BTreeMap<NodeIdentity, MemberKind>,
    catalog: MembershipCatalog,
}

impl SeedAuthority {
    /// Build a seed authority over `catalog` with the given operator
    /// `allowlist` of identities permitted to join (and the kind each is
    /// permitted to join as). Existing members are implicitly allow-listed.
    pub fn new(
        catalog: MembershipCatalog,
        allowlist: impl IntoIterator<Item = (NodeIdentity, MemberKind)>,
    ) -> Self {
        let mut allow: BTreeMap<NodeIdentity, MemberKind> = allowlist.into_iter().collect();
        // Anyone already in the catalog is, by definition, authorized.
        for member in catalog.members() {
            allow
                .entry(member.identity().clone())
                .or_insert(member.kind());
        }
        Self {
            allowlist: allow,
            catalog,
        }
    }

    pub fn cluster_id(&self) -> &ClusterId {
        self.catalog.cluster_id()
    }

    pub fn catalog(&self) -> &MembershipCatalog {
        &self.catalog
    }

    /// Run the join handshake for `request`. On success the candidate is now an
    /// authorized member of the catalog and the returned [`JoinGrant`] carries
    /// the control-plane snapshot to adopt; on failure the catalog is
    /// unchanged and the [`JoinRejection`] says which check failed.
    pub fn evaluate_join(&mut self, request: JoinRequest) -> Result<JoinGrant, JoinRejection> {
        // 1. Verify cluster identity.
        if &request.target_cluster != self.catalog.cluster_id() {
            return Err(JoinRejection::WrongCluster {
                expected: self.catalog.cluster_id().clone(),
                presented: request.target_cluster,
            });
        }

        // 2. Authorize the authenticated peer against the allowlist.
        let allowed_kind = match self.allowlist.get(&request.identity) {
            Some(kind) => *kind,
            None => return Err(JoinRejection::UnauthorizedPeer(request.identity)),
        };

        // 3. The declared kind must match what the peer is allow-listed as.
        if allowed_kind != request.kind {
            return Err(JoinRejection::KindMismatch {
                identity: request.identity,
                allowed: allowed_kind,
                requested: request.kind,
            });
        }

        // 4. Admit as a joined-empty member and snapshot the control plane.
        let member = ClusterMember::joined_empty(request.identity, request.kind);
        let outcome = self.catalog.admit(member);
        Ok(JoinGrant {
            outcome,
            snapshot: self.snapshot(),
        })
    }

    fn snapshot(&self) -> ControlPlaneSnapshot {
        ControlPlaneSnapshot {
            cluster_id: self.catalog.cluster_id().clone(),
            members: self.catalog.members().cloned().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    fn cid() -> ClusterId {
        ClusterId::new("cluster-prod").unwrap()
    }

    /// A two-data-member founding cluster with `node-c` pre-authorized to join.
    fn seed_with_pending_node_c() -> SeedAuthority {
        let catalog = MembershipCatalog::new(
            cid(),
            [
                ClusterMember::joined_empty(ident("CN=node-a"), MemberKind::Data),
                ClusterMember::joined_empty(ident("CN=node-b"), MemberKind::Data),
            ],
        );
        SeedAuthority::new(catalog, [(ident("CN=node-c"), MemberKind::Data)])
    }

    #[test]
    fn successful_join_admits_authorized_data_member() {
        let mut seed = seed_with_pending_node_c();
        let req = JoinRequest::authenticated(cid(), ident("CN=node-c"), MemberKind::Data);

        let grant = seed
            .evaluate_join(req)
            .expect("authorized join should succeed");
        assert_eq!(grant.outcome, AdmissionOutcome::Admitted);

        // The candidate is now an authorized member, and the snapshot reflects
        // the full three-data-member cluster it should adopt.
        assert!(seed.catalog().is_authorized(&ident("CN=node-c")));
        assert_eq!(grant.snapshot.cluster_id, cid());
        assert_eq!(grant.snapshot.members.len(), 3);
        assert!(seed.catalog().assess_baseline().meets_baseline());
    }

    #[test]
    fn joined_data_member_starts_with_no_user_ranges() {
        let mut seed = seed_with_pending_node_c();
        let req = JoinRequest::authenticated(cid(), ident("CN=node-c"), MemberKind::Data);
        seed.evaluate_join(req).unwrap();

        let joined = seed.catalog().member(&ident("CN=node-c")).unwrap();
        assert!(!joined.holds_user_ranges());
        assert_eq!(joined.owned_range_count(), 0);
    }

    #[test]
    fn unauthorized_peer_is_rejected() {
        let mut seed = seed_with_pending_node_c();
        // `node-x` authenticated fine (it has a NodeIdentity) but is not on the
        // allowlist — an unknown/unauthorized peer.
        let req = JoinRequest::authenticated(cid(), ident("CN=node-x"), MemberKind::Data);

        let err = seed
            .evaluate_join(req)
            .expect_err("unknown peer must be rejected");
        assert_eq!(err, JoinRejection::UnauthorizedPeer(ident("CN=node-x")));
        // The catalog is unchanged and the peer is not autodetect-eligible.
        assert!(!seed.catalog().is_autodetect_eligible(&ident("CN=node-x")));
        assert_eq!(seed.catalog().len(), 2);
    }

    #[test]
    fn wrong_cluster_join_is_rejected() {
        let mut seed = seed_with_pending_node_c();
        let other = ClusterId::new("cluster-staging").unwrap();
        // `node-c` is authorized, but it targets the wrong cluster.
        let req = JoinRequest::authenticated(other.clone(), ident("CN=node-c"), MemberKind::Data);

        let err = seed
            .evaluate_join(req)
            .expect_err("wrong-cluster join must be rejected");
        assert_eq!(
            err,
            JoinRejection::WrongCluster {
                expected: cid(),
                presented: other,
            }
        );
        assert!(!seed.catalog().is_authorized(&ident("CN=node-c")));
    }

    #[test]
    fn kind_mismatch_is_rejected() {
        let mut seed = seed_with_pending_node_c();
        // `node-c` is allow-listed as Data but asks to join as a Witness.
        let req = JoinRequest::authenticated(cid(), ident("CN=node-c"), MemberKind::Witness);

        let err = seed
            .evaluate_join(req)
            .expect_err("kind mismatch must be rejected");
        assert_eq!(
            err,
            JoinRejection::KindMismatch {
                identity: ident("CN=node-c"),
                allowed: MemberKind::Data,
                requested: MemberKind::Witness,
            }
        );
    }

    #[test]
    fn rejoin_is_idempotent() {
        let mut seed = seed_with_pending_node_c();
        let req = || JoinRequest::authenticated(cid(), ident("CN=node-c"), MemberKind::Data);

        let first = seed.evaluate_join(req()).unwrap();
        assert_eq!(first.outcome, AdmissionOutcome::Admitted);

        let second = seed.evaluate_join(req()).unwrap();
        assert_eq!(second.outcome, AdmissionOutcome::AlreadyMember);
        assert_eq!(seed.catalog().len(), 3);
    }

    #[test]
    fn autodetect_adopts_only_members_after_join() {
        let mut seed = seed_with_pending_node_c();
        // Before join: node-c is not an autodetect candidate.
        assert!(!seed.catalog().is_autodetect_eligible(&ident("CN=node-c")));

        seed.evaluate_join(JoinRequest::authenticated(
            cid(),
            ident("CN=node-c"),
            MemberKind::Data,
        ))
        .unwrap();

        // After join: it is, and a never-joined peer still is not.
        assert!(seed.catalog().is_autodetect_eligible(&ident("CN=node-c")));
        assert!(!seed.catalog().is_autodetect_eligible(&ident("CN=stranger")));
    }
}
