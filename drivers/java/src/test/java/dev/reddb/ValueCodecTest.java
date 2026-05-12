package dev.reddb;

import com.fasterxml.jackson.databind.JsonNode;
import dev.reddb.redwire.ValueCodec;
import org.junit.jupiter.api.Test;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.time.Instant;
import java.util.List;
import java.util.Map;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

class ValueCodecTest {

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
}
