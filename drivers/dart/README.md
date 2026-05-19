# reddb (Dart driver)

Pure-Dart driver for [RedDB](https://github.com/reddb-io/reddb). Speaks
the RedWire binary protocol over TCP / TLS and the REST/HTTP transport.
Works on the Dart VM (server, CLI) and Flutter (mobile / desktop / web — HTTP
only on the browser since `dart:io` is not available there).

```dart
import 'package:reddb/reddb.dart';

Future<void> main() async {
  final db = await connect('red://localhost:5050');
  try {
    final res = await db.query('SELECT 1');
    print(res);
  } finally {
    await db.close();
  }
}
```

## URI cheat-sheet

```
red://host:5050                       # RedWire plain TCP, port 5050 default
reds://host                           # RedWire over TLS, ALPN redwire/1
red://host?proto=https                # HTTP transport (auto port 8443)
http://host:8080                      # HTTP transport
red://user:pass@host                  # auto /auth/login → bearer token
red://host?token=sk-...               # static bearer token
```

## Parameterized queries

`query` accepts an optional named `params` list. With no params it keeps
emitting the legacy `Query` frame; with params it uses the typed parameter
codec.

```dart
import 'dart:typed_data';

final rows = await db.query(
  r'SELECT * FROM users WHERE age > $1 AND name = $2 AND nick IS $3',
  params: [18, 'alice', null],
);

final embedding = Float32List.fromList([0.12, -0.45, 0.88]);
final hits = await db.query(
  r'SELECT id FROM docs SEARCH SIMILAR embedding TO $1 LIMIT 10',
  params: [embedding],
);
```

Native Dart type mapping:

| Dart type | Wire `Value` |
| --- | --- |
| `null` | `Null` |
| `bool` | `Bool` |
| `int` | `Int` (i64) |
| `double` | `Float` (f64) |
| `String` | `Text` |
| `Uint8List` | `Bytes` |
| `Float32List`, `List<double>` | `Vector` (f32 on-wire) |
| `Map`, `List`, `Value.json(...)` | `Json` (canonical) |
| `DateTime` | `Timestamp` (unix seconds) |
| `Value.uuid(...)` | `Uuid` |

RedWire parameterized queries require the server to advertise `FEATURE_PARAMS`.
Older servers raise `ParamsUnsupported`.

## Layout

```
lib/
  reddb.dart                  public API
  src/
    reddb_base.dart           connect() + Reddb facade
    conn.dart                 Conn abstract base
    options.dart              ConnectOptions / RedwireOptions
    errors.dart               typed RedDBError + subclasses
    value.dart                explicit JSON / UUID parameter wrappers
    url.dart                  parser, default port 5050
    redwire/
      frame.dart              header / encode / decode / 16 MiB cap
      codec.dart              zstd shim (dart-only fallback)
      scram.dart              RFC 5802 + PBKDF2-HMAC-SHA256
      conn.dart               Socket + SecureSocket Conn
    http/
      client.dart             HTTP transport (package:http)
    helpers.dart                rich SDK Helper namespaces
test/
  url_test.dart
  scram_test.dart
  frame_test.dart
  value_codec_test.dart
  http_client_test.dart
  redwire_conn_test.dart
  helpers_test.dart           SDK Helper Spec conformance (fake Querier)
  smoke_test.dart             gated on RED_SMOKE=1
```

## SDK Helper Spec

The driver implements **SDK Helper Spec v1.0**
([`docs/spec/sdk-helpers.md`](../../docs/spec/sdk-helpers.md)). The
constant `Helpers.helperSpecVersion` exposes the version for cross-driver
CI assertions:

```dart
assert(Helpers.helperSpecVersion == '1.0');
```

Rich namespaces hang off `Reddb.helpers`:

```dart
final db = await connect('http://localhost:8080');
final h = db.helpers;

// Documents
final res = await h.documents.insert('people', {'name': 'alice'});
final row = await h.documents.get('people', res.rid);
await h.documents.patch('people', res.rid, {'name': 'bob'});
final bulk = await h.documents.bulkInsert('events', [
  {'kind': 'login'},
  {'kind': 'logout'},
]);
final del = await h.documents.delete('people', res.rid);
assert(del.deleted);

// KV (default collection: kv_default)
await h.kv.set('characters:hansel', 'baker');
final v = await h.kv.get('characters:hansel');
final page = await h.kv.list(prefix: 'characters:');

// Queues (spec-canonical plural; `queue` is kept as a singular alias)
await h.queues.create('jobs');
await h.queues.push('jobs', {'id': 1}, priority: 5);
final n = await h.queues.len('jobs');
final next = await h.queues.pop('jobs', count: 1);

// Transactions (imperative + callback form)
final tx = h.tx();
await tx.begin();
await h.documents.insert('events', {'kind': 'commit'});
await tx.commit();

await h.tx().run((t) async {
  await h.documents.insert('events', {'kind': 'callback'});
}); // commits on success; rolls back + rethrows on error
```

### Return envelopes

| Envelope            | Required fields                                            |
|---------------------|-------------------------------------------------------------|
| `InsertResult`      | `affected` (always 1), `rid`, optional `item`               |
| `BulkInsertResult`  | `affected`, `rids` in input order                           |
| `DeleteResult`      | `affected`, `deleted` (`affected > 0`)                      |
| `ExistsResult`      | `exists`                                                    |
| `ListResult`        | `items`, optional `nextCursor`                              |

Errors are typed (`InvalidArgument`, `NotFound`, `InvalidResponse`) and
match the wording used by the Go / Python / JS drivers.

### Transaction support

Imperative `begin()`/`commit()`/`rollback()` **and** a callback form
`tx.run(body)`. Nested `tx.run` rejects with `INVALID_ARGUMENT` — callers
who need savepoints should issue them directly via `db.query`. Same
decision as the Rust and Java drivers (spec §7.2).

### Conformance matrix (spec §12)

| Case ID                              | Status |
|--------------------------------------|--------|
| `generic.query.no_params`            | ✅ |
| `generic.query_with.params`          | ✅ |
| `generic.insert.rid`                 | ✅ |
| `generic.bulk_insert.rids`           | ✅ |
| `generic.delete`                     | ✅ |
| `documents.crud_nested_patch`        | ✅ |
| `documents.delete_missing_no_error`  | ✅ |
| `documents.patch_empty_rejects`      | ✅ |
| `kv.exact_key_round_trip`            | ✅ |
| `kv.missing_get_returns_none`        | ✅ |
| `kv.delete_returns_envelope`         | ✅ |
| `queues.fifo_peek_pop_len`           | ✅ |
| `queues.empty_pop_returns_empty`     | ✅ |
| `queues.purge_resets_len`            | ✅ |
| `tx.commit_persists`                 | ✅ |
| `tx.rollback_discards`               | ✅ |
| `errors.invalid_argument.empty_sql`  | ✅ (helper-level via `patch_empty_rejects`) |
| `errors.not_found.document_get`      | ✅ |
| `wire.probabilistic.hll_round_trip`  | ✅ |
| `wire.vectors.sql_round_trip`        | reachable via `db.query` (provisional in v1.0) |
| `wire.graph.sql_round_trip`          | reachable via `db.query` (provisional in v1.0) |
| `wire.timeseries.sql_round_trip`     | reachable via `db.query` (provisional in v1.0) |

The four provisional namespaces (`vectors`, `graph`, `timeseries`,
`probabilistic.*`) have **no first-class helpers** in v1.0 — drivers
surface them via raw `db.query` per spec §1. v1.1 will lift the most
common operations into helpers without breaking v1.0 callers.

Run the conformance suite against a real `red` server:

```
RED_SMOKE=1 RED_BIN=/path/to/red \
  dart test test/conformance_test.dart
```

Skipped by default and when `RED_SKIP_SMOKE=1` is set.

## Limitations

* zstd compression on the wire path: not supported in pure Dart. If a peer
  sends a frame with the COMPRESSED flag the driver throws
  `CompressedButNoZstd`. The driver never sets that flag on outbound
  frames, so connections to a server with default config work fine.
* `red://` (embedded) URIs raise `EmbeddedUnsupported` — the engine is
  Rust-only; use one of the network transports.

## Running the tests

```
cd drivers/dart
dart pub get
dart analyze
dart test                         # unit tests only
RED_SMOKE=1 dart test test/smoke_test.dart
```

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
