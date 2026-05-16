# SDK Helper Spec v0.1

GitHub: https://github.com/reddb-io/reddb/issues/459
Status: Draft for human review

This spec is the source of truth for rich helper APIs across official RedDB
drivers. Language bindings may use idiomatic casing and async conventions, but
the semantic contract, return envelopes, error codes, and conformance scenarios
must match this document.

HTTP JSON is the universal semantic baseline. Drivers may implement helpers over
embedded stdio, RedWire, gRPC, or native embedded APIs, but each helper must be
explainable as the same request and response shape a JSON client would observe.

## Naming Rules

- Public item identity is `rid`.
- Helpers must not expose `red_entity_id`, `_entity_id`, or `entity_id` except
  in migration notes.
- Collection names and KV keys are exact strings. Drivers must not normalize
  `:`, `/`, `.`, whitespace, or Unicode.
- Return objects use stable envelope names even when the language exposes typed
  structs or classes.
- Driver READMEs should be generated from, or checked against, this spec before
  release.

## Shared Result Envelopes

| Envelope | Fields |
| --- | --- |
| `QueryResult` | `columns`, `records`, `affected`, optional `stats` |
| `InsertResult` | `affected`, `rid` |
| `BulkInsertResult` | `affected`, `rids` in input order |
| `DeleteResult` | `affected` |
| `ExistsResult` | `exists` |
| `ListResult<T>` | `items`, optional `next_cursor` |
| `QueuePushResult` | `affected`, optional `rid` |
| `QueuePopResult<T>` | `item` or `null` |
| `ProbabilisticResult` | structure-specific fields, always including the requested value |

Drivers may expose language-specific numeric types, but they must preserve all
`rid` values losslessly. JavaScript and TypeScript drivers may use `string` for
large ids when a value cannot be represented safely as `number`.

## Errors

All drivers expose a typed RedDB error with at least:

- `code`
- `message`
- optional `details`

Canonical codes:

| Code | Meaning |
| --- | --- |
| `INVALID_ARGUMENT` | Helper input is malformed or unsupported. |
| `NOT_FOUND` | Requested collection, item, key, queue item, or structure is absent. |
| `CONFLICT` | Transaction or write conflict. |
| `UNAUTHENTICATED` | Authentication is missing or invalid. |
| `PERMISSION_DENIED` | Authenticated caller lacks permission. |
| `UNAVAILABLE` | Transport, embedded binary, or server is not reachable. |
| `INTERNAL` | RedDB returned an unexpected internal failure. |

Validation errors should be raised before a request is sent when the helper can
prove the input is invalid locally. Server-side errors must preserve the server
code and message.

## Generic Helpers

### `query(sql, params?)`

Runs a RedDB query and returns `QueryResult`.

Conformance:

- Positional params preserve int, float, bool, string, null, array, and object
  values.
- Query errors reject with typed RedDB errors.
- `SELECT rid FROM ...` returns `rid`, not legacy identity aliases.

### `insert(collection, payload)`

Inserts one row-like item and returns `InsertResult`.

Conformance:

- `affected` is `1`.
- `rid` is present and lossless.
- The inserted item can be read back by `rid`.

### `bulkInsert(collection, payloads)`

Inserts many row-like items and returns `BulkInsertResult`.

Conformance:

- Empty payloads reject with `INVALID_ARGUMENT`.
- `affected` equals input length.
- `rids` length equals input length and preserves input order.
- A transport may use one native bulk request or an equivalent transaction, but
  it must not silently drop per-row identity.

### `exists(collection, rid)`

Returns `ExistsResult` for a public RedDB ID.

Conformance:

- Existing items return `true`.
- Missing items return `false`, not `NOT_FOUND`.

### `list(collection, options?)`

Lists items with optional `limit`, `cursor`, `filter`, and `order_by`.

Conformance:

- Default ordering is deterministic.
- Explicit ordering with ties uses `rid` as the stable tie-breaker.
- `limit` is bounded by the server or driver maximum and invalid limits reject.

## Document Helpers

Namespace: `documents` or idiomatic equivalent.

| Helper | Required behavior |
| --- | --- |
| `documents.insert(collection, document)` | Creates one document item and returns `InsertResult`. |
| `documents.get(collection, rid)` | Returns a document item by `rid` or `NOT_FOUND`. |
| `documents.list(collection, options?)` | Returns `ListResult<DocumentItem>`. |
| `documents.patch(collection, rid, patch)` | Applies a top-level or JSON patch update and returns the updated item when supported. |
| `documents.delete(collection, rid)` | Deletes by `rid` and returns `DeleteResult`. |

Document items expose `rid`, `collection`, `kind`, `created_at`, `updated_at`,
and document body fields. Drivers must not move user document fields into hidden
driver-only wrappers unless the language type system requires a separate body
property; if so, the raw envelope must still be accessible.

Conformance:

- Insert a nested object, get it by `rid`, and verify nested values round trip.
- Patch one field without dropping unrelated fields.
- Delete returns `affected = 1`; a second get returns `NOT_FOUND`.

## KV Helpers

Namespace: `kv`.

| Helper | Required behavior |
| --- | --- |
| `kv.set(collection, key, value)` | Stores exact key and JSON-compatible value. |
| `kv.get(collection, key)` | Returns value or `NOT_FOUND`. |
| `kv.exists(collection, key)` | Returns `ExistsResult`. |
| `kv.delete(collection, key)` | Deletes exact key and returns `DeleteResult`. |
| `kv.list(collection, options?)` | Lists exact keys and values with optional prefix. |

Conformance:

- Key `characters:hansel` round trips exactly.
- Prefix listing must not rewrite keys.
- Object values and scalar values both round trip.
- Missing key behavior is consistent across transports.

## Queue Helpers

Namespace: `queue`.

| Helper | Required behavior |
| --- | --- |
| `queue.push(queue, payload)` | Enqueues one payload and returns `QueuePushResult`. |
| `queue.pop(queue, options?)` | Removes and returns the next item or `null`. |
| `queue.peek(queue, options?)` | Returns the next item without removing it. |
| `queue.len(queue)` | Returns queue length. |
| `queue.purge(queue)` | Removes all items and returns affected count when available. |

Conformance:

- Push two items, peek first, pop first, then pop second in FIFO order.
- `peek` must not decrement length.
- Empty `pop` returns `null` or language idiomatic optional, not an exception.

## Transactions

Drivers that support transactions expose a callback helper:

```text
transaction(callback, options?)
```

Conformance:

- Successful callback commits.
- Thrown or rejected callback rolls back.
- Nested transactions either use savepoints or reject with `INVALID_ARGUMENT`;
  the README must state which behavior the driver implements.
- Transaction-scoped helpers expose the same API subset as the top-level client
  where the transport can support it.

Drivers that cannot support transactions must document the gap and fail helper
calls with `INVALID_ARGUMENT` or an explicit unsupported-feature error.

## Probabilistic Helpers

Namespace: `probabilistic` or structure-specific namespaces.

Required helpers where the server supports the structure:

| Helper | Required behavior |
| --- | --- |
| `probabilistic.hll.add(name, value)` | Adds one value to a HyperLogLog. |
| `probabilistic.hll.count(name)` | Returns cardinality as `cardinality`. |
| `probabilistic.bloom.add(name, value)` | Adds one value to a Bloom filter. |
| `probabilistic.bloom.contains(name, value)` | Returns `exists`. |
| `probabilistic.cms.add(name, value, count?)` | Adds count to a count-min sketch. |
| `probabilistic.cms.estimate(name, value)` | Returns `estimate`. |

Conformance:

- HLL `count` response includes `cardinality`.
- Bloom contains returns false before add and true after add.
- Count-min estimate is at least the inserted count.

## README and Generated Surface Checks

Each driver README should expose the same helper table:

- install and connection examples;
- helper availability matrix;
- exact return envelopes;
- transaction support statement;
- unsupported helper list with reasons;
- conformance command for that driver.

A future checker should compare README helper names against this spec. Until
that checker exists, implementation issues must update README examples manually.

## Cross-driver Conformance Manifest

Every official driver should be able to run the following scenario list against
HTTP JSON or its native transport:

1. `generic.query.params`
2. `generic.insert.rid`
3. `generic.bulk_insert.rids`
4. `generic.exists`
5. `generic.list.ordering`
6. `documents.crud_nested_patch`
7. `kv.exact_key_round_trip`
8. `queue.fifo_peek_pop_len`
9. `transactions.commit_and_rollback`
10. `probabilistic.hll_cardinality`
11. `probabilistic.bloom_contains`
12. `probabilistic.cms_estimate`
13. `errors.invalid_argument`
14. `errors.not_found`
15. `errors.transport_unavailable`

Language-specific test harnesses may wrap these cases, but case names should
stay stable so CI output can be compared across drivers.

## Human Review Gate

Before broad driver implementation, maintainers must review:

- helper names and namespace shape;
- transaction callback ergonomics;
- whether document patch semantics are JSON Patch, merge patch, or RedDB query
  patch for the first release;
- whether missing `get` returns `NOT_FOUND` or an optional for each language;
- large `rid` representation in JavaScript, PHP, Dart, and JSON-only drivers.

This spec is intentionally versioned as `v0.1` until that review is complete.
