<?php
/**
 * Immutable bag of optional connection knobs. Pass to
 * {@see Reddb::connect()} as an associative array — this class does
 * the field validation centrally so the transport classes can rely
 * on the values being well-formed.
 */

declare(strict_types=1);

namespace Reddb;

final class Options
{
    /** Default request / read timeout in seconds. */
    public const DEFAULT_TIMEOUT = 30.0;

    public function __construct(
        public readonly ?string $username = null,
        public readonly ?string $password = null,
        public readonly ?string $token = null,
        public readonly ?string $apiKey = null,
        public readonly ?string $clientName = null,
        /** Read / connect timeout in seconds. */
        public readonly float $timeout = self::DEFAULT_TIMEOUT,
        /**
         * Optional SSL context options for TLS transports. Forwarded
         * to {@see stream_context_create()}; the driver injects
         * `alpn_protocols=redwire/1` automatically for redwire TLS.
         *
         * @var array<string,mixed>
         */
        public readonly array $ssl = [],
    ) {
    }

    /**
     * Build an Options instance from a loose associative array (the
     * shape `Reddb::connect()` accepts in its second argument).
     *
     * @param array<string,mixed> $opts
     */
    public static function fromArray(array $opts): self
    {
        $timeout = $opts['timeout'] ?? self::DEFAULT_TIMEOUT;
        if (!is_int($timeout) && !is_float($timeout)) {
            throw new \InvalidArgumentException("Options.timeout must be int|float seconds");
        }
        $ssl = $opts['ssl'] ?? [];
        if (!is_array($ssl)) {
            throw new \InvalidArgumentException("Options.ssl must be an array of stream context entries");
        }
        return new self(
            username: self::optStr($opts, 'username'),
            password: self::optStr($opts, 'password'),
            token: self::optStr($opts, 'token'),
            apiKey: self::optStr($opts, 'apiKey') ?? self::optStr($opts, 'api_key'),
            clientName: self::optStr($opts, 'clientName') ?? self::optStr($opts, 'client_name'),
            timeout: (float) $timeout,
            ssl: $ssl,
        );
    }

    /**
     * @param array<string,mixed> $opts
     */
    private static function optStr(array $opts, string $key): ?string
    {
        if (!array_key_exists($key, $opts)) {
            return null;
        }
        $v = $opts[$key];
        if ($v === null) {
            return null;
        }
        if (!is_string($v)) {
            throw new \InvalidArgumentException("Options.{$key} must be string|null");
        }
        return $v;
    }
}
