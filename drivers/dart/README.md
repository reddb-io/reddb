# reddb (Dart driver)

Pure-Dart driver for [RedDB](https://github.com/filipeforattini/reddb). Speaks
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

## Layout

```
lib/
  reddb.dart                  public API
  src/
    reddb_base.dart           connect() + Reddb facade
    conn.dart                 Conn abstract base
    options.dart              ConnectOptions / RedwireOptions
    errors.dart               typed RedDBError + subclasses
    url.dart                  parser, default port 5050
    redwire/
      frame.dart              header / encode / decode / 16 MiB cap
      codec.dart              zstd shim (dart-only fallback)
      scram.dart              RFC 5802 + PBKDF2-HMAC-SHA256
      conn.dart               Socket + SecureSocket Conn
    http/
      client.dart             HTTP transport (package:http)
test/
  url_test.dart
  scram_test.dart
  frame_test.dart
  redwire_conn_test.dart
  smoke_test.dart             gated on RED_SMOKE=1
```

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
