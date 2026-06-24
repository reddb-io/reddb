# reddb-io-types

The neutral keystone crate for RedDB's logical type system. It sits at the
bottom of the workspace graph and owns the vocabulary shared by query parsing,
wire payloads, file codecs, client parameters, and server runtime code.

Every authority crate can depend on `reddb-io-types`; `reddb-io-types` depends
on no other RedDB workspace crate.

## When to use it

Use this crate when you need RedDB values or type metadata without linking the
server:

- `Value` for logical scalar, vector, JSON, array, network, temporal, and
  nullable values;
- `DataType` for stored logical column types;
- `SqlTypeName`, `TypeModifier`, and `TypeCategory` for declared SQL types;
- `Row` for ordered result values;
- value serialization through the canonical `value_codec`;
- coercion, comparison, operator, function-catalog, duration, vector, and JSON
  helpers shared by parser, wire, and runtime layers.

## Install

```toml
[dependencies]
reddb-io-types = "1.13"
```

The Rust import name is `reddb_types`:

```rust
use reddb_types::{DataType, Row, SqlTypeName, Value};

let declared = SqlTypeName::parse_declared("DECIMAL(10)");
assert_eq!(declared.base_name(), "DECIMAL");

let row = Row::from(vec![
    Value::Integer(42),
    Value::text("alice"),
    Value::Boolean(true),
]);

assert_eq!(Value::Integer(42).data_type(), DataType::Integer);
assert_eq!(row.values().len(), 3);
```

## What it owns

- `types`: `Value`, `DataType`, `SqlTypeName`, `TypeModifier`, `TypeCategory`,
  `ValueError`, and `Row`.
- `value_codec`: the canonical binary encoding behind `Value::to_bytes` and
  `Value::from_bytes`.
- `coerce`, `coercion_spine`, `cast_catalog`, and `parametric`: the shared type
  coercion and cast vocabulary.
- `operator`, `operator_catalog`, `function_catalog`, and `polymorphic`: the
  operator/function type catalog used by query analysis.
- `canonical_key`, `value_compare`, `distance`, `duration`, `queue_mode`,
  `vector_metadata`, and `json`: helper vocabulary used across storage, query,
  wire, and client layers.

Physical on-disk encodings still live in `reddb-io-file`; protocol framing
still lives in `reddb-io-wire`; query parsing still lives in `reddb-io-rql`.
This crate owns only the logical value/type vocabulary that those layers share.

## Compatibility posture

Changes here are cross-cutting. Adding a variant, changing serialization,
altering coercion, or renaming declared types can affect storage, wire, query,
and driver behavior at the same time. Keep changes explicit, test them in the
owning layer, and preserve re-export shims in `reddb-io-server` where callers
still rely on historical paths.

## Verification

```sh
cargo test -p reddb-io-types
cargo check -p reddb-io-types
```

## References

- [Monorepo structure](../../docs/dev/monorepo-structure.md)
- [ADR 0052 - type keystone](../../.red/adr/0052-reddb-io-types-keystone.md)

