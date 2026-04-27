<?php
/**
 * Top-level entry point. {@see connect()} returns a {@see Conn}
 * backed by whichever transport the URL selected.
 *
 * Embedded URLs (`red:`, `red://`, `red://memory`, `red:///path`)
 * raise {@see EmbeddedUnsupported} — the PHP driver doesn't ship
 * an embedded engine. Once a FFI binding lands, this factory will
 * pick it up via the same dispatch.
 *
 *   $conn = Reddb::connect('red://localhost:5050');
 *   $rows = json_decode($conn->query('SELECT * FROM users'), true);
 *   $conn->close();
 */

declare(strict_types=1);

namespace Reddb;

use Reddb\Http\HttpClient;
use Reddb\Redwire\RedwireConn;

final class Reddb
{
    /**
     * Open a connection.
     *
     * @param string $uri  See {@see Url::parse()} for the supported shapes.
     * @param array<string,mixed> $opts See {@see Options::fromArray()} for keys.
     * @throws \InvalidArgumentException for unsupported / malformed URIs.
     * @throws RedDBException\EmbeddedUnsupported when the URI selects the embedded engine.
     */
    public static function connect(string $uri, array $opts = []): Conn
    {
        $url = Url::parse($uri);
        $url->assertNotEmbedded();
        $options = Options::fromArray($opts);
        return match ($url->kind) {
            Url::KIND_REDWIRE,
            Url::KIND_REDWIRE_TLS => RedwireConn::connect($url, $options),
            Url::KIND_HTTP,
            Url::KIND_HTTPS => HttpClient::connect($url, $options),
            default => throw new \InvalidArgumentException(
                "unhandled URL kind: '{$url->kind}'"
            ),
        };
    }
}
