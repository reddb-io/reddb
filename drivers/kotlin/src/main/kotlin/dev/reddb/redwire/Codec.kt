package dev.reddb.redwire

import dev.reddb.RedDBException
import java.lang.reflect.Method

/**
 * Lazy wrapper around `com.github.luben:zstd-jni`. We resolve the
 * methods reflectively so the driver still boots when the jar is
 * absent — only inbound COMPRESSED frames will throw
 * [RedDBException.CompressedButNoZstd].
 */
internal object Codec {
    @Volatile private var available: Boolean? = null
    @Volatile private var compressMethod: Method? = null     // Zstd.compress(byte[], int)
    @Volatile private var decompressMethod: Method? = null   // Zstd.decompress(byte[], int)
    @Volatile private var decompressedSize: Method? = null   // Zstd.decompressedSize(byte[])
    @Volatile private var initFailure: Throwable? = null

    @Synchronized
    private fun init() {
        if (available != null) return
        try {
            val zstd = Class.forName("com.github.luben.zstd.Zstd")
            compressMethod = zstd.getMethod("compress", ByteArray::class.java, Int::class.javaPrimitiveType)
            decompressMethod = zstd.getMethod("decompress", ByteArray::class.java, Int::class.javaPrimitiveType)
            decompressedSize = zstd.getMethod("decompressedSize", ByteArray::class.java)
            available = true
        } catch (t: Throwable) {
            initFailure = t
            available = false
        }
    }

    fun isAvailable(): Boolean {
        if (available == null) init()
        return available == true
    }

    /** Compress a payload at level 1 (matches the engine default). */
    fun compress(plaintext: ByteArray): ByteArray {
        if (available == null) init()
        if (available != true) {
            throw RedDBException.CompressedButNoZstd(
                "zstd-jni not on classpath — cannot encode COMPRESSED frame", initFailure
            )
        }
        val level = parseLevel()
        return try {
            compressMethod!!.invoke(null, plaintext, level) as ByteArray
        } catch (t: Throwable) {
            throw RedDBException.ProtocolError("zstd compress failed: ${t.message}", t)
        }
    }

    /** Decompress a payload. Throws [RedDBException.CompressedButNoZstd] when zstd is absent. */
    fun decompress(compressed: ByteArray): ByteArray {
        if (available == null) init()
        if (available != true) {
            throw RedDBException.CompressedButNoZstd(
                "incoming frame has COMPRESSED flag but zstd-jni isn't loadable", initFailure
            )
        }
        try {
            val size = decompressedSize!!.invoke(null, compressed) as Long
            if (size < 0 || size > Frame.MAX_FRAME_SIZE) {
                throw RedDBException.ProtocolError("zstd decompressed size out of range: $size")
            }
            return decompressMethod!!.invoke(null, compressed, size.toInt()) as ByteArray
        } catch (re: RedDBException) {
            throw re
        } catch (t: Throwable) {
            throw RedDBException.ProtocolError("zstd decompress failed: ${t.message}", t)
        }
    }

    private fun parseLevel(): Int {
        val env = System.getenv("RED_REDWIRE_ZSTD_LEVEL") ?: return 1
        if (env.isEmpty()) return 1
        return try {
            val n = env.trim().toInt()
            if (n < 1 || n > 22) 1 else n
        } catch (e: NumberFormatException) {
            1
        }
    }
}
