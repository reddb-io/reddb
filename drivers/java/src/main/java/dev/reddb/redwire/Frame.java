package dev.reddb.redwire;

import dev.reddb.RedDBException;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;

/**
 * RedWire frame — 16-byte header + payload, all little-endian.
 *
 * <pre>
 *   u32 length          (whole frame; max 16 MiB)
 *   u8  kind            (one of {@link Kind})
 *   u8  flags           (bit0 = COMPRESSED, bit1 = MORE_FRAMES)
 *   u16 stream_id
 *   u64 correlation_id
 *   payload[length-16]
 * </pre>
 *
 * Static {@link #encode(Frame)} / {@link #decode(byte[])} helpers
 * are the only entry points; the type itself is immutable.
 */
public final class Frame {
    public static final int HEADER_SIZE = 16;
    public static final int MAX_FRAME_SIZE = 16 * 1024 * 1024;
    /** Bits we recognise — anything else trips {@link RedDBException.UnknownFlags}. */
    public static final int KNOWN_FLAGS = 0b0000_0011;

    /** Magic byte the client writes immediately before the first frame. */
    public static final byte MAGIC = (byte) 0xFE;
    /** Highest minor protocol version this driver speaks. */
    public static final byte SUPPORTED_VERSION = 0x01;

    /** RedWire message kinds. Numeric values are part of the wire spec. */
    public static final class Kind {
        public static final int Query = 0x01;
        public static final int Result = 0x02;
        public static final int Error = 0x03;
        public static final int BulkInsert = 0x04;
        public static final int BulkOk = 0x05;
        public static final int BulkInsertBinary = 0x06;
        public static final int QueryBinary = 0x07;
        public static final int BulkInsertPrevalidated = 0x08;

        public static final int Hello = 0x10;
        public static final int HelloAck = 0x11;
        public static final int AuthRequest = 0x12;
        public static final int AuthResponse = 0x13;
        public static final int AuthOk = 0x14;
        public static final int AuthFail = 0x15;
        public static final int Bye = 0x16;
        public static final int Ping = 0x17;
        public static final int Pong = 0x18;
        public static final int Get = 0x19;
        public static final int Delete = 0x1A;
        public static final int DeleteOk = 0x1B;

        private Kind() {}

        /** Pretty-print a kind byte. Falls back to hex for unknown values. */
        public static String name(int kind) {
            switch (kind) {
                case Query: return "Query";
                case Result: return "Result";
                case Error: return "Error";
                case BulkInsert: return "BulkInsert";
                case BulkOk: return "BulkOk";
                case BulkInsertBinary: return "BulkInsertBinary";
                case QueryBinary: return "QueryBinary";
                case BulkInsertPrevalidated: return "BulkInsertPrevalidated";
                case Hello: return "Hello";
                case HelloAck: return "HelloAck";
                case AuthRequest: return "AuthRequest";
                case AuthResponse: return "AuthResponse";
                case AuthOk: return "AuthOk";
                case AuthFail: return "AuthFail";
                case Bye: return "Bye";
                case Ping: return "Ping";
                case Pong: return "Pong";
                case Get: return "Get";
                case Delete: return "Delete";
                case DeleteOk: return "DeleteOk";
                default: return String.format("0x%02x", kind & 0xff);
            }
        }
    }

    /** Flag bits that may appear in the header. */
    public static final class Flags {
        public static final int COMPRESSED = 0b0000_0001;
        public static final int MORE_FRAMES = 0b0000_0010;
        private Flags() {}
    }

    public final int kind;
    public final int flags;
    public final int streamId;
    public final long correlationId;
    public final byte[] payload;

    public Frame(int kind, int flags, int streamId, long correlationId, byte[] payload) {
        this.kind = kind;
        this.flags = flags;
        this.streamId = streamId;
        this.correlationId = correlationId;
        this.payload = payload == null ? new byte[0] : payload;
    }

    public Frame(int kind, long correlationId, byte[] payload) {
        this(kind, 0, 0, correlationId, payload);
    }

    public boolean compressed() { return (flags & Flags.COMPRESSED) != 0; }

    /**
     * Encode a frame into wire bytes. Honours the COMPRESSED flag —
     * the payload field on {@code frame} is always plaintext; if the
     * flag is set the codec compresses on the wire.
     */
    public static byte[] encode(Frame frame) {
        byte[] body = frame.payload;
        int outFlags = frame.flags & KNOWN_FLAGS;
        if ((outFlags & Flags.COMPRESSED) != 0) {
            try {
                body = Codec.compress(frame.payload);
            } catch (Throwable t) {
                // Match the engine's behaviour: drop the flag and ship plaintext.
                outFlags &= ~Flags.COMPRESSED;
                body = frame.payload;
            }
        }
        int total = HEADER_SIZE + body.length;
        if (total > MAX_FRAME_SIZE) {
            throw new RedDBException.FrameTooLarge(
                "encoded frame size " + total + " exceeds MAX_FRAME_SIZE " + MAX_FRAME_SIZE);
        }
        ByteBuffer buf = ByteBuffer.allocate(total).order(ByteOrder.LITTLE_ENDIAN);
        buf.putInt(total);
        buf.put((byte) (frame.kind & 0xff));
        buf.put((byte) (outFlags & 0xff));
        buf.putShort((short) (frame.streamId & 0xffff));
        buf.putLong(frame.correlationId);
        buf.put(body);
        return buf.array();
    }

    /**
     * Decode a complete frame from the start of {@code bytes}. The buffer
     * must contain at least {@code length} bytes; trailing bytes are
     * ignored (caller can use {@link #encodedLength(byte[])} to slice).
     *
     * @return the decoded frame (payload always plaintext, flag stays set when COMPRESSED)
     */
    public static Frame decode(byte[] bytes) {
        if (bytes == null || bytes.length < HEADER_SIZE) {
            throw new RedDBException.ProtocolError(
                "frame header truncated: got " + (bytes == null ? 0 : bytes.length) + " bytes");
        }
        ByteBuffer buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN);
        int length = buf.getInt();
        if (length < HEADER_SIZE || length > MAX_FRAME_SIZE) {
            throw new RedDBException.FrameTooLarge(
                "frame length out of range: " + length);
        }
        if (bytes.length < length) {
            throw new RedDBException.ProtocolError(
                "frame payload truncated: header says " + length + " bytes, only " + bytes.length + " available");
        }
        int kind = buf.get() & 0xff;
        int flags = buf.get() & 0xff;
        if ((flags & ~KNOWN_FLAGS) != 0) {
            throw new RedDBException.UnknownFlags(
                String.format("unknown flag bits 0x%02x", flags));
        }
        int streamId = buf.getShort() & 0xffff;
        long correlationId = buf.getLong();
        int bodyLen = length - HEADER_SIZE;
        byte[] body = new byte[bodyLen];
        buf.get(body);
        if ((flags & Flags.COMPRESSED) != 0) {
            body = Codec.decompress(body);
        }
        return new Frame(kind, flags, streamId, correlationId, body);
    }

    /** Peek the total frame length from a buffer that has at least 4 bytes. */
    public static int encodedLength(byte[] bytes) {
        if (bytes == null || bytes.length < 4) {
            throw new RedDBException.ProtocolError("not enough bytes for length prefix");
        }
        return ByteBuffer.wrap(bytes, 0, 4).order(ByteOrder.LITTLE_ENDIAN).getInt();
    }
}
