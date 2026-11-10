<?php
/**
 * Top-level entry point. {@see connect()} returns a {@see Conn}
 * backed by whichever transport the URL selected.
 *
 * Embedded URLs (`memory://`, `file:///path`) load the shared native
 * RedDB ABI through PHP FFI.
 *
 *   $conn = Reddb::connect('red://localhost:5050');
 *   $rows = json_decode($conn->query('SELECT * FROM users'), true);
 *   $conn->close();
 */

declare(strict_types=1);

namespace Reddb;

use Reddb\Http\HttpClient;
use Reddb\Embedded\EmbeddedConn;
use Reddb\Redwire\RedwireConn;

final class Reddb
{
    /**
     * Open a connection.
     *
     * @param string $uri  See {@see Url::parse()} for the supported shapes.
     * @param array<string,mixed> $opts See {@see Options::fromArray()} for keys.
     * @throws \InvalidArgumentException for unsupported / malformed URIs.
     * @throws RedDBException\EmbeddedUnsupported when the embedded native runtime is unavailable.
     */
    public static function connect(string $uri, array $opts = []): Conn
    {
        $url = Url::parse($uri);
        $options = Options::fromArray($opts);
        if ($url->isEmbedded()) return EmbeddedConn::open($url->path);
        $protocol = match ($url->kind) {
            Url::KIND_REDWIRE => 'red', Url::KIND_REDWIRE_TLS => 'reds',
            Url::KIND_HTTP => 'http', Url::KIND_HTTPS => 'https',
        };
        $host = str_contains($url->host ?? '', ':') && !str_starts_with($url->host, '[') ? "[{$url->host}]" : $url->host;
        $seed = Topology::address("{$protocol}://{$host}:{$url->port}{$url->path}", $protocol);
        $bounded = new Options($options->username, $options->password, $options->token, $options->apiKey,
            $options->clientName, min(5.0, $options->timeout), $options->ssl);
        $dial = static function (string $address, bool $control) use ($url, $options, $bounded): Conn {
            $target = Url::parse($address);
            $clone = new Url($address, $url->kind, $target->host, $target->port, $target->path,
                $url->username, $url->password, $url->token, $url->apiKey, $url->params);
            return self::connectDirect($clone, $control ? $bounded : $options);
        };
        $router = new RoutedConn($seed, self::connectDirect($url, $options), $protocol,
            strtolower($url->params['route'] ?? '') === 'primary', $dial);
        try { $router->refresh(); return $router; }
        catch (\Throwable $error) { $router->close(); throw $error; }
    }

    private static function connectDirect(Url $url, Options $options): Conn
    {
        return match ($url->kind) {
            Url::KIND_EMBEDDED_FILE,
            Url::KIND_EMBEDDED_MEMORY => EmbeddedConn::open($url->path),
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
