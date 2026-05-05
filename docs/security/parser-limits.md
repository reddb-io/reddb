# Parser DoS Limits

The RedDB query parser ships with hard limits on input size,
recursion depth, and identifier length. The limits exist to keep
adversarial query strings — a small payload that triggers
unbounded recursion or memory growth — bounded by structured
errors instead of process panics or OOM.

## Defaults

| Limit                  | Default | Configured by               |
|------------------------|---------|-----------------------------|
| `max_depth`            | 128     | `ParserLimits::max_depth`   |
| `max_input_bytes`      | 1 MiB   | `ParserLimits::max_input_bytes` |
| `max_identifier_chars` | 256     | `ParserLimits::max_identifier_chars` |

Source: `crates/reddb-server/src/storage/query/parser/limits.rs`.

## Rationale

- **`max_depth = 128`**. Hand-written queries top out around
  10–12 levels (CTE → subquery → expression). 128 leaves an order
  of magnitude of headroom for tools that machine-generate
  queries while still bounding recursion well below typical
  thread-stack ceilings (8 MiB on tokio worker threads).
- **`max_input_bytes = 1 MiB`**. Queries that exceed 1 MiB are
  almost always tooling regressions — even bulk INSERT statements
  belong on the binary `INSERT … VALUES (?, ?, ?)` path, not as a
  multi-megabyte SQL string. Refusing oversized input here keeps
  the lexer's `Chars` iterator bounded.
- **`max_identifier_chars = 256`**. Identifier-keyed HashMaps
  exist on every code path that walks an AST; capping identifier
  length keeps that pressure predictable. 256 chars covers
  legitimate UUID-tagged column names with room to spare.

## Error surface

Limit violations return a `ParseError` whose `kind` field is one
of:

- `ParseErrorKind::DepthLimit { limit_name, value }`
- `ParseErrorKind::InputTooLarge { limit_name, value }`
- `ParseErrorKind::IdentifierTooLong { limit_name, value }`

Callers that want to differentiate DoS refusals from grammar
errors pattern-match on `kind`. The harness (issue #87) and the
snapshot test suite both rely on this — see
`crates/reddb-server/tests/parser_snapshots.rs` for the pinned
wording.

## Overriding limits

Construct a `Parser` with explicit limits when the default is
wrong for a workload:

```rust
use reddb_server::storage::query::parser::{Parser, ParserLimits};

let limits = ParserLimits {
    max_input_bytes: 4 * 1024 * 1024,
    ..ParserLimits::default()
};
let mut p = Parser::with_limits(input, limits)?;
let query = p.parse()?;
```

Production callers should leave the defaults alone unless they
have a measured need; replication apply and admin migration paths
that legitimately need bigger envelopes are the documented
exceptions.

## Fuzz harness

`fuzz/fuzz_targets/sql_parser.rs` (added in issue #87) feeds
arbitrary bytes into `parser::parse` and asserts no panic ever
occurs. The DoS limits described above are the ceiling that lets
the fuzzer terminate quickly — without them, libFuzzer would find
unbounded recursion and time out before reporting useful coverage.

### Local smoke run

```bash
cargo +nightly fuzz run sql_parser -- -max_total_time=10
```

### CI run (per-PR, 5 minutes)

The `Fuzz Parsers` job in `.github/workflows/ci.yml` runs the
target with `-max_total_time=300`. A panic, ASAN finding, or
out-of-memory abort fails the job; libFuzzer's evolved corpus is
uploaded as an artifact for triage.

### Nightly long-window run

A 1-hour scheduled run lives outside the per-PR job to keep PR
latency tight. To set up, copy `.github/workflows/ci.yml`'s
`fuzz-parsers` job into a new scheduled workflow with
`-max_total_time=3600` and a `cron: '0 4 * * *'` schedule.

### Reproducing a crash

libFuzzer writes failing inputs to `fuzz/artifacts/<target>/`.
Replay with:

```bash
cargo +nightly fuzz run sql_parser fuzz/artifacts/sql_parser/<id>
```

The harness is parser-agnostic; subsequent slices (#88, #89, #90)
plug their own parsers into the same scaffolding. See
`crates/reddb-server/tests/support/parser_hardening/README.md`
for the consumer guide.
