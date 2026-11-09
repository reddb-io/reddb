package dev.reddb.embedded

import com.fasterxml.jackson.databind.node.ObjectNode
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import com.sun.jna.Library
import com.sun.jna.Native
import com.sun.jna.Pointer
import com.sun.jna.ptr.PointerByReference
import dev.reddb.Conn
import dev.reddb.RedDBException

/** Coroutine-shaped Kotlin facade over the versioned RedDB embedded C ABI. */
public class EmbeddedConn private constructor(
    private val api: NativeApi,
    private var handle: Pointer?,
) : Conn {
    private interface NativeApi : Library {
        fun reddb_abi_version(): Int
        fun reddb_open(path: String, handle: PointerByReference, error: PointerByReference): Int
        fun reddb_invoke(
            handle: Pointer,
            method: String,
            params: String,
            result: PointerByReference,
            error: PointerByReference,
        ): Int
        fun reddb_close(handle: Pointer, error: PointerByReference): Int
        fun reddb_string_free(value: Pointer)
    }

    private val lock = Any()

    public companion object {
        private const val ABI_VERSION: Int = 1
        private val mapper = jacksonObjectMapper()
        @Volatile private var loadedApi: NativeApi? = null

        public fun open(path: String?): EmbeddedConn {
            val api = nativeApi()
            val handle = PointerByReference()
            val error = PointerByReference()
            val status = api.reddb_open(path ?: "", handle, error)
            val message = takeString(api, error.value)
            if (status != 0) {
                throw RedDBException.EngineError(
                    message.ifEmpty { "reddb_open failed with status $status" }
                )
            }
            val pointer = handle.value
                ?: throw RedDBException.ProtocolError("reddb_open returned a null handle")
            return EmbeddedConn(api, pointer)
        }

        @Synchronized
        private fun nativeApi(): NativeApi {
            loadedApi?.let { return it }
            val path = System.getenv("REDDB_FFI_LIBRARY")?.takeIf { it.isNotBlank() } ?: "reddb_ffi"
            try {
                val api = Native.load(path, NativeApi::class.java)
                val version = api.reddb_abi_version()
                if (version != ABI_VERSION) {
                    throw RedDBException.ProtocolError(
                        "incompatible RedDB embedded ABI: expected $ABI_VERSION, found $version"
                    )
                }
                loadedApi = api
                return api
            } catch (e: RedDBException) {
                throw e
            } catch (e: Throwable) {
                throw RedDBException.ProtocolError(
                    "embedded native library unavailable: ${e.message}", e
                )
            }
        }

        private fun takeString(api: NativeApi, pointer: Pointer?): String {
            if (pointer == null) return ""
            return try {
                pointer.getString(0, Charsets.UTF_8.name())
            } finally {
                api.reddb_string_free(pointer)
            }
        }
    }

    private fun invoke(method: String, params: ObjectNode): ByteArray = synchronized(lock) {
        val current = handle ?: throw IllegalStateException("embedded connection is closed")
        try {
            val result = PointerByReference()
            val error = PointerByReference()
            val status = api.reddb_invoke(current, method, mapper.writeValueAsString(params), result, error)
            val message = takeString(api, error.value)
            if (status != 0) {
                result.value?.let(api::reddb_string_free)
                throw RedDBException.EngineError(
                    message.ifEmpty { "reddb_invoke failed with status $status" }
                )
            }
            val pointer = result.value
                ?: throw RedDBException.ProtocolError("reddb_invoke returned a null result")
            takeString(api, pointer).toByteArray(Charsets.UTF_8)
        } catch (e: RedDBException) {
            throw e
        } catch (e: Throwable) {
            throw RedDBException.ProtocolError("embedded call failed: ${e.message}", e)
        }
    }

    override suspend fun query(sql: String): ByteArray = query(sql, *emptyArray<Any?>())

    override suspend fun query(sql: String, vararg params: Any?): ByteArray {
        val body = mapper.createObjectNode().put("sql", sql)
        if (params.isNotEmpty()) body.set<com.fasterxml.jackson.databind.JsonNode>(
            "params", mapper.valueToTree(params)
        )
        return invoke("query", body)
    }

    override suspend fun insert(collection: String, payload: Any) {
        invoke("insert", mapper.createObjectNode().apply {
            put("collection", collection)
            set<com.fasterxml.jackson.databind.JsonNode>("payload", mapper.valueToTree(payload))
        })
    }

    override suspend fun bulkInsert(collection: String, rows: List<Any?>) {
        invoke("bulk_insert", mapper.createObjectNode().apply {
            put("collection", collection)
            set<com.fasterxml.jackson.databind.JsonNode>("payloads", mapper.valueToTree(rows))
        })
    }

    override suspend fun get(collection: String, id: String): ByteArray =
        invoke("get", mapper.createObjectNode().put("collection", collection).put("id", id))

    override suspend fun delete(collection: String, id: String) {
        invoke("delete", mapper.createObjectNode().put("collection", collection).put("id", id))
    }

    override suspend fun ping() { invoke("health", mapper.createObjectNode()) }

    override fun close(): Unit = synchronized(lock) {
        val current = handle ?: return@synchronized
        handle = null
        val error = PointerByReference()
        val status = api.reddb_close(current, error)
        val message = takeString(api, error.value)
        if (status != 0) {
            throw RedDBException.EngineError(
                message.ifEmpty { "reddb_close failed with status $status" }
            )
        }
    }
}
