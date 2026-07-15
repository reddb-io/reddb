--------------------------- MODULE OwnershipTransition ---------------------------
(***************************************************************************)
(* RedDB range ownership-transition safety model (issue #1838).           *)
(*                                                                         *)
(* This model covers the control/data-plane boundary named by ADR 0037 and *)
(* ADR 0064: the ownership catalog names one Range Owner for a range epoch, *)
(* the local admission gate accepts durable writes only when the owner also *)
(* holds a matching lease, and transitions bump the ownership epoch.        *)
(*                                                                         *)
(* Modelled mechanisms.                                                     *)
(*                                                                         *)
(*   * Normal promotion creates the first owner for an unowned range.       *)
(*   * Cooperative handoff moves ownership from a live old owner to a live  *)
(*     candidate that covers the commit watermark.                          *)
(*   * Crash failover promotes a live candidate when the old owner is down  *)
(*     and the candidate covers the commit watermark.                       *)
(*   * Forced transition uses the same epoch bump and watermark coverage    *)
(*     rule, but records audit evidence and does not require the old owner  *)
(*     to be live, down, or cooperative.                                    *)
(*   * Lease expiry/reacquire model the admission gate closing and opening  *)
(*     below routing.                                                       *)
(*   * Recovery may truncate divergent suffixes, but never an acknowledged  *)
(*     entry already present on that node.                                  *)
(*                                                                         *)
(* Safety properties checked by TLC.                                        *)
(*                                                                         *)
(*   * SingleOwnerPerEpoch: at most one node ever accepts durable writes    *)
(*     for a given range and ownership epoch.                               *)
(*   * NoAcknowledgedWriteLoss: every write acknowledged at or below the    *)
(*     range commit watermark remains present on the current owner after    *)
(*     handoff, failover, forced transition, and recovery.                  *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Nodes,
    Ranges,
    MaxEpoch,
    MaxLogLen

ASSUME MaxEpoch \in Nat /\ MaxLogLen \in Nat

Nil == "none"

Quorum == {S \in SUBSET Nodes : Cardinality(S) * 2 > Cardinality(Nodes)}

BoundedLogs == UNION {[1..i -> 1..MaxEpoch] : i \in 0..MaxLogLen}

Entries == [range : Ranges, lsn : 1..MaxLogLen, epoch : 1..MaxEpoch]
Acceptances == [range : Ranges, epoch : 1..MaxEpoch, node : Nodes]
ForcedEvidence == [range : Ranges, epoch : 1..MaxEpoch, node : Nodes]

VARIABLES
    catalogOwner,    \* authoritative owner in the shard/range catalog
    catalogEpoch,    \* ownership epoch in the catalog
    leaseOwner,      \* owner currently admitted to write locally
    leaseEpoch,      \* epoch attached to that lease
    up,              \* process availability
    log,             \* per-node, per-range durable write epochs
    commitWatermark, \* highest acknowledged LSN per range
    acked,           \* writes acknowledged at/below the watermark
    accepted,        \* history of nodes that accepted durable writes
    forcedAudit      \* forced-transition evidence recorded by the model

vars ==
    <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, up, log,
      commitWatermark, acked, accepted, forcedAudit>>

Prefix(seq, k) == SubSeq(seq, 1, k)

LeaseAdmits(n, r) ==
    /\ catalogOwner[r] = n
    /\ catalogEpoch[r] > 0
    /\ leaseOwner[r] = n
    /\ leaseEpoch[r] = catalogEpoch[r]
    /\ up[n]

HoldsEntry(n, e) ==
    /\ Len(log[n][e.range]) >= e.lsn
    /\ log[n][e.range][e.lsn] = e.epoch

CoversWatermark(n, r) ==
    /\ Len(log[n][r]) >= commitWatermark[r]
    /\ \A e \in acked :
        (e.range = r /\ e.lsn <= commitWatermark[r]) => HoldsEntry(n, e)

TypeOK ==
    /\ catalogOwner \in [Ranges -> Nodes \union {Nil}]
    /\ catalogEpoch \in [Ranges -> 0..MaxEpoch]
    /\ leaseOwner \in [Ranges -> Nodes \union {Nil}]
    /\ leaseEpoch \in [Ranges -> 0..MaxEpoch]
    /\ up \in [Nodes -> BOOLEAN]
    /\ log \in [Nodes -> [Ranges -> BoundedLogs]]
    /\ commitWatermark \in [Ranges -> 0..MaxLogLen]
    /\ acked \subseteq Entries
    /\ accepted \subseteq Acceptances
    /\ forcedAudit \subseteq ForcedEvidence
    /\ \A r \in Ranges :
        /\ (catalogOwner[r] = Nil) = (catalogEpoch[r] = 0)
        /\ (leaseOwner[r] = Nil) = (leaseEpoch[r] = 0)
        /\ leaseOwner[r] # Nil =>
            /\ leaseOwner[r] = catalogOwner[r]
            /\ leaseEpoch[r] = catalogEpoch[r]

Init ==
    /\ catalogOwner = [r \in Ranges |-> Nil]
    /\ catalogEpoch = [r \in Ranges |-> 0]
    /\ leaseOwner = [r \in Ranges |-> Nil]
    /\ leaseEpoch = [r \in Ranges |-> 0]
    /\ up = [n \in Nodes |-> TRUE]
    /\ log = [n \in Nodes |-> [r \in Ranges |-> << >>]]
    /\ commitWatermark = [r \in Ranges |-> 0]
    /\ acked = {}
    /\ accepted = {}
    /\ forcedAudit = {}

NormalPromote(r, n) ==
    /\ catalogOwner[r] = Nil
    /\ up[n]
    /\ MaxEpoch >= 1
    /\ catalogOwner' = [catalogOwner EXCEPT ![r] = n]
    /\ catalogEpoch' = [catalogEpoch EXCEPT ![r] = 1]
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = n]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = 1]
    /\ log' = [log EXCEPT ![n][r] = Prefix(log[n][r], commitWatermark[r])]
    /\ UNCHANGED <<up, commitWatermark, acked, accepted, forcedAudit>>

CooperativeHandoff(r, n) ==
    /\ catalogOwner[r] # Nil
    /\ catalogOwner[r] # n
    /\ up[catalogOwner[r]]
    /\ up[n]
    /\ LeaseAdmits(catalogOwner[r], r)
    /\ catalogEpoch[r] < MaxEpoch
    /\ CoversWatermark(n, r)
    /\ catalogOwner' = [catalogOwner EXCEPT ![r] = n]
    /\ catalogEpoch' = [catalogEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = n]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ log' = [log EXCEPT ![n][r] = Prefix(log[n][r], commitWatermark[r])]
    /\ UNCHANGED <<up, commitWatermark, acked, accepted, forcedAudit>>

CrashFailover(r, n) ==
    /\ catalogOwner[r] # Nil
    /\ catalogOwner[r] # n
    /\ ~up[catalogOwner[r]]
    /\ up[n]
    /\ catalogEpoch[r] < MaxEpoch
    /\ CoversWatermark(n, r)
    /\ catalogOwner' = [catalogOwner EXCEPT ![r] = n]
    /\ catalogEpoch' = [catalogEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = n]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ log' = [log EXCEPT ![n][r] = Prefix(log[n][r], commitWatermark[r])]
    /\ UNCHANGED <<up, commitWatermark, acked, accepted, forcedAudit>>

ForcedTransition(r, n) ==
    /\ catalogOwner[r] # Nil
    /\ catalogOwner[r] # n
    /\ up[n]
    /\ catalogEpoch[r] < MaxEpoch
    /\ CoversWatermark(n, r)
    /\ catalogOwner' = [catalogOwner EXCEPT ![r] = n]
    /\ catalogEpoch' = [catalogEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = n]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = catalogEpoch[r] + 1]
    /\ log' = [log EXCEPT ![n][r] = Prefix(log[n][r], commitWatermark[r])]
    /\ forcedAudit' =
        forcedAudit \union
            {[range |-> r, epoch |-> catalogEpoch[r] + 1, node |-> n]}
    /\ UNCHANGED <<up, commitWatermark, acked, accepted>>

ExpireLease(r) ==
    /\ catalogOwner[r] # Nil
    /\ leaseOwner[r] = catalogOwner[r]
    /\ leaseEpoch[r] = catalogEpoch[r]
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = Nil]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = 0]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, up, log, commitWatermark,
          acked, accepted, forcedAudit>>

RenewLease(r) ==
    /\ catalogOwner[r] # Nil
    /\ up[catalogOwner[r]]
    /\ leaseOwner[r] = Nil
    /\ leaseOwner' = [leaseOwner EXCEPT ![r] = catalogOwner[r]]
    /\ leaseEpoch' = [leaseEpoch EXCEPT ![r] = catalogEpoch[r]]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, up, log, commitWatermark,
          acked, accepted, forcedAudit>>

AcceptWrite(n, r) ==
    /\ LeaseAdmits(n, r)
    /\ Len(log[n][r]) < MaxLogLen
    /\ log' = [log EXCEPT ![n][r] = Append(log[n][r], catalogEpoch[r])]
    /\ accepted' =
        accepted \union {[range |-> r, epoch |-> catalogEpoch[r], node |-> n]}
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, up,
          commitWatermark, acked, forcedAudit>>

Replicate(src, dst, r) ==
    /\ src # dst
    /\ LeaseAdmits(src, r)
    /\ up[dst]
    /\ \E i \in 1..Len(log[src][r]) :
        /\ i <= Len(log[dst][r]) + 1
        /\ (i > 1 =>
            (Len(log[dst][r]) >= i - 1
             /\ log[dst][r][i - 1] = log[src][r][i - 1]))
        /\ log' =
            [log EXCEPT ![dst][r] =
                IF Len(log[dst][r]) >= i /\ log[dst][r][i] = log[src][r][i]
                THEN log[dst][r]
                ELSE Prefix(log[dst][r], i - 1) \o <<log[src][r][i]>>]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, up,
          commitWatermark, acked, accepted, forcedAudit>>

RecoverToWatermark(n, r) ==
    /\ up[n]
    /\ \E k \in 0..Len(log[n][r]) :
        /\ \A e \in acked : (e.range = r /\ HoldsEntry(n, e)) => k >= e.lsn
        /\ log' = [log EXCEPT ![n][r] = Prefix(log[n][r], k)]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, up,
          commitWatermark, acked, accepted, forcedAudit>>

AdvanceCommit(n, r) ==
    /\ LeaseAdmits(n, r)
    /\ \E i \in (commitWatermark[r] + 1)..Len(log[n][r]) :
        /\ log[n][r][i] = catalogEpoch[r]
        /\ \E Q \in Quorum :
            \A m \in Q :
                /\ Len(log[m][r]) >= i
                /\ Prefix(log[m][r], i) = Prefix(log[n][r], i)
        /\ commitWatermark' = [commitWatermark EXCEPT ![r] = i]
        /\ acked' =
            acked \union
                {[range |-> r, lsn |-> j, epoch |-> log[n][r][j]]
                    : j \in 1..i}
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, up,
          log, accepted, forcedAudit>>

Crash(n) ==
    /\ up[n]
    /\ up' = [up EXCEPT ![n] = FALSE]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, log,
          commitWatermark, acked, accepted, forcedAudit>>

Restart(n) ==
    /\ ~up[n]
    /\ up' = [up EXCEPT ![n] = TRUE]
    /\ UNCHANGED
        <<catalogOwner, catalogEpoch, leaseOwner, leaseEpoch, log,
          commitWatermark, acked, accepted, forcedAudit>>

Next ==
    \/ \E r \in Ranges, n \in Nodes : NormalPromote(r, n)
    \/ \E r \in Ranges, n \in Nodes : CooperativeHandoff(r, n)
    \/ \E r \in Ranges, n \in Nodes : CrashFailover(r, n)
    \/ \E r \in Ranges, n \in Nodes : ForcedTransition(r, n)
    \/ \E r \in Ranges : ExpireLease(r)
    \/ \E r \in Ranges : RenewLease(r)
    \/ \E r \in Ranges, n \in Nodes : AcceptWrite(n, r)
    \/ \E r \in Ranges, src \in Nodes, dst \in Nodes : Replicate(src, dst, r)
    \/ \E r \in Ranges, n \in Nodes : RecoverToWatermark(n, r)
    \/ \E r \in Ranges, n \in Nodes : AdvanceCommit(n, r)
    \/ \E n \in Nodes : Crash(n)
    \/ \E n \in Nodes : Restart(n)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* THE PROPERTIES.                                                         *)
(***************************************************************************)

SingleOwnerPerEpoch ==
    \A a, b \in accepted :
        (a.range = b.range /\ a.epoch = b.epoch) => a.node = b.node

AckedPrefixUnique ==
    \A a, b \in acked :
        (a.range = b.range /\ a.lsn = b.lsn) => a.epoch = b.epoch

CurrentOwnerKeepsAckedWrites ==
    \A r \in Ranges :
        catalogOwner[r] # Nil =>
            \A e \in acked :
                (e.range = r /\ e.lsn <= commitWatermark[r]) =>
                    HoldsEntry(catalogOwner[r], e)

NoAcknowledgedWriteLoss ==
    /\ AckedPrefixUnique
    /\ CurrentOwnerKeepsAckedWrites

=============================================================================
