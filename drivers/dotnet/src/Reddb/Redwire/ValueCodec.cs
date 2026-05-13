using System;
using System.Buffers.Binary;
using System.Collections;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Reddb.Redwire;

/// <summary>
/// Parameter codec for RedWire <c>QueryWithParams</c> payloads.
///
/// Layout: <c>u32 sql_len | sql utf8 | u32 param_count | encoded values</c>.
/// </summary>
public static class ValueCodec
{
    public const byte TagNull = 0x00;
    public const byte TagBool = 0x01;
    public const byte TagInt = 0x02;
    public const byte TagFloat = 0x03;
    public const byte TagText = 0x04;
    public const byte TagBytes = 0x05;
    public const byte TagVector = 0x06;
    public const byte TagJson = 0x07;
    public const byte TagTimestamp = 0x08;
    public const byte TagUuid = 0x09;

    public const int MaxParamCount = 65_536;
    public const int MaxValuePayloadLen = Frame.MaxFrameSize;

    public readonly record struct Timestamp(long Seconds);

    public static byte[] EncodeQueryWithParams(string sql, object?[]? parameters)
    {
        object?[] values = parameters ?? Array.Empty<object?>();
        if (values.Length > MaxParamCount)
            throw new ArgumentException($"param_count {values.Length} > {MaxParamCount}", nameof(parameters));

        byte[] sqlBytes = Encoding.UTF8.GetBytes(sql);
        if (sqlBytes.Length > MaxValuePayloadLen)
            throw new ArgumentException($"sql_len {sqlBytes.Length} > {MaxValuePayloadLen}", nameof(sql));

        var encoded = new List<byte[]>(values.Length);
        int total = 4 + sqlBytes.Length + 4;
        for (int i = 0; i < values.Length; i++)
        {
            try
            {
                byte[] value = EncodeValue(values[i]);
                encoded.Add(value);
                total += value.Length;
            }
            catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or JsonException)
            {
                throw new ArgumentException($"param[{i}]: {ex.Message}", nameof(parameters), ex);
            }
        }

        byte[] output = new byte[total];
        BinaryPrimitives.WriteInt32LittleEndian(output.AsSpan(0, 4), sqlBytes.Length);
        sqlBytes.CopyTo(output.AsSpan(4));
        int offset = 4 + sqlBytes.Length;
        BinaryPrimitives.WriteInt32LittleEndian(output.AsSpan(offset, 4), encoded.Count);
        offset += 4;
        foreach (byte[] value in encoded)
        {
            value.CopyTo(output.AsSpan(offset));
            offset += value.Length;
        }
        return output;
    }

    public static byte[] EncodeValue(object? value)
    {
        if (value is null || value is DBNull) return new[] { TagNull };
        if (value is bool b) return new[] { TagBool, b ? (byte)1 : (byte)0 };
        if (TryGetInt64(value, out long i)) return EncodeI64(TagInt, i);
        if (value is float f) return EncodeF64(f);
        if (value is double d) return EncodeF64(d);
        if (value is string s) return EncodeLenPrefixed(TagText, Encoding.UTF8.GetBytes(s));
        if (value is byte[] bytes) return EncodeLenPrefixed(TagBytes, bytes);
        if (value is float[] vector) return EncodeVector(vector);
        if (value is ReadOnlyMemory<float> memory) return EncodeVector(memory.Span);
        if (value is DateTimeOffset dto) return EncodeI64(TagTimestamp, dto.ToUnixTimeSeconds());
        if (value is Timestamp ts) return EncodeI64(TagTimestamp, ts.Seconds);
        if (value is Guid guid) return EncodeUuid(guid);
        if (IsJsonParam(value))
        {
            byte[] json = Encoding.UTF8.GetBytes(CanonicalJson(value));
            return EncodeLenPrefixed(TagJson, json);
        }
        throw new ArgumentException($"unsupported param type {value.GetType().FullName}");
    }

    public static JsonArray ToHttpParams(object?[]? parameters)
    {
        object?[] values = parameters ?? Array.Empty<object?>();
        var output = new JsonArray();
        for (int i = 0; i < values.Length; i++)
        {
            try
            {
                output.Add(ToHttpParam(values[i]));
            }
            catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or JsonException)
            {
                throw new ArgumentException($"param[{i}]: {ex.Message}", nameof(parameters), ex);
            }
        }
        return output;
    }

    private static JsonNode? ToHttpParam(object? value)
    {
        if (value is null || value is DBNull) return null;
        if (value is bool b) return JsonValue.Create(b);
        if (TryGetInt64(value, out long i)) return JsonValue.Create(i);
        if (value is float f) return JsonValue.Create((double)f);
        if (value is double d) return JsonValue.Create(d);
        if (value is string s) return JsonValue.Create(s);
        if (value is byte[] bytes)
        {
            return new JsonObject { ["$bytes"] = Convert.ToBase64String(bytes) };
        }
        if (value is float[] vector) return VectorToJson(vector);
        if (value is ReadOnlyMemory<float> memory) return VectorToJson(memory.Span);
        if (value is DateTimeOffset dto)
        {
            return new JsonObject { ["$ts"] = dto.ToUnixTimeSeconds() };
        }
        if (value is Timestamp ts)
        {
            return new JsonObject { ["$ts"] = ts.Seconds };
        }
        if (value is Guid guid)
        {
            return new JsonObject { ["$uuid"] = guid.ToString("D") };
        }
        if (IsJsonParam(value)) return Canonicalize(value);
        throw new ArgumentException($"unsupported param type {value.GetType().FullName}");
    }

    private static byte[] EncodeI64(byte tag, long value)
    {
        byte[] output = new byte[9];
        output[0] = tag;
        BinaryPrimitives.WriteInt64LittleEndian(output.AsSpan(1), value);
        return output;
    }

    private static byte[] EncodeF64(double value)
    {
        byte[] output = new byte[9];
        output[0] = TagFloat;
        BinaryPrimitives.WriteInt64LittleEndian(output.AsSpan(1), BitConverter.DoubleToInt64Bits(value));
        return output;
    }

    private static byte[] EncodeLenPrefixed(byte tag, ReadOnlySpan<byte> bytes)
    {
        if (bytes.Length > MaxValuePayloadLen)
            throw new ArgumentException($"value len {bytes.Length} > {MaxValuePayloadLen}");
        byte[] output = new byte[1 + 4 + bytes.Length];
        output[0] = tag;
        BinaryPrimitives.WriteInt32LittleEndian(output.AsSpan(1, 4), bytes.Length);
        bytes.CopyTo(output.AsSpan(5));
        return output;
    }

    private static byte[] EncodeVector(ReadOnlySpan<float> values)
    {
        int bytes = checked(values.Length * 4);
        if (bytes > MaxValuePayloadLen)
            throw new ArgumentException($"vector bytes {bytes} > {MaxValuePayloadLen}");
        byte[] output = new byte[1 + 4 + bytes];
        output[0] = TagVector;
        BinaryPrimitives.WriteInt32LittleEndian(output.AsSpan(1, 4), values.Length);
        int offset = 5;
        foreach (float value in values)
        {
            BinaryPrimitives.WriteInt32LittleEndian(output.AsSpan(offset, 4), BitConverter.SingleToInt32Bits(value));
            offset += 4;
        }
        return output;
    }

    private static byte[] EncodeUuid(Guid guid)
    {
        byte[] output = new byte[17];
        output[0] = TagUuid;
        WriteUuidBytes(guid, output.AsSpan(1));
        return output;
    }

    private static void WriteUuidBytes(Guid guid, Span<byte> output)
    {
        string hex = guid.ToString("N");
        for (int i = 0; i < 16; i++)
        {
            output[i] = Convert.ToByte(hex.Substring(i * 2, 2), 16);
        }
    }

    private static JsonArray VectorToJson(ReadOnlySpan<float> vector)
    {
        var output = new JsonArray();
        foreach (float value in vector) output.Add((double)value);
        return output;
    }

    private static bool TryGetInt64(object value, out long result)
    {
        switch (value)
        {
            case sbyte v: result = v; return true;
            case byte v: result = v; return true;
            case short v: result = v; return true;
            case ushort v: result = v; return true;
            case int v: result = v; return true;
            case uint v: result = v; return true;
            case long v: result = v; return true;
            default:
                result = 0;
                return false;
        }
    }

    private static bool IsJsonParam(object value)
    {
        if (value is JsonNode or JsonElement or JsonDocument) return true;
        if (value is string or byte[] or float[] or ReadOnlyMemory<float>) return false;
        return value is IDictionary or IEnumerable;
    }

    private static string CanonicalJson(object value)
    {
        JsonNode? node = Canonicalize(value);
        return node?.ToJsonString(new JsonSerializerOptions { WriteIndented = false }) ?? "null";
    }

    private static JsonNode? Canonicalize(object? value)
    {
        if (value is null || value is DBNull) return null;
        if (value is JsonObject obj)
        {
            var output = new JsonObject();
            foreach (string key in obj.Select(kv => kv.Key).OrderBy(k => k, StringComparer.Ordinal))
            {
                output[key] = Canonicalize(obj[key]);
            }
            return output;
        }
        if (value is JsonArray arr)
        {
            var output = new JsonArray();
            foreach (JsonNode? item in arr) output.Add(Canonicalize(item));
            return output;
        }
        if (value is JsonValue jsonValue)
            return JsonNode.Parse(jsonValue.ToJsonString());
        if (value is JsonDocument doc)
            return Canonicalize(doc.RootElement);
        if (value is JsonElement element)
            return CanonicalizeJsonElement(element);
        if (value is IDictionary dict)
        {
            var output = new JsonObject();
            var entries = dict.Cast<DictionaryEntry>()
                .OrderBy(e => Convert.ToString(e.Key, System.Globalization.CultureInfo.InvariantCulture), StringComparer.Ordinal);
            foreach (DictionaryEntry entry in entries)
            {
                string key = Convert.ToString(entry.Key, System.Globalization.CultureInfo.InvariantCulture) ?? string.Empty;
                output[key] = Canonicalize(entry.Value);
            }
            return output;
        }
        if (value is IEnumerable enumerable && value is not string && value is not byte[])
        {
            var output = new JsonArray();
            foreach (object? item in enumerable) output.Add(Canonicalize(item));
            return output;
        }
        return JsonSerializer.SerializeToNode(value, value.GetType());
    }

    private static JsonNode? CanonicalizeJsonElement(JsonElement element)
    {
        switch (element.ValueKind)
        {
            case JsonValueKind.Object:
            {
                var output = new JsonObject();
                foreach (JsonProperty property in element.EnumerateObject().OrderBy(p => p.Name, StringComparer.Ordinal))
                    output[property.Name] = CanonicalizeJsonElement(property.Value);
                return output;
            }
            case JsonValueKind.Array:
            {
                var output = new JsonArray();
                foreach (JsonElement item in element.EnumerateArray()) output.Add(CanonicalizeJsonElement(item));
                return output;
            }
            case JsonValueKind.Null:
            case JsonValueKind.Undefined:
                return null;
            default:
                return JsonNode.Parse(element.GetRawText());
        }
    }
}
