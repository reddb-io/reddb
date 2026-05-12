<?php
/**
 * Parameter value codec for RedWire `QueryWithParams` frames.
 *
 * Mirrors `reddb_wire::value` and the JS / Go driver codecs: every parameter
 * is encoded as one tag byte followed by either a fixed-width scalar or a
 * little-endian length prefix.
 */

declare(strict_types=1);

namespace Reddb\Redwire;

use Reddb\Value;

final class ValueCodec
{
    public const TAG_NULL = 0x00;
    public const TAG_BOOL = 0x01;
    public const TAG_INT = 0x02;
    public const TAG_FLOAT = 0x03;
    public const TAG_TEXT = 0x04;
    public const TAG_BYTES = 0x05;
    public const TAG_VECTOR = 0x06;
    public const TAG_JSON = 0x07;
    public const TAG_TIMESTAMP = 0x08;
    public const TAG_UUID = 0x09;

    public const MAX_PARAM_COUNT = 65_536;
    public const MAX_VALUE_PAYLOAD_LEN = Frame::MAX_FRAME_SIZE;

    public static function encodeQueryWithParams(string $sql, array $params): string
    {
        if (count($params) > self::MAX_PARAM_COUNT) {
            throw new \InvalidArgumentException(
                'param_count ' . count($params) . ' > ' . self::MAX_PARAM_COUNT
            );
        }
        $sqlLen = strlen($sql);
        if ($sqlLen > self::MAX_VALUE_PAYLOAD_LEN) {
            throw new \InvalidArgumentException(
                "sql_len {$sqlLen} > " . self::MAX_VALUE_PAYLOAD_LEN
            );
        }

        $out = pack('V', $sqlLen) . $sql . pack('V', count($params));
        foreach ($params as $i => $param) {
            try {
                $out .= self::encodeValue($param);
            } catch (\Throwable $e) {
                throw new \InvalidArgumentException(
                    "param[{$i}]: {$e->getMessage()}",
                    0,
                    $e,
                );
            }
        }
        return $out;
    }

    public static function encodeValue(mixed $value): string
    {
        if ($value instanceof Value) {
            return self::encodeWrapped($value);
        }
        if ($value === null) {
            return chr(self::TAG_NULL);
        }
        if (is_bool($value)) {
            return chr(self::TAG_BOOL) . chr($value ? 1 : 0);
        }
        if (is_int($value)) {
            return chr(self::TAG_INT) . self::packI64($value);
        }
        if (is_float($value)) {
            return chr(self::TAG_FLOAT) . pack('e', $value);
        }
        if (is_string($value)) {
            return self::encodeLenPrefixed(self::TAG_TEXT, $value);
        }
        if ($value instanceof \DateTimeImmutable) {
            return chr(self::TAG_TIMESTAMP) . self::packI64($value->getTimestamp());
        }
        if (is_array($value)) {
            if (self::isNumericList($value)) {
                return self::encodeVector($value);
            }
            return self::encodeLenPrefixed(self::TAG_JSON, self::canonicalJson($value));
        }
        if (is_object($value)) {
            return self::encodeLenPrefixed(self::TAG_JSON, self::canonicalJson($value));
        }

        throw new \InvalidArgumentException('unsupported param type ' . get_debug_type($value));
    }

    /** @return array<int,mixed> */
    public static function toHttpParams(array $params): array
    {
        $out = [];
        foreach ($params as $i => $param) {
            try {
                $out[] = self::toHttpParam($param);
            } catch (\Throwable $e) {
                throw new \InvalidArgumentException(
                    "param[{$i}]: {$e->getMessage()}",
                    0,
                    $e,
                );
            }
        }
        return $out;
    }

    private static function encodeWrapped(Value $value): string
    {
        return match ($value->kind) {
            Value::KIND_BYTES => self::encodeLenPrefixed(self::TAG_BYTES, self::expectString($value->value, 'bytes')),
            Value::KIND_JSON => self::encodeLenPrefixed(self::TAG_JSON, self::canonicalJson($value->value)),
            Value::KIND_UUID => chr(self::TAG_UUID) . self::parseUuid(self::expectString($value->value, 'uuid')),
            default => throw new \InvalidArgumentException("unknown Value wrapper '{$value->kind}'"),
        };
    }

    private static function toHttpParam(mixed $value): mixed
    {
        if ($value instanceof Value) {
            return match ($value->kind) {
                Value::KIND_BYTES => ['$bytes' => base64_encode(self::expectString($value->value, 'bytes'))],
                Value::KIND_JSON => self::canonicalize($value->value),
                Value::KIND_UUID => ['$uuid' => self::formatUuid(self::parseUuid(self::expectString($value->value, 'uuid')))],
                default => throw new \InvalidArgumentException("unknown Value wrapper '{$value->kind}'"),
            };
        }
        if ($value instanceof \DateTimeImmutable) {
            return ['$ts' => $value->getTimestamp()];
        }
        if (is_array($value)) {
            return self::canonicalize($value);
        }
        if (is_object($value)) {
            return self::canonicalize($value);
        }
        if ($value === null || is_bool($value) || is_int($value) || is_float($value) || is_string($value)) {
            return $value;
        }
        throw new \InvalidArgumentException('unsupported param type ' . get_debug_type($value));
    }

    private static function encodeLenPrefixed(int $tag, string $bytes): string
    {
        $len = strlen($bytes);
        if ($len > self::MAX_VALUE_PAYLOAD_LEN) {
            throw new \InvalidArgumentException(
                "value len {$len} > " . self::MAX_VALUE_PAYLOAD_LEN
            );
        }
        return chr($tag) . pack('V', $len) . $bytes;
    }

    /** @param array<int,int|float> $values */
    private static function encodeVector(array $values): string
    {
        $bytes = count($values) * 4;
        if ($bytes > self::MAX_VALUE_PAYLOAD_LEN) {
            throw new \InvalidArgumentException(
                "vector bytes {$bytes} > " . self::MAX_VALUE_PAYLOAD_LEN
            );
        }
        $out = chr(self::TAG_VECTOR) . pack('V', count($values));
        foreach ($values as $value) {
            $out .= pack('g', (float) $value);
        }
        return $out;
    }

    private static function packI64(int $value): string
    {
        $out = '';
        for ($i = 0; $i < 8; $i++) {
            $out .= chr(($value >> ($i * 8)) & 0xff);
        }
        return $out;
    }

    private static function parseUuid(string $uuid): string
    {
        $hex = str_replace('-', '', strtolower($uuid));
        if (strlen($hex) !== 32 || !ctype_xdigit($hex)) {
            throw new \InvalidArgumentException("invalid UUID '{$uuid}'");
        }
        $bytes = hex2bin($hex);
        if ($bytes === false) {
            throw new \InvalidArgumentException("invalid UUID '{$uuid}'");
        }
        return $bytes;
    }

    private static function formatUuid(string $bytes): string
    {
        $hex = bin2hex($bytes);
        return substr($hex, 0, 8) . '-'
            . substr($hex, 8, 4) . '-'
            . substr($hex, 12, 4) . '-'
            . substr($hex, 16, 4) . '-'
            . substr($hex, 20, 12);
    }

    private static function canonicalJson(mixed $value): string
    {
        return json_encode(
            self::canonicalize($value),
            JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR,
        );
    }

    private static function canonicalize(mixed $value): mixed
    {
        if ($value instanceof Value) {
            return self::toHttpParam($value);
        }
        if ($value instanceof \DateTimeImmutable) {
            return ['$ts' => $value->getTimestamp()];
        }
        if (is_array($value)) {
            if (array_is_list($value)) {
                return array_map([self::class, 'canonicalize'], $value);
            }
            ksort($value, SORT_STRING);
            $out = [];
            foreach ($value as $k => $v) {
                if (!is_string($k) && !is_int($k)) {
                    throw new \InvalidArgumentException('JSON object keys must be strings or integers');
                }
                $out[(string) $k] = self::canonicalize($v);
            }
            return $out;
        }
        if (is_object($value)) {
            return self::canonicalize(get_object_vars($value));
        }
        if ($value === null || is_bool($value) || is_int($value) || is_float($value) || is_string($value)) {
            return $value;
        }
        throw new \InvalidArgumentException('unsupported JSON param type ' . get_debug_type($value));
    }

    /** @param array<mixed> $value */
    private static function isNumericList(array $value): bool
    {
        if (!array_is_list($value)) {
            return false;
        }
        foreach ($value as $item) {
            if (!is_int($item) && !is_float($item)) {
                return false;
            }
        }
        return true;
    }

    private static function expectString(mixed $value, string $label): string
    {
        if (!is_string($value)) {
            throw new \InvalidArgumentException("{$label} value must be a string");
        }
        return $value;
    }
}
