-------------------------- MODULE OwnershipTransition --------------------------
(***************************************************************************)
(* Range ownership-transition safety for RedDB (issue #1838, ADR 0037).   *)
(*                                                                         *)
(* Property proved: a range ownership epoch has at most one owner that can *)
(* accept durable writes, and every write acknowledged at or below the     *)
(* range commit watermark remains present on the catalog owner across      *)
(* normal promotion, cooperative handoff, crash failover, and forced       *)
(* ownership transition.                                                   *)
(*                                                                         *)
(* Modelled mechanisms.                                                     *)
(*                                                                         *)
(*   * The ownership catalog is authoritative: each range has one catalog  *)
(*     owner and a monotonically increasing ownership epoch.                *)
(*   * The admission gate is below routing. AcceptDurableWrite requires an *)
(*     open gate, a live holder, and a lease whose epoch equals the catalog *)
(*     epoch, so stale owners are fenced even if clients route to them.     *)
(*   * Cooperative handoff closes the old owner's gate before installing a *)
(*     caught-up new owner at the next epoch.                               *)
(*   * Crash failover installs a live caught-up owner after the old owner  *)
(*     has crashed.                                                         *)
(*   * Forced ownership transition skips cooperation but still bumps the   *)
(*     epoch and requires the new owner to cover the commit watermark.      *)
(*                                                                         *)
(* Intentional simplifications.                                             *)
(*                                                                         *)
(*   * A durable write is identified only by (range, ownership epoch, seq); *)
(*     payload bytes, WAL page shape, and collection ids are out of scope.  *)
(*   * ReplicateCommitted abstracts catch-up, snapshot transfer, and WAL    *)
(*     replay as copying already acknowledged entries to another live node. *)
(*   * The model checks safety only. It does not promise that failover is   *)
(*     always available if no live node covers the commit watermark.        *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Nodes,
    Ranges,
    MaxEpoch,
    MaxSeq

ASSUME MaxEpoch \in Nat /\ MaxEpoch >= 1
ASSUME MaxSeq \in Nat /\ MaxSeq >= 1

Nil == "none"

Epochs == 1..MaxEpoch
Entries == [rng : Ranges, epoch : Epochs, seq : 1..MaxSeq]
TransitionKinds == {"normal", "cooperative", "crash", "forced"}

VARIABLES
    catalogOwner,    \* authoritative range owner, or Nil before promotion
    catalogEpoch,    \* authoritative range ownership epoch
    leaseOwner,      \* current holder of the write-admission lease
    leaseEpoch,      \* epoch attached to the current lease
    gateOpen,        \* local write-admission gate for each range
    up,              \* process availability
    durable,         \* durable entries held by each node
    acknowledged,    \* writes acknowledged to clients
    commitWatermark, \* highest acknowledged sequence per range
    acceptedOwners,  \* owners that accepted writes per range/epoch
    seenTransitions  \* transition classes reached by the bounded model

vars ==
    <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, gateOpen, up,
      durable, acknowledged, commitWatermark, acceptedOwners, seenTransitions>>

NoAcceptedOwners == [r \in Ranges |-> [e \in Epochs |-> {}]]

TypeOK ==
    /\ catalogOwner \in [Ranges -> Nodes \union {Nil}]
    /\ catalogEpoch \in [Ranges -> 0..MaxEpoch]
    /\ leaseOwner \in [Ranges -> Nodes \union {Nil}]
    /\ leaseEpoch \in [Ranges -> 0..MaxEpoch]
    /\ gateOpen \in [Ranges -> BOOLEAN]
    /\ up \in [Nodes -> BOOLEAN]
    /\ durable \in [Nodes -> SUBSET Entries]
    /\ acknowledged \subseteq Entries
    /\ commitWatermark \in [Ranges -> 0..MaxSeq]
    /\ acceptedOwners \in [Ranges -> [Epochs -> SUBSET Nodes]]
    /\ seenTransitions \subseteq TransitionKinds
    /\ \A r \in Ranges :
        /\ catalogOwner[r] = Nil <=> catalogEpoch[r] = 0
        /\ leaseOwner[r] = Nil <=> leaseEpoch[r] = 0
        /\ leaseOwner[r] # Nil => leaseEpoch[r] = catalogEpoch[r]
        /\ commitWatermark[r] <= MaxSeq
    /\ \A e \in acknowledged :
        /\ e.seq <= commitWatermark[e.rng]
        /\ e.epoch <= catalogEpoch[e.rng]
        /\ \E n \in Nodes : e \in durable[n]

Init ==
    /\ catalogOwner = [r \in Ranges |-> Nil]
    /\ catalogEpoch = [r \in Ranges |-> 0]
    /\ leaseOwner = [r \in Ranges |-> Nil]
    /\ leaseEpoch = [r \in Ranges |-> 0]
    /\ gateOpen = [r \in Ranges |-> FALSE]
    /\ up = [n \in Nodes |-> TRUE]
    /\ durable = [n \in Nodes |-> {}]
    /\ acknowledged = {}
    /\ commitWatermark = [r \in Ranges |-> 0]
    /\ acceptedOwners = NoAcceptedOwners
    /\ seenTransitions = {}

CoversWatermark(r, n) ==
    \A e \in acknowledged :
        e.rng = r /\ e.seq <= commitWatermark[r] => e \in durable[n]

NormalPromotion(r, n) ==
    /\ catalogOwner[r] = Nil
    /\ catalogEpoch[r] = 0
    /\ up[n]
    /\ catalogOwner' = [catalogOwner EXCEPT ![r] = n]
    /\ catalogEpoch' = [catalogEpoch EXCEPT ![r] = 1]
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = n]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = 1]
    /\ gateOpen' = [gateOpen EXCEPT ![r] = TRUE]
    /\ seenTransitions' = seenTransitions \union {"normal"}
    /\ UNCHANGED <<up, durable, acknowledged, commitWatermark, acceptedOwners>>

CloseAdmissionGate(r) ==
    /\ catalogOwner[r] # Nil
    /\ up[catalogOwner[r]]
    /\ gateOpen[r]
    /\ gateOpen' = [gateOpen EXCEPT ![r] = FALSE]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, up, durable,
          acknowledged, commitWatermark, acceptedOwners, seenTransitions>>

OpenAdmissionGate(r) ==
    /\ catalogOwner[r] # Nil
    /\ up[catalogOwner[r]]
    /\ leaseOwner[r] = catalogOwner[r]
    /\ leaseEpoch[r] = catalogEpoch[r]
    /\ ~gateOpen[r]
    /\ gateOpen' = [gateOpen EXCEPT ![r] = TRUE]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, up, durable,
          acknowledged, commitWatermark, acceptedOwners, seenTransitions>>

AcceptDurableWrite(r, n) ==
    /\ catalogOwner[r] = n
    /\ leaseOwner[r] = n
    /\ leaseEpoch[r] = catalogEpoch[r]
    /\ gateOpen[r]
    /\ up[n]
    /\ catalogEpoch[r] \in Epochs
    /\ commitWatermark[r] < MaxSeq
    /\ LET seq == commitWatermark[r] + 1 IN
       LET entry == [rng |-> r, epoch |-> catalogEpoch[r], seq |-> seq] IN
        /\ durable' = [durable EXCEPT ![n] = durable[n] \union {entry}]
        /\ acknowledged' = acknowledged \union {entry}
        /\ commitWatermark' = [commitWatermark EXCEPT ![r] = seq]
        /\ acceptedOwners' =
            [acceptedOwners EXCEPT
                ![r][catalogEpoch[r]] =
                    acceptedOwners[r][catalogEpoch[r]] \union {n}]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, gateOpen, up,
          seenTransitions>>

ReplicateCommitted(r, n) ==
    /\ up[n]
    /\ \E entry \in acknowledged :
        /\ entry.rng = r
        /\ entry.seq <= commitWatermark[r]
        /\ entry \notin durable[n]
        /\ durable' = [durable EXCEPT ![n] = durable[n] \union {entry}]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, gateOpen, up,
          acknowledged, commitWatermark, acceptedOwners, seenTransitions>>

CooperativeHandoff(r, old, new) ==
    /\ catalogOwner[r] = old
    /\ old # new
    /\ up[old]
    /\ up[new]
    /\ ~gateOpen[r]
    /\ catalogEpoch[r] < MaxEpoch
    /\ CoversWatermark(r, new)
    /\ catalogOwner' = [catalogOwner EXCEPT ![r] = new]
    /\ catalogEpoch' = [catalogEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = new]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ gateOpen' = [gateOpen EXCEPT ![r] = TRUE]
    /\ seenTransitions' = seenTransitions \union {"cooperative"}
    /\ UNCHANGED <<up, durable, acknowledged, commitWatermark, acceptedOwners>>

CrashFailover(r, old, new) ==
    /\ catalogOwner[r] = old
    /\ old # new
    /\ ~up[old]
    /\ up[new]
    /\ catalogEpoch[r] < MaxEpoch
    /\ CoversWatermark(r, new)
    /\ catalogOwner' = [catalogOwner EXCEPT ![r] = new]
    /\ catalogEpoch' = [catalogEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = new]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ gateOpen' = [gateOpen EXCEPT ![r] = TRUE]
    /\ seenTransitions' = seenTransitions \union {"crash"}
    /\ UNCHANGED <<up, durable, acknowledged, commitWatermark, acceptedOwners>>

ForcedTransition(r, new) ==
    /\ catalogOwner[r] # Nil
    /\ catalogOwner[r] # new
    /\ up[new]
    /\ catalogEpoch[r] < MaxEpoch
    /\ CoversWatermark(r, new)
    /\ catalogOwner' = [catalogOwner EXCEPT ![r] = new]
    /\ catalogEpoch' = [catalogEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = new]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ gateOpen' = [gateOpen EXCEPT ![r] = TRUE]
    /\ seenTransitions' = seenTransitions \union {"forced"}
    /\ UNCHANGED <<up, durable, acknowledged, commitWatermark, acceptedOwners>>

Crash(n) ==
    /\ up[n]
    /\ up' = [up EXCEPT ![n] = FALSE]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, gateOpen,
          durable, acknowledged, commitWatermark, acceptedOwners, seenTransitions>>

Restart(n) ==
    /\ ~up[n]
    /\ up' = [up EXCEPT ![n] = TRUE]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, gateOpen,
          durable, acknowledged, commitWatermark, acceptedOwners, seenTransitions>>

Next ==
    \/ \E r \in Ranges, n \in Nodes : NormalPromotion(r, n)
    \/ \E r \in Ranges : CloseAdmissionGate(r)
    \/ \E r \in Ranges : OpenAdmissionGate(r)
    \/ \E r \in Ranges, n \in Nodes : AcceptDurableWrite(r, n)
    \/ \E r \in Ranges, n \in Nodes : ReplicateCommitted(r, n)
    \/ \E r \in Ranges, old \in Nodes, new \in Nodes :
        CooperativeHandoff(r, old, new)
    \/ \E r \in Ranges, old \in Nodes, new \in Nodes :
        CrashFailover(r, old, new)
    \/ \E r \in Ranges, n \in Nodes : ForcedTransition(r, n)
    \/ \E n \in Nodes : Crash(n)
    \/ \E n \in Nodes : Restart(n)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* THE PROPERTIES.                                                         *)
(***************************************************************************)

SingleOwnerPerEpoch ==
    \A r \in Ranges, e \in Epochs :
        Cardinality(acceptedOwners[r][e]) <= 1

NoAcknowledgedWriteLoss ==
    \A r \in Ranges :
        catalogOwner[r] # Nil =>
            \A entry \in acknowledged :
                entry.rng = r /\ entry.seq <= commitWatermark[r] =>
                    entry \in durable[catalogOwner[r]]

=============================================================================
