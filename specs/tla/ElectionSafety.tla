---------------------------- MODULE ElectionSafety ----------------------------
(***************************************************************************)
(* Election safety for RedDB's term-based, quorum-gated automatic          *)
(* election (issue #834, PRD #819, ADR 0030).                              *)
(*                                                                         *)
(* Property proved: NO TWO NODES HOLD PRIMARY IN THE SAME TERM.            *)
(*                                                                         *)
(* The model mirrors the real voter-side vote rule in                      *)
(* `crates/reddb-server/src/replication/election.rs`:                      *)
(*                                                                         *)
(*   * a member's durable last-vote is the pair (term, voted_for)          *)
(*     (the `LastVote` struct); a voter's "current term" IS its last-vote  *)
(*     term (`Voter::current_term` reads it from the durable store);       *)
(*   * within a term a voter grants exactly one candidate (the durable     *)
(*     double-vote guard, `RefusalReason::AlreadyVoted`), and refuses a    *)
(*     candidate from a superseded term (`RefusalReason::StaleTerm`);      *)
(*   * a win requires a strict majority of the *voting* members            *)
(*     (`quorum_threshold` = floor(n/2)+1); witnesses vote but never       *)
(*     stand, exactly as `Member::is_electable` encodes.                   *)
(*                                                                         *)
(* The safety argument is structural: two strict majorities of the same    *)
(* voter set always intersect, and the shared voter casts at most one vote *)
(* per term, so at most one candidate can collect a majority in any term.  *)
(* TLC exhausts the bounded state space to confirm no reachable state has  *)
(* two primaries sharing a term.                                           *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    DataNodes,   \* members that hold data and may stand for election
    Witnesses,   \* vote-only members: count toward quorum, never primary
    MaxTerm      \* bound on the terms TLC explores

Nodes  == DataNodes \union Witnesses
Voters == Nodes              \* every member modelled here is a healthy voter

ASSUME MaxTerm \in Nat
ASSUME DataNodes \intersect Witnesses = {}

\* A quorum is any strict majority of the voting members -- the smallest
\* count such that two quorums always intersect (election.rs quorum_threshold).
Quorum == {S \in SUBSET Voters : Cardinality(S) * 2 > Cardinality(Voters)}

Nil == "none"

VARIABLES
    term,      \* term[n]     : durable highest term  (LastVote.term)
    votedFor,  \* votedFor[n] : durable grant target  (LastVote.voted_for)
    state,     \* state[n]    \in {"follower","candidate","primary"}
    votes      \* votes[n]    : voters that granted n at its current term[n]

vars == <<term, votedFor, state, votes>>

TypeOK ==
    /\ term \in [Nodes -> 0..MaxTerm]
    /\ votedFor \in [Nodes -> Nodes \union {Nil}]
    /\ state \in [Nodes -> {"follower","candidate","primary"}]
    /\ votes \in [Nodes -> SUBSET Voters]

Init ==
    /\ term = [n \in Nodes |-> 0]
    /\ votedFor = [n \in Nodes |-> Nil]
    /\ state = [n \in Nodes |-> "follower"]
    /\ votes = [n \in Nodes |-> {}]

(* A data member stands for election: it bumps to the next term and casts  *)
(* its self-vote, persisting (T, self) as its own durable last-vote -- the  *)
(* candidate's term bump in ElectionCoordinator::run.                       *)
Timeout(c) ==
    /\ c \in DataNodes
    /\ term[c] < MaxTerm
    /\ LET T == term[c] + 1 IN
        /\ term' = [term EXCEPT ![c] = T]
        /\ votedFor' = [votedFor EXCEPT ![c] = c]
        /\ state' = [state EXCEPT ![c] = "candidate"]
        /\ votes' = [votes EXCEPT ![c] = {c}]

(* A voter v considers candidate c's ballot for term T == term[c].         *)
(* This is exactly Voter::consider minus the watermark clause (the         *)
(* watermark/durability property is proved in Durability.tla): refuse a    *)
(* stale term, otherwise grant at most one candidate per term, persisting  *)
(* the grant before it counts.                                             *)
GrantVote(v, c) ==
    /\ c \in DataNodes
    /\ state[c] = "candidate"
    /\ v \in Voters
    /\ v # c
    /\ v \notin votes[c]
    /\ LET T == term[c] IN
        /\ T >= term[v]                                  \* not a StaleTerm
        /\ (T = term[v] => votedFor[v] \in {Nil, c})     \* double-vote guard
        /\ term' = [term EXCEPT ![v] = T]                \* persist (T, c)
        /\ votedFor' = [votedFor EXCEPT ![v] = c]
        /\ state' = [state EXCEPT ![v] = "follower"]     \* a granter is a follower
        /\ votes' = [votes EXCEPT ![c] = votes[c] \union {v}]

(* A candidate that has collected a quorum of grants at its current term    *)
(* is promoted to primary -- ElectionOutcome::Elected.                      *)
BecomeLeader(c) ==
    /\ c \in DataNodes
    /\ state[c] = "candidate"
    /\ votes[c] \in Quorum
    /\ state' = [state EXCEPT ![c] = "primary"]
    /\ UNCHANGED <<term, votedFor, votes>>

Next ==
    \/ \E c \in DataNodes : Timeout(c)
    \/ \E c \in DataNodes, v \in Voters : GrantVote(v, c)
    \/ \E c \in DataNodes : BecomeLeader(c)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* THE PROPERTY: at most one primary per term.                             *)
(***************************************************************************)
ElectionSafety ==
    \A i, j \in DataNodes :
        (state[i] = "primary" /\ state[j] = "primary" /\ term[i] = term[j])
            => i = j

=============================================================================
