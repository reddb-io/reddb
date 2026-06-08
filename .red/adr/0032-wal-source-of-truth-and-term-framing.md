# WAL as Source of Truth, Logical Spool Derived, and Term/Epoch Framing

Status: proposed

RedDB keeps two logs: the storage-engine physical WAL (`storage/wal/`) for crash
recovery, and a separate logical replication spool (`LogicalWalSpool`, v2 framing)
that drives replication and PITR. Today they are written in parallel via a CDC hook
— two sources of truth for "what happened" that can drift apart. ADR 0030
(consistency & failover) requires a **term/epoch** to be stamped somewhere in the
log so divergence detection and the election vote rule are unambiguous. This ADR
fixes where the term lives and the relationship between the two logs.

## Decisions

**Two logs are retained, but the logical spool becomes a strict derived projection
of the physical WAL — not a parallel dual-write.** The physical WAL is the single
source of truth. The logical spool is generated *from* it (the way PostgreSQL
logical decoding derives logical change records from the physical WAL), so the two
can never diverge. The logical/physical split is kept on purpose: logical records
are entity-level, idempotent, and cross-version-safe (good for replication and
forward/backward compatibility), while the physical WAL is page/engine-level (good
for crash recovery). What is removed is the *dual-write*, not the split.

**The term/epoch is stamped on the physical WAL (the source of truth) and carried
into the derived logical records.** Divergence detection compares `(term, lsn)`:
two records with the same LSN and different terms are divergent histories, not a
benign retry. The election vote rule (ADR 0030) reads the term to decide
electability. The logical spool gets a new framing version (v3) that carries the
term forward to replicas.

**Logical record format stays entity-level and idempotent.** This is the standing
reason RedDB does not ship a raw command stream (avoiding Valkey's
non-deterministic-command replication hazards): the replicated unit is the
post-image change record, safe to replay, not the intent.

## Considered Options

- **Keep two independent dual-written logs, term in the spool only.** Rejected:
  leaves two sources of truth that can drift, and divergence detection on the spool
  could disagree with the physical WAL.
- **Fully unify into a single log of record (PostgreSQL model).** Rejected for now:
  a major storage-engine change that couples replication to the physical WAL format;
  the derived-projection approach captures most of the benefit (single source of
  truth, one place for the term) without the rewrite.

## Consequences

- A new logical-spool framing version (v3) carrying the term must be defined; v2
  stays readable for compatibility (as v1 is today).
- The CDC-hook dual-write is replaced by a derivation step from the physical WAL;
  the WAL record format must carry (or let us recompute) everything the logical
  record needs.
- Slot-based retention (ADR-to-be / Q09) must keep the physical WAL back to the
  minimum `restart_lsn`, because the logical projection is replayed from it.
- The physical WAL record header must reserve space for the term/epoch field.
