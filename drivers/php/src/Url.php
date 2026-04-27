<?php
/**
 * Connection-string parser. Mirrors `drivers/js/src/url.js`
 * semantics: one URL covers every transport.
 *
 * Supported shapes:
 *
 *   red://[user[:pass]@]host[:port][?...]      plain RedWire (TCP)
 *   reds://[user[:pass]@]host[:port][?...]     RedWire over TLS
 *   http://host[:port]/                        HTTP (REST)
 *   https://host[:port]/                       HTTPS
 *   red://                                     embedded in-memory   (unsupported here)
 *   red:///abs/path/file.rdb                   embedded persistent  (unsupported here)
 *   red://memory  red://:memory  red://:memory:  embedded aliases   (unsupported here)
 *
 * Default port is 5050 across schemes — matches the engine's
 * RedWire listener default.
 */

declare(strict_types=1);

namespace Reddb;

use Reddb\RedDBException\EmbeddedUnsupported;

final class Url
{
    /** Default port used for every transport. */
    public const DEFAULT_PORT = 5050;

    public const KIND_REDWIRE = 'redwire';
    public const KIND_REDWIRE_TLS = 'redwire_tls';
    public const KIND_HTTP = 'http';
    public const KIND_HTTPS = 'https';
    public const KIND_EMBEDDED_FILE = 'embedded_file';
    public const KIND_EMBEDDED_MEMORY = 'embedded_memory';

    public function __construct(
        public readonly string $original,
        public readonly string $kind,
        public readonly ?string $host = null,
        public readonly int $port = self::DEFAULT_PORT,
        public readonly ?string $path = null,
        public readonly ?string $username = null,
        public readonly ?string $password = null,
        public readonly ?string $token = null,
        public readonly ?string $apiKey = null,
        /** @var array<string,string> */
        public readonly array $params = [],
    ) {
    }

    /** True for `red://` and `reds://` — the binary protocol. */
    public function isRedwire(): bool
    {
        return $this->kind === self::KIND_REDWIRE || $this->kind === self::KIND_REDWIRE_TLS;
    }

    /** True for `reds://` or `https://`. */
    public function isTls(): bool
    {
        return $this->kind === self::KIND_REDWIRE_TLS || $this->kind === self::KIND_HTTPS;
    }

    /** True for either embedded variant. The PHP driver does not ship an embedded engine. */
    public function isEmbedded(): bool
    {
        return $this->kind === self::KIND_EMBEDDED_FILE || $this->kind === self::KIND_EMBEDDED_MEMORY;
    }

    /**
     * Parse any supported URI string.
     *
     * @throws \InvalidArgumentException for unsupported schemes / malformed inputs.
     */
    public static function parse(string $uri): self
    {
        if ($uri === '') {
            throw new \InvalidArgumentException(
                "connect requires a URI string (e.g. 'red://localhost:5050')"
            );
        }

        // Embedded shortcuts.
        $memoryAliases = [
            'red:', 'red:/', 'red://',
            'red://memory', 'red://memory/',
            'red://:memory', 'red://:memory:',
        ];
        if (in_array($uri, $memoryAliases, true)) {
            return new self(original: $uri, kind: self::KIND_EMBEDDED_MEMORY);
        }
        if (str_starts_with($uri, 'red:///')) {
            $path = substr($uri, strlen('red://')); // keeps leading '/'
            return new self(
                original: $uri,
                kind: self::KIND_EMBEDDED_FILE,
                path: $path,
            );
        }

        $scheme = self::schemeOf($uri);
        $kind = self::kindFromScheme($scheme);
        if ($kind === null) {
            throw new \InvalidArgumentException(
                "unsupported URI scheme: '{$scheme}' in '{$uri}'."
                . " Supported: red, reds, http, https"
            );
        }

        $parts = parse_url($uri);
        if ($parts === false || !isset($parts['host']) || $parts['host'] === '') {
            throw new \InvalidArgumentException("URI is missing a host: '{$uri}'");
        }

        $host = $parts['host'];
        $port = isset($parts['port']) ? (int) $parts['port'] : self::DEFAULT_PORT;
        $username = isset($parts['user']) ? rawurldecode($parts['user']) : null;
        $password = isset($parts['pass']) ? rawurldecode($parts['pass']) : null;

        $params = [];
        if (isset($parts['query']) && $parts['query'] !== '') {
            // Hand-rolled parser: parse_str() mangles keys like `api.key` → `api_key`.
            foreach (explode('&', $parts['query']) as $pair) {
                if ($pair === '') {
                    continue;
                }
                $eq = strpos($pair, '=');
                if ($eq === false) {
                    $params[rawurldecode($pair)] = '';
                } else {
                    $params[rawurldecode(substr($pair, 0, $eq))] = rawurldecode(substr($pair, $eq + 1));
                }
            }
        }
        $token = $params['token'] ?? null;
        $apiKey = $params['apiKey'] ?? ($params['api_key'] ?? null);

        $path = $parts['path'] ?? null;
        if ($path !== null && ($path === '' || $path === '/')) {
            $path = null;
        }

        return new self(
            original: $uri,
            kind: $kind,
            host: $host,
            port: $port,
            path: $path,
            username: $username,
            password: $password,
            token: $token,
            apiKey: $apiKey,
            params: $params,
        );
    }

    private static function schemeOf(string $uri): string
    {
        $colon = strpos($uri, ':');
        if ($colon === false || $colon === 0) {
            throw new \InvalidArgumentException("URI missing scheme: '{$uri}'");
        }
        return strtolower(substr($uri, 0, $colon));
    }

    private static function kindFromScheme(string $scheme): ?string
    {
        return match ($scheme) {
            'red' => self::KIND_REDWIRE,
            'reds' => self::KIND_REDWIRE_TLS,
            'http' => self::KIND_HTTP,
            'https' => self::KIND_HTTPS,
            default => null,
        };
    }

    /**
     * Throw if the URI selects an embedded engine the PHP driver
     * cannot serve. Caller can use {@see isEmbedded()} for a
     * non-throwing check.
     */
    public function assertNotEmbedded(): void
    {
        if ($this->isEmbedded()) {
            throw new EmbeddedUnsupported(
                "embedded RedDB ('{$this->original}') needs the native lib — not yet shipped in the PHP driver"
            );
        }
    }
}
