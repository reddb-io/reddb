# MVCC Read Resolver Seam

`TableRowMvccReadResolver` is the table-row visibility seam for runtime reads. New table-row read paths should discover candidate rows however they need to, then ask the resolver whether the candidate is visible before materializing a record, returning an id, or applying a DML target.

Use the resolver for:

- heap/table scans;
- secondary-index candidate rechecks;
- stable logical row lookup by `rid`;
- UPDATE and DELETE target selection;
- AS OF and other captured-snapshot reads;
- parallel scan closures, using a captured resolver.

The resolver owns the visibility decision, not candidate discovery. Indexes, zone maps, bloom hints, and planner fast paths can still reduce the candidate set, but they must not bypass resolver-backed visibility. When an active snapshot may need old row versions, current secondary indexes are not a completeness proof; the read path should fall back to a heap scan or another resolver-backed path that can see the historical version.

Current scope:

- table rows only;
- public identity is `rid`;
- current-row reads use the current store and MVCC recheck;
- historical reads use currently retained table-row versions and explicit fallback behavior.

Out of scope for this slice:

- full history-store implementation;
- new WAL records;
- disk-format changes;
- full transaction write-set overlay;
- changing public SQL syntax.

Conformance coverage lives in `tests/e2e_mvcc_read_resolver_conformance.rs`. It checks that table scan, indexed read, logical lookup, DML target selection, and AS OF table reads agree through the resolver-backed paths.
