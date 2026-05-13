<?php
/**
 * Explicit wrappers for parameter values whose PHP native shape is ambiguous.
 *
 * Plain strings bind as Text, so binary data and UUIDs use these wrappers to
 * select the corresponding engine Value tag.
 */

declare(strict_types=1);

namespace Reddb;

final class Value
{
    public const KIND_BYTES = 'bytes';
    public const KIND_JSON = 'json';
    public const KIND_TIMESTAMP = 'timestamp';
    public const KIND_UUID = 'uuid';

    private function __construct(
        public readonly string $kind,
        public readonly mixed $value,
    ) {
    }

    public static function bytes(string $bytes): self
    {
        return new self(self::KIND_BYTES, $bytes);
    }

    public static function json(mixed $value): self
    {
        return new self(self::KIND_JSON, $value);
    }

    public static function timestamp(int $seconds): self
    {
        return new self(self::KIND_TIMESTAMP, $seconds);
    }

    public static function uuid(string $uuid): self
    {
        return new self(self::KIND_UUID, $uuid);
    }
}
