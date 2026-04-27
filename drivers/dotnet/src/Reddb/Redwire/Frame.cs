using System;
using System.Buffers.Binary;

namespace Reddb.Redwire;

/// <summary>
/// RedWire frame — 16-byte little-endian header + payload.
///
/// <code>
///   u32 length          (whole frame; max 16 MiB)
///   u8  kind            (one of <see cref="Kind"/>)
///   u8  flags           (bit0 = COMPRESSED, bit1 = MORE_FRAMES)
///   u16 stream_id
///   u64 correlation_id
///   payload[length-16]
/// </code>
///
/// Static <see cref="Codec.Encode"/> / <see cref="Codec.Decode"/> are the
/// only entry points; this type is an immutable record-style holder.
/// </summary>
public sealed class Frame
{
    public const int HeaderSize = 16;
    public const int MaxFrameSize = 16 * 1024 * 1024;

    /// <summary>Bit mask of flags this driver recognises.</summary>
    public const byte KnownFlags = 0b0000_0011;

    /// <summary>Magic byte the client writes immediately before the first frame.</summary>
    public const byte Magic = 0xFE;

    /// <summary>Highest minor protocol version this driver speaks.</summary>
    public const byte SupportedVersion = 0x01;

    /// <summary>RedWire message kinds. Numeric values are part of the wire spec.</summary>
    public static class Kind
    {
        public const byte Query = 0x01;
        public const byte Result = 0x02;
        public const byte Error = 0x03;
        public const byte BulkInsert = 0x04;
        public const byte BulkOk = 0x05;
        public const byte BulkInsertBinary = 0x06;
        public const byte QueryBinary = 0x07;
        public const byte BulkInsertPrevalidated = 0x08;

        public const byte Hello = 0x10;
        public const byte HelloAck = 0x11;
        public const byte AuthRequest = 0x12;
        public const byte AuthResponse = 0x13;
        public const byte AuthOk = 0x14;
        public const byte AuthFail = 0x15;
        public const byte Bye = 0x16;
        public const byte Ping = 0x17;
        public const byte Pong = 0x18;
        public const byte Get = 0x19;
        public const byte Delete = 0x1A;
        public const byte DeleteOk = 0x1B;

        /// <summary>Pretty-print a kind byte. Falls back to hex for unknown values.</summary>
        public static string Name(byte kind) => kind switch
        {
            Query => nameof(Query),
            Result => nameof(Result),
            Error => nameof(Error),
            BulkInsert => nameof(BulkInsert),
            BulkOk => nameof(BulkOk),
            BulkInsertBinary => nameof(BulkInsertBinary),
            QueryBinary => nameof(QueryBinary),
            BulkInsertPrevalidated => nameof(BulkInsertPrevalidated),
            Hello => nameof(Hello),
            HelloAck => nameof(HelloAck),
            AuthRequest => nameof(AuthRequest),
            AuthResponse => nameof(AuthResponse),
            AuthOk => nameof(AuthOk),
            AuthFail => nameof(AuthFail),
            Bye => nameof(Bye),
            Ping => nameof(Ping),
            Pong => nameof(Pong),
            Get => nameof(Get),
            Delete => nameof(Delete),
            DeleteOk => nameof(DeleteOk),
            _ => $"0x{kind:x2}",
        };
    }

    /// <summary>Flag bits that may appear in the header.</summary>
    public static class Flags
    {
        public const byte Compressed = 0b0000_0001;
        public const byte MoreFrames = 0b0000_0010;
    }

    public byte MessageKind { get; }
    public byte FlagBits { get; }
    public ushort StreamId { get; }
    public ulong CorrelationId { get; }
    public byte[] Payload { get; }

    public Frame(byte messageKind, byte flags, ushort streamId, ulong correlationId, byte[] payload)
    {
        MessageKind = messageKind;
        FlagBits = flags;
        StreamId = streamId;
        CorrelationId = correlationId;
        Payload = payload ?? Array.Empty<byte>();
    }

    public Frame(byte messageKind, ulong correlationId, byte[] payload)
        : this(messageKind, 0, 0, correlationId, payload) { }

    public bool Compressed => (FlagBits & Flags.Compressed) != 0;

    /// <summary>Peek the total frame length from a buffer that has at least 4 bytes.</summary>
    public static int EncodedLength(ReadOnlySpan<byte> bytes)
    {
        if (bytes.Length < 4)
            throw new RedDBException.ProtocolError("not enough bytes for length prefix");
        return (int)BinaryPrimitives.ReadUInt32LittleEndian(bytes);
    }
}
