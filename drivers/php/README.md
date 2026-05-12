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
$conn = Reddb::connect('https://reddb.example.com:8443', [
    'username' => 'alice',
    'password' => 's3cret',
]);
```

## URI shapes

| URI | Transport | Notes |
| --- | --- | --- |
| `red://host:5050` | TCP redwire | Default port `5050`. |
| `reds://host:5050` | TLS redwire | ALPN `redwire/1` injected automatically. |
| `http://host:8080` | HTTP REST | |
| `https://host:8443` | HTTPS REST | |
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
    public function get(string $collection, string $id): string;
    public function delete(string $collection, string $id): void;
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
