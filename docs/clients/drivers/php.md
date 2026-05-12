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
$rows = json_decode($conn->query('SELECT * FROM users'), true);
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
    public function query(string $sql): string;
    public function insert(string $collection, array|object $payload): void;
    public function bulkInsert(string $collection, iterable $rows): void;
    public function get(string $collection, string $id): string;
    public function delete(string $collection, string $id): void;
    public function ping(): void;
    public function close(): void;
}
```

`query()` and `get()` return raw JSON — pick your decoder (`json_decode`, `JsonMachine`, …).

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

## Production checklist

- Use `reds://` / `https://` outside localhost.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for mTLS / OAuth posture.
- Track credentials in [Secret Inventory](../../operations/secrets.md).

## Driver source

[`drivers/php/README.md`](https://github.com/reddb-io/reddb/blob/main/drivers/php/README.md) — smoke-test harness, build details, error glossary.
