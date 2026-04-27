<?php
/**
 * HTTP transport for the PHP driver. Mirrors the Rust / JS / Java
 * HTTP drivers: a single curl handle pool talking JSON to RedDB's
 * REST endpoints, carrying a bearer token in `Authorization`.
 *
 * Endpoint mapping:
 *   query        → POST /query
 *   insert       → POST /collections/{name}/rows
 *   bulk_insert  → POST /collections/{name}/bulk/rows
 *   get          → GET  /collections/{name}/{id}
 *   delete       → DELETE /collections/{name}/{id}
 *   ping         → GET  /admin/health
 *   auth.login   → POST /auth/login
 *
 * Auto-login: when {@see Options} carries username + password but
 * no token, the constructor performs a login round-trip and
 * remembers the resulting token for subsequent calls.
 */

declare(strict_types=1);

namespace Reddb\Http;

use Reddb\Conn;
use Reddb\Options;
use Reddb\RedDBException\AuthRefused;
use Reddb\RedDBException\EngineError;
use Reddb\RedDBException\ProtocolError;
use Reddb\Url;

final class HttpClient implements Conn
{
    private readonly string $baseUrl;
    private ?string $token;
    private readonly float $timeout;
    private bool $closed = false;
    /** @var resource|\CurlHandle */
    private $curl;

    public function __construct(string $baseUrl, ?string $token = null, float $timeout = Options::DEFAULT_TIMEOUT)
    {
        $this->baseUrl = rtrim($baseUrl, '/');
        $this->token = $token;
        $this->timeout = $timeout;
        $this->curl = curl_init();
    }

    /** Open a fresh client, optionally log in, and return a ready connection. */
    public static function connect(Url $url, Options $opts): self
    {
        if (!in_array($url->kind, [Url::KIND_HTTP, Url::KIND_HTTPS], true)) {
            throw new \InvalidArgumentException(
                "HttpClient::connect requires http:// or https://, got '{$url->kind}'"
            );
        }
        $scheme = $url->kind === Url::KIND_HTTPS ? 'https' : 'http';
        $baseUrl = "{$scheme}://{$url->host}:{$url->port}";

        $token = $opts->token ?? $url->token;
        $client = new self($baseUrl, $token, $opts->timeout);

        if ($token === null) {
            $user = $opts->username ?? $url->username;
            $pass = $opts->password ?? $url->password;
            if ($user !== null && $pass !== null) {
                $client->login($user, $pass);
            }
        }
        return $client;
    }

    /** POST /auth/login → updates this connection's bearer token. */
    public function login(string $username, string $password): void
    {
        $body = json_encode(
            ['username' => $username, 'password' => $password],
            JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR,
        );
        $resp = $this->request('POST', '/auth/login', $body, withAuth: false);
        try {
            /** @var array<string,mixed>|mixed $j */
            $j = json_decode($resp, true, 512, JSON_THROW_ON_ERROR);
        } catch (\JsonException $e) {
            throw new ProtocolError("auth/login: invalid JSON: {$e->getMessage()}");
        }
        $tok = null;
        if (is_array($j)) {
            if (isset($j['token']) && is_string($j['token'])) {
                $tok = $j['token'];
            } elseif (isset($j['result']) && is_array($j['result'])
                && isset($j['result']['token']) && is_string($j['result']['token'])
            ) {
                $tok = $j['result']['token'];
            }
        }
        if ($tok === null) {
            throw new ProtocolError("auth/login response missing 'token'");
        }
        $this->token = $tok;
    }

    public function token(): ?string
    {
        return $this->token;
    }

    // -----------------------------------------------------------------
    // Conn methods
    // -----------------------------------------------------------------

    public function query(string $sql): string
    {
        $body = json_encode(['query' => $sql], JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
        return $this->request('POST', '/query', $body);
    }

    public function insert(string $collection, array|object $payload): void
    {
        $body = json_encode($payload, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
        $this->request(
            'POST',
            '/collections/' . rawurlencode($collection) . '/rows',
            $body,
        );
    }

    public function bulkInsert(string $collection, iterable $rows): void
    {
        $list = [];
        foreach ($rows as $row) {
            $list[] = $row;
        }
        $body = json_encode(['rows' => $list], JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
        $this->request(
            'POST',
            '/collections/' . rawurlencode($collection) . '/bulk/rows',
            $body,
        );
    }

    public function get(string $collection, string $id): string
    {
        return $this->request(
            'GET',
            '/collections/' . rawurlencode($collection) . '/' . rawurlencode($id),
            null,
        );
    }

    public function delete(string $collection, string $id): void
    {
        $this->request(
            'DELETE',
            '/collections/' . rawurlencode($collection) . '/' . rawurlencode($id),
            null,
        );
    }

    public function ping(): void
    {
        $this->request('GET', '/admin/health', null);
    }

    public function close(): void
    {
        if ($this->closed) {
            return;
        }
        $this->closed = true;
        if ($this->curl instanceof \CurlHandle) {
            curl_close($this->curl);
        }
    }

    // -----------------------------------------------------------------
    // curl helpers
    // -----------------------------------------------------------------

    private function request(string $method, string $path, ?string $body, bool $withAuth = true): string
    {
        if ($this->closed) {
            throw new \LogicException('HttpClient is closed');
        }
        $ch = $this->curl;
        curl_reset($ch);
        $headers = ['Accept: application/json'];
        if ($body !== null) {
            $headers[] = 'Content-Type: application/json';
        }
        if ($withAuth && $this->token !== null) {
            $headers[] = 'Authorization: Bearer ' . $this->token;
        }
        curl_setopt_array($ch, [
            CURLOPT_URL => $this->baseUrl . $path,
            CURLOPT_CUSTOMREQUEST => $method,
            CURLOPT_HTTPHEADER => $headers,
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_FOLLOWLOCATION => false,
            CURLOPT_CONNECTTIMEOUT => max(1, (int) ceil($this->timeout)),
            CURLOPT_TIMEOUT_MS => (int) ($this->timeout * 1000),
        ]);
        if ($body !== null) {
            curl_setopt($ch, CURLOPT_POSTFIELDS, $body);
        }
        $respBody = curl_exec($ch);
        if ($respBody === false) {
            throw new ProtocolError("HTTP {$method} {$path}: " . curl_error($ch));
        }
        if (!is_string($respBody)) {
            // CURLOPT_RETURNTRANSFER guarantees a string on success.
            $respBody = (string) $respBody;
        }
        $status = curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        if ($status < 200 || $status >= 300) {
            $msg = $respBody === '' ? "HTTP {$status}" : "HTTP {$status}: {$respBody}";
            if ($status === 401 || $status === 403) {
                throw new AuthRefused("{$method} {$path}: {$msg}");
            }
            throw new EngineError("{$method} {$path}: {$msg}");
        }
        return $respBody;
    }

    public function __destruct()
    {
        // Best-effort: catch un-closed clients leaking curl handles.
        if (!$this->closed && $this->curl instanceof \CurlHandle) {
            curl_close($this->curl);
            $this->closed = true;
        }
    }
}
