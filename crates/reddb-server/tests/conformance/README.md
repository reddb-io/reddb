# Parser Conformance Corpus

Each `.toml` file in this directory is one conformance test case. The runner
(`tests/conformance.rs`) iterates every `*.toml` file and verifies the parser
output matches the declared expectation.

## Adding a case

1. Copy an existing file — e.g. `cp select_simple.toml my_new_case.toml`
2. Edit the four fields:

```toml
input = "SELECT id FROM orders WHERE status = 'open'"
expected_kind = "Table"
source = "docs/data-models/tables.md:22"
kind = "positive"
```

| Field           | Values                                          |
|-----------------|-------------------------------------------------|
| `input`         | Exact RQL string to parse                       |
| `expected_kind` | `QueryExpr` variant name (e.g. `Insert`, `QueueCommand`, `CreateTable`) |
| `source`        | `file:line` of the doc example this pins        |
| `kind`          | `positive` (must parse) or `negative` (must fail) |

Negative cases live under `negative/` and must include the expected rendered
error fragment:

```toml
input = "INSERT INTO users (id)"
expected_error_substring = "Unexpected token: <EOF>"
expected_error_kind = "Syntax"
source = "crates/reddb-server/src/storage/query/parser/error.rs:81"
kind = "negative"
```

`expected_error_kind` is optional, but should be used when the case is meant to
exercise a `ParseErrorKind` branch (`Syntax`, `DepthLimit`, `InputTooLarge`,
`IdentifierTooLong`, `ValueOutOfRange`). Very large or repetitive inputs can be
assembled with `input_repeat_count`, `input_prefix`, and `input_suffix`.
Cases that need smaller parser DoS limits can set `max_depth`,
`max_input_bytes`, or `max_identifier_chars`.

3. Run `cargo test -p reddb-server --test conformance` — it should pass.

No code changes needed. The runner discovers new files automatically.

## Invariant

The 8 `doc_form_*` unit tests in `src/storage/query/parser/tests.rs` remain
the authoritative pin for doc examples. This corpus is additive — it expands
coverage, not replaces those tests.
