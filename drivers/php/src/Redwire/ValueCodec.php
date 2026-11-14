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
    public const TAG_FINGERPRINT = 0x0A;
    public const TAG_FACE = 0x0B;
    public const TAG_IRIS = 0x0C;
    public const TAG_FEATURE_VECTOR = 0x0D;
    public const TAG_BIT_SIGNATURE = 0x0E;

    public const MAX_PARAM_COUNT = 65_536;
    public const MAX_VALUE_PAYLOAD_LEN = Frame::MAX_FRAME_SIZE;
    public const MAX_FEATURE_VECTOR_DIMENSIONS = 65_536;
    public const MAX_BIT_SIGNATURE_BYTES = 1 << 20;

    public static function requiresBiometricCapability(mixed $value): bool
    {
        return is_array($value) && (
            array_key_exists('$biometric', $value)
            || array_key_exists('$feature_vector', $value)
            || array_key_exists('$bit_signature', $value)
        );
    }

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
            if (count($value) === 1 && array_key_exists('$biometric', $value)) {
                return self::encodeBiometric($value['$biometric']);
            }
            if (count($value) === 1 && array_key_exists('$feature_vector', $value)) {
                return self::encodeFeatureVector($value['$feature_vector']);
            }
            if (count($value) === 1 && array_key_exists('$bit_signature', $value)) {
                return self::encodeBitSignature($value['$bit_signature']);
            }
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
            Value::KIND_TIMESTAMP => chr(self::TAG_TIMESTAMP) . self::packI64(self::expectInt($value->value, 'timestamp')),
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
                Value::KIND_TIMESTAMP => ['$ts' => self::expectInt($value->value, 'timestamp')],
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

    private static function encodeBiometric(mixed $value): string
    {
        if (!is_array($value)) {
            throw new \InvalidArgumentException('$biometric must be an object');
        }
        $modality = self::requiredText($value, 'modality');
        $format = self::biometricFormat(self::requiredText($value, 'format'));
        $expected = ['fingerprint' => 1, 'face' => 2, 'iris' => 3][$modality] ?? 0;
        if ($format === 0 || $expected === 0 || ($format >> 8) !== $expected) {
            throw new \InvalidArgumentException('invalid biometric modality or format');
        }
        $data = self::decodeHex(self::requiredText($value, 'data'), 'biometric data');
        if ($data === '' || strlen($data) > self::MAX_VALUE_PAYLOAD_LEN) {
            throw new \InvalidArgumentException('invalid biometric payload length');
        }
        $tag = [
            'fingerprint' => self::TAG_FINGERPRINT,
            'face' => self::TAG_FACE,
            'iris' => self::TAG_IRIS,
        ][$modality];
        return chr($tag) . pack('vV', $format, strlen($data)) . $data;
    }

    private static function encodeFeatureVector(mixed $value): string
    {
        if (!is_array($value) || !isset($value['values']) || !is_array($value['values'])) {
            throw new \InvalidArgumentException('$feature_vector must contain values');
        }
        $values = $value['values'];
        if (!array_is_list($values) || count($values) < 1 || count($values) > self::MAX_FEATURE_VECTOR_DIMENSIONS) {
            throw new \InvalidArgumentException('invalid feature vector dimension');
        }
        $space = self::parseUuid(self::requiredText($value, 'space_id'));
        $out = chr(self::TAG_FEATURE_VECTOR) . $space . pack('V', count($values));
        $norm = 0.0;
        foreach ($values as $coordinate) {
            if ((!is_int($coordinate) && !is_float($coordinate)) || !is_finite((float) $coordinate)) {
                throw new \InvalidArgumentException('feature vector coordinates must be finite numbers');
            }
            $number = (float) $coordinate;
            $norm += $number * $number;
            $out .= pack('g', $number);
        }
        if ($norm === 0.0) {
            throw new \InvalidArgumentException('feature vector norm must be non-zero');
        }
        return $out;
    }

    private static function encodeBitSignature(mixed $value): string
    {
        if (!is_array($value)) {
            throw new \InvalidArgumentException('$bit_signature must be an object');
        }
        $space = self::parseUuid(self::requiredText($value, 'space_id'));
        $bits = self::decodeHex(self::requiredText($value, 'bits'), 'bit signature bits');
        $mask = array_key_exists('mask', $value)
            ? self::decodeHex(self::requiredText($value, 'mask'), 'bit signature mask')
            : str_repeat(chr(0xff), strlen($bits));
        if ($bits === '' || strlen($bits) > self::MAX_BIT_SIGNATURE_BYTES
            || strlen($mask) !== strlen($bits) || trim($mask, "\0") === '') {
            throw new \InvalidArgumentException('invalid bit signature length or mask');
        }
        return chr(self::TAG_BIT_SIGNATURE) . $space . pack('V', strlen($bits)) . $bits . $mask;
    }

    /** @param array<string,mixed> $value */
    private static function requiredText(array $value, string $field): string
    {
        if (!isset($value[$field]) || !is_string($value[$field])) {
            throw new \InvalidArgumentException("typed parameter field '{$field}' must be text");
        }
        return $value[$field];
    }

    private static function decodeHex(string $value, string $field): string
    {
        if (strlen($value) % 2 !== 0 || ($value !== '' && !ctype_xdigit($value))) {
            throw new \InvalidArgumentException("{$field} must be even-length hexadecimal text");
        }
        $bytes = hex2bin($value);
        if ($bytes === false) {
            throw new \InvalidArgumentException("{$field} must be hexadecimal text");
        }
        return $bytes;
    }

    private static function biometricFormat(string $name): int
    {
        return match (strtolower($name)) {
            'iso/iec 39794-2' => 0x0101,
            'iso/iec 39794-4' => 0x0102,
            'iso/iec 19794-2' => 0x0103,
            'iso/iec 19794-4' => 0x0104,
            'ansi incits 378' => 0x0105,
            'ansi incits 381' => 0x0106,
            'ansi/nist-itl type-4' => 0x0107,
            'ansi/nist-itl type-9' => 0x0108,
            'cbeff fingerprint' => 0x0109,
            'iso/iec 39794-5' => 0x0201,
            'iso/iec 19794-5' => 0x0202,
            'ansi/nist-itl type-10' => 0x0203,
            'cbeff face' => 0x0204,
            'iso/iec 39794-6' => 0x0301,
            'iso/iec 19794-6' => 0x0302,
            'ansi/nist-itl type-17' => 0x0303,
            'cbeff iris' => 0x0304,
            default => 0,
        };
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

    private static function expectInt(mixed $value, string $label): int
    {
        if (!is_int($value)) {
            throw new \InvalidArgumentException("{$label} value must be an int");
        }
        return $value;
    }
}
