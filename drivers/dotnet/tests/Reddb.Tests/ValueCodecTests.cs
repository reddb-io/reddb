using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.Text;
using System.Text.Json.Nodes;
using Reddb.Redwire;
using Xunit;

namespace Reddb.Tests;

public class ValueCodecTests
{
    [Fact]
    public void ValueTagTableIsPinned()
    {
        Assert.Equal(0x00, ValueCodec.TagNull);
        Assert.Equal(0x01, ValueCodec.TagBool);
        Assert.Equal(0x02, ValueCodec.TagInt);
        Assert.Equal(0x03, ValueCodec.TagFloat);
        Assert.Equal(0x04, ValueCodec.TagText);
        Assert.Equal(0x05, ValueCodec.TagBytes);
        Assert.Equal(0x06, ValueCodec.TagVector);
        Assert.Equal(0x07, ValueCodec.TagJson);
        Assert.Equal(0x08, ValueCodec.TagTimestamp);
        Assert.Equal(0x09, ValueCodec.TagUuid);
    }

    [Fact]
    public void EncodeScalarValues()
    {
        Assert.Equal(new byte[] { 0x00 }, ValueCodec.EncodeValue(null));
        Assert.Equal(new byte[] { 0x00 }, ValueCodec.EncodeValue(DBNull.Value));
        Assert.Equal(new byte[] { 0x01, 0x01 }, ValueCodec.EncodeValue(true));
        Assert.Equal(new byte[] { 0x01, 0x00 }, ValueCodec.EncodeValue(false));
        Assert.Equal(new byte[] { 0x02, 1, 0, 0, 0, 0, 0, 0, 0 }, ValueCodec.EncodeValue(1));
        Assert.Equal(new byte[]
        {
            0x02, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        }, ValueCodec.EncodeValue(-1L));
        Assert.Equal(new byte[] { 0x04, 1, 0, 0, 0, (byte)'x' }, ValueCodec.EncodeValue("x"));
    }

    [Fact]
    public void EncodeBytesTimestampUuidAndJson()
    {
        Assert.Equal(new byte[]
        {
            0x05, 4, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef,
        }, ValueCodec.EncodeValue(new byte[] { 0xde, 0xad, 0xbe, 0xef }));

        byte[] ts = ValueCodec.EncodeValue(DateTimeOffset.FromUnixTimeSeconds(1_700_000_000L));
        Assert.Equal(ValueCodec.TagTimestamp, ts[0]);
        Assert.Equal(1_700_000_000L, BinaryPrimitives.ReadInt64LittleEndian(ts.AsSpan(1, 8)));

        Guid uuid = Guid.Parse("00112233-4455-6677-8899-aabbccddeeff");
        byte[] encodedUuid = ValueCodec.EncodeValue(uuid);
        Assert.Equal(ValueCodec.TagUuid, encodedUuid[0]);
        Assert.Equal(new byte[]
        {
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
            0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
        }, encodedUuid[1..]);

        byte[] json = ValueCodec.EncodeValue(new Dictionary<string, object?>
        {
            ["b"] = 2,
            ["a"] = 1,
        });
        Assert.Equal(ValueCodec.TagJson, json[0]);
        int len = BinaryPrimitives.ReadInt32LittleEndian(json.AsSpan(1, 4));
        Assert.Equal("{\"a\":1,\"b\":2}", Encoding.UTF8.GetString(json, 5, len));
    }

    [Fact]
    public void EncodeVectorFromFloatArrayAndReadOnlyMemory()
    {
        byte[] encoded = ValueCodec.EncodeValue(new float[] { 1.0f, 2.0f, -0.5f });
        Assert.Equal(ValueCodec.TagVector, encoded[0]);
        Assert.Equal(3, BinaryPrimitives.ReadInt32LittleEndian(encoded.AsSpan(1, 4)));
        Assert.Equal(1.0f, BitConverter.Int32BitsToSingle(BinaryPrimitives.ReadInt32LittleEndian(encoded.AsSpan(5, 4))));
        Assert.Equal(2.0f, BitConverter.Int32BitsToSingle(BinaryPrimitives.ReadInt32LittleEndian(encoded.AsSpan(9, 4))));
        Assert.Equal(-0.5f, BitConverter.Int32BitsToSingle(BinaryPrimitives.ReadInt32LittleEndian(encoded.AsSpan(13, 4))));

        ReadOnlyMemory<float> memory = new float[] { 4.0f, 5.0f };
        byte[] memoryEncoded = ValueCodec.EncodeValue(memory);
        Assert.Equal(ValueCodec.TagVector, memoryEncoded[0]);
        Assert.Equal(2, BinaryPrimitives.ReadInt32LittleEndian(memoryEncoded.AsSpan(1, 4)));
    }

    [Fact]
    public void EncodeQueryWithParamsPayload()
    {
        byte[] encoded = ValueCodec.EncodeQueryWithParams("Q", new object?[] { 42, "x", null });
        Assert.Equal(1, BinaryPrimitives.ReadInt32LittleEndian(encoded.AsSpan(0, 4)));
        Assert.Equal((byte)'Q', encoded[4]);
        Assert.Equal(3, BinaryPrimitives.ReadInt32LittleEndian(encoded.AsSpan(5, 4)));
        Assert.Equal(ValueCodec.TagInt, encoded[9]);
        Assert.Equal(42L, BinaryPrimitives.ReadInt64LittleEndian(encoded.AsSpan(10, 8)));
        Assert.Equal(ValueCodec.TagText, encoded[18]);
        Assert.Equal(new byte[] { 1, 0, 0, 0, (byte)'x' }, encoded[19..24]);
        Assert.Equal(ValueCodec.TagNull, encoded[24]);
        Assert.Equal(25, encoded.Length);
    }

    [Fact]
    public void HttpParamsUseJsonEnvelopesForTaggedValues()
    {
        JsonArray parameters = ValueCodec.ToHttpParams(new object?[]
        {
            null,
            true,
            42,
            1.5d,
            "txt",
            Encoding.UTF8.GetBytes("hi"),
            new float[] { 1.0f, 2.0f },
            new Dictionary<string, object?> { ["b"] = 2, ["a"] = 1 },
            DateTimeOffset.FromUnixTimeSeconds(1_700_000_000L),
            Guid.Parse("00112233-4455-6677-8899-aabbccddeeff"),
            new object?[] { "json", 1 },
        });

        Assert.True(parameters[0] is null);
        Assert.True((bool)parameters[1]!);
        Assert.Equal(42, (int)parameters[2]!);
        Assert.Equal(1.5d, (double)parameters[3]!);
        Assert.Equal("txt", (string)parameters[4]!);
        Assert.Equal("aGk=", (string)parameters[5]!["$bytes"]!);
        Assert.Equal(1.0d, (double)parameters[6]![0]!);
        Assert.Equal(1, (int)parameters[7]!["a"]!);
        Assert.Equal(2, (int)parameters[7]!["b"]!);
        Assert.Equal(1_700_000_000L, (long)parameters[8]!["$ts"]!);
        Assert.Equal("00112233-4455-6677-8899-aabbccddeeff", (string)parameters[9]!["$uuid"]!);
        Assert.Equal("json", (string)parameters[10]![0]!);
    }
}
