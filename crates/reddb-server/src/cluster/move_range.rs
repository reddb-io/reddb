//! Split-and-move planning and the move-range cutover state machine
//! (issue #1004, PRD #987, ADR 0037).
//!
//! The [`WeightedPlacementPlanner`](super::placement::WeightedPlacementPlanner)
//! decides *that* a range should move; this module decides *how* it moves and
//! drives the move safely to completion. It is the glossary's **split-and-move**
//! — *"rebalancing transition that first divides a large or hot shard/range, then
//! moves only the selected subrange to a different writer. Small ranges may move
//! whole without splitting"* — riding the glossary's **move range cutover** —
//! *"the old owner continues serving writes while the target first copies a
//! physical checkpoint/snapshot of the range directory, then catches up through
//! the logical range-indexed stream; only after catch-up does the catalog epoch
//! move write authority to the target."*
//!
//! ## Whole-range vs split-and-move
//!
//! [`classify_move`] is the small/large-or-hot decision: a range whose bytes and
//! traffic both sit under the [`SplitPolicy`] thresholds moves whole
//! ([`MoveKind::Whole`]); a range over either threshold is split first so the
//! move sheds only part of the load ([`MoveKind::Split`]). [`split_range`] then
//! carves the range at a chosen key into a retained child (the keys the owner
//! keeps) and a moved child (a fresh range id the move hands off), tiling the
//! original keyspace with no gap or overlap.
//!
//! ## The cutover, fenced and gated
//!
//! [`MoveRange`] is the state machine for one move. It encodes the move-range
//! invariant directly:
//!
//! 1. **[`CopyingSnapshot`](MovePhase::CopyingSnapshot)** — the target copies a
//!    consistent physical snapshot of the range. Throughout, the catalog still
//!    names the old owner, so the old owner *keeps serving writes*.
//! 2. **[`CatchingUp`](MovePhase::CatchingUp)** — the snapshot is installed at a
//!    consistent [`CommitWatermark`]; the target replays **range-indexed WAL
//!    records** (issue #992) from that point to close the gap to the live commit
//!    watermark, which keeps advancing because the old owner is still writing.
//! 3. **[`cut_over`](MoveRange::cut_over)** — only when the target's applied log
//!    covers the live commit watermark does the fenced
//!    [`Handoff`](super::ownership_transition::TransitionKind::Handoff) transition
//!    move the catalog epoch. The epoch bump fences the old owner (its writes now
//!    carry a stale epoch and [`admit_public_write`] rejects them) and makes the
//!    target authoritative. *The target accepts no public write until this
//!    instant* — before it, the target is a replica and the ownership gate
//!    rejects it.
//!
//! ## Interrupted moves fail safe
//!
//! A move can be interrupted at any point — a supervisor restart, a crashed
//! target. [`recover_interrupted_move`] resumes from the target's persisted
//! catch-up position and **promotes the target only if it covers the range commit
//! watermark**; otherwise it leaves the catalog untouched and the old owner keeps
//! authority. A half-copied target is never promoted, so an interrupted move can
//! lose no committed write.
//!
//! Everything here is a pure data model over the catalog plus the range-indexed
//! WAL contract — no disk, no clock, no network — so the split arithmetic, the
//! catch-up gate, the fencing, and the interrupted-move safety are all exercised
//! deterministically.
//!
//! [`admit_public_write`]: super::ownership::ShardOwnershipCatalog::admit_public_write

use crate::replication::cdc::{
    plan_range_catchup, ChangeRecord, RangeCatchupPlan, RangeStreamPosition,
};

use super::identity::NodeIdentity;
use super::ownership::{
    CatalogError, CatalogVersion, CollectionId, OwnershipEpoch, RangeBoundsError, RangeId,
    RangeOwnership, ShardOwnershipCatalog,
};
use super::ownership_transition::{
    run_transition, CatchUpEvidence, CommitWatermark, TransitionError, TransitionKind,
    TransitionOutcome, TransitionRequest,
};
use super::placement::{CollectionGroupId, CollectionGroupPlacementAuthority, RangeLoad};

/// The thresholds that decide whether a range is small enough to move whole or
/// must be split first.
///
/// The two are independent and either trips a split: a range can be small on disk
/// yet a traffic hotspot, or quiet yet too large to copy and cut over as one
/// unit. Splitting in either case lets the move shed only a subrange instead of
/// relocating the whole load at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitPolicy {
    /// A range strictly **above** this many bytes is "large" — too big to copy
    /// and cut over whole, so it is split and only a subrange moves.
    pub max_whole_move_bytes: u64,
    /// A range serving **at or above** this much read+write traffic in the
    /// observation window is "hot" — split so the move relocates only part of the
    /// traffic.
    pub hot_traffic_threshold: u64,
}

impl Default for SplitPolicy {
    fn default() -> Self {
        // Deliberately coarse defaults: only a genuinely large or genuinely hot
        // range is worth the extra split step; everything else moves whole.
        Self {
            max_whole_move_bytes: 256 * 1024 * 1024,
            hot_traffic_threshold: 10_000,
        }
    }
}

/// How a planned move should be carried out: relocate the whole range, or split
/// it first and move only a subrange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    /// Small and cool: copy and cut over the whole range in one move.
    Whole,
    /// Large or hot: divide the range and move only the selected subrange.
    Split,
}

/// Decide whether a range moves whole or is split first, from its live load and
/// the [`SplitPolicy`]. A range over the byte ceiling **or** at/over the hot
/// traffic threshold is split; otherwise it moves whole.
pub fn classify_move(load: RangeLoad, policy: &SplitPolicy) -> MoveKind {
    let large = load.bytes_used > policy.max_whole_move_bytes;
    let hot = load.traffic() >= policy.hot_traffic_threshold && policy.hot_traffic_threshold > 0;
    if large || hot {
        MoveKind::Split
    } else {
        MoveKind::Whole
    }
}

/// Which child of a split moves to the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitSide {
    /// The lower child `[lower, split_key)` moves; the owner retains the upper.
    Lower,
    /// The upper child `[split_key, upper)` moves; the owner retains the lower.
    Upper,
}

/// The two entries a [`split_range`] produces: the child the owner keeps and the
/// child the move will hand off.
///
/// Applying a split is order-sensitive — the retained child must be **narrowed
/// first**, then the moved child created — or the create would transiently
/// overlap the still-full original and the catalog would reject it.
/// [`apply`](Self::apply) does this in the right order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeSplit {
    retained: RangeOwnership,
    moved: RangeOwnership,
}

impl RangeSplit {
    /// The child the owner keeps writing — the original range id, narrowed to the
    /// non-moved keys, version advanced but epoch unchanged (no authority moved).
    pub fn retained(&self) -> &RangeOwnership {
        &self.retained
    }

    /// The carved-off child the move hands off — a fresh range id, still owned by
    /// the original owner (which keeps serving its keys until cutover) with the
    /// move target enlisted as a replica.
    pub fn moved(&self) -> &RangeOwnership {
        &self.moved
    }

    /// Install the split into the catalog: narrow the retained child first, then
    /// create the moved child. After this the two children tile the original
    /// keyspace and the move can proceed on [`moved`](Self::moved)'s range id.
    pub fn apply(&self, catalog: &mut ShardOwnershipCatalog) -> Result<(), CatalogError> {
        // Narrow the retained child first so the moved child no longer overlaps a
        // still-full original on create.
        catalog.apply_update(self.retained.clone())?;
        catalog.apply_update(self.moved.clone())?;
        Ok(())
    }
}

/// Divide `range` at `split_key` into a retained child and a moved child, with
/// `target` enlisted as a replica of the moved child so a later
/// [`MoveRange`] can hand authority to it.
///
/// `moved_id` is the fresh range id the carved-off subrange takes; it must differ
/// from `range`'s own id. `moved_side` selects which child moves: the retained
/// child keeps `range`'s id (narrowed in place), and the moved child is a brand
/// new entry at epoch/version 1 — its data still lives under the owner until the
/// move cuts over. Fails with [`SplitError`] if the split key does not fall
/// strictly inside the range or the moved id collides with the original.
pub fn split_range(
    range: &RangeOwnership,
    split_key: &[u8],
    moved_side: SplitSide,
    moved_id: RangeId,
    target: NodeIdentity,
) -> Result<RangeSplit, SplitError> {
    if moved_id == range.range_id() {
        return Err(SplitError::MovedIdCollision { id: moved_id });
    }
    let (lower_bounds, upper_bounds) = range
        .bounds()
        .split_at(split_key)
        .map_err(SplitError::Bounds)?;
    let (retained_bounds, moved_bounds) = match moved_side {
        // The moved child is the lower part; the owner retains the upper.
        SplitSide::Lower => (upper_bounds, lower_bounds),
        // The moved child is the upper part; the owner retains the lower.
        SplitSide::Upper => (lower_bounds, upper_bounds),
    };

    // The retained child keeps the original id and owner, narrowed in place.
    let retained = range.with_bounds(retained_bounds);

    // The moved child is a fresh range, still owned by the current owner (it keeps
    // serving these keys until cutover) with the target enlisted as a replica so
    // the handoff has a valid promotion candidate.
    let mut replicas: Vec<NodeIdentity> = range.replicas().to_vec();
    if !replicas.contains(&target) {
        replicas.push(target);
    }
    let moved = RangeOwnership::establish(
        range.collection().clone(),
        moved_id,
        range.shard_key_mode(),
        moved_bounds,
        range.owner().clone(),
        replicas,
        range.placement().clone(),
    );

    Ok(RangeSplit { retained, moved })
}

/// Why a range split could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    /// The split key does not fall strictly inside the range's bounds, so one
    /// child would be empty.
    Bounds(RangeBoundsError),
    /// The moved subrange was given the same range id as the range being split.
    MovedIdCollision { id: RangeId },
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bounds(err) => write!(f, "cannot split range: {err}"),
            Self::MovedIdCollision { id } => write!(
                f,
                "split moved subrange id {id} collides with the range being split"
            ),
        }
    }
}

impl std::error::Error for SplitError {}

/// Where a move-range is in its copy → catch-up → cutover lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovePhase {
    /// The target is copying a consistent physical snapshot of the range. The
    /// catalog still names the old owner, which keeps serving writes.
    CopyingSnapshot,
    /// The snapshot is installed at a consistent watermark; the target is
    /// replaying range-indexed WAL records to catch up to the live commit
    /// watermark.
    CatchingUp,
    /// The catalog epoch has moved: the target is authoritative and the old owner
    /// is fenced.
    Completed,
    /// The move was abandoned; the old owner retains authority.
    Aborted,
}

impl MovePhase {
    fn label(self) -> &'static str {
        match self {
            MovePhase::CopyingSnapshot => "copying-snapshot",
            MovePhase::CatchingUp => "catching-up",
            MovePhase::Completed => "completed",
            MovePhase::Aborted => "aborted",
        }
    }
}

/// One in-flight move-range: the bookkeeping that carries authority for one range
/// from its current owner to a target without losing a write or letting the
/// target serve early.
///
/// Built with [`begin`](Self::begin), which enlists the target as a replica and
/// captures the catalog CAS (owner / epoch / version) the cutover will use. The
/// snapshot point and the target's catch-up progress are filled in as the move
/// runs. Until [`cut_over`](Self::cut_over) succeeds the catalog is unchanged, so
/// the old owner keeps serving and the target — a mere replica — cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRange {
    collection: CollectionId,
    range_id: RangeId,
    /// The range's current owner — the move's source, fenced at cutover.
    source: NodeIdentity,
    /// The move target — promoted at cutover, a replica until then.
    target: NodeIdentity,
    /// The catalog epoch captured at [`begin`](Self::begin) — the cutover CAS.
    expected_epoch: OwnershipEpoch,
    /// The catalog version captured at [`begin`](Self::begin) — the cutover CAS.
    expected_version: CatalogVersion,
    phase: MovePhase,
    /// The consistent point the snapshot was taken at, once installed.
    snapshot_watermark: Option<CommitWatermark>,
    /// The target's range-indexed catch-up position over the shared WAL, once the
    /// snapshot is installed and catch-up begins.
    position: Option<RangeStreamPosition>,
    /// The Collection group Placement Authority that scoped this movement, when
    /// the caller uses the authority-checked workflow.
    placement_authority: Option<CollectionGroupPlacementAuthority>,
}

impl MoveRange {
    /// Start moving `(collection, range_id)` to `target`. Enlists `target` as a
    /// replica of the range if it is not one already (so the cutover has a valid
    /// promotion candidate), then captures the catalog CAS for the eventual
    /// fenced handoff. The move begins in [`CopyingSnapshot`](MovePhase::CopyingSnapshot);
    /// the catalog's *owner* is unchanged, so the old owner keeps serving writes.
    ///
    /// Fails if the range is unknown or `target` is already its owner (a move to
    /// the incumbent is a no-op).
    pub fn begin(
        catalog: &mut ShardOwnershipCatalog,
        collection: CollectionId,
        range_id: RangeId,
        target: NodeIdentity,
    ) -> Result<Self, MoveError> {
        let current =
            catalog
                .range(&collection, range_id)
                .ok_or_else(|| MoveError::UnknownRange {
                    collection: collection.clone(),
                    range_id,
                })?;
        let source = current.owner().clone();
        if target == source {
            return Err(MoveError::TargetIsOwner {
                collection,
                range_id,
                owner: source,
            });
        }

        // Enlist the target as a replica if it is not one yet — a replica is the
        // only valid handoff candidate. This advances the version but not the
        // epoch (no authority moved), so the old owner is not fenced.
        if !current.replicas().contains(&target) {
            let mut replicas: Vec<NodeIdentity> = current.replicas().to_vec();
            replicas.push(target.clone());
            let enlisted = current.update_replicas(replicas);
            catalog.apply_update(enlisted).map_err(MoveError::Catalog)?;
        }

        // Capture the CAS *after* any replica enlistment so the cutover names the
        // current catalog version.
        let current = catalog
            .range(&collection, range_id)
            .expect("range present immediately after enlist");
        Ok(Self {
            collection,
            range_id,
            source,
            target,
            expected_epoch: current.epoch(),
            expected_version: current.version(),
            phase: MovePhase::CopyingSnapshot,
            snapshot_watermark: None,
            position: None,
            placement_authority: None,
        })
    }

    /// Start a move under the Collection group Placement Authority responsible
    /// for this range's collection. The authority scopes the operational move;
    /// the later cutover still transitions only this range's owner and epoch.
    pub fn begin_authorized(
        catalog: &mut ShardOwnershipCatalog,
        collection: CollectionId,
        range_id: RangeId,
        target: NodeIdentity,
        placement_authority: CollectionGroupPlacementAuthority,
        caller: &NodeIdentity,
    ) -> Result<Self, MoveError> {
        validate_placement_authority(&collection, &placement_authority, caller)?;
        let mut movement = Self::begin(catalog, collection, range_id, target)?;
        movement.placement_authority = Some(placement_authority);
        Ok(movement)
    }

    pub fn phase(&self) -> MovePhase {
        self.phase
    }

    pub fn source(&self) -> &NodeIdentity {
        &self.source
    }

    pub fn target(&self) -> &NodeIdentity {
        &self.target
    }

    /// The consistent point the physical snapshot was taken at, once installed.
    pub fn snapshot_watermark(&self) -> Option<CommitWatermark> {
        self.snapshot_watermark
    }

    /// The target's catch-up position over the range-indexed WAL, once catch-up
    /// has begun.
    pub fn position(&self) -> Option<RangeStreamPosition> {
        self.position
    }

    /// Record that the target has installed a consistent physical snapshot taken
    /// at `at`. Moves the move into [`CatchingUp`](MovePhase::CatchingUp) and
    /// seeds the catch-up position from the snapshot point: the target has applied
    /// everything up to `at` and will accept range records ahead of it, fencing
    /// any stamped below the range's current ownership epoch.
    ///
    /// Only valid while copying the snapshot.
    pub fn complete_snapshot(&mut self, at: CommitWatermark) -> Result<(), MoveError> {
        self.expect_phase(MovePhase::CopyingSnapshot)?;
        self.snapshot_watermark = Some(at);
        self.position = Some(RangeStreamPosition::new(
            self.range_id.value(),
            at.lsn,
            at.term,
            self.expected_epoch.value(),
        ));
        self.phase = MovePhase::CatchingUp;
        Ok(())
    }

    /// Replay a slice of the shared logical stream into the target's range-indexed
    /// catch-up, advancing its applied position past every record stamped for this
    /// range (issue #992). Returns the [`RangeCatchupPlan`] so the caller can see
    /// which records applied and which were fenced. Only valid while catching up.
    pub fn record_catch_up(
        &mut self,
        records: &[ChangeRecord],
    ) -> Result<RangeCatchupPlan, MoveError> {
        self.expect_phase(MovePhase::CatchingUp)?;
        let position = self
            .position
            .as_mut()
            .expect("catch-up position present while catching up");
        let plan = plan_range_catchup(position, records);
        *position = plan.resume;
        Ok(plan)
    }

    /// The catch-up evidence the cutover will present for the target: the highest
    /// `(term, lsn)` it has applied for the range. `None` before a snapshot is
    /// installed.
    pub fn catch_up_evidence(&self) -> Option<CatchUpEvidence> {
        self.position.map(|position| {
            CatchUpEvidence::new(
                self.target.clone(),
                position.accepted_term,
                position.applied_lsn,
            )
        })
    }

    /// Whether the target's applied log covers `live` — the live range commit
    /// watermark, which has advanced past the snapshot point as the old owner kept
    /// writing. The cutover may only proceed once this holds.
    pub fn has_caught_up(&self, live: CommitWatermark) -> bool {
        self.catch_up_evidence()
            .map(|evidence| evidence.covers(live))
            .unwrap_or(false)
    }

    /// Cut over: move the catalog epoch to the target through the fenced
    /// [`Handoff`](TransitionKind::Handoff) transition, demoting the old owner to a
    /// replica. The move must be [`CatchingUp`](MovePhase::CatchingUp) and the
    /// target must cover `live` — otherwise this returns
    /// [`TargetBehindWatermark`](MoveError::TargetBehindWatermark) **without
    /// touching the catalog**, so a target that has not caught up is never
    /// promoted and the old owner keeps serving.
    ///
    /// On success the catalog names the target at a new epoch (fencing the old
    /// owner's stale-epoch writes) and the move is [`Completed`](MovePhase::Completed).
    pub fn cut_over(
        &mut self,
        catalog: &mut ShardOwnershipCatalog,
        live: CommitWatermark,
    ) -> Result<TransitionOutcome, MoveError> {
        self.expect_phase(MovePhase::CatchingUp)?;
        let evidence = self
            .catch_up_evidence()
            .expect("catch-up evidence present while catching up");
        if !evidence.covers(live) {
            return Err(MoveError::TargetBehindWatermark {
                collection: self.collection.clone(),
                range_id: self.range_id,
                target: self.target.clone(),
                watermark: live,
                applied_term: evidence.applied_term,
                applied_lsn: evidence.applied_lsn,
            });
        }

        let outcome = attempt_handoff(
            catalog,
            &self.collection,
            self.range_id,
            &self.source,
            self.expected_epoch,
            self.expected_version,
            &self.target,
            evidence,
            live,
        )?;
        self.phase = MovePhase::Completed;
        Ok(outcome)
    }

    /// Cut over under the same Collection group Placement Authority that planned
    /// the move. The authority check gates the workflow, then the catalog update
    /// remains the same per-range fenced handoff as [`cut_over`](Self::cut_over).
    pub fn cut_over_authorized(
        &mut self,
        catalog: &mut ShardOwnershipCatalog,
        live: CommitWatermark,
        caller: &NodeIdentity,
    ) -> Result<TransitionOutcome, MoveError> {
        let placement_authority = self.placement_authority.as_ref().ok_or_else(|| {
            MoveError::MissingPlacementAuthority {
                collection: self.collection.clone(),
                range_id: self.range_id,
            }
        })?;
        validate_placement_authority(&self.collection, placement_authority, caller)?;
        self.cut_over(catalog, live)
    }

    /// Abandon the move. The catalog is untouched (the old owner remains owner);
    /// the target keeps whatever copy it has but is never promoted.
    pub fn abort(&mut self) {
        self.phase = MovePhase::Aborted;
    }

    fn expect_phase(&self, expected: MovePhase) -> Result<(), MoveError> {
        if self.phase == expected {
            Ok(())
        } else {
            Err(MoveError::WrongPhase {
                expected: expected.label(),
                actual: self.phase,
            })
        }
    }
}

fn validate_placement_authority(
    collection: &CollectionId,
    placement_authority: &CollectionGroupPlacementAuthority,
    caller: &NodeIdentity,
) -> Result<(), MoveError> {
    if !placement_authority.covers(collection) {
        return Err(MoveError::CollectionOutsidePlacementAuthority {
            collection: collection.clone(),
            collection_group: placement_authority.collection_group().clone(),
            authority: placement_authority.authority().clone(),
        });
    }
    if placement_authority.authority() != caller {
        return Err(MoveError::WrongPlacementAuthority {
            collection_group: placement_authority.collection_group().clone(),
            expected: placement_authority.authority().clone(),
            actual: caller.clone(),
        });
    }
    Ok(())
}

/// Resume an interrupted move and decide its fate from the target's persisted
/// catch-up position alone — the recovery path after a supervisor restart or a
/// crash mid-move.
///
/// Promotes `target` through the fenced handoff **only if** its applied position
/// covers `live` (the range commit watermark); otherwise it leaves the catalog
/// untouched so the old owner keeps authority. This is the interrupted-move
/// safety rule: a half-copied target is never promoted, so no committed write is
/// lost when a move is cut short.
pub fn recover_interrupted_move(
    catalog: &mut ShardOwnershipCatalog,
    collection: &CollectionId,
    range_id: RangeId,
    target: &NodeIdentity,
    target_position: RangeStreamPosition,
    live: CommitWatermark,
) -> Result<MoveRecovery, MoveError> {
    let current = catalog
        .range(collection, range_id)
        .ok_or_else(|| MoveError::UnknownRange {
            collection: collection.clone(),
            range_id,
        })?;
    let source = current.owner().clone();
    let expected_epoch = current.epoch();
    let expected_version = current.version();

    let evidence = CatchUpEvidence::new(
        target.clone(),
        target_position.accepted_term,
        target_position.applied_lsn,
    );

    // The interrupted-move safety gate: promote only a target that covers the
    // range commit watermark. A target behind it is abandoned and the source
    // retains authority — the catalog is not touched.
    if !evidence.covers(live) {
        return Ok(MoveRecovery::AbortedSourceRetained {
            applied_term: evidence.applied_term,
            applied_lsn: evidence.applied_lsn,
            watermark: live,
        });
    }

    let outcome = attempt_handoff(
        catalog,
        collection,
        range_id,
        &source,
        expected_epoch,
        expected_version,
        target,
        evidence,
        live,
    )?;
    Ok(MoveRecovery::Promoted(outcome))
}

/// The outcome of recovering an interrupted move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveRecovery {
    /// The target covered the watermark and was promoted through a fenced
    /// handoff; the old owner is now fenced.
    Promoted(TransitionOutcome),
    /// The target did not cover the watermark; the move was abandoned and the
    /// source retains authority. Carries the target's applied position and the
    /// watermark it fell short of.
    AbortedSourceRetained {
        applied_term: u64,
        applied_lsn: u64,
        watermark: CommitWatermark,
    },
}

impl MoveRecovery {
    /// Whether recovery promoted the target. False when the move was abandoned.
    pub fn promoted(&self) -> bool {
        matches!(self, MoveRecovery::Promoted(_))
    }
}

/// Build and run the fenced [`Handoff`](TransitionKind::Handoff) that completes a
/// move: the target takes ownership and the old owner is demoted to a replica and
/// fenced by the epoch bump. Shared by the normal cutover and interrupted-move
/// recovery so both run the identical safety gate.
#[allow(clippy::too_many_arguments)]
fn attempt_handoff(
    catalog: &mut ShardOwnershipCatalog,
    collection: &CollectionId,
    range_id: RangeId,
    source: &NodeIdentity,
    expected_epoch: OwnershipEpoch,
    expected_version: CatalogVersion,
    target: &NodeIdentity,
    evidence: CatchUpEvidence,
    watermark: CommitWatermark,
) -> Result<TransitionOutcome, MoveError> {
    let request = TransitionRequest::new(
        TransitionKind::Handoff,
        collection.clone(),
        range_id,
        source.clone(),
        expected_epoch,
        expected_version,
        target.clone(),
        watermark,
    )
    .with_evidence(evidence)
    // Demote the old owner to a replica of the range after cutover.
    .with_replicas([source.clone()]);
    run_transition(catalog, &request).map_err(MoveError::Transition)
}

/// Why a move-range step failed. Every variant that can be returned before the
/// fenced handoff leaves the catalog untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveError {
    /// No range with this `(collection, range_id)` exists in the catalog.
    UnknownRange {
        collection: CollectionId,
        range_id: RangeId,
    },
    /// The move target is already the range's owner — a no-op move.
    TargetIsOwner {
        collection: CollectionId,
        range_id: RangeId,
        owner: NodeIdentity,
    },
    /// A move step was attempted from the wrong phase (e.g. cutting over before a
    /// snapshot was installed).
    WrongPhase {
        expected: &'static str,
        actual: MovePhase,
    },
    /// Cutover was attempted but the target's applied log does not yet cover the
    /// live commit watermark — refused, the catalog untouched.
    TargetBehindWatermark {
        collection: CollectionId,
        range_id: RangeId,
        target: NodeIdentity,
        watermark: CommitWatermark,
        applied_term: u64,
        applied_lsn: u64,
    },
    /// The scoped workflow was used without an authority token.
    MissingPlacementAuthority {
        collection: CollectionId,
        range_id: RangeId,
    },
    /// The authority token does not cover this range's collection.
    CollectionOutsidePlacementAuthority {
        collection: CollectionId,
        collection_group: CollectionGroupId,
        authority: NodeIdentity,
    },
    /// A different Placement Authority attempted the scoped transition.
    WrongPlacementAuthority {
        collection_group: CollectionGroupId,
        expected: NodeIdentity,
        actual: NodeIdentity,
    },
    /// A catalog write (replica enlistment) was rejected.
    Catalog(CatalogError),
    /// The fenced handoff transition was rejected (a CAS or safety failure) or the
    /// activation write failed.
    Transition(TransitionError),
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownRange {
                collection,
                range_id,
            } => write!(f, "no range {collection}/{range_id} to move"),
            Self::TargetIsOwner {
                collection,
                range_id,
                owner,
            } => write!(
                f,
                "move target {owner} is already the owner of {collection}/{range_id}"
            ),
            Self::WrongPhase { expected, actual } => write!(
                f,
                "move-range step expected phase {expected} but the move is {}",
                actual.label()
            ),
            Self::TargetBehindWatermark {
                collection,
                range_id,
                target,
                watermark,
                applied_term,
                applied_lsn,
            } => write!(
                f,
                "cannot cut over {collection}/{range_id} to {target}: applied term {applied_term} lsn {applied_lsn} is behind the commit watermark term {} lsn {}",
                watermark.term, watermark.lsn
            ),
            Self::MissingPlacementAuthority {
                collection,
                range_id,
            } => write!(
                f,
                "move-range {collection}/{range_id} has no collection group placement authority"
            ),
            Self::CollectionOutsidePlacementAuthority {
                collection,
                collection_group,
                authority,
            } => write!(
                f,
                "placement authority {authority} for collection group {collection_group} does not cover collection {collection}"
            ),
            Self::WrongPlacementAuthority {
                collection_group,
                expected,
                actual,
            } => write!(
                f,
                "placement authority {actual} cannot transition collection group {collection_group}; expected {expected}"
            ),
            Self::Catalog(err) => write!(f, "{err}"),
            Self::Transition(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for MoveError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ownership::{
        PlacementMetadata, RangeBound, RangeBounds, RangeRole, RangeWriteReject, ShardKeyMode,
    };
    use crate::cluster::{CollectionGroupId, CollectionGroupPlacementAuthority};
    use crate::replication::cdc::ChangeOperation;

    fn collection(name: &str) -> CollectionId {
        CollectionId::new(name).unwrap()
    }

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    /// A single full-keyspace ordered range owned by `owner` with `replicas`, so
    /// concrete split keys land inside the range.
    fn catalog_with(owner: &str, replicas: &[&str]) -> (ShardOwnershipCatalog, CollectionId) {
        let orders = collection("orders");
        let mut catalog = ShardOwnershipCatalog::new();
        catalog
            .apply_update(RangeOwnership::establish(
                orders.clone(),
                RangeId::new(1),
                ShardKeyMode::Ordered,
                RangeBounds::full(),
                ident(owner),
                replicas.iter().map(|r| ident(r)).collect::<Vec<_>>(),
                PlacementMetadata::with_replication_factor(3),
            ))
            .unwrap();
        (catalog, orders)
    }

    /// A range-indexed WAL record for `range_id` at `(term, lsn)` carrying
    /// ownership `epoch` — the catch-up feed a move-range target replays.
    fn record(range_id: u64, term: u64, lsn: u64, epoch: u64) -> ChangeRecord {
        ChangeRecord {
            term,
            lsn,
            timestamp: 1,
            operation: ChangeOperation::Insert,
            collection: "orders".to_string(),
            entity_id: lsn,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1]),
            metadata: None,
            refresh_records: None,
            range_id: Some(range_id),
            ownership_epoch: Some(epoch),
        }
    }

    // --- criterion 1: whole vs split classification ----------------------

    #[test]
    fn small_cool_range_moves_whole_large_or_hot_range_splits() {
        let policy = SplitPolicy {
            max_whole_move_bytes: 1_000,
            hot_traffic_threshold: 500,
        };
        // Small and cool -> whole.
        assert_eq!(
            classify_move(RangeLoad::idle(900), &policy),
            MoveKind::Whole
        );
        // Large on disk -> split.
        assert_eq!(
            classify_move(RangeLoad::idle(1_001), &policy),
            MoveKind::Split
        );
        // Small but hot (traffic at threshold) -> split.
        assert_eq!(
            classify_move(
                RangeLoad {
                    bytes_used: 10,
                    read_ops: 300,
                    write_ops: 200,
                },
                &policy
            ),
            MoveKind::Split
        );
        // Small and just under the hot threshold -> whole.
        assert_eq!(
            classify_move(
                RangeLoad {
                    bytes_used: 10,
                    read_ops: 250,
                    write_ops: 249,
                },
                &policy
            ),
            MoveKind::Whole
        );
    }

    // --- range split arithmetic ------------------------------------------

    #[test]
    fn split_tiles_the_keyspace_with_no_gap_or_overlap() {
        let (catalog, orders) = catalog_with("CN=node-a", &[]);
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        let split = split_range(
            range,
            b"m",
            SplitSide::Upper,
            RangeId::new(2),
            ident("CN=node-b"),
        )
        .expect("split ok");

        // Retained keeps id 1, narrowed to [Min, "m"); moved is id 2 over ["m", Max).
        assert_eq!(split.retained().range_id(), RangeId::new(1));
        assert_eq!(split.retained().bounds().lower(), &RangeBound::Min);
        assert_eq!(
            split.retained().bounds().upper(),
            &RangeBound::key(b"m".to_vec())
        );
        assert_eq!(split.moved().range_id(), RangeId::new(2));
        assert_eq!(
            split.moved().bounds().lower(),
            &RangeBound::key(b"m".to_vec())
        );
        assert_eq!(split.moved().bounds().upper(), &RangeBound::Max);

        // Both children stay with the original owner; the target is a replica of
        // the moved child only.
        assert_eq!(split.retained().owner(), &ident("CN=node-a"));
        assert_eq!(split.moved().owner(), &ident("CN=node-a"));
        assert_eq!(
            split.moved().role_of(&ident("CN=node-b")),
            RangeRole::Replica
        );

        // The retained child's epoch is unchanged (no authority moved); only the
        // version advanced.
        assert_eq!(split.retained().epoch(), range.epoch());
        assert!(split.retained().version() > range.version());
    }

    #[test]
    fn split_rejects_an_out_of_range_key_and_an_id_collision() {
        let (catalog, orders) = catalog_with("CN=node-a", &[]);
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        // Reusing the original id is a collision.
        assert!(matches!(
            split_range(
                range,
                b"m",
                SplitSide::Upper,
                RangeId::new(1),
                ident("CN=node-b")
            ),
            Err(SplitError::MovedIdCollision { .. })
        ));

        // A bounded range cannot be split at or outside its bounds.
        let bounded = RangeOwnership::establish(
            orders.clone(),
            RangeId::new(5),
            ShardKeyMode::Ordered,
            RangeBounds::new(
                RangeBound::key(b"d".to_vec()),
                RangeBound::key(b"h".to_vec()),
            )
            .unwrap(),
            ident("CN=node-a"),
            Vec::<NodeIdentity>::new(),
            PlacementMetadata::with_replication_factor(1),
        );
        assert!(matches!(
            split_range(
                &bounded,
                b"z",
                SplitSide::Upper,
                RangeId::new(6),
                ident("CN=node-b")
            ),
            Err(SplitError::Bounds(_))
        ));
    }

    #[test]
    fn applying_a_split_installs_two_non_overlapping_ranges() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &[]);
        let range = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        let split = split_range(
            &range,
            b"m",
            SplitSide::Upper,
            RangeId::new(2),
            ident("CN=node-b"),
        )
        .unwrap();
        split.apply(&mut catalog).expect("split applies cleanly");

        assert_eq!(catalog.range_count(), 2);
        // Routing now resolves either side to exactly one range.
        assert_eq!(
            catalog.route(&orders, b"a").unwrap().range_id(),
            RangeId::new(1)
        );
        assert_eq!(
            catalog.route(&orders, b"z").unwrap().range_id(),
            RangeId::new(2)
        );
    }

    // --- criterion 2 + 3 + 4: snapshot, catch-up, fenced cutover ---------

    #[test]
    fn whole_range_move_copies_snapshot_catches_up_then_cuts_over() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &[]);
        let mut mv = MoveRange::begin(
            &mut catalog,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-b"),
        )
        .expect("begin ok");
        assert_eq!(mv.phase(), MovePhase::CopyingSnapshot);

        // Criterion 3: while copying, the catalog still names node-a, which keeps
        // serving public writes.
        let serving_epoch = catalog.range(&orders, RangeId::new(1)).unwrap().epoch();
        assert!(catalog
            .admit_public_write(&ident("CN=node-a"), &orders, b"k", serving_epoch)
            .is_ok());
        // Criterion 4: the target is only a replica, so its writes are rejected.
        let err = catalog
            .admit_public_write(&ident("CN=node-b"), &orders, b"k", serving_epoch)
            .unwrap_err();
        assert!(matches!(err, RangeWriteReject::NotOwner { .. }));

        // Criterion 2: snapshot taken at a consistent point, then range-indexed
        // WAL catch-up closes the gap to the live watermark.
        mv.complete_snapshot(CommitWatermark::new(1, 100)).unwrap();
        assert_eq!(mv.phase(), MovePhase::CatchingUp);
        // The old owner kept writing: live watermark is now term 1 lsn 130.
        let plan = mv
            .record_catch_up(&[
                record(1, 1, 110, 1),
                record(1, 1, 120, 1),
                record(1, 1, 130, 1),
            ])
            .unwrap();
        assert_eq!(plan.apply_count(), 3);
        assert!(mv.has_caught_up(CommitWatermark::new(1, 130)));

        let outcome = mv
            .cut_over(&mut catalog, CommitWatermark::new(1, 130))
            .unwrap();
        assert_eq!(mv.phase(), MovePhase::Completed);
        assert_eq!(outcome.kind, TransitionKind::Handoff);
        assert!(outcome.fenced_old_owner());

        // Criterion 3 (after cutover): node-a is fenced — demoted to a replica at
        // the stale epoch, its public write is rejected.
        let err = catalog
            .admit_public_write(&ident("CN=node-a"), &orders, b"k", serving_epoch)
            .unwrap_err();
        assert!(matches!(
            err,
            RangeWriteReject::NotOwner { .. } | RangeWriteReject::StaleEpoch { .. }
        ));
        // Criterion 4 (after cutover): the target is now the owner and is admitted
        // at the new epoch.
        let new_epoch = catalog.range(&orders, RangeId::new(1)).unwrap().epoch();
        assert!(catalog
            .admit_public_write(&ident("CN=node-b"), &orders, b"k", new_epoch)
            .is_ok());
    }

    // --- criterion 4: cutover refused before catch-up --------------------

    #[test]
    fn cutover_before_catch_up_is_refused_and_leaves_catalog_untouched() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &[]);
        let mut mv = MoveRange::begin(
            &mut catalog,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-b"),
        )
        .unwrap();
        mv.complete_snapshot(CommitWatermark::new(1, 100)).unwrap();
        // Only caught up to lsn 110, but the live watermark is lsn 200.
        mv.record_catch_up(&[record(1, 1, 110, 1)]).unwrap();
        assert!(!mv.has_caught_up(CommitWatermark::new(1, 200)));

        let err = mv
            .cut_over(&mut catalog, CommitWatermark::new(1, 200))
            .unwrap_err();
        assert!(matches!(err, MoveError::TargetBehindWatermark { .. }));
        // The move did not advance and node-a is still the owner.
        assert_eq!(mv.phase(), MovePhase::CatchingUp);
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-a")
        );
    }

    #[test]
    fn authorized_cutover_rejects_the_wrong_collection_group_authority() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &[]);
        let authority = CollectionGroupPlacementAuthority::new(
            CollectionGroupId::new("commerce").unwrap(),
            ident("CN=pa-commerce"),
            [orders.clone()],
        )
        .unwrap();
        let mut mv = MoveRange::begin_authorized(
            &mut catalog,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-b"),
            authority,
            &ident("CN=pa-commerce"),
        )
        .unwrap();
        mv.complete_snapshot(CommitWatermark::new(1, 10)).unwrap();
        mv.record_catch_up(&[record(1, 1, 20, 1)]).unwrap();

        let err = mv
            .cut_over_authorized(
                &mut catalog,
                CommitWatermark::new(1, 20),
                &ident("CN=pa-analytics"),
            )
            .unwrap_err();

        assert!(matches!(err, MoveError::WrongPlacementAuthority { .. }));
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-a")
        );

        mv.cut_over_authorized(
            &mut catalog,
            CommitWatermark::new(1, 20),
            &ident("CN=pa-commerce"),
        )
        .unwrap();
        let moved = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(moved.owner(), &ident("CN=node-b"));
        assert!(moved.epoch().value() > OwnershipEpoch::initial().value());
    }

    // --- split-and-move end to end ---------------------------------------

    #[test]
    fn split_and_move_relocates_only_the_subrange() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &[]);
        // Split the hot/large range and move only the upper subrange to node-b.
        let range = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        let split = split_range(
            &range,
            b"m",
            SplitSide::Upper,
            RangeId::new(2),
            ident("CN=node-b"),
        )
        .unwrap();
        split.apply(&mut catalog).unwrap();

        // Move the carved-off subrange (id 2) to node-b.
        let mut mv = MoveRange::begin(
            &mut catalog,
            orders.clone(),
            RangeId::new(2),
            ident("CN=node-b"),
        )
        .unwrap();
        mv.complete_snapshot(CommitWatermark::new(1, 10)).unwrap();
        mv.record_catch_up(&[record(2, 1, 20, 1)]).unwrap();
        mv.cut_over(&mut catalog, CommitWatermark::new(1, 20))
            .unwrap();

        // The moved subrange is now owned by node-b; the retained subrange stays
        // with node-a, untouched by the move.
        assert_eq!(
            catalog.range(&orders, RangeId::new(2)).unwrap().owner(),
            &ident("CN=node-b")
        );
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-a")
        );
        // node-a still owns the lower keys; node-b owns the upper keys.
        assert_eq!(
            catalog.route(&orders, b"a").unwrap().owner(),
            &ident("CN=node-a")
        );
        assert_eq!(
            catalog.route(&orders, b"z").unwrap().owner(),
            &ident("CN=node-b")
        );
    }

    // --- criterion 2: catch-up only consumes this range's records --------

    #[test]
    fn catch_up_ignores_other_ranges_and_fences_stale_epoch_records() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &[]);
        let mut mv = MoveRange::begin(
            &mut catalog,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-b"),
        )
        .unwrap();
        mv.complete_snapshot(CommitWatermark::new(1, 100)).unwrap();

        // A shared WAL slice: a record for another range, a stale-epoch record
        // from a deposed owner, and two genuine records for this range.
        let plan = mv
            .record_catch_up(&[
                record(99, 1, 105, 1), // other range — skipped
                record(1, 1, 110, 0),  // stale ownership epoch (0 < 1) — fenced
                record(1, 1, 120, 1),  // applied
                record(1, 1, 130, 1),  // applied
            ])
            .unwrap();
        assert_eq!(plan.apply_count(), 2);
        assert_eq!(plan.rejected.len(), 1);
        // Only this range's genuine records advanced the position.
        assert_eq!(mv.position().unwrap().applied_lsn, 130);
        assert!(mv.has_caught_up(CommitWatermark::new(1, 130)));
    }

    // --- criterion 5: interrupted-move recovery safety -------------------

    #[test]
    fn interrupted_move_promotes_a_caught_up_target() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // The supervisor died mid-move; node-b's persisted position covers the
        // live watermark (term 1 lsn 50).
        let position = RangeStreamPosition::new(RangeId::new(1).value(), 50, 1, 1);
        let recovery = recover_interrupted_move(
            &mut catalog,
            &orders,
            RangeId::new(1),
            &ident("CN=node-b"),
            position,
            CommitWatermark::new(1, 50),
        )
        .unwrap();
        assert!(recovery.promoted());
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-b")
        );
    }

    #[test]
    fn interrupted_move_abandons_a_target_behind_the_watermark() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // node-b only applied through lsn 40 but the watermark is lsn 50 — it must
        // not be promoted.
        let position = RangeStreamPosition::new(RangeId::new(1).value(), 40, 1, 1);
        let recovery = recover_interrupted_move(
            &mut catalog,
            &orders,
            RangeId::new(1),
            &ident("CN=node-b"),
            position,
            CommitWatermark::new(1, 50),
        )
        .unwrap();
        assert!(!recovery.promoted());
        assert!(matches!(
            recovery,
            MoveRecovery::AbortedSourceRetained {
                applied_lsn: 40,
                ..
            }
        ));
        // The source kept authority; the catalog is unchanged.
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-a")
        );
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().epoch(),
            OwnershipEpoch::initial()
        );
    }

    #[test]
    fn move_to_the_incumbent_owner_is_rejected() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &[]);
        let err = MoveRange::begin(&mut catalog, orders, RangeId::new(1), ident("CN=node-a"))
            .unwrap_err();
        assert!(matches!(err, MoveError::TargetIsOwner { .. }));
    }

    #[test]
    fn begin_enlists_the_target_as_a_replica() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &[]);
        let mv = MoveRange::begin(
            &mut catalog,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-b"),
        )
        .unwrap();
        assert_eq!(mv.source(), &ident("CN=node-a"));
        // node-b is now a replica of the range — a valid handoff candidate.
        assert_eq!(
            catalog
                .range(&orders, RangeId::new(1))
                .unwrap()
                .role_of(&ident("CN=node-b")),
            RangeRole::Replica
        );
    }
}
