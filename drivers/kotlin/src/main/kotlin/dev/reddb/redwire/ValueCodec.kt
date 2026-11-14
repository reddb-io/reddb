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
    public const val TAG_FINGERPRINT: Int = 0x0a
    public const val TAG_FACE: Int = 0x0b
    public const val TAG_IRIS: Int = 0x0c
    public const val TAG_FEATURE_VECTOR: Int = 0x0d
    public const val TAG_BIT_SIGNATURE: Int = 0x0e

    public const val MAX_PARAM_COUNT: Int = 65_536
    public const val MAX_VALUE_PAYLOAD_LEN: Int = Frame.MAX_FRAME_SIZE
    public const val MAX_FEATURE_VECTOR_DIMENSIONS: Int = 65_536
    public const val MAX_BIT_SIGNATURE_BYTES: Int = 1 shl 20

    private val mapper: ObjectMapper = jacksonObjectMapper()

    public data class Timestamp(public val seconds: Long)

    public fun requiresBiometricCapability(value: Any?): Boolean {
        if (value !is JsonNode && value !is Map<*, *>) return false
        val node = canonicalize(value)
        return node.isObject &&
            (node.has("\$biometric") || node.has("\$feature_vector") || node.has("\$bit_signature"))
    }

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
            is Timestamp -> encodeI64(value.seconds, TAG_TIMESTAMP)
            is UUID -> encodeUuid(value)
            is JsonNode -> encodeStructured(value)
            is Map<*, *> -> encodeStructured(value)
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
            is Timestamp -> JsonNodeFactory.instance.objectNode().apply {
                put("\$ts", value.seconds)
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

    private fun encodeStructured(value: Any?): ByteArray {
        val node = canonicalize(value)
        if (node.isObject && node.size() == 1) {
            if (node.has("\$biometric")) return encodeBiometric(node.get("\$biometric"))
            if (node.has("\$feature_vector")) return encodeFeatureVector(node.get("\$feature_vector"))
            if (node.has("\$bit_signature")) return encodeBitSignature(node.get("\$bit_signature"))
        }
        return encodeJson(value)
    }

    private fun encodeBiometric(value: JsonNode?): ByteArray {
        require(value != null && value.isObject) { "\$biometric must be an object" }
        val modality = requiredText(value, "modality")
        val format = biometricFormat(requiredText(value, "format"))
        val expected = when (modality) {
            "fingerprint" -> 1
            "face" -> 2
            "iris" -> 3
            else -> 0
        }
        require(format != 0 && expected != 0 && format ushr 8 == expected) {
            "invalid biometric modality or format"
        }
        val data = decodeHex(requiredText(value, "data"), "biometric data")
        require(data.isNotEmpty() && data.size <= MAX_VALUE_PAYLOAD_LEN) {
            "invalid biometric payload length"
        }
        val tag = when (modality) {
            "fingerprint" -> TAG_FINGERPRINT
            "face" -> TAG_FACE
            "iris" -> TAG_IRIS
            else -> error("validated modality")
        }
        return ByteBuffer.allocate(1 + 2 + 4 + data.size)
            .order(ByteOrder.LITTLE_ENDIAN)
            .put(tag.toByte())
            .putShort(format.toShort())
            .putInt(data.size)
            .put(data)
            .array()
    }

    private fun encodeFeatureVector(value: JsonNode?): ByteArray {
        require(value != null && value.isObject && value.path("values").isArray) {
            "\$feature_vector must contain values"
        }
        val values = value.get("values")
        require(values.size() in 1..MAX_FEATURE_VECTOR_DIMENSIONS) {
            "invalid feature vector dimension"
        }
        val space = decodeSpaceId(requiredText(value, "space_id"))
        val out = ByteBuffer.allocate(1 + 16 + 4 + values.size() * 4)
            .order(ByteOrder.LITTLE_ENDIAN)
        out.put(TAG_FEATURE_VECTOR.toByte()).put(space).putInt(values.size())
        var norm = 0.0
        for (coordinateNode in values) {
            require(coordinateNode.isNumber) { "feature vector coordinates must be numeric" }
            val coordinate = coordinateNode.floatValue()
            require(coordinate.isFinite()) { "feature vector coordinates must be finite" }
            norm += coordinate.toDouble() * coordinate
            out.putFloat(coordinate)
        }
        require(norm != 0.0) { "feature vector norm must be non-zero" }
        return out.array()
    }

    private fun encodeBitSignature(value: JsonNode?): ByteArray {
        require(value != null && value.isObject) { "\$bit_signature must be an object" }
        val space = decodeSpaceId(requiredText(value, "space_id"))
        val bits = decodeHex(requiredText(value, "bits"), "bit signature bits")
        val mask = if (value.has("mask")) {
            decodeHex(requiredText(value, "mask"), "bit signature mask")
        } else {
            ByteArray(bits.size) { 0xff.toByte() }
        }
        require(
            bits.isNotEmpty() && bits.size <= MAX_BIT_SIGNATURE_BYTES &&
                mask.size == bits.size && mask.any { it.toInt() != 0 }
        ) { "invalid bit signature length or mask" }
        return ByteBuffer.allocate(1 + 16 + 4 + bits.size * 2)
            .order(ByteOrder.LITTLE_ENDIAN)
            .put(TAG_BIT_SIGNATURE.toByte())
            .put(space)
            .putInt(bits.size)
            .put(bits)
            .put(mask)
            .array()
    }

    private fun requiredText(value: JsonNode, field: String): String {
        val child = value.get(field)
        require(child != null && child.isTextual) { "typed parameter field '$field' must be text" }
        return child.asText()
    }

    private fun decodeSpaceId(value: String): ByteArray {
        val bytes = decodeHex(value.replace("-", ""), "space_id")
        require(bytes.size == 16) { "space_id must contain 16 bytes" }
        return bytes
    }

    private fun decodeHex(value: String, field: String): ByteArray {
        require(value.length % 2 == 0) { "$field must be even-length hexadecimal text" }
        return ByteArray(value.length / 2) { index ->
            val high = value[index * 2].digitToIntOrNull(16)
            val low = value[index * 2 + 1].digitToIntOrNull(16)
            require(high != null && low != null) { "$field must be hexadecimal text" }
            ((high shl 4) or low).toByte()
        }
    }

    private fun biometricFormat(name: String): Int = when (name.lowercase()) {
        "iso/iec 39794-2" -> 0x0101
        "iso/iec 39794-4" -> 0x0102
        "iso/iec 19794-2" -> 0x0103
        "iso/iec 19794-4" -> 0x0104
        "ansi incits 378" -> 0x0105
        "ansi incits 381" -> 0x0106
        "ansi/nist-itl type-4" -> 0x0107
        "ansi/nist-itl type-9" -> 0x0108
        "cbeff fingerprint" -> 0x0109
        "iso/iec 39794-5" -> 0x0201
        "iso/iec 19794-5" -> 0x0202
        "ansi/nist-itl type-10" -> 0x0203
        "cbeff face" -> 0x0204
        "iso/iec 39794-6" -> 0x0301
        "iso/iec 19794-6" -> 0x0302
        "ansi/nist-itl type-17" -> 0x0303
        "cbeff iris" -> 0x0304
        else -> 0
    }

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
