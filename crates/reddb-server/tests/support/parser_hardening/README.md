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
- `corpus.rs` — adversarial-input fixtures (deeply nested parens,
  long identifiers, oversized inputs) used by both property tests
  and fuzz seeds. `migration_adversarial_inputs()` covers the
  migration DSL surface (#88).

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
- Snapshot review: `cargo insta review` (writes accepted output
  back to `*.snap` files)
- Fuzz smoke: `cargo fuzz run sql_parser -- -max_total_time=10`
- Fuzz smoke (migration): `cargo fuzz run migration_parser -- -max_total_time=10`
- Fuzz CI run: `cargo fuzz run sql_parser -- -max_total_time=300`
  and `cargo fuzz run migration_parser -- -max_total_time=300`

## Limit defaults

`ParserLimits::default()` ships with:

| Limit                  | Value      |
|------------------------|------------|
| `max_depth`            | 128        |
| `max_input_bytes`      | 1 MiB      |
| `max_identifier_chars` | 256        |

See `docs/security/parser-limits.md` for rationale.
