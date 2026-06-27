------------------------ MODULE ReplicationSafetyEnvelope ------------------------
(***************************************************************************)
(* RedDB replication safety envelope (issue #1490, ADR 0030 / ADR 0032).  *)
(*                                                                         *)
(* This model intentionally sits above implementation details and below a   *)
(* full Raft spec. It combines the safety properties later Maelstrom and    *)
(* Jepsen-style harnesses must preserve:                                    *)
(*                                                                         *)
(*   * one writer/leader per term;                                          *)
(*   * a stale leader cannot accept new writes after a higher-term writer   *)
(*     fence has been installed;                                            *)
(*   * committed writes survive crash/restart/recovery and failover.        *)
(*                                                                         *)
(* Modelled mechanisms.                                                     *)
(*                                                                         *)
(*   * Nodes vote in term-based elections. A durable last-vote pair         *)
(*     (term, votedFor) prevents double voting across crashes.              *)
(*   * The elected writer owns the current writer fence generation          *)
(*     (writerTerm). Leaders from older terms may still exist behind a      *)
(*     partition, but AcceptWrite requires the current writer fence.         *)
(*   * Logs contain term-stamped writes. The commit watermark is abstracted *)
(*     as the set `committed`, populated only when a quorum holds the exact *)
(*     prefix under the current writer.                                     *)
(*   * Directed links model partitioned communication. Elections and        *)
(*     replication require candidate/leader reachability to the receiver.   *)
(*   * Crash/restart preserves durable term, last vote, log, and committed  *)
(*     state. Recovery may truncate a divergent suffix but cannot drop a    *)
(*     committed entry already present on that node.                        *)
(*                                                                         *)
(* Intentional simplifications for follow-up implementation harnesses.      *)
(*                                                                         *)
(*   * A log entry is represented only by its creation term, not by payload *)
(*     bytes, collection id, LSN encoding, or physical WAL page layout.     *)
(*   * The writer fence is a single global generation; the model does not   *)
(*     cover lease-clock expiry, CAS storage failures, or client retries.   *)
(*   * Partitions are directed reachability bits, not latency, loss,        *)
(*     duplication, or message reordering.                                  *)
(*   * Recovery truncates only suffixes. Rollback-file persistence and      *)
(*     operator events from ADR 0030 are out of scope.                      *)
(*   * Local/async writes are represented as uncommitted suffix entries;     *)
(*     only quorum-committed writes are covered by the durability invariant. *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Nodes,
    MaxTerm,
    MaxLogLen

ASSUME MaxTerm \in Nat /\ MaxLogLen \in Nat

Quorum == {S \in SUBSET Nodes : Cardinality(S) * 2 > Cardinality(Nodes)}

Nil == "none"

VARIABLES
    term,       \* durable highest term observed by each node
    votedFor,   \* durable vote target for term[n]
    state,      \* process role: follower, candidate, or leader
    log,        \* term-stamped writes held by each node
    votes,      \* volatile vote responses collected by a candidate
    committed,  \* quorum-committed entries
    up,         \* process availability
    link,       \* directed communication reachability
    writer,     \* node holding the current writer fence
    writerTerm, \* current writer fence generation
    fencedLen   \* log length captured when an older leader is fenced

vars ==
    <<term, votedFor, state, log, votes, committed, up, link,
      writer, writerTerm, fencedLen>>

Entries == [idx : 1..MaxLogLen, tm : 1..MaxTerm, cterm : 1..MaxTerm]

BoundedLogs == UNION {[1..i -> 1..MaxTerm] : i \in 0..MaxLogLen}

LastTerm(l) == IF Len(l) = 0 THEN 0 ELSE l[Len(l)]

UpToDate(candidateLog, voterLog) ==
    \/ LastTerm(candidateLog) > LastTerm(voterLog)
    \/ (LastTerm(candidateLog) = LastTerm(voterLog)
        /\ Len(candidateLog) >= Len(voterLog))

Prefix(logSeq, k) == SubSeq(logSeq, 1, k)

BrokenLinks ==
    Cardinality({<<s, r>> \in Nodes \X Nodes : s # r /\ r \notin link[s]})

OneSidedPartitionBound == BrokenLinks <= 1

TypeOK ==
    /\ term \in [Nodes -> 0..MaxTerm]
    /\ votedFor \in [Nodes -> Nodes \union {Nil}]
    /\ state \in [Nodes -> {"follower", "candidate", "leader"}]
    /\ log \in [Nodes -> BoundedLogs]
    /\ votes \in [Nodes -> SUBSET Nodes]
    /\ committed \subseteq Entries
    /\ up \in [Nodes -> BOOLEAN]
    /\ link \in [Nodes -> SUBSET Nodes]
    /\ \A n \in Nodes : n \in link[n]
    /\ writer \in Nodes \union {Nil}
    /\ writerTerm \in 0..MaxTerm
    /\ fencedLen \in [Nodes -> 0..MaxLogLen]

Init ==
    /\ term = [n \in Nodes |-> 0]
    /\ votedFor = [n \in Nodes |-> Nil]
    /\ state = [n \in Nodes |-> "follower"]
    /\ log = [n \in Nodes |-> << >>]
    /\ votes = [n \in Nodes |-> {}]
    /\ committed = {}
    /\ up = [n \in Nodes |-> TRUE]
    /\ link = [n \in Nodes |-> Nodes]
    /\ writer = Nil
    /\ writerTerm = 0
    /\ fencedLen = [n \in Nodes |-> 0]

Timeout(c) ==
    /\ up[c]
    /\ state[c] # "leader"
    /\ term[c] < MaxTerm
    /\ LET T == term[c] + 1 IN
        /\ term' = [term EXCEPT ![c] = T]
        /\ votedFor' = [votedFor EXCEPT ![c] = c]
        /\ state' = [state EXCEPT ![c] = "candidate"]
        /\ votes' = [votes EXCEPT ![c] = {c}]
    /\ UNCHANGED <<log, committed, up, link, writer, writerTerm, fencedLen>>

GrantVote(v, c) ==
    /\ up[c]
    /\ up[v]
    /\ c # v
    /\ v \in link[c]
    /\ state[c] = "candidate"
    /\ v \notin votes[c]
    /\ LET T == term[c] IN
        /\ T >= term[v]
        /\ (T = term[v] => votedFor[v] \in {Nil, c})
        /\ UpToDate(log[c], log[v])
        /\ term' = [term EXCEPT ![v] = T]
        /\ votedFor' = [votedFor EXCEPT ![v] = c]
        /\ state' = [state EXCEPT ![v] = "follower"]
        /\ votes' = [votes EXCEPT ![c] = votes[c] \union {v}]
    /\ UNCHANGED <<log, committed, up, link, writer, writerTerm, fencedLen>>

BecomeLeader(c) ==
    /\ up[c]
    /\ state[c] = "candidate"
    /\ votes[c] \in Quorum
    /\ state' = [state EXCEPT ![c] = "leader"]
    /\ writer' = IF term[c] >= writerTerm THEN c ELSE writer
    /\ writerTerm' = IF term[c] >= writerTerm THEN term[c] ELSE writerTerm
    /\ fencedLen' =
        [n \in Nodes |->
            IF n = c /\ term[c] < writerTerm
            THEN Len(log[c])
            ELSE IF state[n] = "leader" /\ term[n] < term[c]
            THEN Len(log[n])
            ELSE fencedLen[n]]
    /\ UNCHANGED <<term, votedFor, log, votes, committed, up, link>>

AcceptWrite(c) ==
    /\ up[c]
    /\ state[c] = "leader"
    /\ writer = c
    /\ writerTerm = term[c]
    /\ Len(log[c]) < MaxLogLen
    /\ log' = [log EXCEPT ![c] = Append(log[c], term[c])]
    /\ UNCHANGED
        <<term, votedFor, state, votes, committed, up, link,
          writer, writerTerm, fencedLen>>

Replicate(leader, follower) ==
    /\ up[leader]
    /\ up[follower]
    /\ leader # follower
    /\ follower \in link[leader]
    /\ state[leader] = "leader"
    /\ term[leader] >= term[follower]
    /\ \E i \in 1..Len(log[leader]) :
        /\ i <= Len(log[follower]) + 1
        /\ (i > 1 =>
            (Len(log[follower]) >= i - 1
             /\ log[follower][i - 1] = log[leader][i - 1]))
        /\ log' =
            [log EXCEPT ![follower] =
                IF Len(log[follower]) >= i
                   /\ log[follower][i] = log[leader][i]
                THEN log[follower]
                ELSE Prefix(log[follower], i - 1) \o <<log[leader][i]>>]
    /\ term' = [term EXCEPT ![follower] = term[leader]]
    /\ state' = [state EXCEPT ![follower] = "follower"]
    /\ UNCHANGED
        <<votedFor, votes, committed, up, link, writer, writerTerm, fencedLen>>

AdvanceCommit(c) ==
    /\ up[c]
    /\ state[c] = "leader"
    /\ writer = c
    /\ writerTerm = term[c]
    /\ \E i \in 1..Len(log[c]) :
        /\ log[c][i] = term[c]
        /\ \E Q \in Quorum :
            \A n \in Q :
                /\ Len(log[n]) >= i
                /\ Prefix(log[n], i) = Prefix(log[c], i)
        /\ committed' =
            committed \union
                {[idx |-> j, tm |-> log[c][j], cterm |-> term[c]] : j \in 1..i}
    /\ UNCHANGED
        <<term, votedFor, state, log, votes, up, link,
          writer, writerTerm, fencedLen>>

Crash(n) ==
    /\ up[n]
    /\ up' = [up EXCEPT ![n] = FALSE]
    /\ state' = [state EXCEPT ![n] = "follower"]
    /\ votes' = [votes EXCEPT ![n] = {}]
    /\ UNCHANGED
        <<term, votedFor, log, committed, link, writer, writerTerm, fencedLen>>

Restart(n) ==
    /\ ~up[n]
    /\ up' = [up EXCEPT ![n] = TRUE]
    /\ state' = [state EXCEPT ![n] = "follower"]
    /\ votes' = [votes EXCEPT ![n] = {}]
    /\ UNCHANGED
        <<term, votedFor, log, committed, link, writer, writerTerm, fencedLen>>

Recover(n) ==
    /\ up[n]
    /\ state[n] = "follower"
    /\ \E k \in 0..Len(log[n]) :
        /\ \A e \in committed :
            (Len(log[n]) >= e.idx /\ log[n][e.idx] = e.tm) => k >= e.idx
        /\ log' = [log EXCEPT ![n] = Prefix(log[n], k)]
    /\ UNCHANGED
        <<term, votedFor, state, votes, committed, up, link,
          writer, writerTerm, fencedLen>>

Partition(sender, receiver) ==
    /\ sender # receiver
    /\ receiver \in link[sender]
    /\ link' = [link EXCEPT ![sender] = link[sender] \ {receiver}]
    /\ UNCHANGED
        <<term, votedFor, state, log, votes, committed, up,
          writer, writerTerm, fencedLen>>

Heal(sender, receiver) ==
    /\ sender # receiver
    /\ receiver \notin link[sender]
    /\ link' = [link EXCEPT ![sender] = link[sender] \union {receiver}]
    /\ UNCHANGED
        <<term, votedFor, state, log, votes, committed, up,
          writer, writerTerm, fencedLen>>

Next ==
    \/ \E c \in Nodes : Timeout(c)
    \/ \E c, v \in Nodes : GrantVote(v, c)
    \/ \E c \in Nodes : BecomeLeader(c)
    \/ \E c \in Nodes : AcceptWrite(c)
    \/ \E l, f \in Nodes : Replicate(l, f)
    \/ \E c \in Nodes : AdvanceCommit(c)
    \/ \E n \in Nodes : Crash(n)
    \/ \E n \in Nodes : Restart(n)
    \/ \E n \in Nodes : Recover(n)
    \/ \E s, r \in Nodes : Partition(s, r)
    \/ \E s, r \in Nodes : Heal(s, r)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* THE PROPERTIES.                                                         *)
(***************************************************************************)

SingleWriterPerTerm ==
    \A i, j \in Nodes :
        (state[i] = "leader" /\ state[j] = "leader" /\ term[i] = term[j])
            => i = j

FencedLeaderCannotAcceptWrites ==
    \A n \in Nodes :
        (state[n] = "leader" /\ term[n] < writerTerm)
            => Len(log[n]) = fencedLen[n]

CommittedWritesSurviveRecovery ==
    /\ \A e1, e2 \in committed : e1.idx = e2.idx => e1.tm = e2.tm
    /\ \A n \in Nodes :
        state[n] = "leader" =>
            \A e \in committed :
                term[n] >= e.cterm =>
                    (Len(log[n]) >= e.idx /\ log[n][e.idx] = e.tm)

=============================================================================
