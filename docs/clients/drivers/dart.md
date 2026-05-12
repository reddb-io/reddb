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
    final res = await db.query('SELECT 1');
    print(res);
  } finally {
    await db.close();
  }
}
```

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
