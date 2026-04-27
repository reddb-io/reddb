package dev.reddb.redwire

import dev.reddb.RedDBException
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * RedWire frame — 16-byte header + payload, all little-endian.
 *
 * ```
 *   u32 length          (whole frame; max 16 MiB)
 *   u8  kind            (one of [MessageKind])
 *   u8  flags           (bit0 = COMPRESSED, bit1 = MORE_FRAMES)
 *   u16 stream_id
 *   u64 correlation_id
 *   payload[length-16]
 * ```
 *
 * [encode] / [decode] are the only entry points; the type itself is immutable.
 */
public class Frame(
    public val kind: Int,
    public val flags: Int,
    public val streamId: Int,
    public val correlationId: Long,
    payload: ByteArray,
) {
    public val payload: ByteArray = payload

    public constructor(kind: Int, correlationId: Long, payload: ByteArray) :
        this(kind, 0, 0, correlationId, payload)

    public fun compressed(): Boolean = (flags and Flags.COMPRESSED) != 0

    public companion object {
        public const val HEADER_SIZE: Int = 16
        public const val MAX_FRAME_SIZE: Int = 16 * 1024 * 1024

        /** Bits we recognise — anything else trips [RedDBException.UnknownFlags]. */
        public const val KNOWN_FLAGS: Int = 0b0000_0011

        /** Magic byte the client writes immediately before the first frame. */
        public const val MAGIC: Byte = 0xFE.toByte()

        /** Highest minor protocol version this driver speaks. */
        public const val SUPPORTED_VERSION: Byte = 0x01

        /**
         * Encode a frame into wire bytes. Honours the COMPRESSED flag —
         * the payload field on `frame` is always plaintext; if the
         * flag is set the codec compresses on the wire.
         */
        public fun encode(frame: Frame): ByteArray {
            var body = frame.payload
            var outFlags = frame.flags and KNOWN_FLAGS
            if ((outFlags and Flags.COMPRESSED) != 0) {
                try {
                    body = Codec.compress(frame.payload)
                } catch (t: Throwable) {
                    // Match the engine: drop the flag and ship plaintext.
                    outFlags = outFlags and Flags.COMPRESSED.inv()
                    body = frame.payload
                }
            }
            val total = HEADER_SIZE + body.size
            if (total > MAX_FRAME_SIZE) {
                throw RedDBException.FrameTooLarge(
                    "encoded frame size $total exceeds MAX_FRAME_SIZE $MAX_FRAME_SIZE"
                )
            }
            val buf = ByteBuffer.allocate(total).order(ByteOrder.LITTLE_ENDIAN)
            buf.putInt(total)
            buf.put((frame.kind and 0xff).toByte())
            buf.put((outFlags and 0xff).toByte())
            buf.putShort((frame.streamId and 0xffff).toShort())
            buf.putLong(frame.correlationId)
            buf.put(body)
            return buf.array()
        }

        /**
         * Decode a complete frame from the start of [bytes]. The buffer
         * must contain at least `length` bytes; trailing bytes are
         * ignored (caller can use [encodedLength] to slice).
         *
         * @return the decoded frame (payload always plaintext, flag stays set when COMPRESSED)
         */
        public fun decode(bytes: ByteArray?): Frame {
            if (bytes == null || bytes.size < HEADER_SIZE) {
                throw RedDBException.ProtocolError(
                    "frame header truncated: got ${bytes?.size ?: 0} bytes"
                )
            }
            val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
            val length = buf.int
            if (length < HEADER_SIZE || length > MAX_FRAME_SIZE) {
                throw RedDBException.FrameTooLarge("frame length out of range: $length")
            }
            if (bytes.size < length) {
                throw RedDBException.ProtocolError(
                    "frame payload truncated: header says $length bytes, only ${bytes.size} available"
                )
            }
            val kind = buf.get().toInt() and 0xff
            val flags = buf.get().toInt() and 0xff
            if ((flags and KNOWN_FLAGS.inv()) != 0) {
                throw RedDBException.UnknownFlags(
                    "unknown flag bits 0x%02x".format(flags)
                )
            }
            val streamId = buf.short.toInt() and 0xffff
            val correlationId = buf.long
            val bodyLen = length - HEADER_SIZE
            val body = ByteArray(bodyLen)
            buf.get(body)
            val plain = if ((flags and Flags.COMPRESSED) != 0) Codec.decompress(body) else body
            return Frame(kind, flags, streamId, correlationId, plain)
        }

        /** Peek the total frame length from a buffer that has at least 4 bytes. */
        public fun encodedLength(bytes: ByteArray?): Int {
            if (bytes == null || bytes.size < 4) {
                throw RedDBException.ProtocolError("not enough bytes for length prefix")
            }
            return ByteBuffer.wrap(bytes, 0, 4).order(ByteOrder.LITTLE_ENDIAN).int
        }
    }
}

/** RedWire message kinds. Numeric values are part of the wire spec. */
public object MessageKind {
    public const val Query: Int = 0x01
    public const val Result: Int = 0x02
    public const val Error: Int = 0x03
    public const val BulkInsert: Int = 0x04
    public const val BulkOk: Int = 0x05
    public const val BulkInsertBinary: Int = 0x06
    public const val QueryBinary: Int = 0x07
    public const val BulkInsertPrevalidated: Int = 0x08

    public const val Hello: Int = 0x10
    public const val HelloAck: Int = 0x11
    public const val AuthRequest: Int = 0x12
    public const val AuthResponse: Int = 0x13
    public const val AuthOk: Int = 0x14
    public const val AuthFail: Int = 0x15
    public const val Bye: Int = 0x16
    public const val Ping: Int = 0x17
    public const val Pong: Int = 0x18
    public const val Get: Int = 0x19
    public const val Delete: Int = 0x1A
    public const val DeleteOk: Int = 0x1B

    /** Pretty-print a kind byte. Falls back to hex for unknown values. */
    public fun name(kind: Int): String = when (kind) {
        Query -> "Query"
        Result -> "Result"
        Error -> "Error"
        BulkInsert -> "BulkInsert"
        BulkOk -> "BulkOk"
        BulkInsertBinary -> "BulkInsertBinary"
        QueryBinary -> "QueryBinary"
        BulkInsertPrevalidated -> "BulkInsertPrevalidated"
        Hello -> "Hello"
        HelloAck -> "HelloAck"
        AuthRequest -> "AuthRequest"
        AuthResponse -> "AuthResponse"
        AuthOk -> "AuthOk"
        AuthFail -> "AuthFail"
        Bye -> "Bye"
        Ping -> "Ping"
        Pong -> "Pong"
        Get -> "Get"
        Delete -> "Delete"
        DeleteOk -> "DeleteOk"
        else -> "0x%02x".format(kind and 0xff)
    }
}

/** Flag bits that may appear in the header. */
public object Flags {
    public const val COMPRESSED: Int = 0b0000_0001
    public const val MORE_FRAMES: Int = 0b0000_0010
}
