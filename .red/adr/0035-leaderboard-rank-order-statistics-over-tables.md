# Leaderboard rank: order-statistics as a table capability, hybrid MVCC semantics

"Top-N by score" is **already solved** in RedDB by a sorted secondary index plus `ORDER BY score DESC LIMIT N` (`query_exec/indexed_scan.rs` `try_sorted_index_lookup`); the index is maintained in order on write, so the top-N read is an O(log n) seek + short scan. The genuine gap is the **rank query** — "what position / percentile is row X?" — which a vanilla B-tree index cannot answer cheaply, because rank requires order-statistics (subtree counts) that a plain index does not carry.

RedDB's secondary index is **not snapshot-versioned**: it yields candidate entity IDs and MVCC correctness is enforced by a per-row visibility re-check *after* the seek (`dml_target_scan.rs` `visible_candidate` + `TableRowMvccReadResolver`). A naive subtree-count augmentation would therefore be version-blind — its count includes rows invisible to the querying snapshot — so an exact, globally snapshot-correct rank is either wrong-semantics or expensive (per-version counts).

Decisions:

1. **Hybrid rank semantics.** Exact, MVCC-correct rank is served only for a bounded **top-K head** (where walk-and-filter over visible candidates is cheap), reusing the existing row-visibility machinery. The long tail is served by an **approximate** percentile/rank sketch maintained per `(collection, score column)`. This matches how leaderboards are actually read — exact at the top, "you're in the top X%" for the body — and never violates the MVCC contract.
2. **Capability over a table, not a new Collection model.** Order-statistics is declared on an ordinary `TABLE`'s score column (like an index or a metric descriptor), reusing MVCC, the sorted index, policy, and WAL. No `sortedset` model is introduced. This follows the Analytics precedent already in `.red/CONTEXT.md` (prefer concrete capabilities over ordinary collections; do not introduce new public object types).
3. **Canonical SQL surface + Redis-flavor sugar.** The canonical form is SQL — `RANK() OVER (ORDER BY score DESC)` window semantics and a rank-of-row projection — respecting MVCC, policy, and the Analytics "prefer SELECT over admin verbs" rule. Redis-style `Z*` verbs (`ZRANK`, `ZRANGE … WITHSCORES`) are thin sugar that **desugars** to the canonical SQL, exactly as `SHOW COLLECTIONS` desugars to `SELECT … FROM red.collections`.

## Considered Options

- **Hybrid exact-head + approximate-tail, table capability, SQL+sugar (chosen).** Respects MVCC, reuses the row-visibility and index machinery, and delivers exact rank where it matters (the head) without paying for snapshot-versioned order-statistics across the whole keyspace.
- **Exact latest-committed rank via subtree-count index augmentation (rejected).** Cheap, but returns a rank outside the statement's MVCC snapshot — it can disagree with the rows `SELECT`ed under that same snapshot. A consistency violation dressed as a feature; rejected on RedDB's "correctness > convenience" stance.
- **Exact snapshot-correct rank via versioned order-statistics (rejected for v0).** Per-version subtree counts preserve the MVCC contract but touch the hot index-maintenance path and cost far more than the leaderboard use case justifies. Revisit only if exact global rank under arbitrary snapshots becomes a hard requirement.
- **Pure approximate rank sketch (rejected as sole mechanism).** Sidesteps MVCC entirely and aligns with the existing "probabilistic structures are approximate sidecars" framing, but disappoints exactly at the top of the board — the part users actually stare at. Kept as the *tail* half of the hybrid, not the whole answer.
- **New `sortedset` Collection model (rejected).** 1:1 with Redis and self-contained, but rebuilds MVCC, persistence, and policy from scratch purely to gain Redis syntax — which the chosen `Z*` sugar delivers over the table capability anyway.

## Consequences

- Rank reads carry **two consistency tiers**: the top-K head is exact and snapshot-correct; the tail is an explicitly approximate estimate. Surfaces and docs must label the tail as approximate so callers do not read a tail percentile as an exact position. The K boundary is a tuning knob, not a wire contract.
- The approximate-tail engine (t-digest vs equi-depth histogram vs count-min) and the exact-head index-augmentation mechanics are **implementation details** deliberately left open by this ADR; they are reversible and do not change the rank semantics or the SQL surface contract.
- The `Z*` sugar must not grow semantics the canonical SQL lacks — it is a desugaring layer only, so a future SQL-only reader sees the whole behavior.
- This decision is scoped to *ranking over scored rows*. It does not introduce a keyed sorted-set value type; "one sorted set per key" Redis workloads map onto a table + score column + this capability.
