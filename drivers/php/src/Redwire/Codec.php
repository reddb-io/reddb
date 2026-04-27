<?php
/**
 * Optional zstd compress / decompress helpers. The wrapper is
 * lazy on `ext-zstd`: if the extension isn't loaded an inbound
 * COMPRESSED frame raises {@see CompressedButNoZstd}, mirroring the
 * JS / Java drivers. Outbound frames simply skip the COMPRESSED
 * flag when zstd is unavailable, so a driver without zstd still
 * talks to a server that supports it.
 *
 * The level is read once from `RED_REDWIRE_ZSTD_LEVEL` (1..22, default 1).
 */

declare(strict_types=1);

namespace Reddb\Redwire;

use Reddb\RedDBException\CompressedButNoZstd;
use Reddb\RedDBException\ProtocolError;

final class Codec
{
    private static ?bool $available = null;

    /** True when the runtime can compress / decompress zstd payloads. */
    public static function isAvailable(): bool
    {
        if (self::$available === null) {
            self::$available = function_exists('zstd_compress') && function_exists('zstd_uncompress');
        }
        return self::$available;
    }

    /**
     * Compress a payload at the configured level. Falls back by
     * raising — the caller (Frame::encode) catches and ships
     * plaintext.
     */
    public static function compress(string $plain): string
    {
        if (!self::isAvailable()) {
            throw new CompressedButNoZstd(
                'ext-zstd not loaded — cannot encode COMPRESSED frame'
            );
        }
        $level = self::level();
        // ext-zstd: zstd_compress(string $data, int $level = 3): string|false
        /** @phpstan-ignore-next-line */
        $out = \zstd_compress($plain, $level);
        if ($out === false) {
            throw new ProtocolError('zstd_compress failed');
        }
        return $out;
    }

    /** Decompress a payload. Throws {@see CompressedButNoZstd} when ext-zstd is absent. */
    public static function decompress(string $compressed): string
    {
        if (!self::isAvailable()) {
            throw new CompressedButNoZstd(
                'incoming frame has COMPRESSED flag but ext-zstd is not loaded'
            );
        }
        // ext-zstd: zstd_uncompress(string $data): string|false
        /** @phpstan-ignore-next-line */
        $out = \zstd_uncompress($compressed);
        if ($out === false) {
            throw new ProtocolError('zstd_uncompress failed');
        }
        if (strlen($out) > Frame::MAX_FRAME_SIZE) {
            throw new ProtocolError(
                'zstd decompressed size exceeds MAX_FRAME_SIZE'
            );
        }
        return $out;
    }

    private static function level(): int
    {
        $env = getenv('RED_REDWIRE_ZSTD_LEVEL');
        if ($env === false || $env === '') {
            return 1;
        }
        $n = filter_var($env, FILTER_VALIDATE_INT);
        if ($n === false || $n < 1 || $n > 22) {
            return 1;
        }
        return $n;
    }
}
