using System;
using System.Buffers.Binary;
using System.IO;

using ZstdSharp;

namespace Reddb.Redwire;

/// <summary>
/// Encode / decode for RedWire frames. Wire layout matches the
/// engine's <c>src/wire/redwire/codec.rs</c>.
/// </summary>
public static class Codec
{
    /// <summary>
    /// Encode a frame into wire bytes. If <see cref="Frame.Flags.Compressed"/>
    /// is set on <paramref name="frame"/>, the payload is zstd-compressed
    /// at level 1 (matches the engine default; override with
    /// <c>RED_REDWIRE_ZSTD_LEVEL</c>). When zstd compression itself fails
    /// the codec falls back to plaintext (matches the Rust behaviour).
    /// </summary>
    public static byte[] Encode(Frame frame)
    {
        byte[] body = frame.Payload;
        byte outFlags = (byte)(frame.FlagBits & Frame.KnownFlags);

        if ((outFlags & Frame.Flags.Compressed) != 0)
        {
            try
            {
                int level = ParseLevel();
                using var compressor = new Compressor(level);
                body = compressor.Wrap(frame.Payload).ToArray();
            }
            catch
            {
                // Match engine: drop the flag and ship plaintext.
                outFlags &= unchecked((byte)~Frame.Flags.Compressed);
                body = frame.Payload;
            }
        }

        int total = Frame.HeaderSize + body.Length;
        if (total > Frame.MaxFrameSize)
        {
            throw new RedDBException.FrameTooLarge(
                $"encoded frame size {total} exceeds MAX_FRAME_SIZE {Frame.MaxFrameSize}");
        }

        var buf = new byte[total];
        BinaryPrimitives.WriteUInt32LittleEndian(buf.AsSpan(0, 4), (uint)total);
        buf[4] = frame.MessageKind;
        buf[5] = outFlags;
        BinaryPrimitives.WriteUInt16LittleEndian(buf.AsSpan(6, 2), frame.StreamId);
        BinaryPrimitives.WriteUInt64LittleEndian(buf.AsSpan(8, 8), frame.CorrelationId);
        Buffer.BlockCopy(body, 0, buf, Frame.HeaderSize, body.Length);
        return buf;
    }

    /// <summary>
    /// Decode a complete frame from <paramref name="bytes"/>. The buffer must
    /// hold at least <c>length</c> bytes (i.e. one full frame). Trailing
    /// bytes are ignored.
    /// </summary>
    public static Frame Decode(ReadOnlySpan<byte> bytes)
    {
        if (bytes.Length < Frame.HeaderSize)
        {
            throw new RedDBException.ProtocolError(
                $"frame header truncated: got {bytes.Length} bytes");
        }
        uint length = BinaryPrimitives.ReadUInt32LittleEndian(bytes);
        if (length < Frame.HeaderSize || length > Frame.MaxFrameSize)
        {
            throw new RedDBException.FrameTooLarge(
                $"frame length out of range: {length}");
        }
        if (bytes.Length < length)
        {
            throw new RedDBException.ProtocolError(
                $"frame payload truncated: header says {length} bytes, only {bytes.Length} available");
        }
        byte kind = bytes[4];
        byte flags = bytes[5];
        if ((flags & ~Frame.KnownFlags) != 0)
        {
            throw new RedDBException.UnknownFlags(
                $"unknown flag bits 0x{flags:x2}");
        }
        ushort streamId = BinaryPrimitives.ReadUInt16LittleEndian(bytes.Slice(6, 2));
        ulong correlationId = BinaryPrimitives.ReadUInt64LittleEndian(bytes.Slice(8, 8));
        int bodyLen = (int)length - Frame.HeaderSize;
        var body = bytes.Slice(Frame.HeaderSize, bodyLen).ToArray();

        if ((flags & Frame.Flags.Compressed) != 0)
        {
            try
            {
                using var decompressor = new Decompressor();
                body = decompressor.Unwrap(body).ToArray();
            }
            catch (Exception ex)
            {
                throw new RedDBException.ProtocolError(
                    $"zstd decompress failed: {ex.Message}", ex);
            }
        }
        return new Frame(kind, flags, streamId, correlationId, body);
    }

    private static int ParseLevel()
    {
        string? raw = Environment.GetEnvironmentVariable("RED_REDWIRE_ZSTD_LEVEL");
        if (string.IsNullOrEmpty(raw)) return 1;
        if (!int.TryParse(raw, out int level)) return 1;
        if (level < 1 || level > 22) return 1;
        return level;
    }
}
