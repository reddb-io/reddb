package dev.reddb

import com.fasterxml.jackson.databind.JsonNode
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import dev.reddb.redwire.ValueCodec
import kotlinx.coroutines.runBlocking
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.time.Instant
import java.util.UUID

class ValueCodecTest {
    private val mapper = jacksonObjectMapper()


    @Test
    fun valueTagTableIsPinned() {
        assertEquals(0x00, ValueCodec.TAG_NULL)
        assertEquals(0x01, ValueCodec.TAG_BOOL)
        assertEquals(0x02, ValueCodec.TAG_INT)
        assertEquals(0x03, ValueCodec.TAG_FLOAT)
        assertEquals(0x04, ValueCodec.TAG_TEXT)
        assertEquals(0x05, ValueCodec.TAG_BYTES)
        assertEquals(0x06, ValueCodec.TAG_VECTOR)
        assertEquals(0x07, ValueCodec.TAG_JSON)
        assertEquals(0x08, ValueCodec.TAG_TIMESTAMP)
        assertEquals(0x09, ValueCodec.TAG_UUID)
    }

    @Test
    fun encodeScalarValues() {
        assertArrayEquals(byteArrayOf(0x00), ValueCodec.encodeValue(null))
        assertArrayEquals(byteArrayOf(0x01, 0x01), ValueCodec.encodeValue(true))
        assertArrayEquals(byteArrayOf(0x01, 0x00), ValueCodec.encodeValue(false))
        assertArrayEquals(byteArrayOf(0x02, 1, 0, 0, 0, 0, 0, 0, 0), ValueCodec.encodeValue(1))
        assertArrayEquals(
            byteArrayOf(
                0x02,
                0xff.toByte(),
                0xff.toByte(),
                0xff.toByte(),
                0xff.toByte(),
                0xff.toByte(),
                0xff.toByte(),
                0xff.toByte(),
                0xff.toByte(),
            ),
            ValueCodec.encodeValue(-1L),
        )
        assertArrayEquals(byteArrayOf(0x04, 1, 0, 0, 0, 'x'.code.toByte()), ValueCodec.encodeValue("x"))
    }

    @Test
    fun encodeBytesTimestampUuidAndJson() {
        assertArrayEquals(
            byteArrayOf(0x05, 4, 0, 0, 0, 0xde.toByte(), 0xad.toByte(), 0xbe.toByte(), 0xef.toByte()),
            ValueCodec.encodeValue(byteArrayOf(0xde.toByte(), 0xad.toByte(), 0xbe.toByte(), 0xef.toByte())),
        )

        val ts = ValueCodec.encodeValue(Instant.ofEpochSecond(1_700_000_000L))
        assertEquals(ValueCodec.TAG_TIMESTAMP, ts[0].toInt() and 0xff)
        assertEquals(1_700_000_000L, ByteBuffer.wrap(ts, 1, 8).order(ByteOrder.LITTLE_ENDIAN).long)

        val uuid = UUID.fromString("00112233-4455-6677-8899-aabbccddeeff")
        val encodedUuid = ValueCodec.encodeValue(uuid)
        assertEquals(ValueCodec.TAG_UUID, encodedUuid[0].toInt() and 0xff)
        assertArrayEquals(
            byteArrayOf(
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
                0x88.toByte(), 0x99.toByte(), 0xaa.toByte(), 0xbb.toByte(),
                0xcc.toByte(), 0xdd.toByte(), 0xee.toByte(), 0xff.toByte(),
            ),
            encodedUuid.copyOfRange(1, encodedUuid.size),
        )

        val json = ValueCodec.encodeValue(mapOf("b" to 2, "a" to 1))
        assertEquals(ValueCodec.TAG_JSON, json[0].toInt() and 0xff)
        val len = ByteBuffer.wrap(json, 1, 4).order(ByteOrder.LITTLE_ENDIAN).int
        assertEquals("""{"a":1,"b":2}""", String(json, 5, len, StandardCharsets.UTF_8))
    }

    @Test
    fun encodeVectorFromFloatArrayAndFloatList() {
        val encoded = ValueCodec.encodeValue(floatArrayOf(1.0f, 2.0f, -0.5f))
        assertEquals(ValueCodec.TAG_VECTOR, encoded[0].toInt() and 0xff)
        val buf = ByteBuffer.wrap(encoded).order(ByteOrder.LITTLE_ENDIAN)
        assertEquals(3, buf.getInt(1))
        assertEquals(1.0f, buf.getFloat(5))
        assertEquals(2.0f, buf.getFloat(9))
        assertEquals(-0.5f, buf.getFloat(13))

        val fromList = ValueCodec.encodeValue(listOf(1.0f, 2.0f))
        assertEquals(ValueCodec.TAG_VECTOR, fromList[0].toInt() and 0xff)
        assertEquals(2, ByteBuffer.wrap(fromList, 1, 4).order(ByteOrder.LITTLE_ENDIAN).int)
    }

    @Test
    fun encodeQueryWithParamsPayload() {
        val encoded = ValueCodec.encodeQueryWithParams("Q", arrayOf<Any?>(42, "x", null))
        assertEquals(1, ByteBuffer.wrap(encoded, 0, 4).order(ByteOrder.LITTLE_ENDIAN).int)
        assertEquals('Q'.code.toByte(), encoded[4])
        assertEquals(3, ByteBuffer.wrap(encoded, 5, 4).order(ByteOrder.LITTLE_ENDIAN).int)
        assertEquals(ValueCodec.TAG_INT, encoded[9].toInt() and 0xff)
        assertEquals(42L, ByteBuffer.wrap(encoded, 10, 8).order(ByteOrder.LITTLE_ENDIAN).long)
        assertEquals(ValueCodec.TAG_TEXT, encoded[18].toInt() and 0xff)
        assertArrayEquals(byteArrayOf(1, 0, 0, 0, 'x'.code.toByte()), encoded.copyOfRange(19, 24))
        assertEquals(ValueCodec.TAG_NULL, encoded[24].toInt() and 0xff)
        assertEquals(25, encoded.size)
    }

    @Test
    fun preparedQueryBindsParams() = runBlocking {
        val conn = RecordingConn()

        conn.prepare("SELECT \$1, \$2")
            .bind(42)
            .bind(2, "x")
            .query()

        assertEquals("SELECT \$1, \$2", conn.sql)
        assertArrayEquals(arrayOf<Any?>(42, "x"), conn.params)

        conn.prepare("SELECT \$1")
            .bind(1, null)
            .query()

        assertEquals("SELECT \$1", conn.sql)
        assertArrayEquals(arrayOf<Any?>(null), conn.params)
    }

    @Test
    fun httpParamsUseJsonEnvelopesForTaggedValues() {
        val params: JsonNode = ValueCodec.toHttpParams(
            arrayOf<Any?>(
                null,
                true,
                42,
                1.5,
                "txt",
                "hi".toByteArray(StandardCharsets.UTF_8),
                floatArrayOf(1.0f, 2.0f),
                mapOf("b" to 2, "a" to 1),
                Instant.ofEpochSecond(1_700_000_000L),
                UUID.fromString("00112233-4455-6677-8899-aabbccddeeff"),
                listOf("json", 1),
            ),
        )

        assertTrue(params[0].isNull)
        assertTrue(params[1].asBoolean())
        assertEquals(42, params[2].asInt())
        assertEquals(1.5, params[3].asDouble())
        assertEquals("txt", params[4].asText())
        assertEquals("aGk=", params[5]["\$bytes"].asText())
        assertEquals(1.0, params[6][0].asDouble())
        assertEquals(1, params[7]["a"].asInt())
        assertEquals(2, params[7]["b"].asInt())
        assertEquals(1_700_000_000L, params[8]["\$ts"].asLong())
        assertEquals("00112233-4455-6677-8899-aabbccddeeff", params[9]["\$uuid"].asText())
        assertEquals("json", params[10][0].asText())
    }

    @Test
    fun sharedParameterFixturesMatchManifest() {
        val manifest = mapper.readTree(
            Files.readString(Path.of("..", "..", "testdata", "conformance", "redwire", "params", "manifest.json")),
        )

        for (fixture in manifest["values"]) {
            val name = fixture["name"].asText()
            assertEquals(
                fixture["redwire_hex"].asText(),
                ValueCodec.encodeValue(fixtureValue(name)).toHex(),
                name,
            )
        }

        val query = manifest["queries"][0]
        val params = query["params"].map { fixtureValue(it.asText()) }.toTypedArray()
        assertEquals(
            query["redwire_hex"].asText(),
            ValueCodec.encodeQueryWithParams(query["sql"].asText(), params).toHex(),
            query["name"].asText(),
        )
    }

    private fun fixtureValue(name: String): Any? = when (name) {
        "null" -> null
        "bool_true" -> true
        "bool_false" -> false
        "int_min" -> Long.MIN_VALUE
        "int_max" -> Long.MAX_VALUE
        "int_42" -> 42L
        "float_nan" -> Double.fromBits(0x7ff8000000000000L)
        "float_pos_inf" -> Double.POSITIVE_INFINITY
        "float_neg_inf" -> Double.NEGATIVE_INFINITY
        "float_subnormal_min" -> Double.fromBits(1L)
        "text_unicode" -> "h\u00e9llo"
        "text_x" -> "x"
        "bytes_empty" -> byteArrayOf()
        "bytes_deadbeef" -> byteArrayOf(0xde.toByte(), 0xad.toByte(), 0xbe.toByte(), 0xef.toByte())
        "bytes_256" -> ByteArray(256) { it.toByte() }
        "json_nested" -> linkedMapOf<String, Any?>(
            "z" to listOf(1, mapOf("deep" to listOf(true, false))),
            "a" to null,
        )
        "timestamp_zero" -> ValueCodec.Timestamp(0L)
        "timestamp_max" -> ValueCodec.Timestamp(Long.MAX_VALUE)
        "uuid_001122" -> UUID.fromString("00112233-4455-6677-8899-aabbccddeeff")
        "vector_empty" -> floatArrayOf()
        "vector_three" -> floatArrayOf(1.0f, 2.0f, -0.5f)
        "vector_128" -> FloatArray(128) { it.toFloat() }
        else -> throw IllegalArgumentException("unknown fixture $name")
    }

    private fun ByteArray.toHex(): String =
        joinToString("") { "%02x".format(it.toInt() and 0xff) }

    private class RecordingConn : Conn {
        lateinit var sql: String
        lateinit var params: Array<out Any?>

        override suspend fun query(sql: String): ByteArray {
            this.sql = sql
            this.params = emptyArray()
            return byteArrayOf()
        }

        override suspend fun query(sql: String, vararg params: Any?): ByteArray {
            this.sql = sql
            this.params = params
            return byteArrayOf()
        }

        override suspend fun insert(collection: String, payload: Any) {}

        override suspend fun bulkInsert(collection: String, rows: List<Any?>) {}

        override suspend fun get(collection: String, id: String): ByteArray = byteArrayOf()

        override suspend fun delete(collection: String, id: String) {}

        override suspend fun ping() {}

        override fun close() {}
    }
}
