# Mode detector: tighten `?`-placeholder SPARQL heuristic

Status: done

Implemented:

- Replaced the broad `" ?"` SPARQL heuristic with an explicit `?` followed by an ASCII identifier-start character.
- Bare SQL placeholders (`?`) and numbered placeholders (`?1`) now stay in SQL mode.
- Real SPARQL variables such as `?x` still route to SPARQL.
- Added detector tests for:
  - `SELECT name FROM t WHERE id = ?`
  - `SELECT name FROM t WHERE id = ?1`
  - `INSERT INTO t (id, name) VALUES (?, ?)`
  - `SELECT ?x WHERE { ?x rdf:type :Foo }`

Verification:

- `cargo test -q -p reddb-io-server storage::query::modes::detect::tests::test_sql_detection --lib -- --test-threads=1`
- `cargo test -q -p reddb-io-server storage::query::modes::detect::tests::test_sparql_detection --lib -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
- `git diff --check`

Notes:

- `cargo test` emitted pre-existing `unused_mut` warnings from `crates/reddb-server/src/serde_json.rs` macro expansion through `application/merge_json.rs`.
- GitHub issue `#467` does not exist in `reddb-io/reddb`; no remote comment or close was possible.
