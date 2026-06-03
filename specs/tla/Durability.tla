------------------------------ MODULE Durability ------------------------------
(***************************************************************************)
(* Durability across failover for RedDB replication (ADR 0030, PRD #819).  *)
(*                                                                         *)
(* Property proved: A WRITE AT OR BELOW THE COMMIT WATERMARK IS NEVER       *)
(* ROLLED BACK -- not by a failover, not by a divergent-tail truncation.   *)
(* This is Mongo's `NeverRollbackCommitted`, the guarantee ADR 0030 makes  *)
(* for synchronous writes.                                                 *)
(*                                                                         *)
(* Model.  Each node keeps a log of entries; an entry is identified by its *)
(* (index, term) the way RedDB stamps a term/epoch onto every WAL record   *)
(* (ADR 0032).  Because elections are safe (a single leader per term -- see *)
(* ElectionSafety.tla) the pair (index, term) names a unique write.        *)
(*                                                                         *)
(* The commit watermark is the highest index durably replicated to a       *)
(* quorum under the leader's current term -- exactly                       *)
(* `QuorumCoordinator`/`CommitWaiter` advancing the watermark once a quorum *)
(* has acked.  `committed` records every (index, term) at or below it.      *)
(*                                                                         *)
(* The election vote rule is modelled by the standard up-to-date check: a  *)
(* voter only grants a candidate whose log is at least as up-to-date as its *)
(* own.  This is the abstraction of `Voter::consider`'s watermark clause    *)
(* (`RefusalReason::WatermarkNotCovered`): a quorum that holds a committed  *)
(* write will refuse any candidate that does not carry it, so no leader     *)
(* lacking a committed write can ever be elected -- and a divergent tail a  *)
(* deposed primary later truncates is, by construction, never a committed   *)
(* one (ADR 0030 auto-rollback with tail preservation).                    *)
(*                                                                         *)
(* TLC exhausts the bounded space and confirms the two safety invariants   *)
(* below hold in every reachable state.                                    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Nodes,       \* the data members (each holds a log and may be primary)
    MaxTerm,     \* bound on election terms
    MaxLogLen    \* bound on log length

ASSUME MaxTerm \in Nat /\ MaxLogLen \in Nat

Quorum == {S \in SUBSET Nodes : Cardinality(S) * 2 > Cardinality(Nodes)}

Nil == "none"

VARIABLES
    term,       \* term[n]     : node n's current term
    votedFor,   \* votedFor[n] : durable last-vote target this term
    state,      \* state[n]    \in {"follower","candidate","primary"}
    log,        \* log[n]      : Seq of terms; entry i was created in log[n][i]
    votes,      \* votes[n]    : voters that granted n at term[n]
    committed   \* set of [idx,tm,cterm] entries at/below the watermark

vars == <<term, votedFor, state, log, votes, committed>>

\* Term of the last log entry (0 for an empty log) -- the up-to-date key.
LastTerm(l) == IF Len(l) = 0 THEN 0 ELSE l[Len(l)]

\* Raft's "at least as up-to-date" relation; the abstraction of the
\* watermark vote rule. A voter grants only if the candidate is >= itself.
UpToDate(cl, vl) ==
    \/ LastTerm(cl) > LastTerm(vl)
    \/ (LastTerm(cl) = LastTerm(vl) /\ Len(cl) >= Len(vl))

\* A committed entry records its index, the term that created it, AND the
\* term it was committed under (cterm). Leader Completeness is keyed on the
\* commit term -- an entry committed in term T is held by every leader of a
\* term >= T, NOT necessarily by a leader that was already deposed before T.
Entries == [idx : 1..MaxLogLen, tm : 1..MaxTerm, cterm : 1..MaxTerm]

TypeOK ==
    /\ term \in [Nodes -> 0..MaxTerm]
    /\ votedFor \in [Nodes -> Nodes \union {Nil}]
    /\ state \in [Nodes -> {"follower","candidate","primary"}]
    /\ log \in [Nodes -> Seq(1..MaxTerm)]
    /\ votes \in [Nodes -> SUBSET Nodes]
    /\ committed \subseteq Entries

Init ==
    /\ term = [n \in Nodes |-> 0]
    /\ votedFor = [n \in Nodes |-> Nil]
    /\ state = [n \in Nodes |-> "follower"]
    /\ log = [n \in Nodes |-> << >>]
    /\ votes = [n \in Nodes |-> {}]
    /\ committed = {}

(* Stand for election: bump term, self-vote. *)
Timeout(c) ==
    /\ term[c] < MaxTerm
    /\ LET T == term[c] + 1 IN
        /\ term' = [term EXCEPT ![c] = T]
        /\ votedFor' = [votedFor EXCEPT ![c] = c]
        /\ state' = [state EXCEPT ![c] = "candidate"]
        /\ votes' = [votes EXCEPT ![c] = {c}]
        /\ UNCHANGED <<log, committed>>

(* Voter v grants candidate c: vote-once per term AND the up-to-date rule, *)
(* which is what forbids electing a leader that lacks a committed write.    *)
GrantVote(v, c) ==
    /\ state[c] = "candidate"
    /\ v # c
    /\ v \notin votes[c]
    /\ LET T == term[c] IN
        /\ T >= term[v]
        /\ (T = term[v] => votedFor[v] \in {Nil, c})
        /\ UpToDate(log[c], log[v])
        /\ term' = [term EXCEPT ![v] = T]
        /\ votedFor' = [votedFor EXCEPT ![v] = c]
        /\ state' = [state EXCEPT ![v] = "follower"]
        /\ votes' = [votes EXCEPT ![c] = votes[c] \union {v}]
        /\ UNCHANGED <<log, committed>>

(* A candidate with a quorum of grants becomes primary. *)
BecomeLeader(c) ==
    /\ state[c] = "candidate"
    /\ votes[c] \in Quorum
    /\ state' = [state EXCEPT ![c] = "primary"]
    /\ UNCHANGED <<term, votedFor, log, votes, committed>>

(* The primary appends a client write in its own term. *)
ClientWrite(c) ==
    /\ state[c] = "primary"
    /\ Len(log[c]) < MaxLogLen
    /\ log' = [log EXCEPT ![c] = Append(log[c], term[c])]
    /\ UNCHANGED <<term, votedFor, state, votes, committed>>

(* A faithful AppendEntries step. A follower copies entry i from the        *)
(* primary, adopting the primary's prefix up to i (which both appends entry *)
(* i and truncates any divergent follower suffix beyond it). Two guards make *)
(* this match real Raft -- and they are exactly what stops a deposed         *)
(* primary from corrupting the timeline:                                     *)
(*                                                                          *)
(*   * TERM GUARD: a follower rejects a leader whose term is older than its  *)
(*     own (`term[leader] >= term[f]`). A stale primary therefore cannot     *)
(*     push entries onto a node that has already moved to a newer term, so   *)
(*     it can never assemble a quorum to commit -- the model's stand-in for  *)
(*     the apply-boundary / stale-term fence (#835).                         *)
(*   * LOG-MATCHING: entry i is accepted only where the preceding entry      *)
(*     already agrees (`log[f][i-1] = log[leader][i-1]`), Raft's Log         *)
(*     Matching precondition.                                                *)
(*                                                                          *)
(* Accepting AppendEntries also advances the follower's term to the         *)
(* leader's and leaves it a follower (a primary that hears a newer-or-equal  *)
(* leader steps down).                                                       *)
Replicate(leader, f) ==
    /\ state[leader] = "primary"
    /\ f # leader
    /\ term[leader] >= term[f]
    /\ \E i \in 1..Len(log[leader]) :
        /\ i <= Len(log[f]) + 1
        /\ (i > 1 => (Len(log[f]) >= i - 1 /\ log[f][i-1] = log[leader][i-1]))
        \* Raft Sec 5.3: delete-and-truncate ONLY on a real conflict at i.
        \* A matching entry (and any matching suffix) is left intact, so a
        \* committed entry beyond i is never dropped by a redundant append.
        /\ log' = [log EXCEPT ![f] =
              IF Len(log[f]) >= i /\ log[f][i] = log[leader][i]
                THEN log[f]
                ELSE SubSeq(log[f], 1, i - 1) \o << log[leader][i] >> ]
    /\ term' = [term EXCEPT ![f] = term[leader]]
    /\ state' = [state EXCEPT ![f] = "follower"]
    /\ UNCHANGED <<votedFor, votes, committed>>

(* Advance the commit watermark: the primary marks a prefix committed once  *)
(* a quorum holds that exact prefix and the boundary entry is from the      *)
(* primary's current term (Raft's current-term commit rule). Every          *)
(* (index, term) at/below the new watermark is recorded committed.          *)
AdvanceCommit(c) ==
    /\ state[c] = "primary"
    /\ \E i \in 1..Len(log[c]) :
        /\ log[c][i] = term[c]
        /\ \E Q \in Quorum :
            \A f \in Q : /\ Len(log[f]) >= i
                         /\ SubSeq(log[f], 1, i) = SubSeq(log[c], 1, i)
        /\ committed' = committed \union
               { [idx |-> j, tm |-> log[c][j], cterm |-> term[c]] : j \in 1..i }
    /\ UNCHANGED <<term, votedFor, state, log, votes>>

Next ==
    \/ \E c \in Nodes : Timeout(c)
    \/ \E c \in Nodes, v \in Nodes : GrantVote(v, c)
    \/ \E c \in Nodes : BecomeLeader(c)
    \/ \E c \in Nodes : ClientWrite(c)
    \/ \E l \in Nodes, f \in Nodes : Replicate(l, f)
    \/ \E c \in Nodes : AdvanceCommit(c)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* THE PROPERTIES.                                                         *)
(***************************************************************************)

\* A committed write is never overwritten by a different write at the same
\* position: the (index -> term) mapping of committed entries is a function.
\* This is the direct never-rollback statement -- a committed LSN keeps its
\* write forever.
CommittedNeverRolledBack ==
    \A e1, e2 \in committed : e1.idx = e2.idx => e1.tm = e2.tm

\* The mechanism (Raft Leader Completeness): any leader whose term is at or
\* above a committed write's term carries that write, so a failover to a new
\* term can never strand it (ADR 0030 "nothing at or below the watermark is
\* rolled back"). The term guard is essential and faithful: a *deposed*
\* primary still flagged `primary` at an OLD term is deliberately excused --
\* it has been superseded, cannot commit, and is precisely the divergent-tail
\* case ADR 0030 heals by auto-rollback when it rejoins. The real leader of
\* the current (higher) term provably holds the committed write.
LeaderCompleteness ==
    \A n \in Nodes :
        state[n] = "primary" =>
            \A e \in committed :
                term[n] >= e.cterm =>
                    (Len(log[n]) >= e.idx /\ log[n][e.idx] = e.tm)

=============================================================================
