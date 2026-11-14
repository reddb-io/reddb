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
    public const byte TagFingerprint = 0x0a;
    public const byte TagFace = 0x0b;
    public const byte TagIris = 0x0c;
    public const byte TagFeatureVector = 0x0d;
    public const byte TagBitSignature = 0x0e;

    public const int MaxParamCount = 65_536;
    public const int MaxValuePayloadLen = Frame.MaxFrameSize;
    public const int MaxFeatureVectorDimensions = 65_536;
    public const int MaxBitSignatureBytes = 1 << 20;

    public readonly record struct Timestamp(long Seconds);

    public static bool RequiresBiometricCapability(object? value)
    {
        if (!IsJsonParam(value ?? string.Empty)) return false;
        JsonObject? envelope = Canonicalize(value) as JsonObject;
        return envelope is not null &&
            (envelope.ContainsKey("$biometric") || envelope.ContainsKey("$feature_vector")
             || envelope.ContainsKey("$bit_signature"));
    }

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
        if (value is DateTime dt) return EncodeI64(TagTimestamp, DateTimeToUnixSeconds(dt));
        if (value is DateTimeOffset dto) return EncodeI64(TagTimestamp, dto.ToUnixTimeSeconds());
        if (value is Timestamp ts) return EncodeI64(TagTimestamp, ts.Seconds);
        if (value is Guid guid) return EncodeUuid(guid);
        if (IsJsonParam(value))
        {
            JsonObject? envelope = Canonicalize(value) as JsonObject;
            if (envelope is not null && envelope.Count == 1)
            {
                if (envelope.TryGetPropertyValue("$biometric", out JsonNode? biometric))
                    return EncodeBiometric(biometric);
                if (envelope.TryGetPropertyValue("$feature_vector", out JsonNode? feature))
                    return EncodeFeatureVector(feature);
                if (envelope.TryGetPropertyValue("$bit_signature", out JsonNode? bits))
                    return EncodeBitSignature(bits);
            }
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
        if (value is DateTime dt)
        {
            return new JsonObject { ["$ts"] = DateTimeToUnixSeconds(dt) };
        }
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

    private static byte[] EncodeBiometric(JsonNode? node)
    {
        if (node is not JsonObject value)
            throw new ArgumentException("$biometric must be an object");
        string modality = RequiredText(value, "modality");
        int format = BiometricFormat(RequiredText(value, "format"));
        int expected = modality switch { "fingerprint" => 1, "face" => 2, "iris" => 3, _ => 0 };
        if (format == 0 || expected == 0 || (format >> 8) != expected)
            throw new ArgumentException("invalid biometric modality or format");
        byte[] data = DecodeHex(RequiredText(value, "data"), "biometric data");
        if (data.Length == 0 || data.Length > MaxValuePayloadLen)
            throw new ArgumentException("invalid biometric payload length");
        byte tag = modality switch
        {
            "fingerprint" => TagFingerprint,
            "face" => TagFace,
            "iris" => TagIris,
            _ => throw new ArgumentException("invalid biometric modality"),
        };
        byte[] output = new byte[1 + 2 + 4 + data.Length];
        output[0] = tag;
        BinaryPrimitives.WriteUInt16LittleEndian(output.AsSpan(1, 2), (ushort)format);
        BinaryPrimitives.WriteInt32LittleEndian(output.AsSpan(3, 4), data.Length);
        data.CopyTo(output.AsSpan(7));
        return output;
    }

    private static byte[] EncodeFeatureVector(JsonNode? node)
    {
        if (node is not JsonObject value || value["values"] is not JsonArray values)
            throw new ArgumentException("$feature_vector must contain values");
        if (values.Count == 0 || values.Count > MaxFeatureVectorDimensions)
            throw new ArgumentException("invalid feature vector dimension");
        byte[] space = DecodeSpaceId(RequiredText(value, "space_id"));
        byte[] output = new byte[1 + 16 + 4 + values.Count * 4];
        output[0] = TagFeatureVector;
        space.CopyTo(output.AsSpan(1));
        BinaryPrimitives.WriteInt32LittleEndian(output.AsSpan(17, 4), values.Count);
        double norm = 0;
        for (int index = 0; index < values.Count; index++)
        {
            float coordinate;
            try { coordinate = values[index]!.GetValue<float>(); }
            catch (Exception ex) { throw new ArgumentException("feature vector coordinates must be numeric", ex); }
            if (!float.IsFinite(coordinate))
                throw new ArgumentException("feature vector coordinates must be finite");
            norm += (double)coordinate * coordinate;
            BinaryPrimitives.WriteInt32LittleEndian(
                output.AsSpan(21 + index * 4, 4), BitConverter.SingleToInt32Bits(coordinate));
        }
        if (norm == 0) throw new ArgumentException("feature vector norm must be non-zero");
        return output;
    }

    private static byte[] EncodeBitSignature(JsonNode? node)
    {
        if (node is not JsonObject value)
            throw new ArgumentException("$bit_signature must be an object");
        byte[] space = DecodeSpaceId(RequiredText(value, "space_id"));
        byte[] bits = DecodeHex(RequiredText(value, "bits"), "bit signature bits");
        byte[] mask = value.ContainsKey("mask")
            ? DecodeHex(RequiredText(value, "mask"), "bit signature mask")
            : Enumerable.Repeat((byte)0xff, bits.Length).ToArray();
        if (bits.Length == 0 || bits.Length > MaxBitSignatureBytes || mask.Length != bits.Length
            || !mask.Any(item => item != 0))
            throw new ArgumentException("invalid bit signature length or mask");
        byte[] output = new byte[1 + 16 + 4 + bits.Length * 2];
        output[0] = TagBitSignature;
        space.CopyTo(output.AsSpan(1));
        BinaryPrimitives.WriteInt32LittleEndian(output.AsSpan(17, 4), bits.Length);
        bits.CopyTo(output.AsSpan(21));
        mask.CopyTo(output.AsSpan(21 + bits.Length));
        return output;
    }

    private static string RequiredText(JsonObject value, string field)
    {
        if (value[field] is not JsonValue node || !node.TryGetValue(out string? text) || text is null)
            throw new ArgumentException($"typed parameter field '{field}' must be text");
        return text;
    }

    private static byte[] DecodeSpaceId(string value)
    {
        byte[] bytes = DecodeHex(value.Replace("-", string.Empty, StringComparison.Ordinal), "space_id");
        if (bytes.Length != 16) throw new ArgumentException("space_id must contain 16 bytes");
        return bytes;
    }

    private static byte[] DecodeHex(string value, string field)
    {
        if ((value.Length & 1) != 0)
            throw new ArgumentException($"{field} must be even-length hexadecimal text");
        byte[] output = new byte[value.Length / 2];
        for (int index = 0; index < output.Length; index++)
        {
            if (!byte.TryParse(value.AsSpan(index * 2, 2),
                    System.Globalization.NumberStyles.HexNumber,
                    System.Globalization.CultureInfo.InvariantCulture,
                    out output[index]))
                throw new ArgumentException($"{field} must be hexadecimal text");
        }
        return output;
    }

    private static int BiometricFormat(string name) => name.ToLowerInvariant() switch
    {
        "iso/iec 39794-2" => 0x0101,
        "iso/iec 39794-4" => 0x0102,
        "iso/iec 19794-2" => 0x0103,
        "iso/iec 19794-4" => 0x0104,
        "ansi incits 378" => 0x0105,
        "ansi incits 381" => 0x0106,
        "ansi/nist-itl type-4" => 0x0107,
        "ansi/nist-itl type-9" => 0x0108,
        "cbeff fingerprint" => 0x0109,
        "iso/iec 39794-5" => 0x0201,
        "iso/iec 19794-5" => 0x0202,
        "ansi/nist-itl type-10" => 0x0203,
        "cbeff face" => 0x0204,
        "iso/iec 39794-6" => 0x0301,
        "iso/iec 19794-6" => 0x0302,
        "ansi/nist-itl type-17" => 0x0303,
        "cbeff iris" => 0x0304,
        _ => 0,
    };

    private static long DateTimeToUnixSeconds(DateTime value)
    {
        DateTime utc = value.Kind == DateTimeKind.Unspecified
            ? DateTime.SpecifyKind(value, DateTimeKind.Utc)
            : value.ToUniversalTime();
        return new DateTimeOffset(utc).ToUnixTimeSeconds();
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
            var entries = new List<(string Key, object? Value)>();
            foreach (object? key in dict.Keys)
            {
                string normalized = Convert.ToString(key, System.Globalization.CultureInfo.InvariantCulture) ?? string.Empty;
                entries.Add((normalized, dict[key]));
            }
            foreach (var entry in entries.OrderBy(e => e.Key, StringComparer.Ordinal))
            {
                output[entry.Key] = Canonicalize(entry.Value);
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
