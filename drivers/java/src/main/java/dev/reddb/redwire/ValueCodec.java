package dev.reddb.redwire;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ArrayNode;
import com.fasterxml.jackson.databind.node.JsonNodeFactory;
import com.fasterxml.jackson.databind.node.ObjectNode;
import dev.reddb.RedDBException;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.time.Instant;
import java.util.ArrayList;
import java.util.Base64;
import java.util.Comparator;
import java.util.List;
import java.util.Map;
import java.util.UUID;

/**
 * Parameter codec for RedWire {@code QueryWithParams} payloads.
 *
 * Layout: {@code u32 sql_len | sql utf8 | u32 param_count | encoded values}.
 */
public final class ValueCodec {
    public static final int TAG_NULL = 0x00;
    public static final int TAG_BOOL = 0x01;
    public static final int TAG_INT = 0x02;
    public static final int TAG_FLOAT = 0x03;
    public static final int TAG_TEXT = 0x04;
    public static final int TAG_BYTES = 0x05;
    public static final int TAG_VECTOR = 0x06;
    public static final int TAG_JSON = 0x07;
    public static final int TAG_TIMESTAMP = 0x08;
    public static final int TAG_UUID = 0x09;
    public static final int TAG_FINGERPRINT = 0x0a;
    public static final int TAG_FACE = 0x0b;
    public static final int TAG_IRIS = 0x0c;
    public static final int TAG_FEATURE_VECTOR = 0x0d;
    public static final int TAG_BIT_SIGNATURE = 0x0e;

    public static final int MAX_PARAM_COUNT = 65_536;
    public static final int MAX_VALUE_PAYLOAD_LEN = Frame.MAX_FRAME_SIZE;
    public static final int MAX_FEATURE_VECTOR_DIMENSIONS = 65_536;
    public static final int MAX_BIT_SIGNATURE_BYTES = 1 << 20;

    private static final ObjectMapper MAPPER = new ObjectMapper();

    private ValueCodec() {}

    public record Timestamp(long seconds) {}

    public static boolean requiresBiometricCapability(Object value) {
        JsonNode node = envelopeNode(value);
        return node != null && (node.has("$biometric") || node.has("$feature_vector")
            || node.has("$bit_signature"));
    }

    public static byte[] encodeQueryWithParams(String sql, Object[] params) {
        Object[] values = params == null ? new Object[0] : params;
        if (values.length > MAX_PARAM_COUNT) {
            throw new IllegalArgumentException("param_count " + values.length + " > " + MAX_PARAM_COUNT);
        }
        byte[] sqlBytes = sql.getBytes(StandardCharsets.UTF_8);
        if (sqlBytes.length > MAX_VALUE_PAYLOAD_LEN) {
            throw new IllegalArgumentException("sql_len " + sqlBytes.length + " > " + MAX_VALUE_PAYLOAD_LEN);
        }

        List<byte[]> encoded = new ArrayList<>(values.length);
        int total = 4 + sqlBytes.length + 4;
        for (int i = 0; i < values.length; i++) {
            try {
                byte[] value = encodeValue(values[i]);
                encoded.add(value);
                total += value.length;
            } catch (RuntimeException e) {
                throw new IllegalArgumentException("param[" + i + "]: " + e.getMessage(), e);
            }
        }

        ByteBuffer out = ByteBuffer.allocate(total).order(ByteOrder.LITTLE_ENDIAN);
        out.putInt(sqlBytes.length);
        out.put(sqlBytes);
        out.putInt(encoded.size());
        for (byte[] value : encoded) out.put(value);
        return out.array();
    }

    public static byte[] encodeValue(Object value) {
        if (value == null) return new byte[]{(byte) TAG_NULL};
        if (value instanceof Boolean b) return new byte[]{(byte) TAG_BOOL, (byte) (b ? 1 : 0)};
        if (value instanceof Byte || value instanceof Short || value instanceof Integer || value instanceof Long) {
            return encodeI64(((Number) value).longValue(), TAG_INT);
        }
        if (value instanceof Float || value instanceof Double) {
            ByteBuffer out = ByteBuffer.allocate(9).order(ByteOrder.LITTLE_ENDIAN);
            out.put((byte) TAG_FLOAT);
            out.putDouble(((Number) value).doubleValue());
            return out.array();
        }
        if (value instanceof String s) return encodeLenPrefixed(TAG_TEXT, s.getBytes(StandardCharsets.UTF_8));
        if (value instanceof byte[] bytes) return encodeLenPrefixed(TAG_BYTES, bytes);
        if (value instanceof float[] vector) return encodeVector(vector);
        if (value instanceof Instant instant) return encodeI64(instant.getEpochSecond(), TAG_TIMESTAMP);
        if (value instanceof Timestamp timestamp) return encodeI64(timestamp.seconds(), TAG_TIMESTAMP);
        if (value instanceof UUID uuid) return encodeUuid(uuid);
        if (value instanceof JsonNode || value instanceof Map<?, ?> || value instanceof List<?>) {
            JsonNode node = canonicalize(value);
            if (node.isObject() && node.size() == 1) {
                if (node.has("$biometric")) return encodeBiometric(node.get("$biometric"));
                if (node.has("$feature_vector")) return encodeFeatureVector(node.get("$feature_vector"));
                if (node.has("$bit_signature")) return encodeBitSignature(node.get("$bit_signature"));
            }
            byte[] json = canonicalJson(value).getBytes(StandardCharsets.UTF_8);
            return encodeLenPrefixed(TAG_JSON, json);
        }
        throw new IllegalArgumentException("unsupported param type " + value.getClass().getName());
    }

    public static ArrayNode toHttpParams(Object[] params) {
        Object[] values = params == null ? new Object[0] : params;
        ArrayNode out = JsonNodeFactory.instance.arrayNode();
        for (int i = 0; i < values.length; i++) {
            try {
                out.add(toHttpParam(values[i]));
            } catch (RuntimeException e) {
                throw new IllegalArgumentException("param[" + i + "]: " + e.getMessage(), e);
            }
        }
        return out;
    }

    private static JsonNode toHttpParam(Object value) {
        if (value == null) return JsonNodeFactory.instance.nullNode();
        if (value instanceof Boolean b) return JsonNodeFactory.instance.booleanNode(b);
        if (value instanceof Byte || value instanceof Short || value instanceof Integer || value instanceof Long) {
            return JsonNodeFactory.instance.numberNode(((Number) value).longValue());
        }
        if (value instanceof Float || value instanceof Double) {
            return JsonNodeFactory.instance.numberNode(((Number) value).doubleValue());
        }
        if (value instanceof String s) return JsonNodeFactory.instance.textNode(s);
        if (value instanceof byte[] bytes) {
            ObjectNode node = JsonNodeFactory.instance.objectNode();
            node.put("$bytes", Base64.getEncoder().encodeToString(bytes));
            return node;
        }
        if (value instanceof float[] vector) {
            ArrayNode node = JsonNodeFactory.instance.arrayNode();
            for (float v : vector) node.add((double) v);
            return node;
        }
        if (value instanceof Instant instant) {
            ObjectNode node = JsonNodeFactory.instance.objectNode();
            node.put("$ts", instant.getEpochSecond());
            return node;
        }
        if (value instanceof Timestamp timestamp) {
            ObjectNode node = JsonNodeFactory.instance.objectNode();
            node.put("$ts", timestamp.seconds());
            return node;
        }
        if (value instanceof UUID uuid) {
            ObjectNode node = JsonNodeFactory.instance.objectNode();
            node.put("$uuid", uuid.toString());
            return node;
        }
        if (value instanceof JsonNode || value instanceof Map<?, ?> || value instanceof List<?>) {
            return canonicalize(value);
        }
        throw new IllegalArgumentException("unsupported param type " + value.getClass().getName());
    }

    private static byte[] encodeI64(long value, int tag) {
        ByteBuffer out = ByteBuffer.allocate(9).order(ByteOrder.LITTLE_ENDIAN);
        out.put((byte) tag);
        out.putLong(value);
        return out.array();
    }

    private static byte[] encodeLenPrefixed(int tag, byte[] bytes) {
        if (bytes.length > MAX_VALUE_PAYLOAD_LEN) {
            throw new IllegalArgumentException("value len " + bytes.length + " > " + MAX_VALUE_PAYLOAD_LEN);
        }
        ByteBuffer out = ByteBuffer.allocate(1 + 4 + bytes.length).order(ByteOrder.LITTLE_ENDIAN);
        out.put((byte) tag);
        out.putInt(bytes.length);
        out.put(bytes);
        return out.array();
    }

    private static byte[] encodeVector(float[] values) {
        int bytes = values.length * 4;
        if (bytes > MAX_VALUE_PAYLOAD_LEN) {
            throw new IllegalArgumentException("vector bytes " + bytes + " > " + MAX_VALUE_PAYLOAD_LEN);
        }
        ByteBuffer out = ByteBuffer.allocate(1 + 4 + bytes).order(ByteOrder.LITTLE_ENDIAN);
        out.put((byte) TAG_VECTOR);
        out.putInt(values.length);
        for (float value : values) out.putFloat(value);
        return out.array();
    }

    private static byte[] encodeUuid(UUID uuid) {
        ByteBuffer out = ByteBuffer.allocate(17).order(ByteOrder.BIG_ENDIAN);
        out.put((byte) TAG_UUID);
        out.putLong(uuid.getMostSignificantBits());
        out.putLong(uuid.getLeastSignificantBits());
        return out.array();
    }

    private static byte[] encodeBiometric(JsonNode value) {
        if (value == null || !value.isObject()) {
            throw new IllegalArgumentException("$biometric must be an object");
        }
        String modality = requiredText(value, "modality");
        int format = biometricFormat(requiredText(value, "format"));
        int expected = switch (modality) {
            case "fingerprint" -> 1;
            case "face" -> 2;
            case "iris" -> 3;
            default -> 0;
        };
        if (format == 0 || expected == 0 || (format >>> 8) != expected) {
            throw new IllegalArgumentException("invalid biometric modality or format");
        }
        byte[] data = decodeHex(requiredText(value, "data"), "biometric data");
        if (data.length == 0 || data.length > MAX_VALUE_PAYLOAD_LEN) {
            throw new IllegalArgumentException("invalid biometric payload length");
        }
        int tag = switch (modality) {
            case "fingerprint" -> TAG_FINGERPRINT;
            case "face" -> TAG_FACE;
            case "iris" -> TAG_IRIS;
            default -> throw new IllegalArgumentException("invalid biometric modality");
        };
        ByteBuffer out = ByteBuffer.allocate(1 + 2 + 4 + data.length)
            .order(ByteOrder.LITTLE_ENDIAN);
        out.put((byte) tag).putShort((short) format).putInt(data.length).put(data);
        return out.array();
    }

    private static byte[] encodeFeatureVector(JsonNode value) {
        if (value == null || !value.isObject() || !value.path("values").isArray()) {
            throw new IllegalArgumentException("$feature_vector must contain values");
        }
        JsonNode values = value.get("values");
        if (values.isEmpty() || values.size() > MAX_FEATURE_VECTOR_DIMENSIONS) {
            throw new IllegalArgumentException("invalid feature vector dimension");
        }
        byte[] space = decodeSpaceId(requiredText(value, "space_id"));
        ByteBuffer out = ByteBuffer.allocate(1 + 16 + 4 + values.size() * 4)
            .order(ByteOrder.LITTLE_ENDIAN);
        out.put((byte) TAG_FEATURE_VECTOR).put(space).putInt(values.size());
        double norm = 0;
        for (JsonNode coordinateNode : values) {
            if (!coordinateNode.isNumber()) {
                throw new IllegalArgumentException("feature vector coordinates must be numeric");
            }
            float coordinate = coordinateNode.floatValue();
            if (!Float.isFinite(coordinate)) {
                throw new IllegalArgumentException("feature vector coordinates must be finite");
            }
            norm += (double) coordinate * coordinate;
            out.putFloat(coordinate);
        }
        if (norm == 0) throw new IllegalArgumentException("feature vector norm must be non-zero");
        return out.array();
    }

    private static byte[] encodeBitSignature(JsonNode value) {
        if (value == null || !value.isObject()) {
            throw new IllegalArgumentException("$bit_signature must be an object");
        }
        byte[] space = decodeSpaceId(requiredText(value, "space_id"));
        byte[] bits = decodeHex(requiredText(value, "bits"), "bit signature bits");
        byte[] mask = value.has("mask")
            ? decodeHex(requiredText(value, "mask"), "bit signature mask")
            : fullMask(bits.length);
        boolean hasValidBit = false;
        for (byte b : mask) hasValidBit |= b != 0;
        if (bits.length == 0 || bits.length > MAX_BIT_SIGNATURE_BYTES
            || mask.length != bits.length || !hasValidBit) {
            throw new IllegalArgumentException("invalid bit signature length or mask");
        }
        ByteBuffer out = ByteBuffer.allocate(1 + 16 + 4 + bits.length * 2)
            .order(ByteOrder.LITTLE_ENDIAN);
        out.put((byte) TAG_BIT_SIGNATURE).put(space).putInt(bits.length).put(bits).put(mask);
        return out.array();
    }

    private static JsonNode envelopeNode(Object value) {
        if (!(value instanceof JsonNode) && !(value instanceof Map<?, ?>)) return null;
        JsonNode node = canonicalize(value);
        return node.isObject() ? node : null;
    }

    private static String requiredText(JsonNode value, String field) {
        JsonNode child = value.get(field);
        if (child == null || !child.isTextual()) {
            throw new IllegalArgumentException("typed parameter field '" + field + "' must be text");
        }
        return child.asText();
    }

    private static byte[] decodeSpaceId(String value) {
        byte[] bytes = decodeHex(value.replace("-", ""), "space_id");
        if (bytes.length != 16) throw new IllegalArgumentException("space_id must contain 16 bytes");
        return bytes;
    }

    private static byte[] decodeHex(String value, String field) {
        if ((value.length() & 1) != 0) {
            throw new IllegalArgumentException(field + " must be even-length hexadecimal text");
        }
        byte[] out = new byte[value.length() / 2];
        for (int i = 0; i < out.length; i++) {
            int high = Character.digit(value.charAt(i * 2), 16);
            int low = Character.digit(value.charAt(i * 2 + 1), 16);
            if (high < 0 || low < 0) {
                throw new IllegalArgumentException(field + " must be hexadecimal text");
            }
            out[i] = (byte) ((high << 4) | low);
        }
        return out;
    }

    private static byte[] fullMask(int length) {
        byte[] mask = new byte[length];
        java.util.Arrays.fill(mask, (byte) 0xff);
        return mask;
    }

    private static int biometricFormat(String name) {
        return switch (name.toLowerCase(java.util.Locale.ROOT)) {
            case "iso/iec 39794-2" -> 0x0101;
            case "iso/iec 39794-4" -> 0x0102;
            case "iso/iec 19794-2" -> 0x0103;
            case "iso/iec 19794-4" -> 0x0104;
            case "ansi incits 378" -> 0x0105;
            case "ansi incits 381" -> 0x0106;
            case "ansi/nist-itl type-4" -> 0x0107;
            case "ansi/nist-itl type-9" -> 0x0108;
            case "cbeff fingerprint" -> 0x0109;
            case "iso/iec 39794-5" -> 0x0201;
            case "iso/iec 19794-5" -> 0x0202;
            case "ansi/nist-itl type-10" -> 0x0203;
            case "cbeff face" -> 0x0204;
            case "iso/iec 39794-6" -> 0x0301;
            case "iso/iec 19794-6" -> 0x0302;
            case "ansi/nist-itl type-17" -> 0x0303;
            case "cbeff iris" -> 0x0304;
            default -> 0;
        };
    }

    private static String canonicalJson(Object value) {
        try {
            return MAPPER.writeValueAsString(canonicalize(value));
        } catch (Exception e) {
            throw new RedDBException.ProtocolError("json param encode failed: " + e.getMessage(), e);
        }
    }

    private static JsonNode canonicalize(Object value) {
        if (value == null) return JsonNodeFactory.instance.nullNode();
        if (value instanceof JsonNode node) {
            if (node.isObject()) {
                ObjectNode out = JsonNodeFactory.instance.objectNode();
                List<String> names = new ArrayList<>();
                node.fieldNames().forEachRemaining(names::add);
                names.sort(Comparator.naturalOrder());
                for (String name : names) out.set(name, canonicalize(node.get(name)));
                return out;
            }
            if (node.isArray()) {
                ArrayNode out = JsonNodeFactory.instance.arrayNode();
                for (JsonNode item : node) out.add(canonicalize(item));
                return out;
            }
            return node;
        }
        if (value instanceof Map<?, ?> map) {
            ObjectNode out = JsonNodeFactory.instance.objectNode();
            map.entrySet().stream()
                .sorted(Comparator.comparing(e -> String.valueOf(e.getKey())))
                .forEach(e -> out.set(String.valueOf(e.getKey()), canonicalize(e.getValue())));
            return out;
        }
        if (value instanceof List<?> list) {
            ArrayNode out = JsonNodeFactory.instance.arrayNode();
            for (Object item : list) out.add(canonicalize(item));
            return out;
        }
        return MAPPER.valueToTree(value);
    }
}
