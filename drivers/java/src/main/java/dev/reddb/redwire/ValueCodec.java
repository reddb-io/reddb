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

    public static final int MAX_PARAM_COUNT = 65_536;
    public static final int MAX_VALUE_PAYLOAD_LEN = Frame.MAX_FRAME_SIZE;

    private static final ObjectMapper MAPPER = new ObjectMapper();

    private ValueCodec() {}

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
        if (value instanceof UUID uuid) return encodeUuid(uuid);
        if (value instanceof JsonNode || value instanceof Map<?, ?> || value instanceof List<?>) {
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
