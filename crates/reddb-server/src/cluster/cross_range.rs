//! First-cut cross-range transaction and read behaviour (issue #1002, PRD #987).
//!
//! Once a collection is split into independently-owned ranges (issue #989's
//! catalog, routed by [`plan_route`](ShardOwnershipCatalog::plan_route)), a
//! single client operation can touch keys that land in *different* ranges — and,
//! in a multi-writer cluster, those ranges can be owned by *different writers*.
//! This module is the request-layer gate that decides what the first multi-writer
//! cut is allowed to do with such an operation. It deliberately refuses to
//! pretend a cross-writer operation is globally atomic or globally
//! snapshot-consistent when nothing underneath guarantees it.
//!
//! Three decisions live here, all pure reads of the [`ShardOwnershipCatalog`]:
//!
//! * **Write transactions** ([`plan_write_transaction`]). Resolve every targeted
//!   key to its owning range and group by writer. A transaction confined to a
//!   *single* writer (even one that spans several of that writer's own ranges)
//!   commits on that owner and is admitted. A transaction whose keys span ranges
//!   owned by *different* writers has no atomic-commit path in this cut, so it is
//!   rejected with [`WriteTransactionReject::CrossRange`] naming every writer
//!   involved — a clear "unsupported" contract rather than a silent partial
//!   commit.
//!
//! * **Explicit bounded read fanout** ([`plan_read_fanout`]). A best-effort read
//!   may span range owners only when the caller opts into fanout and supplies a
//!   budget. The plan collects one [`ReadLeg`] per owner and returns
//!   [`ReadFanoutTrace`] metadata so the transport can expose participating
//!   owners/ranges instead of hiding a scatter query. This is explicitly *not* a
//!   globally consistent snapshot — each leg observes its owner at whatever point
//!   it happens to be at — and the type name says so.
//!
//! * **Consistent / transactional reads** ([`plan_consistent_read`]). A read that
//!   must look globally consistent needs a safe snapshot point that covers every
//!   range it touches. The caller supplies a [`GlobalReadWatermark`]; the plan
//!   pins each leg to that range's watermark. With no snapshot the request fails
//!   with [`ConsistentReadReject::NoSafeSnapshot`]; with a snapshot that is
//!   missing a targeted range it fails with [`ConsistentReadReject::WatermarkGap`].
//!   Either way the caller learns it cannot get a consistent answer rather than
//!   getting an inconsistent one dressed up as consistent.
//!
//! Like the rest of the cluster module this is a pure decision layer with no I/O:
//! it maps a catalog plus a set of [`KeyTarget`]s to an intent, so the
//! cross-range contract is exercised deterministically. The transport that
//! actually fans the legs out and the storage that admits each write are layered
//! on top.

use std::collections::BTreeMap;

use super::identity::NodeIdentity;
use super::ownership::{CollectionId, OwnershipEpoch, RangeId, ShardOwnershipCatalog};
use super::ownership_transition::CommitWatermark;

/// One `(collection, key)` a cross-range operation touches.
///
/// A transaction or multi-key read is just a set of these; the catalog resolves
/// each to its owning range to decide whether the operation crosses writers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyTarget {
    collection: CollectionId,
    key: Vec<u8>,
}

impl KeyTarget {
    pub fn new(collection: CollectionId, key: impl Into<Vec<u8>>) -> Self {
        Self {
            collection,
            key: key.into(),
        }
    }

    pub fn collection(&self) -> &CollectionId {
        &self.collection
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }
}

/// A targeted key resolved to the range that owns it — the catalog read every
/// cross-range decision is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    collection: CollectionId,
    key: Vec<u8>,
    range_id: RangeId,
    owner: NodeIdentity,
    epoch: OwnershipEpoch,
}

impl ResolvedTarget {
    pub fn collection(&self) -> &CollectionId {
        &self.collection
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn range_id(&self) -> RangeId {
        self.range_id
    }

    pub fn owner(&self) -> &NodeIdentity {
        &self.owner
    }

    pub fn epoch(&self) -> OwnershipEpoch {
        self.epoch
    }
}

/// A range a writer owns that an operation touches, with the epoch the caller
/// must fence each write under (the same epoch
/// [`admit_public_write`](ShardOwnershipCatalog::admit_public_write) checks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeParticipant {
    collection: CollectionId,
    range_id: RangeId,
    epoch: OwnershipEpoch,
}

impl RangeParticipant {
    pub fn collection(&self) -> &CollectionId {
        &self.collection
    }

    pub fn range_id(&self) -> RangeId {
        self.range_id
    }

    pub fn epoch(&self) -> OwnershipEpoch {
        self.epoch
    }
}

/// One writer's participation in a cross-range write transaction: the writer and
/// the distinct ranges of theirs the transaction touches. Used both as the
/// admitted single-writer plan and, in the rejection, to name each writer the
/// transaction would have had to coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterParticipation {
    writer: NodeIdentity,
    ranges: Vec<RangeParticipant>,
}

impl WriterParticipation {
    pub fn writer(&self) -> &NodeIdentity {
        &self.writer
    }

    pub fn ranges(&self) -> &[RangeParticipant] {
        &self.ranges
    }
}

/// An admitted single-writer write transaction: every targeted key resolves to a
/// range owned by the *same* writer, so the transaction commits atomically on
/// that owner. Carries each participating range so the caller fences every write
/// at the right epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteTransactionPlan {
    participation: WriterParticipation,
}

impl WriteTransactionPlan {
    /// The single writer that owns every range the transaction touches.
    pub fn writer(&self) -> &NodeIdentity {
        self.participation.writer()
    }

    /// The distinct ranges (with fencing epochs) the transaction writes.
    pub fn ranges(&self) -> &[RangeParticipant] {
        self.participation.ranges()
    }
}

/// An admitted exact claim: every candidate key resolves to one writer, so that
/// owner is the only member allowed to choose the winners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactClaimPlan {
    participation: WriterParticipation,
}

impl ExactClaimPlan {
    /// The single writer that owns every candidate range.
    pub fn writer(&self) -> &NodeIdentity {
        self.participation.writer()
    }

    /// The distinct ranges (with fencing epochs) the claim may inspect.
    pub fn ranges(&self) -> &[RangeParticipant] {
        self.participation.ranges()
    }
}

/// Why a write transaction could not be planned in the first multi-writer cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteTransactionReject {
    /// The transaction named no targets — there is nothing to commit.
    Empty,
    /// A targeted key resolves to no range of its collection; routing is stale or
    /// the collection is not yet placed. The caller must refresh its catalog.
    Unroutable {
        collection: CollectionId,
        key: Vec<u8>,
    },
    /// The transaction's keys span ranges owned by **different writers**. There is
    /// no atomic cross-writer commit path in this cut, so the transaction is
    /// rejected rather than committed partially. Carries every writer involved (in
    /// identity order) so the caller sees exactly which owners it straddled.
    CrossRange { writers: Vec<WriterParticipation> },
}

impl std::fmt::Display for WriteTransactionReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "write transaction names no targets"),
            Self::Unroutable { collection, key } => write!(
                f,
                "no range of collection {collection} covers key {} — re-resolve routing",
                DisplayKey(key)
            ),
            Self::CrossRange { writers } => {
                write!(
                    f,
                    "cross-range write transaction spans {} writers and is unsupported: ",
                    writers.len()
                )?;
                for (i, w) in writers.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{} owns ", w.writer())?;
                    for (j, r) in w.ranges().iter().enumerate() {
                        if j > 0 {
                            write!(f, "+")?;
                        }
                        write!(f, "{}/{}", r.collection(), r.range_id())?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for WriteTransactionReject {}

/// Why an exact claim could not be planned in the first cluster contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactClaimReject {
    /// The claim named no candidate keys.
    Empty,
    /// A candidate key resolves to no range of its collection.
    Unroutable {
        collection: CollectionId,
        key: Vec<u8>,
    },
    /// Candidate keys span ranges owned by different writers. Exact claim needs
    /// one authority to decide all winners, so this is explicitly unsupported in
    /// the first cluster contract.
    CrossOwner { writers: Vec<WriterParticipation> },
}

impl std::fmt::Display for ExactClaimReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "exact claim names no candidate targets"),
            Self::Unroutable { collection, key } => write!(
                f,
                "no range of collection {collection} covers claim candidate key {} — re-resolve routing",
                DisplayKey(key)
            ),
            Self::CrossOwner { writers } => {
                write!(
                    f,
                    "cross-owner exact claim spans {} writers and is unsupported in the first cluster contract: ",
                    writers.len()
                )?;
                for (i, w) in writers.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{} owns ", w.writer())?;
                    for (j, r) in w.ranges().iter().enumerate() {
                        if j > 0 {
                            write!(f, "+")?;
                        }
                        write!(f, "{}/{}", r.collection(), r.range_id())?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ExactClaimReject {}

/// One owner's leg of a read: the owner and the resolved targets to read there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadLeg {
    owner: NodeIdentity,
    targets: Vec<ResolvedTarget>,
}

impl ReadLeg {
    pub fn owner(&self) -> &NodeIdentity {
        &self.owner
    }

    pub fn targets(&self) -> &[ResolvedTarget] {
        &self.targets
    }
}

/// The explicit policy a caller supplies before a read may fan out.
///
/// The default posture is [`SingleOwnerOnly`](Self::SingleOwnerOnly): a read that
/// would cross owners is rejected before execution. To scatter, a caller must
/// choose [`ExplicitFanout`](Self::ExplicitFanout) and provide a
/// [`ReadFanoutBudget`]. That is the ADR 0055 contract in code: no hidden,
/// unbounded fanout path for cold-cache or missing-shard-key query shapes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReadFanoutPolicy {
    /// Only reads that resolve to one owner are admitted.
    #[default]
    SingleOwnerOnly,
    /// Cross-owner fanout is allowed, bounded, and marked with partial-result
    /// policy for the executor/transport.
    ExplicitFanout {
        budget: ReadFanoutBudget,
        allow_partial: bool,
    },
}

impl ReadFanoutPolicy {
    pub fn single_owner_only() -> Self {
        Self::SingleOwnerOnly
    }

    pub fn explicit(budget: ReadFanoutBudget) -> Self {
        Self::ExplicitFanout {
            budget,
            allow_partial: false,
        }
    }

    pub fn allowing_partial(mut self) -> Self {
        if let Self::ExplicitFanout {
            ref mut allow_partial,
            ..
        } = self
        {
            *allow_partial = true;
        }
        self
    }
}

/// Hard caps for one explicit best-effort read fanout plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFanoutBudget {
    max_owners: usize,
    max_ranges: usize,
    max_targets: usize,
}

impl ReadFanoutBudget {
    pub fn new(max_owners: usize, max_ranges: usize, max_targets: usize) -> Self {
        Self {
            max_owners,
            max_ranges,
            max_targets,
        }
    }

    pub fn max_owners(&self) -> usize {
        self.max_owners
    }

    pub fn max_ranges(&self) -> usize {
        self.max_ranges
    }

    pub fn max_targets(&self) -> usize {
        self.max_targets
    }
}

impl Default for ReadFanoutBudget {
    fn default() -> Self {
        Self {
            max_owners: 8,
            max_ranges: 64,
            max_targets: 512,
        }
    }
}

/// Observable metadata for a best-effort fanout plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFanoutTrace {
    target_count: usize,
    owner_count: usize,
    range_count: usize,
    partial_allowed: bool,
}

impl ReadFanoutTrace {
    pub fn target_count(&self) -> usize {
        self.target_count
    }

    pub fn owner_count(&self) -> usize {
        self.owner_count
    }

    pub fn range_count(&self) -> usize {
        self.range_count
    }

    pub fn partial_allowed(&self) -> bool {
        self.partial_allowed
    }
}

/// A simple, best-effort cross-range read split into one [`ReadLeg`] per owner.
///
/// **Not** a globally consistent snapshot: each leg observes its owner at
/// whatever point that owner is at when it answers, so two legs may reflect
/// different moments in time. For a globally consistent answer use
/// [`plan_consistent_read`](ShardOwnershipCatalog::plan_consistent_read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFanout {
    legs: Vec<ReadLeg>,
    trace: ReadFanoutTrace,
}

impl ReadFanout {
    /// One leg per distinct owner, in identity order.
    pub fn legs(&self) -> &[ReadLeg] {
        &self.legs
    }

    /// Observable fanout metadata the executor/transport should trace.
    pub fn trace(&self) -> ReadFanoutTrace {
        self.trace
    }

    /// Whether the read touches more than one range.
    pub fn is_cross_range(&self) -> bool {
        self.trace.range_count > 1
    }
}

/// Why a simple read fanout could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadFanoutReject {
    /// The read named no targets.
    Empty,
    /// A targeted key resolves to no range of its collection.
    Unroutable {
        collection: CollectionId,
        key: Vec<u8>,
    },
    /// The read would cross owners, but the caller did not explicitly opt into
    /// fanout.
    FanoutNotExplicit { owners: Vec<NodeIdentity> },
    /// The request named more targets than the fanout budget permits.
    TargetBudgetExceeded { requested: usize, max: usize },
    /// The plan would contact more owners than the fanout budget permits.
    OwnerBudgetExceeded { requested: usize, max: usize },
    /// The plan would touch more ranges than the fanout budget permits.
    RangeBudgetExceeded { requested: usize, max: usize },
}

impl std::fmt::Display for ReadFanoutReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "read fanout names no targets"),
            Self::Unroutable { collection, key } => write!(
                f,
                "no range of collection {collection} covers key {} — re-resolve routing",
                DisplayKey(key)
            ),
            Self::FanoutNotExplicit { owners } => {
                write!(
                    f,
                    "read would fan out to {} owners but fanout was not explicit: ",
                    owners.len()
                )?;
                for (i, owner) in owners.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{owner}")?;
                }
                Ok(())
            }
            Self::TargetBudgetExceeded { requested, max } => write!(
                f,
                "read fanout targets {requested} keys, exceeding budget {max}"
            ),
            Self::OwnerBudgetExceeded { requested, max } => write!(
                f,
                "read fanout would contact {requested} owners, exceeding budget {max}"
            ),
            Self::RangeBudgetExceeded { requested, max } => write!(
                f,
                "read fanout would touch {requested} ranges, exceeding budget {max}"
            ),
        }
    }
}

impl std::error::Error for ReadFanoutReject {}

/// A safe snapshot point for a globally consistent cross-range read: a commit
/// watermark per `(collection, range)` that the read pins itself to.
///
/// This is the "explicit safe snapshot/watermark path" the issue requires before
/// a cross-range read may claim to be consistent. A consistent read must find a
/// watermark here for **every** range it touches; a missing entry means the
/// snapshot does not cover that range and the read cannot be served consistently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalReadWatermark {
    marks: BTreeMap<(CollectionId, RangeId), CommitWatermark>,
}

impl GlobalReadWatermark {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin `range`'s safe read point to `watermark` (builder form).
    pub fn with(
        mut self,
        collection: CollectionId,
        range_id: RangeId,
        watermark: CommitWatermark,
    ) -> Self {
        self.marks.insert((collection, range_id), watermark);
        self
    }

    /// Record `range`'s safe read point.
    pub fn insert(
        &mut self,
        collection: CollectionId,
        range_id: RangeId,
        watermark: CommitWatermark,
    ) {
        self.marks.insert((collection, range_id), watermark);
    }

    /// The pinned watermark for a range, or `None` if the snapshot does not cover
    /// it.
    pub fn covers(&self, collection: &CollectionId, range_id: RangeId) -> Option<CommitWatermark> {
        self.marks.get(&(collection.clone(), range_id)).copied()
    }
}

/// One owner's leg of a consistent read: each resolved target paired with the
/// safe watermark its range is pinned to for this snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsistentReadLeg {
    owner: NodeIdentity,
    targets: Vec<PinnedTarget>,
}

impl ConsistentReadLeg {
    pub fn owner(&self) -> &NodeIdentity {
        &self.owner
    }

    pub fn targets(&self) -> &[PinnedTarget] {
        &self.targets
    }
}

/// A resolved target pinned to the safe read watermark of its range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedTarget {
    target: ResolvedTarget,
    watermark: CommitWatermark,
}

impl PinnedTarget {
    pub fn target(&self) -> &ResolvedTarget {
        &self.target
    }

    pub fn watermark(&self) -> CommitWatermark {
        self.watermark
    }
}

/// A globally consistent cross-range read, pinned to a safe snapshot covering
/// every range it touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsistentReadPlan {
    legs: Vec<ConsistentReadLeg>,
}

impl ConsistentReadPlan {
    /// One leg per distinct owner, in identity order, each pinned to its range's
    /// safe watermark.
    pub fn legs(&self) -> &[ConsistentReadLeg] {
        &self.legs
    }
}

/// Why a consistent cross-range read could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsistentReadReject {
    /// The read named no targets.
    Empty,
    /// A targeted key resolves to no range of its collection.
    Unroutable {
        collection: CollectionId,
        key: Vec<u8>,
    },
    /// No safe snapshot was supplied. A cross-range read cannot be served
    /// consistently without a global watermark, so the request fails clearly
    /// rather than silently degrading to a best-effort fanout.
    NoSafeSnapshot,
    /// The supplied snapshot does not cover a targeted range, so the read cannot
    /// be pinned to a single safe point across all of its ranges.
    WatermarkGap {
        collection: CollectionId,
        range_id: RangeId,
    },
}

impl std::fmt::Display for ConsistentReadReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "consistent read names no targets"),
            Self::Unroutable { collection, key } => write!(
                f,
                "no range of collection {collection} covers key {} — re-resolve routing",
                DisplayKey(key)
            ),
            Self::NoSafeSnapshot => write!(
                f,
                "consistent cross-range read requires a global safe snapshot/watermark, none supplied"
            ),
            Self::WatermarkGap {
                collection,
                range_id,
            } => write!(
                f,
                "safe snapshot does not cover {collection}/{range_id}; cannot serve a consistent read"
            ),
        }
    }
}

impl std::error::Error for ConsistentReadReject {}

/// Hex-ish key rendering for error messages — keys are arbitrary bytes.
struct DisplayKey<'a>(&'a [u8]);

impl std::fmt::Display for DisplayKey<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x")?;
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl ShardOwnershipCatalog {
    /// Resolve every target to its owning range, preserving caller order.
    /// `Err` on the first key no range covers.
    fn resolve_targets(
        &self,
        targets: &[KeyTarget],
    ) -> Result<Vec<ResolvedTarget>, (CollectionId, Vec<u8>)> {
        let mut resolved = Vec::with_capacity(targets.len());
        for t in targets {
            match self.route_shard_key(t.collection(), t.key()) {
                Some(range) => resolved.push(ResolvedTarget {
                    collection: t.collection().clone(),
                    key: t.key().to_vec(),
                    range_id: range.range_id(),
                    owner: range.owner().clone(),
                    epoch: range.epoch(),
                }),
                None => return Err((t.collection().clone(), t.key().to_vec())),
            }
        }
        Ok(resolved)
    }

    /// Plan a write transaction over `targets` in the first multi-writer cut
    /// (issue #1002).
    ///
    /// Resolves every targeted key to its owning range and groups by writer:
    ///
    /// * all targets owned by one writer → [`WriteTransactionPlan`] (the
    ///   transaction commits atomically on that owner, even across several of its
    ///   own ranges);
    /// * targets span ranges of different writers →
    ///   [`WriteTransactionReject::CrossRange`] naming every writer — this cut has
    ///   no atomic cross-writer commit;
    /// * a target routes nowhere → [`WriteTransactionReject::Unroutable`].
    ///
    /// Pure: it reads the catalog and returns intent. Each admitted write still
    /// passes [`admit_public_write`](Self::admit_public_write) at the owner's
    /// current epoch, so a stale plan cannot smuggle a write past fencing.
    pub fn plan_write_transaction(
        &self,
        targets: &[KeyTarget],
    ) -> Result<WriteTransactionPlan, WriteTransactionReject> {
        if targets.is_empty() {
            return Err(WriteTransactionReject::Empty);
        }
        let resolved = self
            .resolve_targets(targets)
            .map_err(|(collection, key)| WriteTransactionReject::Unroutable { collection, key })?;

        let writers = group_by_owner(&resolved);
        if writers.len() == 1 {
            let (writer, ranges) = writers.into_iter().next().expect("exactly one writer");
            Ok(WriteTransactionPlan {
                participation: WriterParticipation { writer, ranges },
            })
        } else {
            Err(WriteTransactionReject::CrossRange {
                writers: writers
                    .into_iter()
                    .map(|(writer, ranges)| WriterParticipation { writer, ranges })
                    .collect(),
            })
        }
    }

    /// Plan an exact claim over `targets` in the first cluster contract.
    ///
    /// A claim winner is a non-deterministic write decision, so only one write
    /// authority may inspect the candidate set and choose winners. Claims whose
    /// candidates all route to one owner are admitted for that owner. Claims
    /// whose candidates require multiple owners are rejected explicitly; later
    /// cluster contracts can add coordinated claims without weakening this one.
    pub fn plan_exact_claim(
        &self,
        targets: &[KeyTarget],
    ) -> Result<ExactClaimPlan, ExactClaimReject> {
        if targets.is_empty() {
            return Err(ExactClaimReject::Empty);
        }
        let resolved = self
            .resolve_targets(targets)
            .map_err(|(collection, key)| ExactClaimReject::Unroutable { collection, key })?;

        let writers = group_by_owner(&resolved);
        if writers.len() == 1 {
            let (writer, ranges) = writers.into_iter().next().expect("exactly one writer");
            Ok(ExactClaimPlan {
                participation: WriterParticipation { writer, ranges },
            })
        } else {
            Err(ExactClaimReject::CrossOwner {
                writers: writers
                    .into_iter()
                    .map(|(writer, ranges)| WriterParticipation { writer, ranges })
                    .collect(),
            })
        }
    }

    /// Plan a simple, best-effort read over `targets` (issue #1002, ADR 0055).
    ///
    /// A read that resolves to one owner is admitted under
    /// [`ReadFanoutPolicy::SingleOwnerOnly`]. A read that would contact multiple
    /// owners must use [`ReadFanoutPolicy::ExplicitFanout`] and fit inside the
    /// supplied budget. The returned [`ReadFanoutTrace`] records target, owner,
    /// range, and partial-result policy so a later executor can surface fanout
    /// rather than making it invisible.
    pub fn plan_read_fanout(
        &self,
        targets: &[KeyTarget],
        policy: ReadFanoutPolicy,
    ) -> Result<ReadFanout, ReadFanoutReject> {
        if targets.is_empty() {
            return Err(ReadFanoutReject::Empty);
        }
        if let ReadFanoutPolicy::ExplicitFanout { budget, .. } = policy {
            if targets.len() > budget.max_targets() {
                return Err(ReadFanoutReject::TargetBudgetExceeded {
                    requested: targets.len(),
                    max: budget.max_targets(),
                });
            }
        }
        let resolved = self
            .resolve_targets(targets)
            .map_err(|(collection, key)| ReadFanoutReject::Unroutable { collection, key })?;

        let owner_count = distinct_owner_count(&resolved);
        let range_count = distinct_range_count(&resolved);
        let partial_allowed = match policy {
            ReadFanoutPolicy::SingleOwnerOnly => {
                if owner_count > 1 {
                    return Err(ReadFanoutReject::FanoutNotExplicit {
                        owners: distinct_owners(&resolved),
                    });
                }
                false
            }
            ReadFanoutPolicy::ExplicitFanout {
                budget,
                allow_partial,
            } => {
                if owner_count > budget.max_owners() {
                    return Err(ReadFanoutReject::OwnerBudgetExceeded {
                        requested: owner_count,
                        max: budget.max_owners(),
                    });
                }
                if range_count > budget.max_ranges() {
                    return Err(ReadFanoutReject::RangeBudgetExceeded {
                        requested: range_count,
                        max: budget.max_ranges(),
                    });
                }
                allow_partial
            }
        };

        Ok(ReadFanout {
            legs: group_targets_by_owner(resolved),
            trace: ReadFanoutTrace {
                target_count: targets.len(),
                owner_count,
                range_count,
                partial_allowed,
            },
        })
    }

    /// Plan a globally consistent cross-range read over `targets`, pinned to
    /// `snapshot` (issue #1002).
    ///
    /// A consistent read must pin every range it touches to a single safe point:
    ///
    /// * `snapshot` is `None` → [`ConsistentReadReject::NoSafeSnapshot`]; the
    ///   caller must obtain a global watermark first;
    /// * a targeted range is absent from `snapshot` →
    ///   [`ConsistentReadReject::WatermarkGap`];
    /// * a target routes nowhere → [`ConsistentReadReject::Unroutable`];
    /// * otherwise → a [`ConsistentReadPlan`] with each leg pinned to its range's
    ///   watermark.
    ///
    /// This is the explicit safe-snapshot path: without it a cross-range read may
    /// only be served as a best-effort [`ReadFanout`], never as a consistent one.
    pub fn plan_consistent_read(
        &self,
        targets: &[KeyTarget],
        snapshot: Option<&GlobalReadWatermark>,
    ) -> Result<ConsistentReadPlan, ConsistentReadReject> {
        if targets.is_empty() {
            return Err(ConsistentReadReject::Empty);
        }
        let resolved = self
            .resolve_targets(targets)
            .map_err(|(collection, key)| ConsistentReadReject::Unroutable { collection, key })?;

        let snapshot = snapshot.ok_or(ConsistentReadReject::NoSafeSnapshot)?;

        // Every targeted range must be covered by the snapshot before any leg is
        // built — a partial pin is not a consistent read.
        let mut pinned = Vec::with_capacity(resolved.len());
        for target in resolved {
            let watermark = snapshot
                .covers(target.collection(), target.range_id())
                .ok_or_else(|| ConsistentReadReject::WatermarkGap {
                    collection: target.collection().clone(),
                    range_id: target.range_id(),
                })?;
            pinned.push(PinnedTarget { target, watermark });
        }

        Ok(ConsistentReadPlan {
            legs: group_pinned_by_owner(pinned),
        })
    }
}

/// Group resolved targets by owner into `(writer, distinct ranges)` pairs, in
/// identity order. Ranges within a writer are deduplicated and ordered by id.
fn group_by_owner(resolved: &[ResolvedTarget]) -> Vec<(NodeIdentity, Vec<RangeParticipant>)> {
    let mut by_owner: BTreeMap<NodeIdentity, BTreeMap<RangeId, RangeParticipant>> = BTreeMap::new();
    for t in resolved {
        by_owner
            .entry(t.owner().clone())
            .or_default()
            .entry(t.range_id())
            .or_insert_with(|| RangeParticipant {
                collection: t.collection().clone(),
                range_id: t.range_id(),
                epoch: t.epoch(),
            });
    }
    by_owner
        .into_iter()
        .map(|(owner, ranges)| (owner, ranges.into_values().collect()))
        .collect()
}

/// Group resolved targets into one [`ReadLeg`] per owner, in identity order.
fn group_targets_by_owner(resolved: Vec<ResolvedTarget>) -> Vec<ReadLeg> {
    let mut by_owner: BTreeMap<NodeIdentity, Vec<ResolvedTarget>> = BTreeMap::new();
    for t in resolved {
        by_owner.entry(t.owner().clone()).or_default().push(t);
    }
    by_owner
        .into_iter()
        .map(|(owner, targets)| ReadLeg { owner, targets })
        .collect()
}

fn distinct_owners(resolved: &[ResolvedTarget]) -> Vec<NodeIdentity> {
    resolved
        .iter()
        .map(|target| target.owner().clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn distinct_owner_count(resolved: &[ResolvedTarget]) -> usize {
    distinct_owners(resolved).len()
}

fn distinct_range_count(resolved: &[ResolvedTarget]) -> usize {
    resolved
        .iter()
        .map(|target| (target.collection().clone(), target.range_id()))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// Group pinned targets into one [`ConsistentReadLeg`] per owner, in identity
/// order.
fn group_pinned_by_owner(pinned: Vec<PinnedTarget>) -> Vec<ConsistentReadLeg> {
    let mut by_owner: BTreeMap<NodeIdentity, Vec<PinnedTarget>> = BTreeMap::new();
    for p in pinned {
        by_owner
            .entry(p.target().owner().clone())
            .or_default()
            .push(p);
    }
    by_owner
        .into_iter()
        .map(|(owner, targets)| ConsistentReadLeg { owner, targets })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ownership::{PlacementMetadata, RangeBound, RangeBounds, ShardKeyMode};

    fn collection(name: &str) -> CollectionId {
        CollectionId::new(name).unwrap()
    }

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    fn bounds(lower: &[u8], upper: &[u8]) -> RangeBounds {
        RangeBounds::new(RangeBound::key(lower), RangeBound::key(upper)).unwrap()
    }

    /// A range over `[lower, upper)` of `coll` owned by `owner`.
    fn range(
        coll: &CollectionId,
        id: u64,
        bnds: RangeBounds,
        owner: &str,
    ) -> super::super::ownership::RangeOwnership {
        super::super::ownership::RangeOwnership::establish(
            coll.clone(),
            RangeId::new(id),
            ShardKeyMode::Ordered,
            bnds,
            ident(owner),
            [ident("CN=replica-1")],
            PlacementMetadata::with_replication_factor(3),
        )
    }

    /// Catalog with `orders` split into two ranges: [a,m) owned by node-a,
    /// [m,Max) owned by node-b.
    fn two_range_catalog() -> (ShardOwnershipCatalog, CollectionId) {
        let orders = collection("orders");
        let mut catalog = ShardOwnershipCatalog::new();
        catalog
            .apply_update(range(&orders, 1, bounds(b"a", b"m"), "CN=node-a"))
            .unwrap();
        catalog
            .apply_update(range(
                &orders,
                2,
                RangeBounds::new(RangeBound::key(b"m"), RangeBound::Max).unwrap(),
                "CN=node-b",
            ))
            .unwrap();
        (catalog, orders)
    }

    fn target(coll: &CollectionId, key: &[u8]) -> KeyTarget {
        KeyTarget::new(coll.clone(), key.to_vec())
    }

    // AC #5: a write transaction whose keys all land in one writer's ranges is
    // admitted — even when it spans several of that writer's ranges.
    #[test]
    fn single_writer_transaction_succeeds() {
        let orders = collection("orders");
        let mut catalog = ShardOwnershipCatalog::new();
        // Two ranges both owned by node-a.
        catalog
            .apply_update(range(&orders, 1, bounds(b"a", b"m"), "CN=node-a"))
            .unwrap();
        catalog
            .apply_update(range(
                &orders,
                2,
                RangeBounds::new(RangeBound::key(b"m"), RangeBound::Max).unwrap(),
                "CN=node-a",
            ))
            .unwrap();

        let plan = catalog
            .plan_write_transaction(&[target(&orders, b"alice"), target(&orders, b"zeb")])
            .expect("single-writer transaction is admitted");
        assert_eq!(plan.writer(), &ident("CN=node-a"));
        // Both of node-a's ranges participate, deduplicated and id-ordered.
        let ids: Vec<u64> = plan.ranges().iter().map(|r| r.range_id().value()).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(plan.ranges()[0].epoch(), OwnershipEpoch::initial());
    }

    // AC #5: keys that all land in a single range are trivially single-writer.
    #[test]
    fn single_range_transaction_succeeds() {
        let (catalog, orders) = two_range_catalog();
        let plan = catalog
            .plan_write_transaction(&[target(&orders, b"alice"), target(&orders, b"bob")])
            .expect("single-range transaction is admitted");
        assert_eq!(plan.writer(), &ident("CN=node-a"));
        assert_eq!(plan.ranges().len(), 1);
        assert_eq!(plan.ranges()[0].range_id(), RangeId::new(1));
    }

    // AC #1 + #2: a transaction straddling ranges owned by different writers is
    // detected and rejected, naming both writers.
    #[test]
    fn cross_range_write_transaction_is_rejected() {
        let (catalog, orders) = two_range_catalog();
        let err = catalog
            .plan_write_transaction(&[target(&orders, b"alice"), target(&orders, b"zeb")])
            .expect_err("cross-writer transaction is rejected");
        match err {
            WriteTransactionReject::CrossRange { writers } => {
                assert_eq!(writers.len(), 2);
                assert_eq!(writers[0].writer(), &ident("CN=node-a"));
                assert_eq!(writers[1].writer(), &ident("CN=node-b"));
                assert_eq!(writers[0].ranges()[0].range_id(), RangeId::new(1));
                assert_eq!(writers[1].ranges()[0].range_id(), RangeId::new(2));
            }
            other => panic!("expected CrossRange, got {other:?}"),
        }
    }

    #[test]
    fn empty_write_transaction_is_rejected() {
        let catalog = ShardOwnershipCatalog::new();
        assert_eq!(
            catalog.plan_write_transaction(&[]),
            Err(WriteTransactionReject::Empty)
        );
    }

    #[test]
    fn unroutable_write_transaction_is_rejected() {
        let catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        match catalog.plan_write_transaction(&[target(&orders, b"x")]) {
            Err(WriteTransactionReject::Unroutable { collection, key }) => {
                assert_eq!(collection, orders);
                assert_eq!(key, b"x");
            }
            other => panic!("expected Unroutable, got {other:?}"),
        }
    }

    // AC #3: a simple read fanout collects one leg per owner across ranges.
    #[test]
    fn explicit_read_fanout_collects_per_owner_legs() {
        let (catalog, orders) = two_range_catalog();
        let fanout = catalog
            .plan_read_fanout(
                &[
                    target(&orders, b"alice"),
                    target(&orders, b"zeb"),
                    target(&orders, b"bob"),
                ],
                ReadFanoutPolicy::explicit(ReadFanoutBudget::default()).allowing_partial(),
            )
            .expect("fanout planned");
        assert!(fanout.is_cross_range());
        assert_eq!(fanout.legs().len(), 2);
        assert_eq!(fanout.trace().target_count(), 3);
        assert_eq!(fanout.trace().owner_count(), 2);
        assert_eq!(fanout.trace().range_count(), 2);
        assert!(fanout.trace().partial_allowed());
        // node-a leg gets alice + bob (range 1); node-b leg gets zeb (range 2).
        let a = &fanout.legs()[0];
        assert_eq!(a.owner(), &ident("CN=node-a"));
        assert_eq!(a.targets().len(), 2);
        let b = &fanout.legs()[1];
        assert_eq!(b.owner(), &ident("CN=node-b"));
        assert_eq!(b.targets().len(), 1);
        assert_eq!(b.targets()[0].key(), b"zeb");
    }

    #[test]
    fn cross_owner_read_requires_explicit_fanout() {
        let (catalog, orders) = two_range_catalog();
        match catalog.plan_read_fanout(
            &[target(&orders, b"alice"), target(&orders, b"zeb")],
            ReadFanoutPolicy::single_owner_only(),
        ) {
            Err(ReadFanoutReject::FanoutNotExplicit { owners }) => {
                assert_eq!(owners, vec![ident("CN=node-a"), ident("CN=node-b")]);
            }
            other => panic!("expected FanoutNotExplicit, got {other:?}"),
        }
    }

    #[test]
    fn explicit_read_fanout_enforces_owner_budget() {
        let (catalog, orders) = two_range_catalog();
        match catalog.plan_read_fanout(
            &[target(&orders, b"alice"), target(&orders, b"zeb")],
            ReadFanoutPolicy::explicit(ReadFanoutBudget::new(1, 2, 8)),
        ) {
            Err(ReadFanoutReject::OwnerBudgetExceeded { requested, max }) => {
                assert_eq!(requested, 2);
                assert_eq!(max, 1);
            }
            other => panic!("expected OwnerBudgetExceeded, got {other:?}"),
        }
    }

    #[test]
    fn explicit_read_fanout_enforces_range_budget() {
        let (catalog, orders) = two_range_catalog();
        match catalog.plan_read_fanout(
            &[target(&orders, b"alice"), target(&orders, b"zeb")],
            ReadFanoutPolicy::explicit(ReadFanoutBudget::new(2, 1, 8)),
        ) {
            Err(ReadFanoutReject::RangeBudgetExceeded { requested, max }) => {
                assert_eq!(requested, 2);
                assert_eq!(max, 1);
            }
            other => panic!("expected RangeBudgetExceeded, got {other:?}"),
        }
    }

    #[test]
    fn explicit_read_fanout_enforces_target_budget_before_routing() {
        let (catalog, orders) = two_range_catalog();
        match catalog.plan_read_fanout(
            &[target(&orders, b"alice"), target(&orders, b"bob")],
            ReadFanoutPolicy::explicit(ReadFanoutBudget::new(2, 2, 1)),
        ) {
            Err(ReadFanoutReject::TargetBudgetExceeded { requested, max }) => {
                assert_eq!(requested, 2);
                assert_eq!(max, 1);
            }
            other => panic!("expected TargetBudgetExceeded, got {other:?}"),
        }
    }

    #[test]
    fn single_owner_read_is_not_cross_range() {
        let (catalog, orders) = two_range_catalog();
        let fanout = catalog
            .plan_read_fanout(
                &[target(&orders, b"alice"), target(&orders, b"bob")],
                ReadFanoutPolicy::single_owner_only(),
            )
            .expect("fanout planned");
        assert!(!fanout.is_cross_range());
        assert_eq!(fanout.legs().len(), 1);
        assert_eq!(fanout.trace().owner_count(), 1);
        assert_eq!(fanout.trace().range_count(), 1);
        assert!(!fanout.trace().partial_allowed());
    }

    #[test]
    fn single_owner_multi_range_read_is_cross_range_but_not_cross_owner() {
        let orders = collection("orders");
        let mut catalog = ShardOwnershipCatalog::new();
        catalog
            .apply_update(range(&orders, 1, bounds(b"a", b"m"), "CN=node-a"))
            .unwrap();
        catalog
            .apply_update(range(
                &orders,
                2,
                RangeBounds::new(RangeBound::key(b"m"), RangeBound::Max).unwrap(),
                "CN=node-a",
            ))
            .unwrap();

        let fanout = catalog
            .plan_read_fanout(
                &[target(&orders, b"alice"), target(&orders, b"zeb")],
                ReadFanoutPolicy::single_owner_only(),
            )
            .expect("same-owner multi-range read is admitted");
        assert!(fanout.is_cross_range());
        assert_eq!(fanout.trace().owner_count(), 1);
        assert_eq!(fanout.trace().range_count(), 2);
    }

    #[test]
    fn unroutable_read_fanout_is_rejected() {
        let catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        match catalog.plan_read_fanout(
            &[target(&orders, b"x")],
            ReadFanoutPolicy::single_owner_only(),
        ) {
            Err(ReadFanoutReject::Unroutable { collection, .. }) => {
                assert_eq!(collection, orders)
            }
            other => panic!("expected Unroutable, got {other:?}"),
        }
    }

    // AC #4: a consistent cross-range read with no snapshot fails clearly.
    #[test]
    fn consistent_read_without_snapshot_is_rejected() {
        let (catalog, orders) = two_range_catalog();
        assert_eq!(
            catalog
                .plan_consistent_read(&[target(&orders, b"alice"), target(&orders, b"zeb")], None),
            Err(ConsistentReadReject::NoSafeSnapshot)
        );
    }

    // AC #4: a snapshot missing a targeted range fails with a watermark gap.
    #[test]
    fn consistent_read_with_incomplete_snapshot_is_rejected() {
        let (catalog, orders) = two_range_catalog();
        // Snapshot covers range 1 but not range 2.
        let snapshot = GlobalReadWatermark::new().with(
            orders.clone(),
            RangeId::new(1),
            CommitWatermark::new(1, 100),
        );
        match catalog.plan_consistent_read(
            &[target(&orders, b"alice"), target(&orders, b"zeb")],
            Some(&snapshot),
        ) {
            Err(ConsistentReadReject::WatermarkGap {
                collection,
                range_id,
            }) => {
                assert_eq!(collection, orders);
                assert_eq!(range_id, RangeId::new(2));
            }
            other => panic!("expected WatermarkGap, got {other:?}"),
        }
    }

    // AC #4: with a snapshot covering every targeted range, the consistent read
    // is planned and each leg is pinned to its range's watermark.
    #[test]
    fn consistent_read_with_full_snapshot_succeeds() {
        let (catalog, orders) = two_range_catalog();
        let snapshot = GlobalReadWatermark::new()
            .with(
                orders.clone(),
                RangeId::new(1),
                CommitWatermark::new(1, 100),
            )
            .with(
                orders.clone(),
                RangeId::new(2),
                CommitWatermark::new(1, 250),
            );
        let plan = catalog
            .plan_consistent_read(
                &[target(&orders, b"alice"), target(&orders, b"zeb")],
                Some(&snapshot),
            )
            .expect("consistent read planned");
        assert_eq!(plan.legs().len(), 2);
        let a = &plan.legs()[0];
        assert_eq!(a.owner(), &ident("CN=node-a"));
        assert_eq!(a.targets()[0].watermark(), CommitWatermark::new(1, 100));
        let b = &plan.legs()[1];
        assert_eq!(b.owner(), &ident("CN=node-b"));
        assert_eq!(b.targets()[0].watermark(), CommitWatermark::new(1, 250));
    }

    #[test]
    fn empty_consistent_read_is_rejected() {
        let catalog = ShardOwnershipCatalog::new();
        assert_eq!(
            catalog.plan_consistent_read(&[], None),
            Err(ConsistentReadReject::Empty)
        );
    }

    // The rejection contract renders a readable, writer-naming message.
    #[test]
    fn cross_range_rejection_message_names_writers() {
        let (catalog, orders) = two_range_catalog();
        let err = catalog
            .plan_write_transaction(&[target(&orders, b"alice"), target(&orders, b"zeb")])
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cross-range write transaction"));
        assert!(msg.contains("CN=node-a"));
        assert!(msg.contains("CN=node-b"));
    }

    #[test]
    fn exact_claim_confined_to_one_owner_is_admitted() {
        let (catalog, orders) = two_range_catalog();

        let plan = catalog
            .plan_exact_claim(&[target(&orders, b"alice"), target(&orders, b"bob")])
            .expect("owner-local exact claim is admitted");

        assert_eq!(plan.writer(), &ident("CN=node-a"));
        assert_eq!(plan.ranges().len(), 1);
        assert_eq!(plan.ranges()[0].range_id(), RangeId::new(1));
    }

    #[test]
    fn cross_owner_exact_claim_is_rejected_explicitly() {
        let (catalog, orders) = two_range_catalog();

        let err = catalog
            .plan_exact_claim(&[target(&orders, b"alice"), target(&orders, b"zeb")])
            .expect_err("cross-owner exact claim is outside the first cluster contract");

        match &err {
            ExactClaimReject::CrossOwner { writers } => {
                assert_eq!(writers.len(), 2);
                assert_eq!(writers[0].writer(), &ident("CN=node-a"));
                assert_eq!(writers[1].writer(), &ident("CN=node-b"));
            }
            other => panic!("expected CrossOwner, got {other:?}"),
        }
        assert!(err.to_string().contains("cross-owner exact claim"));
        assert!(err.to_string().contains("first cluster contract"));
    }
}
