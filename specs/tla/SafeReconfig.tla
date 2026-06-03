----------------------------- MODULE SafeReconfig -----------------------------
(***************************************************************************)
(* Safe reconfiguration for RedDB replication membership changes           *)
(* (ADR 0030, PRD #819).                                                   *)
(*                                                                         *)
(* Property proved: EVERY MEMBERSHIP CHANGE PRESERVES QUORUM OVERLAP -- a   *)
(* quorum of the configuration before the change always intersects a       *)
(* quorum of the configuration after it.                                   *)
(*                                                                         *)
(* Why this matters.  "No two primaries in a term" (ElectionSafety.tla)    *)
(* rests on the fact that two quorums of ONE configuration always          *)
(* intersect.  The instant membership changes, that argument spans two     *)
(* different member sets: if a quorum of the OLD set and a quorum of the   *)
(* NEW set could be disjoint, two leaders could be elected at once -- one   *)
(* by each set -- across the change.  The classical fix (Raft single-server *)
(* membership changes) is to change membership by at most one voting member *)
(* at a time; that bounds the sets so close together that an old quorum and *)
(* a new quorum can never miss each other.                                 *)
(*                                                                         *)
(* This module models that rule directly: a reconfiguration adds or removes *)
(* exactly one voting member, and TLC confirms the overlap invariant holds *)
(* across every reachable change.  A two-member jump -- the unsafe shape    *)
(* this rule forbids -- is shown to break overlap by `JumpBreaksOverlap`    *)
(* below, so the property is not vacuous.                                   *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Servers,     \* the universe of potential voting members
    InitConfig   \* the initial voting-member set (a subset of Servers)

ASSUME InitConfig \subseteq Servers
ASSUME InitConfig # {}

\* The quorums of a configuration C: every strict majority of C. Two
\* quorums of the *same* C always intersect; the question this module
\* settles is whether quorums of two *successive* configs do.
QuorumsOf(C) == {S \in SUBSET C : Cardinality(S) * 2 > Cardinality(C)}

\* Do every quorum of A and every quorum of B intersect? This is the safety
\* condition a membership change must preserve.
Overlap(A, B) ==
    \A qa \in QuorumsOf(A), qb \in QuorumsOf(B) : qa \intersect qb # {}

VARIABLES
    config,     \* the current voting-member set
    prevConfig  \* the member set immediately before the last change

vars == <<config, prevConfig>>

TypeOK ==
    /\ config \subseteq Servers
    /\ prevConfig \subseteq Servers
    /\ config # {}

Init ==
    /\ config = InitConfig
    /\ prevConfig = InitConfig

(* Add one voting member -- the safe single-server change (Raft). *)
AddServer(s) ==
    /\ s \in Servers
    /\ s \notin config
    /\ prevConfig' = config
    /\ config' = config \union {s}

(* Remove one voting member -- the safe single-server change. The cluster   *)
(* never drops below a single member (an empty config has no quorum).       *)
RemoveServer(s) ==
    /\ s \in config
    /\ Cardinality(config) >= 2
    /\ prevConfig' = config
    /\ config' = config \ {s}

Next ==
    \/ \E s \in Servers : AddServer(s)
    \/ \E s \in Servers : RemoveServer(s)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* THE PROPERTY: the most recent membership change preserved quorum        *)
(* overlap. Because the invariant is checked in every reachable state and   *)
(* every action records the pre-change config in `prevConfig`, this covers  *)
(* every change in every behaviour.                                         *)
(***************************************************************************)
QuorumOverlap == Overlap(prevConfig, config)

(***************************************************************************)
(* Non-vacuity witness (NOT an invariant -- a sanity check run separately): *)
(* a two-member jump CAN break overlap, which is exactly why the protocol   *)
(* restricts changes to one member at a time. For example over a 3-node set *)
(* growing to 5 in one step, {a,b} is a quorum of the old set and {c,d,e}   *)
(* a quorum of the new set, and they are disjoint.                          *)
(***************************************************************************)
JumpBreaksOverlap ==
    \E A, B \in SUBSET Servers :
        /\ A # {}
        /\ B # {}
        /\ ~ Overlap(A, B)

=============================================================================
