# DDL accept: `CREATE VECTOR <name> DIM d [METRIC m]` [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#445

## What to build

Add the DDL surface for the native vector collection that lands in #454. This issue is parser-only:

- `CREATE VECTOR <name> DIM <d>` is accepted; dimension is mandatory.
- `CREATE VECTOR <name> DIM <d> METRIC cosine|l2|inner_product` accepts an optional metric. Default is `cosine`.
- The DDL persists the collection's dimension and metric in the catalog so subsequent inserts and `VECTOR SEARCH` can validate against them.

The storage backing and the search executor are NOT in this issue - they land in #454. Until then, the parser accepts the form and the executor returns `NOT_YET_SUPPORTED` (using the variant from #451) for inserts and searches.

## Acceptance criteria

- [x] `CREATE VECTOR v DIM 4` parses and creates a catalog entry.
- [x] `CREATE VECTOR v DIM 4 METRIC cosine` parses; metric stored.
- [x] `CREATE VECTOR v DIM 4 METRIC unknown` produces a clear parse error naming the supported metric set.
- [x] `CREATE VECTOR v` without `DIM` produces a clear parse error.
- [x] `SHOW COLLECTIONS` reports the vector collection with its dim and metric.
- [x] Parser unit tests for each accept and reject form.

## Blocked by

- #451 (parse-error vocabulary)

## Completion

- Added parser, AST, SQL command, and runtime DDL support for `CREATE VECTOR`.
- Persisted `vector_dimension` and `vector_metric` in collection contracts/catalog JSON.
- Exposed vector `dimension` and `metric` through `red.collections` / `SHOW COLLECTIONS`.
- Verified with targeted parser, runtime, red schema, and red collections tests plus `cargo check`.
