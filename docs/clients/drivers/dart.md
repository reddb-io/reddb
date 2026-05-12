# Dart / Flutter driver

Pure-Dart client for RedDB. Speaks the RedWire binary protocol over TCP / TLS and the REST/HTTP transport. Works on the Dart VM (server, CLI) and Flutter (mobile, desktop, web — HTTP only on browsers since `dart:io` isn't available there).

- **Package:** pub.dev: [`reddb`](https://pub.dev/packages/reddb)
- **Source:** [`drivers/dart/`](https://github.com/reddb-io/reddb/tree/main/drivers/dart)
- **Status:** Preview

## Install

```bash
dart pub add reddb
# or, in pubspec.yaml:
#   dependencies:
#     reddb: ^1.0.0
```

## Quickstart

```dart
import 'package:reddb/reddb.dart';

Future<void> main() async {
  final db = await connect('red://localhost:5050');
  try {
    final res = await db.query(
      r'SELECT * FROM users WHERE age > $1 AND name = $2',
      [18, 'alice'],
    );
    print(res);
  } finally {
    await db.close();
  }
}
```

## Safe parameter binding

`query` accepts positional `$N` bind values as an optional second argument. Use
that form for user input and vector values instead of interpolating values into
SQL strings. The parameterized-query design is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352).

```dart
import 'dart:typed_data';

final rows = await db.query(
  r'SELECT * FROM users WHERE age > $1 AND name = $2 AND nick IS $3',
  [18, 'alice', null],
);

final embedding = Float32List.fromList([0.12, -0.45, 0.88]);
final hits = await db.query(
  r'SEARCH SIMILAR $1 IN embeddings K 5',
  [embedding],
);
```

Native Dart type mapping:

| Dart type | Engine value |
| --- | --- |
| `null` | `Null` |
| `bool` | `Bool` |
| `int` | `Int` (i64) |
| `double` | `Float` (f64) |
| `String` | `Text` |
| `Uint8List` | `Bytes` |
| `Float32List`, `List<double>` | `Vector` (f32 on wire) |
| `Map`, `List`, `Value.json(...)` | `Json` (canonical) |
| `DateTime` | `Timestamp` (Unix seconds) |
| `Value.uuid(...)` | `Uuid` |

`db.query(sql)` with no params stays on the legacy single-query path. RedWire
parameterized queries require the server to advertise `FEATURE_PARAMS`; older
servers raise `ParamsUnsupported`. HTTP sends typed params through `/query`.

## Connection URIs

```
red://host:5050               # RedWire plain TCP, port 5050 default
reds://host                   # RedWire over TLS, ALPN redwire/1
red://host?proto=https        # HTTP transport (auto port 8443)
http://host:8080              # HTTP transport
red://user:pass@host          # auto /auth/login → bearer token
red://host?token=sk-...       # static bearer token
```

## Limitations

- **zstd compression** on the wire path is not supported in pure Dart. If a peer sends a frame with the `COMPRESSED` flag the driver throws `CompressedButNoZstd`. The driver never sets that flag on outbound frames, so connections to a server with default config work fine.
- **Embedded URIs** (`red://`, `red:///path`, `memory://`) raise `EmbeddedUnsupported` — the engine is Rust-only.
- **Flutter web** is HTTP-only (no `dart:io` socket access). Use `http://` / `https://` URIs there.

## Production checklist

- Use `reds://` (or `https://`) outside localhost.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for mTLS / OAuth posture.
- Track credentials in [Secret Inventory](../../operations/secrets.md).

## Driver source

[`drivers/dart/README.md`](https://github.com/reddb-io/reddb/blob/main/drivers/dart/README.md) — layout, test commands, smoke gating.
