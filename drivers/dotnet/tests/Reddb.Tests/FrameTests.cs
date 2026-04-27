using System;
using System.Buffers.Binary;
using System.Linq;
using System.Text;
using Reddb;
using Reddb.Redwire;
using Xunit;

namespace Reddb.Tests;

public class FrameTests
{
    [Fact]
    public void RoundTrip_EmptyPayload()
    {
        var f = new Frame(Frame.Kind.Ping, 0, 0, 1UL, Array.Empty<byte>());
        var bytes = Codec.Encode(f);
        Assert.Equal(Frame.HeaderSize, bytes.Length);
        var back = Codec.Decode(bytes);
        Assert.Equal(f.MessageKind, back.MessageKind);
        Assert.Equal(f.StreamId, back.StreamId);
        Assert.Equal(f.CorrelationId, back.CorrelationId);
        Assert.Empty(back.Payload);
    }

    [Fact]
    public void RoundTrip_WithPayloadAndStream()
    {
        var body = Encoding.UTF8.GetBytes("SELECT 1");
        var f = new Frame(Frame.Kind.Query, 0, 7, 42UL, body);
        var back = Codec.Decode(Codec.Encode(f));
        Assert.Equal(Frame.Kind.Query, back.MessageKind);
        Assert.Equal((ushort)7, back.StreamId);
        Assert.Equal(42UL, back.CorrelationId);
        Assert.Equal(body, back.Payload);
    }

    [Fact]
    public void Encode_WritesLittleEndianHeader()
    {
        byte[] body = { 1, 2, 3 };
        var f = new Frame(Frame.Kind.Query, 0, 0, 0x0102030405060708UL, body);
        var bytes = Codec.Encode(f);
        Assert.Equal(Frame.HeaderSize + body.Length, BinaryPrimitives.ReadInt32LittleEndian(bytes));
        Assert.Equal(Frame.Kind.Query, bytes[4]);
        Assert.Equal(0, bytes[5]);
        Assert.Equal((ushort)0, BinaryPrimitives.ReadUInt16LittleEndian(bytes.AsSpan(6, 2)));
        Assert.Equal(0x0102030405060708UL, BinaryPrimitives.ReadUInt64LittleEndian(bytes.AsSpan(8, 8)));
    }

    [Fact]
    public void Decode_RejectsTruncatedHeader()
    {
        Assert.Throws<RedDBException.ProtocolError>(() => Codec.Decode(new byte[5]));
        Assert.Throws<RedDBException.ProtocolError>(() => Codec.Decode(Array.Empty<byte>()));
    }

    [Fact]
    public void Decode_RejectsLengthBelowHeader()
    {
        var bytes = new byte[Frame.HeaderSize];
        BinaryPrimitives.WriteUInt32LittleEndian(bytes, 15);
        Assert.Throws<RedDBException.FrameTooLarge>(() => Codec.Decode(bytes));
    }

    [Fact]
    public void Decode_RejectsLengthAboveMax()
    {
        var bytes = new byte[Frame.HeaderSize];
        BinaryPrimitives.WriteUInt32LittleEndian(bytes, (uint)(Frame.MaxFrameSize + 1));
        Assert.Throws<RedDBException.FrameTooLarge>(() => Codec.Decode(bytes));
    }

    [Fact]
    public void Decode_RejectsUnknownFlagBits()
    {
        var bytes = new byte[Frame.HeaderSize];
        BinaryPrimitives.WriteUInt32LittleEndian(bytes, Frame.HeaderSize);
        bytes[4] = Frame.Kind.Ping;
        bytes[5] = 0b1000_0000;
        Assert.Throws<RedDBException.UnknownFlags>(() => Codec.Decode(bytes));
    }

    [Fact]
    public void Decode_RejectsTruncatedPayload()
    {
        // length says 32, only supply 20 bytes.
        var bytes = new byte[20];
        BinaryPrimitives.WriteUInt32LittleEndian(bytes, 32);
        bytes[4] = Frame.Kind.Query;
        Assert.Throws<RedDBException.ProtocolError>(() => Codec.Decode(bytes));
    }

    [Fact]
    public void Encode_RefusesPayloadAboveMax()
    {
        var huge = new byte[Frame.MaxFrameSize]; // header + huge > max
        var f = new Frame(Frame.Kind.Query, 1UL, huge);
        Assert.Throws<RedDBException.FrameTooLarge>(() => Codec.Encode(f));
    }

    [Fact]
    public void EncodedLength_ReadsLengthPrefix()
    {
        var f = new Frame(Frame.Kind.Result, 0, 0, 5UL, new byte[] { 1, 2, 3 });
        var bytes = Codec.Encode(f);
        Assert.Equal(bytes.Length, Frame.EncodedLength(bytes));
    }

    [Fact]
    public void Compressed_RoundTripRecoversPlaintext()
    {
        // Highly compressible payload — `abc` × 100.
        var plain = Enumerable.Range(0, 300).Select(i => (byte)"abc"[i % 3]).ToArray();
        var f = new Frame(Frame.Kind.Result, Frame.Flags.Compressed, 0, 7UL, plain);
        var bytes = Codec.Encode(f);
        Assert.True(bytes.Length < Frame.HeaderSize + plain.Length,
            $"compressed wire size {bytes.Length} >= plaintext {Frame.HeaderSize + plain.Length}");
        var back = Codec.Decode(bytes);
        Assert.Equal(Frame.Kind.Result, back.MessageKind);
        Assert.True(back.Compressed);
        Assert.Equal(plain, back.Payload);
    }

    [Fact]
    public void UncompressedFrame_DecodesUnchanged()
    {
        var plain = Encoding.UTF8.GetBytes("hello world");
        var f = new Frame(Frame.Kind.Result, 1UL, plain);
        var back = Codec.Decode(Codec.Encode(f));
        Assert.Equal(plain, back.Payload);
        Assert.False(back.Compressed);
    }

    [Fact]
    public void BackToBackFrames_DecodeIndependently()
    {
        var f1 = new Frame(Frame.Kind.Query, 1UL, Encoding.UTF8.GetBytes("a"));
        var f2 = new Frame(Frame.Kind.Query, 2UL, Encoding.UTF8.GetBytes("bb"));
        var a = Codec.Encode(f1);
        var b = Codec.Encode(f2);
        var both = new byte[a.Length + b.Length];
        Buffer.BlockCopy(a, 0, both, 0, a.Length);
        Buffer.BlockCopy(b, 0, both, a.Length, b.Length);
        // Slice on declared length.
        int firstLen = Frame.EncodedLength(both);
        var back1 = Codec.Decode(both.AsSpan(0, firstLen));
        var back2 = Codec.Decode(both.AsSpan(firstLen));
        Assert.Equal(Encoding.UTF8.GetBytes("a"), back1.Payload);
        Assert.Equal(Encoding.UTF8.GetBytes("bb"), back2.Payload);
    }
}
