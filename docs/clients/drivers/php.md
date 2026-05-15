# PHP driver

Official PHP client for RedDB. Speaks the binary `redwire/1` TCP protocol and the HTTP REST surface from a single entry point. Targets PHP 8.2+.

- **Package:** Composer: [`reddb-io/reddb`](https://packagist.org/packages/reddb-io/reddb)
- **Source:** [`drivers/php/`](https://github.com/reddb-io/reddb/tree/main/drivers/php)
- **Status:** Preview

## Install

```bash
composer require reddb-io/reddb
```

Optional: install the PECL `zstd` extension if you need to receive zstd-compressed frames. Without it the driver still talks to zstd-capable servers as long as they don't actually flip the `COMPRESSED` bit (otherwise raises `CompressedButNoZstd`).

## Quickstart

```php
<?php
require __DIR__ . '/vendor/autoload.php';

use Reddb\Reddb;

// Binary protocol (anonymous)
$conn = Reddb::connect('red://localhost:5050');
$conn->ping();
$conn->insert('users', ['name' => 'alice', 'age' => 30]);
$rows = json_decode(
    $conn->query(
        'SELECT name FROM users WHERE age = $1 AND name = $2',
        [30, 'alice'],
    ),
    true,
);
$conn->close();

// HTTPS with auto /auth/login
$conn = Reddb::connect('https://reddb.example.com:8443', [
    'username' => 'alice',
    'password' => 's3cret',
]);
```

## Connection URIs

| URI                                       | Transport      | Notes                                          |
|-------------------------------------------|----------------|------------------------------------------------|
| `red://host:5050`                         | RedWire TCP    | Default port `5050`.                           |
| `reds://host:5050`                        | RedWire + TLS  | ALPN `redwire/1` injected automatically.       |
| `http://host:8080`                        | HTTP REST      |                                                |
| `https://host:8443`                       | HTTPS REST     |                                                |
| `red://`, `red://memory`, `red:///path`   | embedded       | Throws `EmbeddedUnsupported` — remote-only.   |

`username`, `password`, `token`, and `apiKey` may ride in the URI or in the `$opts` array. The array wins on collision.

## API surface

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

`query()` and `get()` return raw JSON — pick your decoder (`json_decode`, `JsonMachine`, …).

## Safe parameter binding

`query()` accepts positional `$N` bind values in the optional `$params` array.
Use it for any user-supplied value — concatenation is a SQL-injection footgun.
The cross-driver contract is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352):

```php
// Scalar params: int / text / null
$rows = json_decode(
    $conn->query(
        'SELECT id, name FROM users WHERE id = $1 AND tenant = $2 AND deleted_at IS $3',
        [42, 'acme', null],
    ),
    true,
);

// Vector param (HNSW / IVF similarity search):
$hits = json_decode(
    $conn->query(
        'SEARCH SIMILAR $1 IN embeddings K 5',
        [[0.1, 0.2, 0.3]],
    ),
    true,
);
```

Native PHP → engine type mapping:

| PHP value | Engine value |
|-----------|--------------|
| `null` | Null |
| `bool` | Bool |
| `int` | Int |
| `float` | Float |
| `string` | Text |
| `Value::bytes($binary)` | Bytes |
| numeric array, e.g. `[0.1, 0.2]` | Vector |
| associative array, object, or `Value::json($value)` | Json |
| `DateTimeImmutable` | Timestamp |
| `Value::uuid('00112233-4455-6677-8899-aabbccddeeff')` | Uuid |

RedWire routes non-empty params through the binary `QueryWithParams` frame
when the server advertises `FEATURE_PARAMS`; older servers raise
`ParamsUnsupported` instead of silently dropping the params. HTTP forwards the
same typed values as the `/query` JSON `params` array. `query($sql)` with no
params stays byte-identical to the legacy path.

## Errors

All exceptions extend `Reddb\RedDBException`:

| Class                                       | Raised when                                                  |
|---------------------------------------------|--------------------------------------------------------------|
| `RedDBException\AuthRefused`                | Server rejected the auth handshake.                          |
| `RedDBException\ProtocolError`              | Frame malformed, JSON decode failure, etc.                   |
| `RedDBException\EngineError`                | Engine returned an error frame / HTTP 4xx-5xx.               |
| `RedDBException\FrameTooLarge`              | Frame length out of range.                                   |
| `RedDBException\UnknownFlags`               | Peer set a flag bit we don't recognise.                      |
| `RedDBException\CompressedButNoZstd`        | Compressed frame but `ext-zstd` isn't loaded.                |
| `RedDBException\EmbeddedUnsupported`        | URL selected the embedded engine.                            |
| `RedDBException\ParamsUnsupported`          | Bind values sent to a server without `FEATURE_PARAMS`.       |

## Production checklist

- Use `reds://` / `https://` outside localhost.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for mTLS / OAuth posture.
- Track credentials in [Secret Inventory](../../operations/secrets.md).

## Driver source

[`drivers/php/README.md`](https://github.com/reddb-io/reddb/blob/main/drivers/php/README.md) — smoke-test harness, build details, error glossary.
