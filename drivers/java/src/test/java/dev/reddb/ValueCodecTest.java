package dev.reddb;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import dev.reddb.redwire.ValueCodec;
import org.junit.jupiter.api.Test;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Instant;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class ValueCodecTest {
    private static final ObjectMapper MAPPER = new ObjectMapper();

    @Test
    void valueTagTableIsPinned() {
        assertEquals(0x00, ValueCodec.TAG_NULL);
        assertEquals(0x01, ValueCodec.TAG_BOOL);
        assertEquals(0x02, ValueCodec.TAG_INT);
        assertEquals(0x03, ValueCodec.TAG_FLOAT);
        assertEquals(0x04, ValueCodec.TAG_TEXT);
        assertEquals(0x05, ValueCodec.TAG_BYTES);
        assertEquals(0x06, ValueCodec.TAG_VECTOR);
        assertEquals(0x07, ValueCodec.TAG_JSON);
        assertEquals(0x08, ValueCodec.TAG_TIMESTAMP);
        assertEquals(0x09, ValueCodec.TAG_UUID);
    }

    @Test
    void encodeScalarValues() {
        assertArrayEquals(new byte[]{0x00}, ValueCodec.encodeValue(null));
        assertArrayEquals(new byte[]{0x01, 0x01}, ValueCodec.encodeValue(true));
        assertArrayEquals(new byte[]{0x01, 0x00}, ValueCodec.encodeValue(false));
        assertArrayEquals(new byte[]{0x02, 1, 0, 0, 0, 0, 0, 0, 0}, ValueCodec.encodeValue(1));
        assertArrayEquals(new byte[]{
            0x02, (byte) 0xff, (byte) 0xff, (byte) 0xff, (byte) 0xff,
            (byte) 0xff, (byte) 0xff, (byte) 0xff, (byte) 0xff,
        }, ValueCodec.encodeValue(-1L));
        assertArrayEquals(new byte[]{0x04, 1, 0, 0, 0, 'x'}, ValueCodec.encodeValue("x"));
    }

    @Test
    void encodeBytesTimestampUuidAndJson() {
        assertArrayEquals(
            new byte[]{0x05, 4, 0, 0, 0, (byte) 0xde, (byte) 0xad, (byte) 0xbe, (byte) 0xef},
            ValueCodec.encodeValue(new byte[]{(byte) 0xde, (byte) 0xad, (byte) 0xbe, (byte) 0xef})
        );

        byte[] ts = ValueCodec.encodeValue(Instant.ofEpochSecond(1_700_000_000L));
        assertEquals(ValueCodec.TAG_TIMESTAMP, ts[0] & 0xff);
        assertEquals(1_700_000_000L, ByteBuffer.wrap(ts, 1, 8).order(ByteOrder.LITTLE_ENDIAN).getLong());

        UUID uuid = UUID.fromString("00112233-4455-6677-8899-aabbccddeeff");
        byte[] encodedUuid = ValueCodec.encodeValue(uuid);
        assertEquals(ValueCodec.TAG_UUID, encodedUuid[0] & 0xff);
        assertArrayEquals(new byte[]{
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
            (byte) 0x88, (byte) 0x99, (byte) 0xaa, (byte) 0xbb,
            (byte) 0xcc, (byte) 0xdd, (byte) 0xee, (byte) 0xff,
        }, java.util.Arrays.copyOfRange(encodedUuid, 1, encodedUuid.length));

        byte[] json = ValueCodec.encodeValue(Map.of("b", 2, "a", 1));
        assertEquals(ValueCodec.TAG_JSON, json[0] & 0xff);
        int len = ByteBuffer.wrap(json, 1, 4).order(ByteOrder.LITTLE_ENDIAN).getInt();
        assertEquals("{\"a\":1,\"b\":2}", new String(json, 5, len, java.nio.charset.StandardCharsets.UTF_8));
    }

    @Test
    void encodeVectorFromFloatArray() {
        byte[] encoded = ValueCodec.encodeValue(new float[]{1.0f, 2.0f, -0.5f});
        assertEquals(ValueCodec.TAG_VECTOR, encoded[0] & 0xff);
        ByteBuffer buf = ByteBuffer.wrap(encoded).order(ByteOrder.LITTLE_ENDIAN);
        assertEquals(3, buf.getInt(1));
        assertEquals(1.0f, buf.getFloat(5));
        assertEquals(2.0f, buf.getFloat(9));
        assertEquals(-0.5f, buf.getFloat(13));
    }

    @Test
    void encodeQueryWithParamsPayload() {
        byte[] encoded = ValueCodec.encodeQueryWithParams("Q", new Object[]{42, "x", null});
        assertEquals(1, ByteBuffer.wrap(encoded, 0, 4).order(ByteOrder.LITTLE_ENDIAN).getInt());
        assertEquals('Q', encoded[4]);
        assertEquals(3, ByteBuffer.wrap(encoded, 5, 4).order(ByteOrder.LITTLE_ENDIAN).getInt());
        assertEquals(ValueCodec.TAG_INT, encoded[9] & 0xff);
        assertEquals(42L, ByteBuffer.wrap(encoded, 10, 8).order(ByteOrder.LITTLE_ENDIAN).getLong());
        assertEquals(ValueCodec.TAG_TEXT, encoded[18] & 0xff);
        assertArrayEquals(new byte[]{1, 0, 0, 0, 'x'}, java.util.Arrays.copyOfRange(encoded, 19, 24));
        assertEquals(ValueCodec.TAG_NULL, encoded[24] & 0xff);
        assertEquals(25, encoded.length);
    }

    @Test
    void preparedStatementBindsParams() {
        RecordingConn conn = new RecordingConn();

        conn.prepare("SELECT $1, $2")
            .bind(42)
            .bind(2, "x")
            .query();

        assertEquals("SELECT $1, $2", conn.sql);
        assertArrayEquals(new Object[]{42, "x"}, conn.params);

        conn.prepare("SELECT $1")
            .bind(1, null)
            .query();

        assertEquals("SELECT $1", conn.sql);
        assertArrayEquals(new Object[]{null}, conn.params);
    }

    @Test
    void httpParamsUseJsonEnvelopesForTaggedValues() {
        JsonNode params = ValueCodec.toHttpParams(new Object[]{
            null,
            true,
            42,
            1.5d,
            "txt",
            "hi".getBytes(java.nio.charset.StandardCharsets.UTF_8),
            new float[]{1.0f, 2.0f},
            Map.of("b", 2, "a", 1),
            Instant.ofEpochSecond(1_700_000_000L),
            UUID.fromString("00112233-4455-6677-8899-aabbccddeeff"),
            List.of("json", 1),
        });

        assertTrue(params.get(0).isNull());
        assertTrue(params.get(1).asBoolean());
        assertEquals(42, params.get(2).asInt());
        assertEquals(1.5d, params.get(3).asDouble());
        assertEquals("txt", params.get(4).asText());
        assertEquals("aGk=", params.get(5).get("$bytes").asText());
        assertEquals(1.0d, params.get(6).get(0).asDouble());
        assertEquals(1, params.get(7).get("a").asInt());
        assertEquals(2, params.get(7).get("b").asInt());
        assertEquals(1_700_000_000L, params.get(8).get("$ts").asLong());
        assertEquals("00112233-4455-6677-8899-aabbccddeeff", params.get(9).get("$uuid").asText());
        assertEquals("json", params.get(10).get(0).asText());
    }

    @Test
    void sharedParameterFixturesMatchManifest() throws IOException {
        JsonNode manifest = readFixtureManifest();

        for (JsonNode fixture : manifest.get("values")) {
            String name = fixture.get("name").asText();
            assertEquals(
                fixture.get("redwire_hex").asText(),
                hex(ValueCodec.encodeValue(fixtureValue(name))),
                name
            );
        }

        JsonNode query = manifest.get("queries").get(0);
        List<Object> params = new ArrayList<>();
        for (JsonNode param : query.get("params")) {
            params.add(fixtureValue(param.asText()));
        }

        assertEquals(
            query.get("redwire_hex").asText(),
            hex(ValueCodec.encodeQueryWithParams(query.get("sql").asText(), params.toArray())),
            query.get("name").asText()
        );
    }

    private static JsonNode readFixtureManifest() throws IOException {
        Path path = Path.of("..", "..", "testdata", "conformance", "redwire", "params", "manifest.json");
        return MAPPER.readTree(Files.readString(path));
    }

    private static Object fixtureValue(String name) {
        return switch (name) {
            case "null" -> null;
            case "bool_true" -> true;
            case "bool_false" -> false;
            case "int_min" -> Long.MIN_VALUE;
            case "int_max" -> Long.MAX_VALUE;
            case "int_42" -> 42L;
            case "float_nan" -> Double.longBitsToDouble(0x7ff8000000000000L);
            case "float_pos_inf" -> Double.POSITIVE_INFINITY;
            case "float_neg_inf" -> Double.NEGATIVE_INFINITY;
            case "float_subnormal_min" -> Double.MIN_VALUE;
            case "text_unicode" -> "h\u00e9llo";
            case "text_x" -> "x";
            case "bytes_empty" -> new byte[]{};
            case "bytes_deadbeef" -> new byte[]{(byte) 0xde, (byte) 0xad, (byte) 0xbe, (byte) 0xef};
            case "bytes_256" -> bytes256();
            case "json_nested" -> jsonNestedFixture();
            case "timestamp_zero" -> new ValueCodec.Timestamp(0L);
            case "timestamp_max" -> new ValueCodec.Timestamp(Long.MAX_VALUE);
            case "uuid_001122" -> UUID.fromString("00112233-4455-6677-8899-aabbccddeeff");
            case "vector_empty" -> new float[]{};
            case "vector_three" -> new float[]{1.0f, 2.0f, -0.5f};
            case "vector_128" -> vector128();
            default -> throw new IllegalArgumentException("unknown fixture " + name);
        };
    }

    private static byte[] bytes256() {
        byte[] out = new byte[256];
        for (int i = 0; i < out.length; i++) out[i] = (byte) i;
        return out;
    }

    private static float[] vector128() {
        float[] out = new float[128];
        for (int i = 0; i < out.length; i++) out[i] = (float) i;
        return out;
    }

    private static Map<String, Object> jsonNestedFixture() {
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("z", List.of(1, Map.of("deep", List.of(true, false))));
        out.put("a", null);
        return out;
    }

    private static String hex(byte[] bytes) {
        StringBuilder out = new StringBuilder(bytes.length * 2);
        for (byte b : bytes) {
            out.append(String.format("%02x", b & 0xff));
        }
        return out.toString();
    }

    private static final class RecordingConn implements Conn {
        String sql;
        Object[] params;

        @Override
        public byte[] query(String sql) {
            this.sql = sql;
            this.params = new Object[0];
            return new byte[0];
        }

        @Override
        public byte[] query(String sql, Object... params) {
            this.sql = sql;
            this.params = params;
            return new byte[0];
        }

        @Override
        public void insert(String collection, Object payload) {}

        @Override
        public void bulkInsert(String collection, List<?> rows) {}

        @Override
        public byte[] get(String collection, String id) {
            return new byte[0];
        }

        @Override
        public void delete(String collection, String id) {}

        @Override
        public void ping() {}

        @Override
        public void close() {}
    }
}
