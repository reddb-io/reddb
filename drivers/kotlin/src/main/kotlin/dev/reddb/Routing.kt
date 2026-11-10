package dev.reddb

import com.fasterxml.jackson.databind.node.ObjectNode
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.time.Duration.Companion.seconds

public enum class ReadConsistency { STRONG, LOCAL, BOUNDED_STALENESS }
public data class ReadOptions(val consistency: ReadConsistency, val maxLagMs: Long = 0) {
    init { require(maxLagMs >= 0) { "CONSISTENCY_UNSUPPORTED" } }
}
public interface TopologySource {
    public suspend fun topology(protocol: String): ObjectNode?
}

internal class RoutedConn(
    private val seed: String,
    initial: Conn,
    private val protocol: String,
    private val forcePrimary: Boolean,
    private val dial: suspend (String, Boolean) -> Conn,
) : Conn {
    private data class Replica(val address: String, val lag: Long)
    private val mutex = Mutex()
    private val clients = java.util.concurrent.ConcurrentHashMap(mapOf(seed to initial))
    private var primary = seed
    private var replicas = emptyList<Replica>()
    private var pinned: String? = null
    private var session = false
    private var broken = false
    @Volatile private var closed = false
    private var refreshAt = 0L
    private var next = 0L

    internal suspend fun initialize() { refresh() }
    private suspend fun endpoint(address: String): Conn {
        clients[address]?.let { return it }
        val client = withTimeout(5000) { dial(address, false) }
        try { withTimeout(5000) { client.ping() } }
        catch (error: Throwable) { client.close(); throw error }
        synchronized(clients) {
            check(!closed) { client.close(); "connection is closed" }
            clients[address] = client
        }
        return client
    }
    private suspend fun refresh() {
        if (pinned != null || closed) return
        refreshAt = System.nanoTime() + 30.seconds.inWholeNanoseconds
        run {
            val deadline = System.nanoTime() + 5.seconds.inWholeNanoseconds
            val candidates = mutableListOf(primary, seed)
            candidates += replicas.map { it.address }
            var index = 0
            while (index < candidates.size && index < 258 && System.nanoTime() < deadline) {
                val address = candidates[index++]
                var control: Conn? = null
                try {
                    control = withTimeout(5000) { dial(address, true) }
                    val topology = withTimeout(5000) { (control as? TopologySource)?.topology(protocol) } ?: continue
                    if (topology.path("version").asInt() != 1) continue
                    val writer = TopologyCodec.address(topology.path("primary").path("addr").asText(), protocol) ?: continue
                    if (writer !in candidates) candidates += writer
                    endpoint(writer)
                    val ready = mutableListOf<Replica>()
                    for (replica in topology.path("replicas").take(256)) {
                        val target = TopologyCodec.address(replica.path("addr").asText(), protocol) ?: continue
                        if (target == writer || !replica.path("healthy").asBoolean() || replica.path("rebootstrapping").asBoolean()) continue
                        if (System.nanoTime() >= deadline) break
                        try {
                            val remaining = ((deadline - System.nanoTime()) / 1_000_000).coerceAtLeast(1)
                            if (withTimeoutOrNull(remaining) { endpoint(target) } != null) ready += Replica(target, replica.path("lag_ms").asLong(Long.MAX_VALUE))
                        }
                        catch (error: Exception) { currentCoroutineContext().ensureActive() }
                    }
                    currentCoroutineContext().ensureActive()
                    primary = writer; replicas = ready
                    val keep = ready.map { it.address }.toSet() + seed + writer
                    for (retired in clients.keys.toList()) if (retired !in keep) clients.remove(retired)?.close()
                    if (writer == address) return@run
                } catch (error: RedDBException.AuthRefused) { throw error }
                catch (error: Exception) { currentCoroutineContext().ensureActive() }
                finally { control?.close() }
            }
        }
    }
    private fun network(error: Throwable): Boolean {
        var current: Throwable? = error
        repeat(16) {
            val value = current ?: return false
            if (value is java.io.IOException || value is kotlinx.coroutines.CancellationException) return true
            current = value.cause
        }
        return false
    }
    private suspend fun <T> invoke(
        sql: String? = null, declaredRead: Boolean = false, options: ReadOptions? = null,
        operation: suspend (Conn) -> T,
    ): T = mutex.withLock {
        check(!closed && !broken) { "connection is closed" }
        if (pinned == null && System.nanoTime() >= refreshAt) refresh()
        val policy = sql?.let(RoutingSql::policy)
        val read = declaredRead || options != null && policy?.read() == true
        val eligible = if (forcePrimary || options?.consistency == ReadConsistency.STRONG) emptyList()
            else replicas.filter { options?.consistency != ReadConsistency.BOUNDED_STALENESS || it.lag <= options.maxLagMs }
        val address = pinned ?: if (read && eligible.isNotEmpty()) eligible[Math.floorMod(next++, eligible.size.toLong()).toInt()].address else primary
        if (policy?.state() in listOf("begin", "session")) { pinned = address; session = session || policy?.state() == "session" }
        try {
            val result = operation(endpoint(address))
            if (policy?.state() == "end" && !session) pinned = null
            result
        } catch (error: Throwable) {
            if (!network(error)) throw error
            refreshAt = 0
            clients.remove(address)?.close()
            if (pinned != null) broken = true
            currentCoroutineContext().ensureActive()
            if (pinned != null || !read) throw error
            refresh()
            operation(endpoint(primary))
        }
    }
    override suspend fun query(sql: String): ByteArray = query(sql, *emptyArray<Any?>())
    override suspend fun query(sql: String, vararg params: Any?): ByteArray = invoke(sql) { it.query(sql, *params) }
    override suspend fun queryWithOptions(sql: String, options: ReadOptions, vararg params: Any?): ByteArray =
        invoke(sql, options = options) { it.query(sql, *params) }
    override suspend fun insert(collection: String, payload: Any) { invoke { it.insert(collection, payload) } }
    override suspend fun bulkInsert(collection: String, rows: List<Any?>) { invoke { it.bulkInsert(collection, rows) } }
    override suspend fun get(collection: String, id: String): ByteArray = invoke(declaredRead = true) { it.get(collection, id) }
    override suspend fun delete(collection: String, id: String) { invoke { it.delete(collection, id) } }
    override suspend fun ping() { invoke { it.ping() } }
    override fun close() {
        synchronized(clients) {
            closed = true
            clients.values.forEach { it.close() }
            clients.clear()
        }
    }
}
