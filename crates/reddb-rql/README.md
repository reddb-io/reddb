# reddb-io-rql

The RedDB Query Language front-end and conformance authority. This crate owns
the storage-agnostic language layer: lexer, parser, AST, SQL/RQL routing,
mode translators, analyzer, expression typing, optimizer, and sqllogictest
corpus data.

It does not execute queries. Physical execution, indexes, runtime state, and
storage access stay in `reddb-io-server`.

## When to use it

Use this crate when you need to parse, analyze, lower, or validate RedDB query
text without linking the server:

- parse SQL-like RQL statements into the shared AST;
- route front-end statements across SQL, graph, vector, queue, KV,
  time-series, admin, and RedDB-specific command families;
- apply parser DoS limits through `ParserLimits`;
- render values into sqllogictest cells for conformance comparisons;
- read or extend the canonical query conformance corpus.

## Install

```toml
[dependencies]
reddb-io-rql = "1.13"
```

The Rust import name is `reddb_rql`:

```rust
use reddb_rql::{parse, parse_frontend, ParserLimits};

# fn run() -> Result<(), Box<dyn std::error::Error>> {
let query = parse("SELECT name FROM users WHERE id = $1")?;
let frontend = parse_frontend("KV GET config.theme")?;

let limits = ParserLimits {
    max_input_bytes: 1024,
    ..ParserLimits::default()
};
let mut parser = reddb_rql::Parser::with_limits("SELECT 1", limits)?;
let limited = parser.parse()?;
# let _ = (query, frontend, limited);
# Ok(())
# }
```

## What it owns

- `lexer`, `parser`, `ast`, and `sql`: tokenization, recursive-descent parsing,
  statement routing, AST nodes, and error positions.
- `modes`: Gremlin, Cypher, SPARQL, Path, Natural, SQL, and vector-extension
  translators that feed the shared logical shape.
- `analyzer`, `expr_typing`, `planner`, `optimizer`, `filter_optimizer`, and
  `sql_lowering`: storage-agnostic validation and plan shaping.
- `conformance`: SQLite-style result-cell rendering plus the sqllogictest
  corpus roots under `tests/corpus` and `tests/reddb_corpus`.

The crate depends only on `reddb-io-types` inside the workspace. That keeps the
query front-end below the server and prevents a crate cycle.

## Conformance model

The standard SQL corpus in `tests/corpus` uses SQLite sqllogictest output as the
oracle. RedDB-only surfaces in `tests/reddb_corpus` use hand-authored semantic
goldens. Engine-output characterization is allowed only as a clearly marked
regression layer; it must not become truth for a query surface.

## Verification

```sh
cargo test -p reddb-io-rql
cargo check -p reddb-io-rql
```

The server-backed conformance harnesses live at the workspace level because
they execute this crate's corpus against `reddb-io-server`:

```sh
cargo test --test grouped_sql_core
cargo test --test grouped_surface_contracts
```

## References

- [Standard SQL corpus notes](tests/corpus/README.md)
- [RedDB-only corpus notes](tests/reddb_corpus/README.md)
- [ADR 0052 - type keystone](../../.red/adr/0052-reddb-io-types-keystone.md)
- [ADR 0053 - RQL boundary](../../.red/adr/0053-reddb-io-rql-boundary.md)

