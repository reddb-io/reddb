# Parser DoS Limits

The RedDB query parser ships with hard limits on input size,
recursion depth, identifier length, and consumed token count. The limits exist to keep
adversarial query strings — a small payload that triggers
unbounded recursion or memory growth — bounded by structured
errors instead of process panics or OOM.

## Defaults

| Limit                  | Default | Configured by               |
|------------------------|---------|-----------------------------|
| `max_depth`            | 16      | `ParserLimits::max_depth`   |
| `max_input_bytes`      | 1 MiB   | `ParserLimits::max_input_bytes` |
| `max_identifier_chars` | 256     | `ParserLimits::max_identifier_chars` |
| `max_tokens`           | 8192    | `ParserLimits::max_tokens`  |

Source: `crates/reddb-rql/src/limits.rs`.

## Rationale

- **`max_depth = 16`**. Hand-written queries top out around
  10–12 levels (CTE → subquery → expression). 16 leaves focused
  headroom for generated queries while keeping nested
  SELECT/function-call payloads below the default test/fuzz
  thread stack, including the stack-sensitive fuzz seed from
  issue #1479.
- **`max_input_bytes = 1 MiB`**. Queries that exceed 1 MiB are
  almost always tooling regressions — even bulk INSERT statements
  belong on the binary `INSERT … VALUES (?, ?, ?)` path, not as a
  multi-megabyte SQL string. Refusing oversized input here keeps
  the lexer's `Chars` iterator bounded.
- **`max_identifier_chars = 256`**. Identifier-keyed HashMaps
  exist on every code path that walks an AST; capping identifier
  length keeps that pressure predictable. 256 chars covers
  legitimate UUID-tagged column names with room to spare.
- **`max_tokens = 8192`**. Flat adversarial inputs can stay under
  byte, identifier, and recursion-depth limits while still forcing
  long token streams and large expression/projection trees. Capping
  consumed tokens keeps parser work bounded for those cases.

## Error surface

Limit violations return a `ParseError` whose `kind` field is one
of:

- `ParseErrorKind::DepthLimit { limit_name, value }`
- `ParseErrorKind::InputTooLarge { limit_name, value }`
- `ParseErrorKind::IdentifierTooLong { limit_name, value }`
- `ParseErrorKind::TokenLimit { limit_name, value }`

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

### CI run (per-PR smoke)

The `Fuzz Parsers` job in `.github/workflows/ci.yml` runs every
parser fuzz target in one required check with a `--dev` fuzz build
and `-max_total_time=30` per target. This keeps pull-request cost
bounded while still replaying seed corpora and catching deterministic
panics, ASAN findings, and out-of-memory aborts. Crash artifacts are
uploaded for triage.

### Nightly long-window run

A 1-hour scheduled run lives in
`.github/workflows/parser-fuzz-nightly.yml` outside the per-PR job
to keep PR latency tight. It runs the same parser fuzz targets with
`-max_total_time=3600` and preserves the evolved corpus between
runs.

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
