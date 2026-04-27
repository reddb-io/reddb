<?php
/**
 * RedWire client over a single stream resource.
 *
 * One {@see RedwireConn} owns one stream, a monotonic correlation
 * id, and (when constructed via {@see connect()}) a TLS context.
 * All methods are synchronous; the caller must serialise concurrent
 * use externally (PHP request lifecycle is single-threaded so this
 * is the common case).
 *
 * Auth methods supported in this cut: `anonymous` and `bearer`.
 * SCRAM helpers are exported by {@see Scram} for callers that drive
 * the handshake themselves; the engine still negotiates SCRAM via
 * `auth_methods` so a future flip-on doesn't change this surface.
 */

declare(strict_types=1);

namespace Reddb\Redwire;

use Reddb\Conn;
use Reddb\Options;
use Reddb\RedDBException\AuthRefused;
use Reddb\RedDBException\EngineError;
use Reddb\RedDBException\ProtocolError;
use Reddb\Url;

final class RedwireConn implements Conn
{
    /** @var resource */
    private $stream;
    private bool $closed = false;
    private int $nextCorrelation = 1;

    /**
     * @param resource $stream A connected, post-handshake stream resource.
     * @param array<string,mixed> $session The decoded `AuthOk` payload (session_id, role, ...).
     */
    public function __construct($stream, public readonly array $session = [])
    {
        if (!is_resource($stream)) {
            throw new \InvalidArgumentException('RedwireConn requires a stream resource');
        }
        $this->stream = $stream;
    }

    /** Open a TCP / TLS connection and run the RedWire handshake. */
    public static function connect(Url $url, Options $opts): self
    {
        if (!$url->isRedwire()) {
            throw new \InvalidArgumentException(
                "RedwireConn::connect requires red:// or reds://, got '{$url->kind}'"
            );
        }
        $host = $url->host ?? '';
        $port = $url->port;
        $address = ($url->isTls() ? 'tls://' : 'tcp://') . $host . ':' . $port;

        $contextOpts = [];
        if ($url->isTls()) {
            $contextOpts['ssl'] = array_replace([
                'alpn_protocols' => 'redwire/1',
                'verify_peer' => true,
                'verify_peer_name' => true,
                'SNI_enabled' => true,
                'peer_name' => $host,
            ], $opts->ssl);
        }
        $context = stream_context_create($contextOpts);

        $errno = 0;
        $errstr = '';
        // Connect over TCP first (no TLS yet) so we can drive the magic
        // preamble + redwire handshake. For `reds://` we do the TLS
        // upgrade right after the socket is up, before the magic byte.
        $tcpAddress = 'tcp://' . $host . ':' . $port;
        $stream = @stream_socket_client(
            $tcpAddress,
            $errno,
            $errstr,
            $opts->timeout,
            STREAM_CLIENT_CONNECT,
            $context,
        );
        if ($stream === false) {
            throw new ProtocolError("redwire connect failed: {$errstr} (errno={$errno})");
        }
        stream_set_timeout($stream, (int) max(1, $opts->timeout));
        // PHP's TCP_NODELAY toggle is not available before 8.4; the
        // engine's listener already disables Nagle on its side, so the
        // small extra latency hits only the very first round trip.

        if ($url->isTls()) {
            // alpn_protocols was set on the context above. The crypto
            // method "any client" lets PHP pick the highest version
            // both sides support — same effective behaviour as the
            // tls:// scheme.
            $ok = @stream_socket_enable_crypto(
                $stream,
                true,
                STREAM_CRYPTO_METHOD_TLS_CLIENT,
            );
            if ($ok !== true) {
                $err = error_get_last();
                fclose($stream);
                throw new ProtocolError(
                    'redwire TLS handshake failed: ' . ($err['message'] ?? 'unknown')
                );
            }
        }

        try {
            $session = self::performHandshake(
                $stream,
                username: $opts->username ?? $url->username,
                password: $opts->password ?? $url->password,
                token: $opts->token ?? $url->token,
                clientName: $opts->clientName ?? 'reddb-php/0.1',
            );
        } catch (\Throwable $e) {
            @fclose($stream);
            throw $e;
        }
        return new self($stream, $session);
    }

    /**
     * Drive the handshake on a raw stream resource. Public + static
     * so tests can run it over a {@see stream_socket_pair()} without
     * a real TCP listener.
     *
     * @param resource $stream
     * @return array<string,mixed> The decoded AuthOk payload.
     */
    public static function performHandshake(
        $stream,
        ?string $username,
        ?string $password,
        ?string $token,
        ?string $clientName,
    ): array {
        // 1. Magic preamble + minor-version byte. Both bytes ride
        // ahead of the first frame so the server can fail fast on a
        // future protocol version.
        self::writeAll($stream, chr(Frame::MAGIC) . chr(Frame::SUPPORTED_VERSION));

        // 2. Hello — advertise the methods we actually support.
        $methods = $token !== null
            ? ['bearer']
            : (($username !== null && $password !== null)
                ? ['scram-sha-256', 'bearer']
                : ['anonymous', 'bearer']);

        $helloPayload = json_encode(array_filter([
            'versions' => [Frame::SUPPORTED_VERSION],
            'auth_methods' => $methods,
            'features' => 0,
            'client_name' => $clientName,
        ], static fn ($v) => $v !== null), JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
        self::writeFrame($stream, Frame::make(Frame::KIND_HELLO, 1, $helloPayload));

        // 3. HelloAck or AuthFail.
        $ack = self::readFrame($stream);
        if ($ack->kind === Frame::KIND_AUTH_FAIL) {
            throw new AuthRefused(self::reasonOrDefault($ack->payload, 'AuthFail at HelloAck'));
        }
        if ($ack->kind !== Frame::KIND_HELLO_ACK) {
            throw new ProtocolError(
                'expected HelloAck, got ' . Frame::kindName($ack->kind)
            );
        }
        $ackJson = self::decodeJson($ack->payload, 'HelloAck');
        $chosen = is_array($ackJson) && isset($ackJson['auth']) && is_string($ackJson['auth'])
            ? $ackJson['auth']
            : null;
        if ($chosen === null) {
            throw new ProtocolError("HelloAck missing 'auth' field");
        }

        // 4. Auth dispatch.
        switch ($chosen) {
            case 'anonymous':
                self::writeFrame($stream, Frame::make(Frame::KIND_AUTH_RESPONSE, 2, ''));
                return self::finishOneRtt($stream);
            case 'bearer':
                if ($token === null) {
                    throw new AuthRefused(
                        'server demanded bearer but no token was supplied'
                    );
                }
                $body = json_encode(['token' => $token], JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
                self::writeFrame($stream, Frame::make(Frame::KIND_AUTH_RESPONSE, 2, $body));
                return self::finishOneRtt($stream);
            case 'scram-sha-256':
                if ($username === null || $password === null) {
                    throw new AuthRefused(
                        'server picked scram-sha-256 but no username/password configured'
                    );
                }
                return self::performScram($stream, $username, $password);
            default:
                throw new ProtocolError(
                    "server picked unsupported auth method: {$chosen}"
                );
        }
    }

    /** @param resource $stream */
    private static function finishOneRtt($stream): array
    {
        $f = self::readFrame($stream);
        if ($f->kind === Frame::KIND_AUTH_FAIL) {
            throw new AuthRefused(self::reasonOrDefault($f->payload, 'auth refused'));
        }
        if ($f->kind !== Frame::KIND_AUTH_OK) {
            throw new ProtocolError(
                'expected AuthOk, got ' . Frame::kindName($f->kind)
            );
        }
        $j = self::decodeJson($f->payload, 'AuthOk');
        return is_array($j) ? $j : [];
    }

    /** @param resource $stream */
    private static function performScram($stream, string $username, string $password): array
    {
        $clientNonce = Scram::newClientNonce();
        $clientFirst = Scram::clientFirst($username, $clientNonce);
        $clientFirstBare = Scram::clientFirstBare($clientFirst);

        $cf = json_encode(['client_first' => $clientFirst], JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
        self::writeFrame($stream, Frame::make(Frame::KIND_AUTH_RESPONSE, 2, $cf));

        $chall = self::readFrame($stream);
        if ($chall->kind === Frame::KIND_AUTH_FAIL) {
            throw new AuthRefused(self::reasonOrDefault($chall->payload, 'scram challenge refused'));
        }
        if ($chall->kind !== Frame::KIND_AUTH_REQUEST) {
            throw new ProtocolError(
                'scram: expected AuthRequest, got ' . Frame::kindName($chall->kind)
            );
        }
        $serverFirstStr = self::scramServerFirst($chall->payload);
        $sf = Scram::parseServerFirst($serverFirstStr, $clientNonce);

        $clientFinalNoProof = Scram::clientFinalNoProof($sf['combinedNonce']);
        $authMessage = Scram::authMessage($clientFirstBare, $sf['raw'], $clientFinalNoProof);
        $proof = Scram::clientProof($password, $sf['salt'], $sf['iter'], $authMessage);
        $clientFinal = Scram::clientFinal($sf['combinedNonce'], $proof);

        $cfin = json_encode(['client_final' => $clientFinal], JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
        self::writeFrame($stream, Frame::make(Frame::KIND_AUTH_RESPONSE, 3, $cfin));

        $ok = self::readFrame($stream);
        if ($ok->kind === Frame::KIND_AUTH_FAIL) {
            throw new AuthRefused(self::reasonOrDefault($ok->payload, 'scram refused'));
        }
        if ($ok->kind !== Frame::KIND_AUTH_OK) {
            throw new ProtocolError(
                'scram: expected AuthOk, got ' . Frame::kindName($ok->kind)
            );
        }
        $j = self::decodeJson($ok->payload, 'AuthOk');
        $session = is_array($j) ? $j : [];
        $sig = self::parseServerSignature($session);
        if ($sig !== null
            && !Scram::verifyServerSignature($password, $sf['salt'], $sf['iter'], $authMessage, $sig)
        ) {
            throw new AuthRefused('scram: server signature did not verify — possible MITM');
        }
        return $session;
    }

    /** Pull the server-first string out of an AuthRequest payload. */
    private static function scramServerFirst(string $payload): string
    {
        if ($payload !== '' && $payload[0] === '{') {
            $j = self::decodeJson($payload, 'AuthRequest');
            if (!is_array($j) || !isset($j['server_first']) || !is_string($j['server_first'])) {
                throw new ProtocolError("AuthRequest JSON missing 'server_first'");
            }
            return $j['server_first'];
        }
        return $payload;
    }

    /**
     * Engine sends server signature as base64 under "v" or hex
     * under "server_signature" — accept both.
     *
     * @param array<string,mixed> $authOk
     */
    private static function parseServerSignature(array $authOk): ?string
    {
        if (isset($authOk['v']) && is_string($authOk['v'])) {
            $b = base64_decode($authOk['v'], true);
            if ($b !== false) {
                return $b;
            }
        }
        if (isset($authOk['server_signature']) && is_string($authOk['server_signature'])) {
            $hex = $authOk['server_signature'];
            if (strlen($hex) % 2 === 0 && ctype_xdigit($hex)) {
                $bin = hex2bin($hex);
                return $bin === false ? null : $bin;
            }
        }
        return null;
    }

    // -----------------------------------------------------------------
    // Conn methods
    // -----------------------------------------------------------------

    public function query(string $sql): string
    {
        $this->ensureOpen();
        $corr = $this->nextCorr();
        self::writeFrame($this->stream, Frame::make(Frame::KIND_QUERY, $corr, $sql));
        $resp = self::readFrame($this->stream);
        $this->assertCorr($resp, $corr);
        if ($resp->kind === Frame::KIND_RESULT) {
            return $resp->payload;
        }
        if ($resp->kind === Frame::KIND_ERROR) {
            throw new EngineError($resp->payload);
        }
        throw new ProtocolError(
            'expected Result/Error, got ' . Frame::kindName($resp->kind)
        );
    }

    public function insert(string $collection, array|object $payload): void
    {
        $this->sendInsert([
            'collection' => $collection,
            'payload' => $payload,
        ]);
    }

    public function bulkInsert(string $collection, iterable $rows): void
    {
        $list = [];
        foreach ($rows as $row) {
            $list[] = $row;
        }
        $this->sendInsert([
            'collection' => $collection,
            'payloads' => $list,
        ]);
    }

    public function get(string $collection, string $id): string
    {
        $this->ensureOpen();
        $corr = $this->nextCorr();
        $body = json_encode(
            ['collection' => $collection, 'id' => $id],
            JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR,
        );
        self::writeFrame($this->stream, Frame::make(Frame::KIND_GET, $corr, $body));
        $resp = self::readFrame($this->stream);
        $this->assertCorr($resp, $corr);
        if ($resp->kind === Frame::KIND_RESULT) {
            return $resp->payload;
        }
        if ($resp->kind === Frame::KIND_ERROR) {
            throw new EngineError($resp->payload);
        }
        throw new ProtocolError(
            'expected Result/Error, got ' . Frame::kindName($resp->kind)
        );
    }

    public function delete(string $collection, string $id): void
    {
        $this->ensureOpen();
        $corr = $this->nextCorr();
        $body = json_encode(
            ['collection' => $collection, 'id' => $id],
            JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR,
        );
        self::writeFrame($this->stream, Frame::make(Frame::KIND_DELETE, $corr, $body));
        $resp = self::readFrame($this->stream);
        $this->assertCorr($resp, $corr);
        if ($resp->kind === Frame::KIND_DELETE_OK) {
            return;
        }
        if ($resp->kind === Frame::KIND_ERROR) {
            throw new EngineError($resp->payload);
        }
        throw new ProtocolError(
            'expected DeleteOk/Error, got ' . Frame::kindName($resp->kind)
        );
    }

    public function ping(): void
    {
        $this->ensureOpen();
        $corr = $this->nextCorr();
        self::writeFrame($this->stream, Frame::make(Frame::KIND_PING, $corr, ''));
        $resp = self::readFrame($this->stream);
        $this->assertCorr($resp, $corr);
        if ($resp->kind !== Frame::KIND_PONG) {
            throw new ProtocolError(
                'expected Pong, got ' . Frame::kindName($resp->kind)
            );
        }
    }

    public function close(): void
    {
        if ($this->closed) {
            return;
        }
        $this->closed = true;
        try {
            $corr = $this->nextCorr();
            self::writeFrame($this->stream, Frame::make(Frame::KIND_BYE, $corr, ''));
        } catch (\Throwable) {
            // best-effort
        }
        @fclose($this->stream);
    }

    /** @param array<string,mixed> $body */
    private function sendInsert(array $body): void
    {
        $this->ensureOpen();
        $corr = $this->nextCorr();
        $bytes = json_encode($body, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
        self::writeFrame($this->stream, Frame::make(Frame::KIND_BULK_INSERT, $corr, $bytes));
        $resp = self::readFrame($this->stream);
        $this->assertCorr($resp, $corr);
        if ($resp->kind === Frame::KIND_BULK_OK) {
            return;
        }
        if ($resp->kind === Frame::KIND_ERROR) {
            throw new EngineError($resp->payload);
        }
        throw new ProtocolError(
            'expected BulkOk/Error, got ' . Frame::kindName($resp->kind)
        );
    }

    private function assertCorr(Frame $resp, int $expected): void
    {
        // Engine echoes back the same correlation id; mismatch means
        // we lost frame sync — fail loud rather than handing the
        // caller a stale result.
        if ($resp->correlationId !== $expected) {
            throw new ProtocolError(
                "correlation id mismatch: expected {$expected}, got {$resp->correlationId}"
            );
        }
    }

    private function ensureOpen(): void
    {
        if ($this->closed) {
            throw new \LogicException('RedwireConn is closed');
        }
    }

    private function nextCorr(): int
    {
        return $this->nextCorrelation++;
    }

    // -----------------------------------------------------------------
    // Stream helpers
    // -----------------------------------------------------------------

    /** @param resource $stream */
    public static function writeFrame($stream, Frame $frame): void
    {
        self::writeAll($stream, Frame::encode($frame));
    }

    /**
     * @param resource $stream
     */
    public static function readFrame($stream): Frame
    {
        $header = self::readExactly($stream, Frame::HEADER_SIZE);
        $length = Frame::encodedLength($header);
        if ($length < Frame::HEADER_SIZE || $length > Frame::MAX_FRAME_SIZE) {
            throw new \Reddb\RedDBException\FrameTooLarge(
                "frame length out of range: {$length}"
            );
        }
        $rest = $length === Frame::HEADER_SIZE
            ? ''
            : self::readExactly($stream, $length - Frame::HEADER_SIZE);
        return Frame::decode($header . $rest);
    }

    /** @param resource $stream */
    private static function writeAll($stream, string $bytes): void
    {
        $remaining = strlen($bytes);
        $offset = 0;
        while ($remaining > 0) {
            $chunk = substr($bytes, $offset, $remaining);
            $n = @fwrite($stream, $chunk, $remaining);
            if ($n === false || $n === 0) {
                $meta = stream_get_meta_data($stream);
                if (!empty($meta['timed_out'])) {
                    throw new ProtocolError('redwire: write timed out');
                }
                throw new ProtocolError('redwire: write failed (peer closed?)');
            }
            $offset += $n;
            $remaining -= $n;
        }
    }

    /** @param resource $stream */
    private static function readExactly($stream, int $n): string
    {
        $buf = '';
        $remaining = $n;
        while ($remaining > 0) {
            $chunk = @fread($stream, $remaining);
            if ($chunk === false || $chunk === '') {
                $meta = stream_get_meta_data($stream);
                if (!empty($meta['timed_out'])) {
                    throw new ProtocolError('redwire: read timed out');
                }
                if (!empty($meta['eof'])) {
                    throw new ProtocolError('redwire: connection closed by peer');
                }
                throw new ProtocolError('redwire: read failed');
            }
            $buf .= $chunk;
            $remaining -= strlen($chunk);
        }
        return $buf;
    }

    private static function reasonOrDefault(string $payload, string $fallback): string
    {
        if ($payload === '') {
            return $fallback;
        }
        try {
            $j = json_decode($payload, true, 512, JSON_THROW_ON_ERROR);
        } catch (\JsonException) {
            return $payload;
        }
        if (is_array($j) && isset($j['reason']) && is_string($j['reason'])) {
            return $j['reason'];
        }
        return $payload;
    }

    /**
     * @return array<int|string,mixed>|null
     */
    private static function decodeJson(string $payload, string $label): ?array
    {
        if ($payload === '') {
            return null;
        }
        try {
            /** @var array<int|string,mixed>|mixed $v */
            $v = json_decode($payload, true, 512, JSON_THROW_ON_ERROR);
        } catch (\JsonException $e) {
            throw new ProtocolError("{$label}: invalid JSON: {$e->getMessage()}");
        }
        if (!is_array($v)) {
            throw new ProtocolError("{$label}: expected JSON object");
        }
        return $v;
    }
}
