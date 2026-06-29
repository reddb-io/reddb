# ADR 0063 — DOCUMENT collections: single source of truth in a native binary body

Status: accepted

Date: 2026-06-28
Implemented: 2026-06-28

## Context

RedDB's DOCUMENT collection type is positioned as the MongoDB-competitive store. Today a document is stored in **two parallel representations**:

1. a `body` column (`Value::Json`) holding the whole document, and
2. **promoted columns** — each top-level key materialised as a named column for fast filtering/projection.

These two can diverge, and did: #1394 — a partial `UPDATE … DOCUMENTS SET <field>` wrote the promoted column but never re-serialised `body`, so `SELECT body` returned stale data. PR #1395 forced the invariant `body ⊇ promoted columns`, but the desnormalisation (two sources of truth) remains, with its write-amplification and divergence risk.

This was grilled in a `/start` session (2026-06-25). Key findings from the code:

- RedDB **already** has a derived-on-write secondary index layer (`hash_index.rs`, `impl_dml.rs::refresh_update_secondary_indexes`) — the "Mongo-style index re-extracted on write" already exists.
- The native value codec (`crates/reddb-types/src/value_codec.rs`) is a tag-byte + varint-length + payload binary format with **rich semantic types BSON lacks** — `Email`, `Ipv4`, `Subnet`, `Color`, plus TOAST (out-of-line large values).
- MongoDB's model is **one** source of truth (BSON blob per document in a WiredTiger B-tree keyed by `RecordId`) with indexes as separate structures derived from the document on write. The "single root attribute" is a *storage* fact (one blob holds the whole doc), not a model restriction — Mongo documents have many queryable root fields.

The maintainer's anxiety ("did I make a mistake allowing many root attributes?") resolves to: many root attributes are fine; the real issue is **how many sources of truth**, not how many fields.

## Decision

Migrate DOCUMENT collections to a **single source of truth**: the document **body**, stored in RedDB's **native binary format** (not BSON-the-spec). Specifically:

1. **One source of truth.** The body is canonical. **Remove the materialised promoted columns.** Their two roles split cleanly: *filtering* → the (already-derived) secondary index; *projection* → an offset-read from the body.

2. **Native binary body, not BSON-the-spec.** Reuse/extend the `reddb-types` typed value codec. Rejecting BSON-the-spec is deliberate: BSON would **flatten** RedDB's rich semantic types (`Email`, `Ipv4`, `Subnet`, `Color`, …) into generic string/binary, losing validation, semantic indexing and display — a downgrade.

3. **Improve on BSON structurally.** The native body gains:
   - an **offset table** in the body header → O(1) field access (BSON is sequential skip-scan), and
   - a **per-collection key dictionary** (field-name ↔ id), so homogeneous collections don't repeat key strings in every document. The dictionary is **append-only** for common keys, with an **inline-key fallback** for rare/unique keys so a heterogeneous collection can't blow up the shared catalogue (preserving Mongo's schemaless flexibility).

4. **Indexes are explicit and ordered.** `CREATE INDEX`, Mongo-style — a field is consultable whether or not it is indexed (unindexed → scan + offset-read), so **having many root attributes costs nothing** (they live only in the body). Declared indexes are **ordered B-trees** (range, `ORDER BY`, top-N, pagination), keeping the existing **hash index** for high-cardinality equality.

5. **Wire stays JSON.** The binary format is **storage-only**; the server decodes binary→JSON on read. No driver/client contract changes now (the wire/file authority boundary of ADR 0046 is untouched). Pushing binary to the wire is a separate, later decision.

6. **All-at-once migration, executed reversibly.** New-format data is written into **fresh files alongside the old**, indexes are built (including a one-time **auto-`CREATE INDEX` for every previously-promoted field** to avoid a silent post-deploy perf regression), counts are verified, then an **atomic swap** cuts over. The old files are retained as the **rollback point** (the maintainer was previously burned by a data migration; the untouched-until-swap design is the explicit safety margin).

## Alternatives considered (rejected)

- **Keep the dual representation, hardened (status quo + #1394 invariant).** Rejected: contains divergence rather than eliminating it, and keeps write amplification; not the Mongo single-source model.
- **BSON-the-spec (wire-compatible).** Rejected: drags in MongoDB *driver-protocol* compatibility (orders of magnitude larger than a storage format, colliding with ADR 0046), and would flatten RedDB's rich semantic types.
- **Phased / non-destructive execution.** The author recommended it; the maintainer chose all-at-once with eyes open. Captured here as the rejected option, with the atomic-swap + retained-old-files design as the mitigation.
- **Auto-index every top-level field.** Rejected: reintroduces the write amplification we are removing.

## Consequences

- The #1394 class of bug is eliminated **at the root** — there is no second copy to drift.
- Writes get cheaper (no N materialised columns to maintain per write); reads of a single field stay O(1) via the offset table.
- Unindexed filters become scans (acceptable: offset-read makes per-doc field extraction cheap, and hot fields are explicitly indexed).
- New surfaces to build/own: the binary body codec evolution (offset table + key dictionary), the ordered B-tree index, the migration tool, and the read-path binary→JSON decode.
- Deferred (own decisions later): the per-collection key dictionary's compaction story; pushing binary to the wire (capability-negotiated); a BSON edge-translator if external-tool compat is ever wanted; the gRPC `0 affected_rows` connect-path defect (tracked as #1396).

## Reversibility

Hard to reverse once data is rewritten — hence the retained old files as the immediate rollback point for the cutover, and this ADR documenting the "why" (especially why native-binary over BSON, and why single-source over the hardened dual model).
