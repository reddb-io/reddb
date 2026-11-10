<?php
declare(strict_types=1);
namespace Reddb;

use Reddb\RedDBException\ProtocolError;

/** Translation of reddb-wire::topology, checked by shared fixtures. */
final class Topology
{
    public const KIND = 0x32;
    public const FEATURE = 2;

    public static function decode(string $bytes): ?array
    {
        if (strlen($bytes) < 5) throw new ProtocolError('truncated topology');
        $length = unpack('V', substr($bytes, 1, 4))[1];
        if ($length > strlen($bytes) - 5) throw new ProtocolError('truncated topology body');
        if (ord($bytes[0]) !== 1) return null;
        $offset = 5;
        $end = 5 + $length;
        $take = static function (int $size) use ($bytes, &$offset, $end): string {
            if ($size < 0 || $size > $end - $offset) throw new ProtocolError('truncated topology field');
            $value = substr($bytes, $offset, $size);
            $offset += $size;
            return $value;
        };
        $u32 = static fn(): int => unpack('V', $take(4))[1];
        $u64 = static function () use ($take): string {
            $bytes = $take(8);
            $decimal = '0';
            for ($index = 7; $index >= 0; $index--) {
                $carry = ord($bytes[$index]); $next = '';
                for ($digit = strlen($decimal) - 1; $digit >= 0; $digit--) {
                    $value = (int)$decimal[$digit] * 256 + $carry;
                    $next = (string)($value % 10) . $next;
                    $carry = intdiv($value, 10);
                }
                $decimal = ($carry ? (string)$carry : '') . $next;
            }
            return $decimal;
        };
        $text = static function () use ($u32, $take): string {
            $size = $u32();
            if ($size > 4096) throw new ProtocolError('topology string too long');
            $text = $take($size);
            if (preg_match('//u', $text) !== 1) throw new ProtocolError('invalid topology UTF-8');
            return $text;
        };
        $epoch = $u64();
        $primary = ['addr' => $text(), 'region' => $text()];
        $count = $u32();
        if ($count > 256) throw new ProtocolError('too many topology replicas');
        $replicas = [];
        for ($index = 0; $index < $count; $index++) {
            $replicas[] = ['addr' => $text(), 'region' => $text(), 'healthy' => ord($take(1)) !== 0,
                'lag_ms' => $u32(), 'last_applied_lsn' => $u64(), 'rebootstrapping' => false];
        }
        if ($end - $offset >= $count) {
            foreach ($replicas as &$replica) $replica['rebootstrapping'] = ord($take(1)) !== 0;
            unset($replica);
        }
        return ['version' => 1, 'epoch' => $epoch, 'primary' => $primary, 'replicas' => $replicas];
    }

    public static function address(mixed $address, string $protocol): ?string
    {
        if (!is_string($address) || $address === '' || strlen($address) > 4096) return null;
        if (!str_contains($address, '://')) $address = "$protocol://$address";
        $parsed = parse_url($address);
        if ($parsed === false || ($parsed['scheme'] ?? '') !== $protocol || empty($parsed['host'])
            || isset($parsed['user']) || isset($parsed['pass']) || isset($parsed['query']) || isset($parsed['fragment'])) return null;
        return rtrim($address, '/');
    }
}
