# SQL row models, native reuse and keyed-read visibility

## Reproduced failures

On runtime source `689438210c4e72c1c29eee9c5f1444bea40b82ac`, two bulk-inserted
table rows, a unique ID index, a sealed baseline and three SQL updates are
enough to change the derived text frame merely by reopening the database.
The warm source produces 14 postings / 326 bytes; the reopened source produces
10 postings / 278 bytes for the same five retained physical row versions.
The regression fails at exact derived-byte equality before the correction.

The SQL update row fast path treats any row with at least two positional
columns, or a field called `value`, as a KV. Preserving its invented `key`
promotes the table row into a named representation while retaining positional
values. Later updates change the authoritative named fields while leaving
stale positional values. Native derivation observes both; the row codec
persists the named representation. Cold source-proof rejection is therefore
correct, but the mutation causes avoidable index changes and reconstruction.

A separate control found that an ordinary SQL update of a real KV can return
the new value through SELECT while KV GET returns the old value. A standalone
probe linked to the independently hashed original server library reproduces
this before the correction. GET's resolver uses the PUT-history flag to decide
whether to apply visibility, although SQL updates create MVCC versions even
when PUT history is disabled. Its first raw candidate can be superseded.

## Corrections and proof

SQL row updates now use the catalog's declared collection model for KV key
preservation. Table width and field names do not identify a KV. A table may
have ordinary columns named `key` and `value`, including updates to its key
column. Explicit KV identity remains preserved.

Plain KV keyed reads use the existing snapshot-visible candidate resolver
for SQL versions as well. PUT's history policy, config/vault read policies,
row serialization, source-proof format, WAL ordering, double-write protection,
checkpoint admission and native publication ordering are unchanged. No new
lock is introduced; snapshot capture still precedes the bounded projection
under the segment read guard. Existing source-proof revalidation still rejects
unproved sources and changed physical references.

All 30 focused tests pass, including the new regressions:

- `updated_columnar_rows_keep_native_sources_stable_across_reopen`: no invented
  key, identical warm/cold derived bytes, exact retained physical references,
  recovered current payloads and invisible historical values.
- `sql_updates_preserve_only_catalog_kv_identity`: a table's ordinary key can
  change, while real KV SQL updates and KV GET agree on its preserved identity.
- `plain_kv_reads_follow_sql_commit_and_rollback`: own SQL writes, rollback,
  commit, checkpoint and cold recovery agree through KV GET.

The original cold proof, concurrent preparation, changed-source rejection,
native frame/delta and source-proof crash tests remain in the same focused
run. An initial control fixture failed at an unquoted reserved SQL identifier;
its corrected quoted DDL then exposed the independent keyed-read failure.
Both failed observations were retained rather than treated as a passing run.

## Qualification boundary

These small failures and fixes do not prove the cause of every historical GB
of growth, remove persisted extra fields from historical rows, or make index
work proportional to six edits. Full-frame text serialization and collection
source hashing still perform global work. They also do not qualify the rejected
RC377, whose first historical Platform checkpoint took 13.828 seconds.

Expanded local qualification passed: 30 focal native/keyed-read tests, 30
additional KV tests, 31 storage recovery tests, 17 SQL update conformance tests,
eight document/KV update tests and five snapshot-retention tests (121 total),
plus locked server/test checking and workspace formatting. Full CI on the exact
runtime source is [40382040196](https://github.com/reddb-io/reddb/actions/runs/40382040196);
its final result and a signed candidate are tracked separately. The same final artifact
must still pass historical Auth and Platform, held snapshots and reuse after
release, acknowledged-write crash recovery and the unchanged 10-second/64 MiB
gates. No live database or cloud workload was modified by these reproductions.

Private receipts: `columnar-source-red-prep1382.private.log`,
`columnar-source-green-prep1384.private.log`,
`fulltext-scaling-receipt-prep1385.private.json`,
`fulltext-scaling-results-prep1385.private.jsonl`,
`columnar-kv-green-prep1386.private.log` and
`row-model-qualified-prep1387.private.json` in the existing qualification area.
