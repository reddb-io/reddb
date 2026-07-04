<?php
/**
 * Pure SQL builders + envelope parsing — unit-testable, no I/O.
 * Mirrors `drivers/go/helpers.go`.
 */

declare(strict_types=1);

namespace Reddb\Helpers;

final class Sql
{
    private function __construct() {}

    public static function kvPath(string $collection, string $key): string
    {
        $len = strlen($collection);
        for ($i = 0; $i < $len; $i++) {
            $ch = $collection[$i];
            if (!self::isIdentChar($ch)) {
                throw new InvalidArgument(sprintf(
                    'invalid KV collection "%s": character "%s" is not supported',
                    $collection, $ch
                ));
            }
        }
        return $collection . '.' . self::kvKeySegment($key);
    }

    public static function kvKeySegment(string $value): string
    {
        if ($value !== '' && self::allIdentChars($value)) return $value;
        return "'" . str_replace("'", "''", $value) . "'";
    }

    public static function kvValueLiteral(mixed $value): string
    {
        if ($value === null) return 'NULL';
        if ($value === true) return 'true';
        if ($value === false) return 'false';
        if (is_int($value) || is_float($value)) {
            // Force . decimal separator regardless of locale.
            return is_int($value) ? (string)$value : self::floatStr($value);
        }
        if (is_string($value)) return "'" . str_replace("'", "''", $value) . "'";
        $encoded = json_encode($value, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
        if ($encoded === false) {
            throw new InvalidArgument('failed to JSON-encode value: ' . json_last_error_msg());
        }
        return "'" . str_replace("'", "''", $encoded) . "'";
    }

    public static function kvTagLiteral(string $tag): string
    {
        return "'" . str_replace("'", "''", $tag) . "'";
    }

    public static function queueValueLiteral(mixed $value): string
    {
        if ($value === null) return 'NULL';
        if ($value === true) return 'true';
        if ($value === false) return 'false';
        if (is_int($value)) return (string)$value;
        if (is_float($value)) return self::floatStr($value);
        if (is_string($value)) return "'" . str_replace("'", "''", $value) . "'";
        $encoded = json_encode($value, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
        if ($encoded === false) {
            throw new InvalidArgument('failed to JSON-encode value: ' . json_last_error_msg());
        }
        return $encoded;
    }

    public static function valueLiteral(mixed $value): string
    {
        return self::kvValueLiteral($value);
    }

    public static function jsonLiteral(mixed $value): string
    {
        $encoded = json_encode($value, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
        if ($encoded === false) {
            throw new InvalidArgument('failed to JSON-encode value: ' . json_last_error_msg());
        }
        return "'" . str_replace("'", "''", $encoded) . "'";
    }

    /**
     * ADR 0067 (#1709): a document body is written as an inline strict-JSON
     * literal (no surrounding quotes) — the quoted-string coercion is removed.
     */
    public static function jsonInlineLiteral(mixed $value): string
    {
        $encoded = json_encode($value, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
        if ($encoded === false) {
            throw new InvalidArgument('failed to JSON-encode value: ' . json_last_error_msg());
        }
        return $encoded;
    }

    public static function identifier(string $value): string
    {
        if ($value !== '' && self::allIdentChars($value)) return $value;
        return '"' . str_replace('"', '""', $value) . '"';
    }

    public static function identifierPath(string $value): string
    {
        if (!str_contains($value, '.')) return self::identifier($value);
        return implode('.', array_map([self::class, 'identifier'], explode('.', $value)));
    }

    public static function assertIdentifier(string $value, string $label): void
    {
        if ($value === '' || !self::allIdentChars($value)) {
            throw new InvalidArgument(sprintf(
                'invalid %s "%s": must match [A-Za-z0-9_]+', $label, $value
            ));
        }
    }

    public static function normalizeLimit(int $value): int
    {
        if ($value === 0) return 100;
        if ($value < 0) throw new InvalidArgument('limit must be a positive integer');
        return $value;
    }

    public static function isIdentChar(string $c): bool
    {
        return ($c >= 'a' && $c <= 'z') || ($c >= 'A' && $c <= 'Z')
            || ($c >= '0' && $c <= '9') || $c === '_';
    }

    public static function allIdentChars(string $s): bool
    {
        $len = strlen($s);
        for ($i = 0; $i < $len; $i++) {
            if (!self::isIdentChar($s[$i])) return false;
        }
        return true;
    }

    private static function floatStr(float $value): string
    {
        $s = (string) $value;
        // PHP may render with locale-specific decimal in older configs; normalise.
        return str_replace(',', '.', $s);
    }

    // --- response parsing ----------------------------------------------

    /** @return array<string,mixed>|null */
    public static function decodeBody(string $body): ?array
    {
        if ($body === '') return null;
        $obj = json_decode($body, true);
        return is_array($obj) ? $obj : null;
    }

    /** @param array<string,mixed> $obj */
    public static function affectedFromMap(array $obj): int
    {
        $v = $obj['affected'] ?? null;
        if (is_int($v)) return $v;
        if (is_float($v)) return (int) $v;
        return 0;
    }

    /** @return array{0:?array<string,mixed>,1:int} */
    public static function firstRow(string $body): array
    {
        $obj = self::decodeBody($body);
        if ($obj === null) return [null, 0];
        $affected = self::affectedFromMap($obj);
        $rows = $obj['rows'] ?? null;
        if (!is_array($rows) || count($rows) === 0) {
            $nested = $obj['result'] ?? null;
            if (is_array($nested)) {
                $rows = $nested['rows'] ?? null;
                if ($affected === 0) $affected = self::affectedFromMap($nested);
            }
        }
        if (!is_array($rows) || count($rows) === 0) return [null, $affected];
        $first = $rows[0] ?? null;
        if (!is_array($first)) return [null, $affected];
        return [$first, $affected];
    }

    /** @return list<array<string,mixed>> */
    public static function allRows(string $body): array
    {
        $obj = self::decodeBody($body);
        if ($obj === null) return [];
        $raw = $obj['rows'] ?? null;
        if (!is_array($raw)) {
            $nested = $obj['result'] ?? null;
            if (is_array($nested)) $raw = $nested['rows'] ?? null;
        }
        if (!is_array($raw)) return [];
        $out = [];
        foreach ($raw as $r) {
            if (is_array($r)) $out[] = $r;
        }
        return $out;
    }

    public static function affectedFromBody(string $body): int
    {
        $obj = self::decodeBody($body);
        if ($obj === null) return 0;
        $direct = self::affectedFromMap($obj);
        if ($direct > 0) return $direct;
        $nested = $obj['result'] ?? null;
        if (is_array($nested)) return self::affectedFromMap($nested);
        return 0;
    }

    public static function ridString(mixed $value): ?string
    {
        if (is_string($value)) return $value;
        if (is_int($value)) return (string) $value;
        if (is_float($value)) return self::floatStr($value);
        return null;
    }
}
