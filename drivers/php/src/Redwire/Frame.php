<?php
/**
 * RedWire frame layout — 16-byte header + payload, all
 * little-endian. Mirrors the engine-side codec
 * (`src/wire/redwire/frame.rs`) and the JS / Java drivers.
 *
 *   u32 length          (whole frame; max 16 MiB)
 *   u8  kind            (one of {@see Frame::KIND_*})
 *   u8  flags           (bit0 = COMPRESSED, bit1 = MORE_FRAMES)
 *   u16 stream_id
 *   u64 correlation_id
 *   payload[length-16]
 *
 * Encoding / decoding go through PHP's binary `pack` / `unpack`. The
 * compression bit is honoured by {@see Codec::compress()} /
 * {@see Codec::decompress()}; the payload field on the value object
 * is always plaintext.
 */

declare(strict_types=1);

namespace Reddb\Redwire;

use Reddb\RedDBException\FrameTooLarge;
use Reddb\RedDBException\ProtocolError;
use Reddb\RedDBException\UnknownFlags;

final class Frame
{
    public const HEADER_SIZE = 16;
    public const MAX_FRAME_SIZE = 16 * 1024 * 1024;
    /** Bits we recognise — anything else trips {@see UnknownFlags}. */
    public const KNOWN_FLAGS = 0b0000_0011;

    /** Magic byte the client writes immediately before the first frame. */
    public const MAGIC = 0xFE;
    /** Highest minor protocol version this driver speaks. */
    public const SUPPORTED_VERSION = 0x01;

    // --- Message kinds (numeric values are part of the wire spec) ---
    public const KIND_QUERY = 0x01;
    public const KIND_RESULT = 0x02;
    public const KIND_ERROR = 0x03;
    public const KIND_BULK_INSERT = 0x04;
    public const KIND_BULK_OK = 0x05;
    public const KIND_BULK_INSERT_BINARY = 0x06;
    public const KIND_QUERY_BINARY = 0x07;
    public const KIND_BULK_INSERT_PREVALIDATED = 0x08;
    public const KIND_HELLO = 0x10;
    public const KIND_HELLO_ACK = 0x11;
    public const KIND_AUTH_REQUEST = 0x12;
    public const KIND_AUTH_RESPONSE = 0x13;
    public const KIND_AUTH_OK = 0x14;
    public const KIND_AUTH_FAIL = 0x15;
    public const KIND_BYE = 0x16;
    public const KIND_PING = 0x17;
    public const KIND_PONG = 0x18;
    public const KIND_GET = 0x19;
    public const KIND_DELETE = 0x1A;
    public const KIND_DELETE_OK = 0x1B;

    // --- Flag bits ---
    public const FLAG_COMPRESSED = 0b0000_0001;
    public const FLAG_MORE_FRAMES = 0b0000_0010;

    public function __construct(
        public readonly int $kind,
        public readonly int $flags,
        public readonly int $streamId,
        public readonly int $correlationId,
        public readonly string $payload,
    ) {
    }

    /** Convenience constructor for the common case (no flags, stream=0). */
    public static function make(int $kind, int $correlationId, string $payload, int $flags = 0): self
    {
        return new self($kind, $flags, 0, $correlationId, $payload);
    }

    public function compressed(): bool
    {
        return ($this->flags & self::FLAG_COMPRESSED) !== 0;
    }

    /**
     * Encode a frame into wire bytes. Honours the COMPRESSED flag.
     * The {@see $payload} field is always plaintext on a Frame value
     * object; this method runs zstd if requested.
     */
    public static function encode(self $frame): string
    {
        $body = $frame->payload;
        $outFlags = $frame->flags & self::KNOWN_FLAGS;
        if (($outFlags & self::FLAG_COMPRESSED) !== 0) {
            try {
                $body = Codec::compress($frame->payload);
            } catch (\Throwable) {
                // Match the engine: drop the flag and ship plaintext.
                $outFlags &= ~self::FLAG_COMPRESSED;
                $body = $frame->payload;
            }
        }
        $bodyLen = strlen($body);
        $total = self::HEADER_SIZE + $bodyLen;
        if ($total > self::MAX_FRAME_SIZE) {
            throw new FrameTooLarge(
                "encoded frame size {$total} exceeds MAX_FRAME_SIZE " . self::MAX_FRAME_SIZE
            );
        }
        // pack: V=u32 LE, C=u8, v=u16 LE, P=u64 LE.
        $header = pack(
            'VCCvP',
            $total,
            $frame->kind & 0xff,
            $outFlags & 0xff,
            $frame->streamId & 0xffff,
            $frame->correlationId,
        );
        return $header . $body;
    }

    /**
     * Decode a complete frame from the start of {@code $bytes}. The
     * buffer must contain at least the declared length; trailing
     * bytes are ignored (use {@see encodedLength()} to slice).
     *
     * @return self plaintext payload (the COMPRESSED flag stays set on the returned object).
     */
    public static function decode(string $bytes): self
    {
        $available = strlen($bytes);
        if ($available < self::HEADER_SIZE) {
            throw new ProtocolError(
                "frame header truncated: got {$available} bytes"
            );
        }
        // unpack returns 1-indexed assoc; ask for named keys.
        $hdr = unpack('Vlength/Ckind/Cflags/vstream/Pcorr', $bytes);
        if ($hdr === false) {
            throw new ProtocolError('frame header: unpack failed');
        }
        $length = $hdr['length'];
        if ($length < self::HEADER_SIZE || $length > self::MAX_FRAME_SIZE) {
            throw new FrameTooLarge("frame length out of range: {$length}");
        }
        if ($available < $length) {
            throw new ProtocolError(
                "frame payload truncated: header says {$length} bytes, only {$available} available"
            );
        }
        $kind = $hdr['kind'];
        $flags = $hdr['flags'];
        if (($flags & ~self::KNOWN_FLAGS) !== 0) {
            throw new UnknownFlags(sprintf('unknown flag bits 0x%02x', $flags));
        }
        $streamId = $hdr['stream'];
        $corr = $hdr['corr'];
        $body = substr($bytes, self::HEADER_SIZE, $length - self::HEADER_SIZE);
        if (($flags & self::FLAG_COMPRESSED) !== 0) {
            $body = Codec::decompress($body);
        }
        return new self($kind, $flags, $streamId, $corr, $body);
    }

    /**
     * Peek the total frame length from a buffer holding at least 4
     * bytes. Used by {@see RedwireConn} to pick the next frame off
     * the read accumulator without copying.
     */
    public static function encodedLength(string $bytes): int
    {
        if (strlen($bytes) < 4) {
            throw new ProtocolError('not enough bytes for length prefix');
        }
        $u = unpack('Vlength', substr($bytes, 0, 4));
        if ($u === false) {
            throw new ProtocolError('failed to unpack length prefix');
        }
        return $u['length'];
    }

    /** Pretty-print a kind byte. Falls back to hex for unknown values. */
    public static function kindName(int $kind): string
    {
        return match ($kind) {
            self::KIND_QUERY => 'Query',
            self::KIND_RESULT => 'Result',
            self::KIND_ERROR => 'Error',
            self::KIND_BULK_INSERT => 'BulkInsert',
            self::KIND_BULK_OK => 'BulkOk',
            self::KIND_BULK_INSERT_BINARY => 'BulkInsertBinary',
            self::KIND_QUERY_BINARY => 'QueryBinary',
            self::KIND_BULK_INSERT_PREVALIDATED => 'BulkInsertPrevalidated',
            self::KIND_HELLO => 'Hello',
            self::KIND_HELLO_ACK => 'HelloAck',
            self::KIND_AUTH_REQUEST => 'AuthRequest',
            self::KIND_AUTH_RESPONSE => 'AuthResponse',
            self::KIND_AUTH_OK => 'AuthOk',
            self::KIND_AUTH_FAIL => 'AuthFail',
            self::KIND_BYE => 'Bye',
            self::KIND_PING => 'Ping',
            self::KIND_PONG => 'Pong',
            self::KIND_GET => 'Get',
            self::KIND_DELETE => 'Delete',
            self::KIND_DELETE_OK => 'DeleteOk',
            default => sprintf('0x%02x', $kind & 0xff),
        };
    }
}
