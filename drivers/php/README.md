# reddb-php

Official PHP driver for [RedDB](https://github.com/reddb-io/reddb).

Speaks both the binary `redwire/1` TCP protocol and the HTTP REST
surface from a single entry point. Targets PHP 8.2 and up.

## Install

```bash
composer require reddb-io/reddb
```

Optional: install the PECL `zstd` extension to handshake with
servers that compress frames. Without it, the driver still talks to
zstd-capable servers as long as they don't actually flip on the
COMPRESSED bit (and refuses cleanly with `CompressedButNoZstd` if
they do).

## Quick start

```php
<?php
require __DIR__ . '/vendor/autoload.php';

use Reddb\Reddb;
use Reddb\Value;

// Binary protocol (anonymous)
$conn = Reddb::connect('red://localhost:5050');
$conn->ping();
$conn->insert('users', ['name' => 'alice', 'age' => 30]);
$rows = json_decode(
    $conn->query('SELECT * FROM users WHERE age = $1 AND name = $2', [30, 'alice']),
    true,
);
$conn->query(
    'INSERT INTO embeddings VECTOR (dense, content) VALUES ($1, $2)',
    [[0.7, 0.7], 'parameterized doc'],
);
$conn->close();

// HTTPS with auto-login
$conn = Reddb::connect('https://reddb.example.com:55555', [
    'username' => 'alice',
    'password' => 's3cret',
]);
```

## URI shapes

| URI | Transport | Notes |
| --- | --- | --- |
| `red://host:5050` | TCP redwire | Default port `5050`. |
| `reds://host:5050` | TLS redwire | ALPN `redwire/1` injected automatically. |
| `http://host:5000` | HTTP REST | |
| `https://host:55555` | HTTPS REST | |
| `red://`, `red://memory`, `red:///path` | embedded | Throws `EmbeddedUnsupported`. |

Username, password, token, and apiKey may be carried in the URL or
the `$opts` array; the latter wins on collision.

## API

```php
interface Reddb\Conn
{
    public function query(string $sql, array $params = []): string;
    public function insert(string $collection, array|object $payload): void;
    public function bulkInsert(string $collection, iterable $rows): void;
    public function get(string $collection, string $rid): string;
    public function delete(string $collection, string $rid): void;
    public function ping(): void;
    public function close(): void;
}
```

`query` binds positional `$N` placeholders when `$params` is non-empty. The
single-argument form is unchanged and still sends the legacy query frame.
RedWire requires the server to advertise `FEATURE_PARAMS`; HTTP forwards the
same typed values as the `/query` JSON `params` array.

| PHP value | Engine value |
| --- | --- |
| `int` | `Int` |
| `float` | `Float` |
| `bool` | `Bool` |
| `null` | `Null` |
| `string` | `Text` |
| `Value::bytes($binary)` | `Bytes` |
| `array` of numbers, e.g. `[0.1, 0.2]` | `Vector` |
| associative array, object, or `Value::json($value)` | `Json` |
| `DateTimeImmutable` | `Timestamp` |
| `Value::uuid('00112233-4455-6677-8899-aabbccddeeff')` | `Uuid` |

`query` and `get` return raw JSON — pick your own decoder
(`json_decode`, `JsonMachine`, ...).

## Rich helpers (SDK Helper Spec v1.0)

`Reddb\Helpers\Helpers` wraps a `Conn` with the canonical helper surface
defined in [`docs/spec/sdk-helpers.md`](../../docs/spec/sdk-helpers.md) —
mirroring the Rust / Go / JS / .NET / Dart drivers. The spec revision this
driver satisfies is published on the class:

```php
Reddb\Helpers\Helpers::HELPER_SPEC_VERSION; // "1.0"
```

```php
use Reddb\Helpers\Helpers;

$helpers = Helpers::for($conn);

// Generic (spec §3)
$res = $helpers->query('SELECT * FROM people WHERE active = $1', [true]);
$ins = $helpers->insert('people', ['name' => 'alice']);          // InsertResult
$bulk = $helpers->bulkInsert('people', [['n' => 1], ['n' => 2]]); // BulkInsertResult

// Documents (spec §4)
$ins = $helpers->documents()->insert('people', ['name' => 'alice']);
$row = $helpers->documents()->get('people', $ins->rid);
$helpers->documents()->patch('people', $ins->rid, ['name' => 'alyce']);
$del = $helpers->documents()->delete('people', $ins->rid);        // DeleteResult

// KV (default collection: kv_default) (spec §5)
$helpers->kv()->set('characters:hansel', 'witch'); // exact key, never normalised
$v = $helpers->kv()->get('characters:hansel');     // null when missing, not an error
$list = $helpers->kv()->list(['prefix' => 'characters:']);
$kvDel = $helpers->kv()->delete('characters:hansel'); // DeleteResult

// Queues (spec §6) — plural namespace is canonical; queue() is the alias
$helpers->queues()->create('jobs');
$helpers->queues()->push('jobs', ['id' => 1], ['priority' => 5]);
$len  = $helpers->queues()->len('jobs');
$jobs = $helpers->queues()->pop('jobs', 10);

// Transactions (spec §7) — imperative + callback
$tx = $helpers->tx();
$tx->begin();
$helpers->insert('people', ['name' => 'eve']);
$tx->commit(); // or $tx->rollback();

$helpers->tx()->run(function ($tx) use ($helpers) {
    $helpers->insert('people', ['name' => 'frank']);
    // throwing here rolls back and re-throws; returning commits
});
```

### Return envelopes

| Envelope            | Fields |
|---------------------|--------|
| `InsertResult`      | `affected` (always 1), `rid`, `item` |
| `BulkInsertResult`  | `affected`, `rids` (input order) |
| `DeleteResult`      | `affected`, `deleted` (`affected > 0`) |
| `ExistsResult`      | `exists` |
| `ListResult`        | `items`, `nextCursor` |
| `QueuePushResult`   | `affected`, `rid` |

Generic `query`, raw `tx` statements, and KV `get`/`put` return the raw JSON
envelope string (decode with `json_decode`). `documents.get` / `documents.patch`
return the row as an associative array keyed by column name (`rid`, `body`, …).

### Transaction support

**Imperative + callback.** `tx()->begin()/commit()/rollback()` drive the
session-stateful transaction; `tx()->run($fn)` commits on success and rolls
back + re-throws on a thrown callback. Nested `run` is rejected with
`INVALID_ARGUMENT` — the PHP driver does **not** open savepoints
automatically; issue `SAVEPOINT` yourself via `$tx->query('SAVEPOINT s1')`.

### Conformance matrix

`tests/ConformanceTest.php` ports the spec §12 case IDs. Provisional
namespaces (`vectors`, `graph`, `timeseries`, the rest of `probabilistic`)
have no first-class helpers in v1.0 — reach them via `$conn->query(...)` /
`$helpers->query(...)` raw SQL.

| Case ID | Status |
|---------|--------|
| `generic.query.no_params` | ✅ helper |
| `generic.query_with.params` | ✅ helper |
| `generic.insert.rid` | ✅ helper |
| `generic.bulk_insert.rids` | ✅ helper |
| `generic.delete` | ✅ helper |
| `documents.crud_nested_patch` | ✅ helper |
| `documents.delete_missing_no_error` | ✅ helper |
| `documents.patch_empty_rejects` | ✅ helper |
| `kv.exact_key_round_trip` | ✅ helper |
| `kv.missing_get_returns_none` | ✅ helper |
| `kv.delete_returns_envelope` | ✅ helper |
| `queues.fifo_peek_pop_len` | ✅ helper |
| `queues.empty_pop_returns_empty` | ✅ helper |
| `queues.purge_resets_len` | ✅ helper |
| `tx.commit_persists` | ✅ helper |
| `tx.rollback_discards` | ✅ helper |
| `errors.invalid_argument.empty_sql` | ✅ helper |
| `errors.not_found.document_get` | ✅ helper |
| `wire.probabilistic.hll_round_trip` | ✅ raw SQL |
| `wire.vectors.sql_round_trip` | raw SQL (provisional, not pinned) |
| `wire.graph.sql_round_trip` | raw SQL (provisional, not pinned) |
| `wire.timeseries.sql_round_trip` | raw SQL (provisional, not pinned) |

Run the conformance harness against a real engine:

```bash
RED_SMOKE=1 RED_BIN=/path/to/red ./vendor/bin/phpunit --filter ConformanceTest
```

It is skipped by default (and when `RED_SKIP_SMOKE=1`), so a vanilla
`phpunit` run stays offline.

### Out-of-scope helpers (v1.0)

These are reachable via raw SQL today; first-class helpers are deferred to
v1.x per the spec:

- `vectors.search` / `vectors.upsert` — provisional namespace (§8).
- `graph.shortest_path` / `graph.community` — provisional namespace (§9).
- `timeseries.write` / `timeseries.downsample` — provisional namespace (§10).
- `probabilistic.hll.*` / `probabilistic.cms.*` / Cuckoo filters — provisional (§11).
- `kv.expire` (TTL) — use `expireMs` on `kv.set` / `WITH TTL` until v1.1 (§5).
- Priority queues / consumer groups / dead-letter routing — raw `QUEUE` SQL (§6).
- Isolation-level argument on `tx.begin`, cross-shard transactions (§7).

Typed errors: `Reddb\Helpers\InvalidArgument`,
`Reddb\Helpers\NotFound`, `Reddb\Helpers\InvalidResponse`.

## Errors

All exceptions extend `Reddb\RedDBException`:

| Class | Raised when |
| --- | --- |
| `RedDBException\AuthRefused` | Server rejects the auth handshake. |
| `RedDBException\ProtocolError` | Frame malformed, JSON decode failure, etc. |
| `RedDBException\EngineError` | Engine returned an error frame / HTTP 4xx-5xx. |
| `RedDBException\FrameTooLarge` | Frame length out of range. |
| `RedDBException\UnknownFlags` | Peer set a flag bit we don't recognise. |
| `RedDBException\CompressedButNoZstd` | Compressed frame but `ext-zstd` isn't loaded. |
| `RedDBException\EmbeddedUnsupported` | URL selected the embedded engine. |
| `RedDBException\ParamsUnsupported` | Parameterized RedWire query sent to a server without `FEATURE_PARAMS`. |

## Testing

```bash
composer install
./vendor/bin/phpunit
```

The smoke test (`tests/SmokeTest.php`) is gated on `RED_SMOKE=1` and
spawns the engine binary via `cargo run`. CI uses it to catch wire
regressions; it's skipped by default so a vanilla `phpunit` run
stays under a couple of seconds.

## Production deploy

When you're ready to point this driver at a production RedDB cluster:

- **Run RedDB with the encrypted vault** so auth state and
  `red.secret.*` values are protected at rest. See
  [`docs/security/vault.md`](../../docs/security/vault.md).
- **Use Docker secrets or your cloud secret manager** to inject the
  certificate — never bake it into an image. See
  [`docs/getting-started/docker.md`](../../docs/getting-started/docker.md).
- **Track every secret** the driver consumes (bearer tokens, mTLS
  cert + key, OAuth JWTs) in
  [`docs/operations/secrets.md`](../../docs/operations/secrets.md).
- **Use `reds://` (TLS)** or `red://...?tls=true` for any traffic
  crossing the network — never plain `red://` outside localhost.
- **TLS posture, mTLS, OAuth/JWT and reverse-proxy patterns** are
  covered in [`docs/security/transport-tls.md`](../../docs/security/transport-tls.md).
- See [Policies](../../docs/security/policies.md) for IAM-style authorization.

<!-- contract-matrix:begin -->
## Public-surface support

> Generated from [`docs/conformance/public-surface-contract-matrix.json`](/docs/conformance/public-surface-contract-matrix.json) by `scripts/gen-docs-from-matrix.mjs`. Do not edit between the markers by hand — run `node scripts/gen-docs-from-matrix.mjs --write`. The matrix is the source of truth; this block can never claim more than it, and CI (`docs-matrix`) fails on drift.
>
> Driver-helper (SDK Helper Spec v1.0) support for every public promise. A helper not marked supported here is not promised by this driver.

| Promise | driver_helpers |
| --- | --- |
| **PSC-001** — RedDB is one multi-model database (tables, graph, KV, timeseries, probabilistic, vector, queue, documents) backed by a single file. | ✅ supported |
| **PSC-002** — MATCH supports node, edge, label, property, and LIMIT projections. | ✅ supported |
| **PSC-003** — GRAPH algorithms accept semantic identifiers, limits, ordering, and return stable rich rows. | ❌ unsupported |
| **PSC-004** — INSERT creates rows, documents, and native timeseries points. | ✅ supported |
| **PSC-005** — HLL/SKETCH/FILTER expose write and read commands for cardinality, frequency, and membership. | ⚠️ partial |
| **PSC-006** — Timeseries stores timestamped metrics with tags and supports query/readback. | ⚠️ partial |
| **PSC-007** — Documents are first-class: create, read, update, delete, and SQL analytics over JSON. | ✅ supported |
| **PSC-008** — KV helpers expose get/put/delete; get of a missing key returns null, delete reports affected. | ✅ supported |
| **PSC-009** — Queue helpers expose create/push/peek/pop/len/purge with FIFO semantics; empty pop is not an error. | ✅ supported |
| **PSC-010** — Transactions are imperative (begin/commit/rollback) plus a run(callback) form; empty SQL rejects with INVALID_ARGUMENT. | ✅ supported |
| **PSC-011** — SQL aggregate, projection, expression, and mutation behaviour matches ordinary SQL expectations where advertised. | ✅ supported |
| **PSC-012** — Server transports expose the same query contract as embedded (HTTP, RedWire, gRPC parity). | ✅ supported |
| **PSC-013** — Official drivers implement the SDK Helper Spec v1.0 conformance suite (all 22 §12 case IDs). | ✅ supported |
| **PSC-014** — ASK / SEARCH semantic surfaces return ranked results with stable shape. | ⚠️ partial |

_Status legend: ✅ supported · ⚠️ partial (known gaps) · ❌ unsupported._
<!-- contract-matrix:end -->
