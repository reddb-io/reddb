package dev.reddb.helpers;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.util.Arrays;
import java.util.List;
import java.util.Map;
import java.util.stream.Collectors;

/** Pure SQL builders + envelope parsing — unit-testable, no I/O. */
final class Sql {
    private Sql() {}

    static final ObjectMapper JSON = new ObjectMapper();

    /** Builds a fully qualified {@code collection.key} reference. */
    static String kvPath(String collection, String key) {
        for (int i = 0; i < collection.length(); i++) {
            char ch = collection.charAt(i);
            if (!isIdentChar(ch)) {
                throw new HelperException.InvalidArgument(
                    "invalid KV collection \"" + collection + "\": character \"" + ch +
                    "\" is not supported");
            }
        }
        return collection + "." + kvKeySegment(key);
    }

    static String kvKeySegment(String value) {
        if (!value.isEmpty() && allIdentChars(value)) return value;
        return "'" + value.replace("'", "''") + "'";
    }

    static String kvValueLiteral(Object value) {
        if (value == null) return "NULL";
        if (value instanceof Boolean b) return b ? "true" : "false";
        if (value instanceof Number n) return n.toString();
        if (value instanceof String s) return "'" + s.replace("'", "''") + "'";
        try {
            String s = JSON.writeValueAsString(value);
            return "'" + s.replace("'", "''") + "'";
        } catch (JsonProcessingException e) {
            throw new HelperException.InvalidArgument(e.getMessage());
        }
    }

    static String kvTagLiteral(String tag) {
        return "'" + tag.replace("'", "''") + "'";
    }

    static String queueValueLiteral(Object value) {
        if (value == null) return "NULL";
        if (value instanceof Boolean b) return b ? "true" : "false";
        if (value instanceof Number n) return n.toString();
        if (value instanceof String s) return "'" + s.replace("'", "''") + "'";
        try {
            return JSON.writeValueAsString(value);
        } catch (JsonProcessingException e) {
            throw new HelperException.InvalidArgument(e.getMessage());
        }
    }

    static String valueLiteral(Object value) { return kvValueLiteral(value); }

    static String jsonLiteral(Object value) {
        try {
            String s = JSON.writeValueAsString(value);
            return "'" + s.replace("'", "''") + "'";
        } catch (JsonProcessingException e) {
            throw new HelperException.InvalidArgument(e.getMessage());
        }
    }

    /** Raw inline JSON literal (no surrounding quotes, no SQL escaping) for RQL's inline JSON body form. */
    static String jsonInlineLiteral(Object value) {
        try {
            return JSON.writeValueAsString(value);
        } catch (JsonProcessingException e) {
            throw new HelperException.InvalidArgument(e.getMessage());
        }
    }

    static String sqlIdentifier(String value) {
        if (!value.isEmpty() && allIdentChars(value)) return value;
        return "\"" + value.replace("\"", "\"\"") + "\"";
    }

    static String sqlIdentifierPath(String value) {
        if (!value.contains(".")) return sqlIdentifier(value);
        return Arrays.stream(value.split("\\."))
            .map(Sql::sqlIdentifier)
            .collect(Collectors.joining("."));
    }

    static void assertIdentifier(String value, String label) {
        if (value == null || value.isEmpty() || !allIdentChars(value)) {
            throw new HelperException.InvalidArgument(
                "invalid " + label + " \"" + value + "\": must match [A-Za-z0-9_]+");
        }
    }

    static int normalizeLimit(int value) {
        if (value == 0) return 100;
        if (value < 0) {
            throw new HelperException.InvalidArgument("limit must be a positive integer");
        }
        return value;
    }

    static boolean isIdentChar(char c) {
        return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
            || (c >= '0' && c <= '9') || c == '_';
    }

    static boolean allIdentChars(String s) {
        for (int i = 0; i < s.length(); i++) {
            if (!isIdentChar(s.charAt(i))) return false;
        }
        return true;
    }

    // --- response parsing ---------------------------------------------

    @SuppressWarnings("unchecked")
    static Map<String, Object> decodeBody(byte[] body) {
        if (body == null || body.length == 0) return null;
        try {
            Object obj = JSON.readValue(body, Object.class);
            if (obj instanceof Map<?, ?> m) return (Map<String, Object>) m;
            return null;
        } catch (Exception e) {
            return null;
        }
    }

    static long affectedFromMap(Map<String, Object> obj) {
        Object v = obj.get("affected");
        if (v instanceof Number n) return n.longValue();
        return 0L;
    }

    /** Returns {row, affected}. */
    @SuppressWarnings("unchecked")
    static Object[] firstRow(byte[] body) {
        Map<String, Object> obj = decodeBody(body);
        if (obj == null) return new Object[]{null, 0L};
        long affected = affectedFromMap(obj);
        Object rowsObj = obj.get("rows");
        List<Object> rows = rowsObj instanceof List<?> l ? (List<Object>) l : null;
        if (rows == null || rows.isEmpty()) {
            Object nested = obj.get("result");
            if (nested instanceof Map<?, ?> nm) {
                Map<String, Object> nestedMap = (Map<String, Object>) nm;
                Object nr = nestedMap.get("rows");
                rows = nr instanceof List<?> nl ? (List<Object>) nl : null;
                if (affected == 0L) affected = affectedFromMap(nestedMap);
            }
        }
        if (rows == null || rows.isEmpty()) return new Object[]{null, affected};
        Object first = rows.get(0);
        if (first instanceof Map<?, ?> fm) return new Object[]{(Map<String, Object>) fm, affected};
        return new Object[]{null, affected};
    }

    @SuppressWarnings("unchecked")
    static List<Map<String, Object>> allRows(byte[] body) {
        Map<String, Object> obj = decodeBody(body);
        if (obj == null) return List.of();
        Object raw = obj.get("rows");
        if (!(raw instanceof List<?>)) {
            Object nested = obj.get("result");
            if (nested instanceof Map<?, ?> nm) raw = ((Map<String, Object>) nm).get("rows");
        }
        if (!(raw instanceof List<?> list)) return List.of();
        return list.stream()
            .filter(r -> r instanceof Map<?, ?>)
            .map(r -> (Map<String, Object>) r)
            .collect(Collectors.toList());
    }

    @SuppressWarnings("unchecked")
    static long affectedFromBody(byte[] body) {
        Map<String, Object> obj = decodeBody(body);
        if (obj == null) return 0L;
        long direct = affectedFromMap(obj);
        if (direct > 0L) return direct;
        Object nested = obj.get("result");
        if (nested instanceof Map<?, ?> nm) return affectedFromMap((Map<String, Object>) nm);
        return 0L;
    }

    static String ridString(Object value) {
        if (value == null) return null;
        if (value instanceof String s) return s;
        if (value instanceof Number n) {
            if (n instanceof Long || n instanceof Integer || n instanceof Short || n instanceof Byte)
                return Long.toString(n.longValue());
            return n.toString();
        }
        return null;
    }
}
