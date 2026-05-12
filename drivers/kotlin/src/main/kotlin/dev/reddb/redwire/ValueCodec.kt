package dev.reddb.redwire

import com.fasterxml.jackson.databind.JsonNode
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.node.ArrayNode
import com.fasterxml.jackson.databind.node.JsonNodeFactory
import com.fasterxml.jackson.databind.node.ObjectNode
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import dev.reddb.RedDBException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.util.Base64
import java.util.UUID

/**
 * Parameter codec for RedWire QueryWithParams payloads.
 *
 * Layout: u32 sql_len | sql utf8 | u32 param_count | encoded values.
 */
public object ValueCodec {
    public const val TAG_NULL: Int = 0x00
    public const val TAG_BOOL: Int = 0x01
    public const val TAG_INT: Int = 0x02
    public const val TAG_FLOAT: Int = 0x03
    public const val TAG_TEXT: Int = 0x04
    public const val TAG_BYTES: Int = 0x05
    public const val TAG_VECTOR: Int = 0x06
    public const val TAG_JSON: Int = 0x07
    public const val TAG_TIMESTAMP: Int = 0x08
    public const val TAG_UUID: Int = 0x09

    public const val MAX_PARAM_COUNT: Int = 65_536
    public const val MAX_VALUE_PAYLOAD_LEN: Int = Frame.MAX_FRAME_SIZE

    private val mapper: ObjectMapper = jacksonObjectMapper()

    public fun encodeQueryWithParams(sql: String, params: Array<out Any?>?): ByteArray {
        val values = params ?: emptyArray()
        require(values.size <= MAX_PARAM_COUNT) {
            "param_count ${values.size} > $MAX_PARAM_COUNT"
        }
        val sqlBytes = sql.toByteArray(StandardCharsets.UTF_8)
        require(sqlBytes.size <= MAX_VALUE_PAYLOAD_LEN) {
            "sql_len ${sqlBytes.size} > $MAX_VALUE_PAYLOAD_LEN"
        }

        val encoded = ArrayList<ByteArray>(values.size)
        var total = 4 + sqlBytes.size + 4
        for (i in values.indices) {
            try {
                val value = encodeValue(values[i])
                encoded.add(value)
                total += value.size
            } catch (e: RuntimeException) {
                throw IllegalArgumentException("param[$i]: ${e.message}", e)
            }
        }

        val out = ByteBuffer.allocate(total).order(ByteOrder.LITTLE_ENDIAN)
        out.putInt(sqlBytes.size)
        out.put(sqlBytes)
        out.putInt(encoded.size)
        for (value in encoded) out.put(value)
        return out.array()
    }

    public fun encodeValue(value: Any?): ByteArray {
        return when (value) {
            null -> byteArrayOf(TAG_NULL.toByte())
            is Boolean -> byteArrayOf(TAG_BOOL.toByte(), if (value) 1.toByte() else 0.toByte())
            is Byte -> encodeI64(value.toLong(), TAG_INT)
            is Short -> encodeI64(value.toLong(), TAG_INT)
            is Int -> encodeI64(value.toLong(), TAG_INT)
            is Long -> encodeI64(value, TAG_INT)
            is Float -> encodeF64(value.toDouble())
            is Double -> encodeF64(value)
            is String -> encodeLenPrefixed(TAG_TEXT, value.toByteArray(StandardCharsets.UTF_8))
            is ByteArray -> encodeLenPrefixed(TAG_BYTES, value)
            is FloatArray -> encodeVector(value)
            is Instant -> encodeI64(value.epochSecond, TAG_TIMESTAMP)
            is UUID -> encodeUuid(value)
            is JsonNode -> encodeJson(value)
            is Map<*, *> -> encodeJson(value)
            is List<*> -> if (isFloatList(value)) {
                encodeVector(FloatArray(value.size) { (value[it] as Float) })
            } else {
                encodeJson(value)
            }
            else -> throw IllegalArgumentException("unsupported param type ${value::class.java.name}")
        }
    }

    public fun toHttpParams(params: Array<out Any?>?): ArrayNode {
        val values = params ?: emptyArray()
        val out = JsonNodeFactory.instance.arrayNode()
        for (i in values.indices) {
            try {
                out.add(toHttpParam(values[i]))
            } catch (e: RuntimeException) {
                throw IllegalArgumentException("param[$i]: ${e.message}", e)
            }
        }
        return out
    }

    private fun toHttpParam(value: Any?): JsonNode {
        return when (value) {
            null -> JsonNodeFactory.instance.nullNode()
            is Boolean -> JsonNodeFactory.instance.booleanNode(value)
            is Byte -> JsonNodeFactory.instance.numberNode(value.toLong())
            is Short -> JsonNodeFactory.instance.numberNode(value.toLong())
            is Int -> JsonNodeFactory.instance.numberNode(value.toLong())
            is Long -> JsonNodeFactory.instance.numberNode(value)
            is Float -> JsonNodeFactory.instance.numberNode(value.toDouble())
            is Double -> JsonNodeFactory.instance.numberNode(value)
            is String -> JsonNodeFactory.instance.textNode(value)
            is ByteArray -> JsonNodeFactory.instance.objectNode().apply {
                put("\$bytes", Base64.getEncoder().encodeToString(value))
            }
            is FloatArray -> vectorNode(value)
            is Instant -> JsonNodeFactory.instance.objectNode().apply {
                put("\$ts", value.epochSecond)
            }
            is UUID -> JsonNodeFactory.instance.objectNode().apply {
                put("\$uuid", value.toString())
            }
            is JsonNode -> canonicalize(value)
            is Map<*, *> -> canonicalize(value)
            is List<*> -> if (isFloatList(value)) {
                vectorNode(FloatArray(value.size) { (value[it] as Float) })
            } else {
                canonicalize(value)
            }
            else -> throw IllegalArgumentException("unsupported param type ${value::class.java.name}")
        }
    }

    private fun encodeI64(value: Long, tag: Int): ByteArray {
        val out = ByteBuffer.allocate(9).order(ByteOrder.LITTLE_ENDIAN)
        out.put(tag.toByte())
        out.putLong(value)
        return out.array()
    }

    private fun encodeF64(value: Double): ByteArray {
        val out = ByteBuffer.allocate(9).order(ByteOrder.LITTLE_ENDIAN)
        out.put(TAG_FLOAT.toByte())
        out.putDouble(value)
        return out.array()
    }

    private fun encodeLenPrefixed(tag: Int, bytes: ByteArray): ByteArray {
        require(bytes.size <= MAX_VALUE_PAYLOAD_LEN) {
            "value len ${bytes.size} > $MAX_VALUE_PAYLOAD_LEN"
        }
        val out = ByteBuffer.allocate(1 + 4 + bytes.size).order(ByteOrder.LITTLE_ENDIAN)
        out.put(tag.toByte())
        out.putInt(bytes.size)
        out.put(bytes)
        return out.array()
    }

    private fun encodeVector(values: FloatArray): ByteArray {
        val bytes = values.size * 4
        require(bytes <= MAX_VALUE_PAYLOAD_LEN) {
            "vector bytes $bytes > $MAX_VALUE_PAYLOAD_LEN"
        }
        val out = ByteBuffer.allocate(1 + 4 + bytes).order(ByteOrder.LITTLE_ENDIAN)
        out.put(TAG_VECTOR.toByte())
        out.putInt(values.size)
        for (value in values) out.putFloat(value)
        return out.array()
    }

    private fun encodeUuid(uuid: UUID): ByteArray {
        val out = ByteBuffer.allocate(17).order(ByteOrder.BIG_ENDIAN)
        out.put(TAG_UUID.toByte())
        out.putLong(uuid.mostSignificantBits)
        out.putLong(uuid.leastSignificantBits)
        return out.array()
    }

    private fun encodeJson(value: Any?): ByteArray =
        encodeLenPrefixed(TAG_JSON, canonicalJson(value).toByteArray(StandardCharsets.UTF_8))

    private fun canonicalJson(value: Any?): String {
        return try {
            mapper.writeValueAsString(canonicalize(value))
        } catch (e: Exception) {
            throw RedDBException.ProtocolError("json param encode failed: ${e.message}", e)
        }
    }

    private fun canonicalize(value: Any?): JsonNode {
        return when (value) {
            null -> JsonNodeFactory.instance.nullNode()
            is JsonNode -> {
                when {
                    value.isObject -> {
                        val out = JsonNodeFactory.instance.objectNode()
                        val names = value.fieldNames().asSequence().toList().sorted()
                        for (name in names) out.set<JsonNode>(name, canonicalize(value.get(name)))
                        out
                    }
                    value.isArray -> {
                        val out = JsonNodeFactory.instance.arrayNode()
                        for (item in value) out.add(canonicalize(item))
                        out
                    }
                    else -> value
                }
            }
            is Map<*, *> -> {
                val out: ObjectNode = JsonNodeFactory.instance.objectNode()
                value.entries
                    .sortedBy { it.key.toString() }
                    .forEach { out.set<JsonNode>(it.key.toString(), canonicalize(it.value)) }
                out
            }
            is List<*> -> {
                val out = JsonNodeFactory.instance.arrayNode()
                for (item in value) out.add(canonicalize(item))
                out
            }
            else -> mapper.valueToTree(value)
        }
    }

    private fun vectorNode(values: FloatArray): ArrayNode {
        val out = JsonNodeFactory.instance.arrayNode()
        for (value in values) out.add(value.toDouble())
        return out
    }

    private fun isFloatList(value: List<*>): Boolean =
        value.all { it is Float }
}
