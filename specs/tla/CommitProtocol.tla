---------------------------- MODULE CommitProtocol ----------------------------
(***************************************************************************)
(* RedDB optimistic transaction commit protocol (TM v2, issue #1646).      *)
(*                                                                         *)
(* This is the small bounded model for the live SQL commit path described  *)
(* by ADR 0065: SnapshotManager allocates xids and captures in-progress    *)
(* sets; the pure MVCC visibility predicate decides which row version a    *)
(* transaction sees; commit performs first-committer-wins (FCW) checks over *)
(* logical rows; savepoints use sub-xid frames that can be released or      *)
(* rolled back; the SSI extension tracks rw-antidependency edges and aborts *)
(* a commit that would admit a dangerous structure (two consecutive rw      *)
(* edges).                                                                 *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

Nil == "none"
Levels == {"SI", "SSI"}
Statuses == {"idle", "active", "committed", "aborted"}
AbortReasons == {"fcw", "ssi"}

CONSTANTS
    Txns,
    Rows,
    EnabledLevels

ASSUME
    /\ Cardinality(Rows) > 0
    /\ EnabledLevels \subseteq Levels
    /\ EnabledLevels # {}

VARIABLES
    nextXid,        \* next xid rank to allocate
    status,         \* transaction lifecycle
    isolation,      \* SI or SSI for active/finished transactions
    xidRank,        \* xid allocation order; 0 means not allocated
    activeAtBegin,  \* SnapshotManager in-progress set captured at BEGIN
    readSet,        \* logical rows read by each transaction
    readFrom,       \* row -> writer xid observed by the first read
    writeSet,       \* logical rows written by each transaction
    savepointOpen,  \* whether a sub-xid frame is open
    savepointWrites,\* writes stamped by the open sub-xid frame
    subXidAllocated,\* transactions that allocated at least one sub-xid
    rwEdges,        \* SSI rw-antidependency edges
    abortReason     \* commit-time abort classes reached by the model

vars ==
    <<nextXid, status, isolation, xidRank, activeAtBegin, readSet, readFrom,
      writeSet, savepointOpen, savepointWrites, subXidAllocated, rwEdges,
      abortReason>>

TypeOK ==
    /\ nextXid \in 1..(Cardinality(Txns) + 1)
    /\ status \in [Txns -> Statuses]
    /\ isolation \in [Txns -> Levels \union {Nil}]
    /\ xidRank \in [Txns -> 0..Cardinality(Txns)]
    /\ activeAtBegin \in [Txns -> SUBSET Txns]
    /\ readSet \in [Txns -> SUBSET Rows]
    /\ readFrom \in [Txns -> [Rows -> Txns \union {0}]]
    /\ writeSet \in [Txns -> SUBSET Rows]
    /\ savepointOpen \in [Txns -> BOOLEAN]
    /\ savepointWrites \in [Txns -> SUBSET Rows]
    /\ subXidAllocated \subseteq Txns
    /\ rwEdges \subseteq Txns \X Txns
    /\ abortReason \subseteq AbortReasons
    /\ \A t \in Txns : savepointWrites[t] \subseteq writeSet[t]
    /\ \A <<a, b>> \in rwEdges : a # b

Init ==
    /\ nextXid = 1
    /\ status = [t \in Txns |-> "idle"]
    /\ isolation = [t \in Txns |-> Nil]
    /\ xidRank = [t \in Txns |-> 0]
    /\ activeAtBegin = [t \in Txns |-> {}]
    /\ readSet = [t \in Txns |-> {}]
    /\ readFrom = [t \in Txns |-> [r \in Rows |-> 0]]
    /\ writeSet = [t \in Txns |-> {}]
    /\ savepointOpen = [t \in Txns |-> FALSE]
    /\ savepointWrites = [t \in Txns |-> {}]
    /\ subXidAllocated = {}
    /\ rwEdges = {}
    /\ abortReason = {}

CommittedWriters(row) ==
    {t \in Txns : status[t] = "committed" /\ row \in writeSet[t]}

VisibleCommittedWriters(reader, row) ==
    {t \in CommittedWriters(row) :
        /\ xidRank[t] <= xidRank[reader]
        /\ t \notin activeAtBegin[reader]}

VisibleWriter(reader, row) ==
    LET candidates == VisibleCommittedWriters(reader, row) IN
        IF candidates = {}
        THEN 0
        ELSE CHOOSE w \in candidates :
            \A other \in candidates : xidRank[other] <= xidRank[w]

NotVisibleTo(reader, writer) ==
    \/ status[writer] # "committed"
    \/ xidRank[writer] > xidRank[reader]
    \/ writer \in activeAtBegin[reader]

VisibleTo(reader, writer) ==
    /\ status[writer] = "committed"
    /\ xidRank[writer] <= xidRank[reader]
    /\ writer \notin activeAtBegin[reader]

ConcurrentWriters(t, row) ==
    {c \in CommittedWriters(row) :
        \/ xidRank[c] > xidRank[t]
        \/ c \in activeAtBegin[t]}

FCWConflictFree(t) ==
    \A row \in writeSet[t] : ConcurrentWriters(t, row) = {}

NewRwEdges(t) ==
    {edge \in Txns \X Txns :
        /\ edge[2] = t
        /\ edge[1] # t
        /\ status[edge[1]] = "committed"
        /\ \E row \in Rows :
            /\ row \in readSet[edge[1]]
            /\ row \in writeSet[t]
            /\ NotVisibleTo(edge[1], t)}
    \union
    {edge \in Txns \X Txns :
        /\ edge[1] = t
        /\ edge[2] # t
        /\ status[edge[2]] = "committed"
        /\ \E row \in Rows :
            /\ row \in readSet[t]
            /\ row \in writeSet[edge[2]]
            /\ NotVisibleTo(t, edge[2])}

Dangerous(edges) ==
    \E a, b, c \in Txns :
        /\ a # b
        /\ b # c
        /\ <<a, b>> \in edges
        /\ <<b, c>> \in edges

Begin(t, level) ==
    /\ nextXid <= Cardinality(Txns)
    /\ level \in EnabledLevels
    /\ status[t] = "idle"
    /\ status' = [status EXCEPT ![t] = "active"]
    /\ isolation' = [isolation EXCEPT ![t] = level]
    /\ xidRank' = [xidRank EXCEPT ![t] = nextXid]
    /\ activeAtBegin' =
        [activeAtBegin EXCEPT ![t] = {u \in Txns : status[u] = "active"}]
    /\ nextXid' = nextXid + 1
    /\ UNCHANGED
        <<readSet, readFrom, writeSet, savepointOpen, savepointWrites,
          subXidAllocated, rwEdges, abortReason>>

Read(t, row) ==
    /\ status[t] = "active"
    /\ row \in Rows
    /\ row \notin writeSet[t]
    /\ row \notin readSet[t]
    /\ readSet' = [readSet EXCEPT ![t] = @ \union {row}]
    /\ readFrom' = [readFrom EXCEPT ![t][row] = VisibleWriter(t, row)]
    /\ UNCHANGED
        <<nextXid, status, isolation, xidRank, activeAtBegin, writeSet,
          savepointOpen, savepointWrites, subXidAllocated, rwEdges,
          abortReason>>

Write(t, row) ==
    /\ status[t] = "active"
    /\ row \in Rows
    /\ writeSet[t] = {}
    /\ writeSet' = [writeSet EXCEPT ![t] = @ \union {row}]
    /\ savepointWrites' =
        [savepointWrites EXCEPT ![t] =
            IF savepointOpen[t] THEN @ \union {row} ELSE @]
    /\ UNCHANGED
        <<nextXid, status, isolation, xidRank, activeAtBegin, readSet,
          readFrom, savepointOpen, subXidAllocated, rwEdges, abortReason>>

Savepoint(t) ==
    /\ status[t] = "active"
    /\ ~savepointOpen[t]
    /\ subXidAllocated = {}
    /\ savepointOpen' = [savepointOpen EXCEPT ![t] = TRUE]
    /\ savepointWrites' = [savepointWrites EXCEPT ![t] = {}]
    /\ subXidAllocated' = subXidAllocated \union {t}
    /\ UNCHANGED
        <<nextXid, status, isolation, xidRank, activeAtBegin, readSet,
          readFrom, writeSet, rwEdges, abortReason>>

ReleaseSavepoint(t) ==
    /\ status[t] = "active"
    /\ savepointOpen[t]
    /\ savepointOpen' = [savepointOpen EXCEPT ![t] = FALSE]
    /\ savepointWrites' = [savepointWrites EXCEPT ![t] = {}]
    /\ UNCHANGED
        <<nextXid, status, isolation, xidRank, activeAtBegin, readSet,
          readFrom, writeSet, subXidAllocated, rwEdges, abortReason>>

RollbackToSavepoint(t) ==
    /\ status[t] = "active"
    /\ savepointOpen[t]
    /\ writeSet' = [writeSet EXCEPT ![t] = @ \ savepointWrites[t]]
    /\ savepointWrites' = [savepointWrites EXCEPT ![t] = {}]
    /\ UNCHANGED
        <<nextXid, status, isolation, xidRank, activeAtBegin, readSet,
          readFrom, savepointOpen, subXidAllocated, rwEdges, abortReason>>

Commit(t) ==
    /\ status[t] = "active"
    /\ FCWConflictFree(t)
    /\ LET newEdges == NewRwEdges(t) IN
        /\ isolation[t] = "SI" \/ ~Dangerous(rwEdges \union newEdges)
        /\ status' = [status EXCEPT ![t] = "committed"]
        /\ rwEdges' = rwEdges \union newEdges
    /\ UNCHANGED
        <<nextXid, isolation, xidRank, activeAtBegin, readSet, readFrom,
          writeSet, savepointOpen, savepointWrites, subXidAllocated,
          abortReason>>

AbortFCW(t) ==
    /\ status[t] = "active"
    /\ ~FCWConflictFree(t)
    /\ status' = [status EXCEPT ![t] = "aborted"]
    /\ abortReason' = abortReason \union {"fcw"}
    /\ UNCHANGED
        <<nextXid, isolation, xidRank, activeAtBegin, readSet, readFrom,
          writeSet, savepointOpen, savepointWrites, subXidAllocated, rwEdges>>

AbortSSI(t) ==
    /\ status[t] = "active"
    /\ isolation[t] = "SSI"
    /\ FCWConflictFree(t)
    /\ Dangerous(rwEdges \union NewRwEdges(t))
    /\ status' = [status EXCEPT ![t] = "aborted"]
    /\ abortReason' = abortReason \union {"ssi"}
    /\ UNCHANGED
        <<nextXid, isolation, xidRank, activeAtBegin, readSet, readFrom,
          writeSet, savepointOpen, savepointWrites, subXidAllocated, rwEdges>>

Next ==
    \/ \E t \in Txns, level \in EnabledLevels : Begin(t, level)
    \/ \E t \in Txns, row \in Rows : Read(t, row)
    \/ \E t \in Txns, row \in Rows : Write(t, row)
    \/ \E t \in Txns : Savepoint(t)
    \/ \E t \in Txns : ReleaseSavepoint(t)
    \/ \E t \in Txns : RollbackToSavepoint(t)
    \/ \E t \in Txns : Commit(t)
    \/ \E t \in Txns : AbortFCW(t)
    \/ \E t \in Txns : AbortSSI(t)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Serial oracle. A committed history is serializable when some serial      *)
(* ordering of the admitted transactions explains every read: each read saw *)
(* the last prior transaction in that order that wrote the row, or 0 for    *)
(* the initial row value.                                                   *)
(***************************************************************************)
Orders(S) ==
    {order \in [1..Cardinality(S) -> S] :
        \A a, b \in 1..Cardinality(S) : order[a] = order[b] => a = b}

Pos(order, t) == CHOOSE i \in DOMAIN order : order[i] = t

WritersBefore(order, t, row) ==
    {w \in DOMAIN order :
        /\ Pos(order, order[w]) < Pos(order, t)
        /\ row \in writeSet[order[w]]}

LastWriterBefore(order, t, row) ==
    LET writers == WritersBefore(order, t, row) IN
        IF writers = {}
        THEN 0
        ELSE order[CHOOSE i \in writers : \A j \in writers : j <= i]

SerializableSet(S) ==
    \E order \in Orders(S) :
        \A t \in S :
            \A row \in readSet[t] :
                readFrom[t][row] = LastWriterBefore(order, t, row)

(***************************************************************************)
(* Safety properties checked by TLC.                                        *)
(***************************************************************************)

SI_NoLostUpdate ==
    \A a, b \in Txns :
        /\ status[a] = "committed"
        /\ status[b] = "committed"
        /\ a # b
        /\ writeSet[a] \cap writeSet[b] # {}
        /\ ~VisibleTo(a, b)
        /\ ~VisibleTo(b, a)
        => FALSE

SI_SnapshotStability ==
    \A t \in Txns :
        \A row \in readSet[t] :
            row \notin writeSet[t] =>
            readFrom[t][row] = VisibleWriter(t, row)

SSI_Serializable ==
    LET committed == {t \in Txns : status[t] = "committed"} IN
        (\A t \in committed : isolation[t] = "SSI") => SerializableSet(committed)

(***************************************************************************)
(* Non-vacuity witnesses. They are included as named model predicates so    *)
(* TLC state dumps can be searched and future dedicated witness configs can *)
(* target them directly.                                                    *)
(***************************************************************************)

FCWConflictReached == "fcw" \in abortReason

DangerousStructureReached == "ssi" \in abortReason

OptimisticAdmitsMoreThan2PL ==
    \E a, b \in Txns :
        /\ a # b
        /\ status[a] = "committed"
        /\ status[b] = "committed"
        /\ isolation[a] = "SSI"
        /\ isolation[b] = "SSI"
        /\ writeSet[a] \cap readSet[b] # {}
        /\ writeSet[b] \cap readSet[a] = {}

\* Negated witness predicates are useful for local reachability checks:
\* adding any one of these as a temporary TLC invariant must fail.
NoFCWConflictReached == ~FCWConflictReached
NoDangerousStructureReached == ~DangerousStructureReached
NoOptimisticAdmitsMoreThan2PL == ~OptimisticAdmitsMoreThan2PL

=============================================================================
