# Parser Hardening Harness

Reusable test scaffolding for parser hardening (issue #87). The
SQL parser is the first consumer; subsequent slices (#88, #89,
#90) reuse the same harness against other parsers.

## What's here

- `mod.rs` — generic harness types: `ParseFn`, `HardenedParser`
  trait, `assert_no_panic_on`, `roundtrip_property`, snapshot
  helpers.
- `sql_grammar.rs` — proptest strategies that emit syntactically
  valid SQL strings for SELECT/INSERT/UPDATE/DELETE.
- `migration_grammar.rs` — proptest strategies for the migration
  DSL (CREATE/APPLY/ROLLBACK/EXPLAIN MIGRATION). Consumed by
  `tests/migration_parser.rs` (#88).
- `vector_search_grammar.rs` — proptest strategies for the
  vector-search surface (SEARCH SIMILAR / VECTOR SEARCH /
  SEARCH HYBRID / HYBRID FROM / INSERT WITH AUTO EMBED).
  Consumed by `tests/vector_search_parser.rs` and
  `tests/vector_search_snapshots.rs` (#100).
- `corpus.rs` — adversarial-input fixtures (deeply nested parens,
  long identifiers, oversized inputs) used by both property tests
  and fuzz seeds. `migration_adversarial_inputs()` covers the
  migration DSL surface (#88). `vector_search_adversarial_inputs()`
  covers the vector-search surface (#100): malformed vector
  literals, NaN / Infinity / oversized-dim cases, AUTO EMBED edge
  cases, HYBRID fusion-strategy errors.
- `secret_redactor.rs` — shared `insta` filter set that masks
  secret-shaped substrings (bearer headers, JWTs, conn-string
  credential params, `sk_/rs_/reddb_` API keys) before insta
  diffs the snapshot (#98). Every parser snapshot test in this
  crate must opt in via `install_redactions()`.

## Snapshot secret redaction (#98)

Bad inputs may contain secret-shaped substrings. To prevent a real
credential from being pinned into a `*.snap` file, every parser
snapshot test must install the shared redactor before calling
`insta::assert_snapshot!`:

```rust
use super::secret_redactor;

let _guard = secret_redactor::install_redactions();
insta::assert_snapshot!(name, formatted);
```

The lint `tests/snapshot_redaction_lint.rs` re-greps every
committed `*.snap` file with the same patterns and fails CI on a
single unmasked match.

## How a new parser consumes the harness

1. Implement the `HardenedParser` trait for your parser type:

   ```rust
   impl HardenedParser for MyParser {
       type Error = MyParseError;
       fn parse(input: &str) -> Result<(), Self::Error> { ... }
       fn parse_with_limits(input: &str, limits: ParserLimits)
           -> Result<(), Self::Error> { ... }
   }
   ```

2. Add a property-test generator that emits valid input strings
   (see `sql_grammar.rs` for the SQL pattern).

3. Drop a snapshot test module that calls
   `snapshot_parse_error(name, input)` for each pinned error
   path.

4. Add a fuzz target `fuzz/fuzz_targets/<name>.rs` that calls
   `assert_no_panic_on::<MyParser>` (provided by the harness).

## Running

- Property + snapshot suite (SQL): `cargo test -p reddb-server --test parser_hardening`
- Property suite (migration DSL): `cargo test -p reddb-server --test migration_parser`
- Snapshot suite (migration DSL): `cargo test -p reddb-server --test migration_parser_snapshots`
- Property suite (vector-search): `cargo test -p reddb-server --test vector_search_parser`
- Snapshot suite (vector-search): `cargo test -p reddb-server --test vector_search_snapshots`
- Snapshot review: `cargo insta review` (writes accepted output
  back to `*.snap` files)
- Fuzz smoke: `cargo fuzz run sql_parser -- -max_total_time=10`
- Fuzz smoke (migration): `cargo fuzz run migration_parser -- -max_total_time=10`
- Fuzz CI smoke: `cargo fuzz run sql_parser -- -max_total_time=30`
  and `cargo fuzz run migration_parser -- -max_total_time=30`

## Limit defaults

`ParserLimits::default()` ships with:

| Limit                  | Value      |
|------------------------|------------|
| `max_depth`            | 128        |
| `max_input_bytes`      | 1 MiB      |
| `max_identifier_chars` | 256        |

See `docs/security/parser-limits.md` for rationale.
