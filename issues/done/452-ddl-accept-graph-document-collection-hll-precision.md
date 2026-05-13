# Accept `CREATE GRAPH | DOCUMENT | COLLECTION` and `CREATE HLL ... PRECISION p` [DONE]

Completed locally. GitHub issue #452 does not exist in this repository, so there was no remote issue to comment on, move, or close.

## What changed

- Added parser/runtime support for `CREATE GRAPH <name>`.
- Added parser support for `CREATE DOCUMENT <name>` with executor-level `NOT_YET_SUPPORTED`.
- Added `CREATE COLLECTION <name> KIND <kind>` with graph dispatch and executor-level unsupported-kind errors.
- Added `CREATE HLL <name> PRECISION p` and exposes precision through `HLL INFO`.

## Verification

- `cargo test -q -p reddb-io-server --lib test_parse_create_graph_document_and_collection_forms -- --test-threads=1`
- `cargo test -q -p reddb-io-server --test probabilistic_parser happy_create_hll -- --test-threads=1`
- `cargo test -q -p reddb-io-server --test runtime_query_behavior create_ -- --test-threads=1`
- `cargo test -q -p reddb-io-server --lib hyperloglog -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
- `git diff --check`
