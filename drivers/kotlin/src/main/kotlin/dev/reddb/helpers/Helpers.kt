package dev.reddb.helpers

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import dev.reddb.Conn

/**
 * SDK Helper Spec v0.1 — rich helper surface on top of any transport
 * exposing a [Querier]. Mirrors `drivers/go/helpers.go` 1:1.
 *
 * See `docs/clients/sdk-helper-spec.md`.
 */

/** Minimal contract helpers need. Tests pass fakes that record SQL. */
public fun interface Querier {
    /** Run a SQL statement; return the engine's JSON envelope bytes. */
    public suspend fun query(sql: String, vararg params: Any?): ByteArray
}

/** Typed helper errors mirroring Go/Java/.NET/PHP. */
public sealed class HelperException(message: String) : RuntimeException(message) {
    public class InvalidArgument(message: String) : HelperException(message)
    public class NotFound(message: String) : HelperException(message)
    public class InvalidResponse(message: String) : HelperException(message)
}

// --- Envelopes --------------------------------------------------------------

public data class InsertResult(
    val affected: Long,
    val rid: String,
    val item: Map<String, Any?>?,
)

public data class DeleteResult(val affected: Long)
public data class ExistsResult(val exists: Boolean)
public data class ListResult(
    val items: List<Map<String, Any?>>,
    val nextCursor: String? = null,
)
public data class QueuePushResult(val affected: Long, val rid: String? = null)

// --- Entry point ------------------------------------------------------------

public class Helpers(private val q: Querier) {
    public fun documents(): DocumentClient = DocumentClient(q)
    public fun kv(collection: String = "kv_default"): KvClient = KvClient(q, collection)
    public fun queue(): QueueClient = QueueClient(q)

    public companion object {
        /** Adapt a [Conn] into a [Helpers] instance. */
        public fun of(conn: Conn): Helpers = Helpers(Querier { sql, params ->
            if (params.isEmpty()) conn.query(sql) else conn.query(sql, *params)
        })
    }
}

// --- Documents --------------------------------------------------------------

public class DocumentClient internal constructor(private val q: Querier) {
    public data class ListOptions(
        val limit: Int = 0,
        val orderBy: String? = null,
        val filter: String? = null,
    )

    public suspend fun insert(collection: String, document: Map<String, Any?>?): InsertResult {
        if (document == null) {
            throw HelperException.InvalidArgument("documents.insert document must be an object")
        }
        ensureCollection(collection)
        val sql = "INSERT INTO ${Sql.identifierPath(collection)} DOCUMENT VALUES " +
            "(${Sql.jsonInlineLiteral(document)}) RETURNING *"
        val body = q.query(sql)
        val (row, affectedRaw) = Sql.firstRow(body)
        if (row == null || row["rid"] == null) {
            throw HelperException.InvalidResponse(
                "documents.insert expected one returned item with rid")
        }
        val affected = if (affectedRaw == 0L) 1L else affectedRaw
        return InsertResult(affected, Sql.ridString(row["rid"])!!, row)
    }

    public suspend fun get(collection: String, rid: String): Map<String, Any?> {
        val sql = "SELECT * FROM ${Sql.identifierPath(collection)} WHERE rid = \$1 LIMIT 1"
        val body = q.query(sql, rid)
        val (row, _) = Sql.firstRow(body)
        return row ?: throw HelperException.NotFound("document \"$rid\" was not found")
    }

    public suspend fun list(collection: String, opts: ListOptions = ListOptions()): ListResult {
        val limit = Sql.normalizeLimit(opts.limit)
        val order = opts.orderBy?.takeIf { it.isNotEmpty() } ?: "rid ASC"
        val where = opts.filter?.takeIf { it.isNotEmpty() }?.let { " WHERE $it" } ?: ""
        val sql = "SELECT * FROM ${Sql.identifierPath(collection)}$where ORDER BY $order LIMIT $limit"
        val body = q.query(sql)
        return ListResult(Sql.allRows(body))
    }

    public suspend fun patch(collection: String, rid: String, patch: Map<String, Any?>?): Map<String, Any?> {
        if (patch == null) {
            throw HelperException.InvalidArgument("documents.patch patch must be an object")
        }
        if (patch.isEmpty()) return get(collection, rid)
        val parts = patch.entries.map { (k, v) ->
            if ('/' in k) {
                throw HelperException.InvalidArgument(
                    "documents.patch currently accepts top-level document fields")
            }
            "${Sql.identifier(k)} = ${Sql.valueLiteral(v)}"
        }
        val sql = "UPDATE ${Sql.identifierPath(collection)} SET " +
            parts.joinToString(", ") + " WHERE rid = \$1 RETURNING *"
        val body = q.query(sql, rid)
        val (row, _) = Sql.firstRow(body)
        return row ?: throw HelperException.NotFound("document \"$rid\" was not found")
    }

    public suspend fun delete(collection: String, rid: String): DeleteResult {
        val sql = "DELETE FROM ${Sql.identifierPath(collection)} WHERE rid = \$1"
        val body = q.query(sql, rid)
        return DeleteResult(Sql.affectedFromBody(body))
    }

    private suspend fun ensureCollection(collection: String) {
        try {
            q.query("CREATE DOCUMENT ${Sql.identifierPath(collection)}")
        } catch (e: Exception) {
            if (e.message?.contains("already exists") != true) throw e
        }
    }
}

// --- KV ---------------------------------------------------------------------

public class KvClient internal constructor(
    private val q: Querier,
    public val collection: String,
) {
    public data class SetOptions(
        val collection: String? = null,
        val tags: List<String>? = null,
        val expireMs: Long = 0L,
    )

    public data class ListOpts(
        val collection: String? = null,
        val limit: Int = 0,
        val prefix: String? = null,
    )

    public suspend fun set(key: String, value: Any?, opts: SetOptions = SetOptions()): Unit =
        put(key, value, opts)

    public suspend fun put(key: String, value: Any?, opts: SetOptions = SetOptions()) {
        val coll = opts.collection?.takeIf { it.isNotEmpty() } ?: collection
        val lit = Sql.kvValueLiteral(value)
        val expire = if (opts.expireMs > 0) " EXPIRE ${opts.expireMs} ms" else ""
        val tagClause = opts.tags?.takeIf { it.isNotEmpty() }?.let { tags ->
            " TAGS [" + tags.joinToString(", ") { Sql.kvTagLiteral(it) } + "]"
        } ?: ""
        val path = Sql.kvPath(coll, key)
        q.query("KV PUT $path = $lit$expire$tagClause")
    }

    public suspend fun get(key: String, collection: String? = null): Any? {
        val coll = collection?.takeIf { it.isNotEmpty() } ?: this.collection
        val path = Sql.kvPath(coll, key)
        val body = q.query("KV GET $path")
        val (row, _) = Sql.firstRow(body)
        return row?.get("value")
    }

    public suspend fun exists(key: String, collection: String? = null): ExistsResult =
        ExistsResult(get(key, collection) != null)

    public suspend fun delete(key: String, collection: String? = null): DeleteResult {
        val coll = collection?.takeIf { it.isNotEmpty() } ?: this.collection
        val path = Sql.kvPath(coll, key)
        val body = q.query("KV DELETE $path")
        return DeleteResult(Sql.affectedFromBody(body))
    }

    public suspend fun list(opts: ListOpts = ListOpts()): ListResult {
        val coll = opts.collection?.takeIf { it.isNotEmpty() } ?: collection
        val limit = Sql.normalizeLimit(opts.limit)
        val sql = "SELECT key, value FROM ${Sql.identifier(coll)} ORDER BY key ASC LIMIT $limit"
        val body = q.query(sql)
        var rows = Sql.allRows(body)
        opts.prefix?.takeIf { it.isNotEmpty() }?.let { p ->
            rows = rows.filter { (it["key"] as? String)?.startsWith(p) == true }
        }
        return ListResult(rows)
    }
}

// --- Queue ------------------------------------------------------------------

public class QueueClient internal constructor(private val q: Querier) {
    public data class PushOptions(val priority: Int? = null)

    public suspend fun push(
        queue: String,
        value: Any?,
        opts: PushOptions = PushOptions(),
    ): QueuePushResult {
        Sql.assertIdentifier(queue, "queue name")
        val lit = Sql.queueValueLiteral(value)
        val priority = opts.priority?.let { " PRIORITY $it" } ?: ""
        val sql = "QUEUE PUSH ${Sql.identifier(queue)} $lit$priority"
        val body = q.query(sql)
        var affected = Sql.affectedFromBody(body)
        if (affected == 0L) affected = 1L
        val (row, _) = Sql.firstRow(body)
        val rid = row?.get("rid")?.let { Sql.ridString(it) }
        return QueuePushResult(affected, rid)
    }

    public suspend fun pop(queue: String, count: Int? = null): List<Any?> =
        fetch("POP", queue, count)

    public suspend fun peek(queue: String, count: Int? = null): List<Any?> =
        fetch("PEEK", queue, count)

    private suspend fun fetch(verb: String, queue: String, count: Int?): List<Any?> {
        Sql.assertIdentifier(queue, "queue name")
        if (count != null && count < 0) {
            throw HelperException.InvalidArgument("queue count must be a non-negative integer")
        }
        val suffix = if (count != null) " COUNT $count" else ""
        val body = q.query("QUEUE $verb ${Sql.identifier(queue)}$suffix")
        return Sql.allRows(body).map { it["payload"] }
    }

    public suspend fun len(queue: String): Long {
        Sql.assertIdentifier(queue, "queue name")
        val body = q.query("QUEUE LEN ${Sql.identifier(queue)}")
        val (row, _) = Sql.firstRow(body)
        return when (val v = row?.get("len")) {
            null -> 0L
            is Number -> v.toLong()
            else -> 0L
        }
    }

    public suspend fun purge(queue: String): DeleteResult {
        Sql.assertIdentifier(queue, "queue name")
        val body = q.query("QUEUE PURGE ${Sql.identifier(queue)}")
        return DeleteResult(Sql.affectedFromBody(body))
    }

    public data class ReadWaitOptions(val group: String? = null, val count: Int? = null)

    /**
     * Live `QUEUE READ … WAIT <ms>` helper (PRD #718 / #725). Blocks
     * until a message is available for [consumer] on [queue], the
     * [wait] budget elapses, or the server cancels. Timeout returns
     * an empty list — same shape as an empty [pop]; never throws.
     * [wait] is required; there is no infinite-wait default.
     * Cancellation and cap rejection surface via the transport.
     */
    public suspend fun readWait(
        queue: String,
        consumer: String,
        wait: kotlin.time.Duration,
        opts: ReadWaitOptions = ReadWaitOptions(),
    ): List<Any?> {
        Sql.assertIdentifier(queue, "queue name")
        Sql.assertIdentifier(consumer, "consumer name")
        if (wait.isNegative()) {
            throw HelperException.InvalidArgument(
                "queue readWait requires a non-negative wait duration (no infinite wait)"
            )
        }
        val groupClause = opts.group?.takeIf { it.isNotEmpty() }?.let { g ->
            Sql.assertIdentifier(g, "group name")
            " GROUP ${Sql.identifier(g)}"
        } ?: ""
        val countClause = opts.count?.let { c ->
            if (c < 0) throw HelperException.InvalidArgument(
                "queue count must be a non-negative integer"
            )
            " COUNT $c"
        } ?: ""
        val waitMs = wait.inWholeMilliseconds
        val sql = "QUEUE READ ${Sql.identifier(queue)}$groupClause CONSUMER ${Sql.identifier(consumer)}$countClause WAIT ${waitMs}ms"
        val body = q.query(sql)
        return Sql.allRows(body).map { it["payload"] }
    }
}

// --- pure SQL helpers + envelope parsing (unit-testable) --------------------

internal object Sql {
    private val mapper: ObjectMapper = jacksonObjectMapper()

    fun kvPath(collection: String, key: String): String {
        for (ch in collection) {
            if (!isIdentChar(ch)) {
                throw HelperException.InvalidArgument(
                    "invalid KV collection \"$collection\": character \"$ch\" is not supported")
            }
        }
        return collection + "." + kvKeySegment(key)
    }

    fun kvKeySegment(value: String): String =
        if (value.isNotEmpty() && allIdentChars(value)) value
        else "'" + value.replace("'", "''") + "'"

    fun kvValueLiteral(value: Any?): String = when (value) {
        null -> "NULL"
        is Boolean -> if (value) "true" else "false"
        is String -> "'" + value.replace("'", "''") + "'"
        is Byte, is Short, is Int, is Long -> value.toString()
        is Float, is Double -> value.toString()
        else -> "'" + mapper.writeValueAsString(value).replace("'", "''") + "'"
    }

    fun kvTagLiteral(tag: String): String = "'" + tag.replace("'", "''") + "'"

    fun queueValueLiteral(value: Any?): String = when (value) {
        null -> "NULL"
        is Boolean -> if (value) "true" else "false"
        is String -> "'" + value.replace("'", "''") + "'"
        is Byte, is Short, is Int, is Long -> value.toString()
        is Float, is Double -> value.toString()
        else -> mapper.writeValueAsString(value)
    }

    fun valueLiteral(value: Any?): String = kvValueLiteral(value)

    // ADR 0067 (#1709): a document body is written as an inline strict-JSON
    // literal (no surrounding quotes) — the quoted-string coercion is removed.
    fun jsonInlineLiteral(value: Any?): String = mapper.writeValueAsString(value)

    fun identifier(value: String): String =
        if (value.isNotEmpty() && allIdentChars(value)) value
        else "\"" + value.replace("\"", "\"\"") + "\""

    fun identifierPath(value: String): String =
        if ('.' !in value) identifier(value)
        else value.split('.').joinToString(".") { identifier(it) }

    fun assertIdentifier(value: String, label: String) {
        if (value.isEmpty() || !allIdentChars(value)) {
            throw HelperException.InvalidArgument(
                "invalid $label \"$value\": must match [A-Za-z0-9_]+")
        }
    }

    fun normalizeLimit(value: Int): Int {
        if (value == 0) return 100
        if (value < 0) throw HelperException.InvalidArgument("limit must be a positive integer")
        return value
    }

    fun isIdentChar(c: Char): Boolean =
        (c in 'a'..'z') || (c in 'A'..'Z') || (c in '0'..'9') || c == '_'

    fun allIdentChars(s: String): Boolean = s.all { isIdentChar(it) }

    // --- response parsing ---------------------------------------------------

    @Suppress("UNCHECKED_CAST")
    fun decodeBody(body: ByteArray): Map<String, Any?>? {
        if (body.isEmpty()) return null
        return try {
            mapper.readValue(body, Map::class.java) as? Map<String, Any?>
        } catch (_: Exception) {
            null
        }
    }

    fun affectedFromMap(obj: Map<String, Any?>): Long =
        (obj["affected"] as? Number)?.toLong() ?: 0L

    @Suppress("UNCHECKED_CAST")
    fun firstRow(body: ByteArray): Pair<Map<String, Any?>?, Long> {
        val obj = decodeBody(body) ?: return null to 0L
        var affected = affectedFromMap(obj)
        var rows = obj["rows"] as? List<Any?>
        if (rows.isNullOrEmpty()) {
            val nested = obj["result"] as? Map<String, Any?>
            if (nested != null) {
                rows = nested["rows"] as? List<Any?>
                if (affected == 0L) affected = affectedFromMap(nested)
            }
        }
        if (rows.isNullOrEmpty()) return null to affected
        val row = rows[0] as? Map<String, Any?>
        return row to affected
    }

    @Suppress("UNCHECKED_CAST")
    fun allRows(body: ByteArray): List<Map<String, Any?>> {
        val obj = decodeBody(body) ?: return emptyList()
        var raw = obj["rows"] as? List<Any?>
        if (raw == null) {
            val nested = obj["result"] as? Map<String, Any?>
            if (nested != null) raw = nested["rows"] as? List<Any?>
        }
        if (raw == null) return emptyList()
        return raw.mapNotNull { it as? Map<String, Any?> }
    }

    fun affectedFromBody(body: ByteArray): Long {
        val obj = decodeBody(body) ?: return 0L
        val direct = affectedFromMap(obj)
        if (direct > 0L) return direct
        @Suppress("UNCHECKED_CAST")
        val nested = obj["result"] as? Map<String, Any?>
        return if (nested != null) affectedFromMap(nested) else 0L
    }

    fun ridString(value: Any?): String? = when (value) {
        null -> null
        is String -> value
        is Number -> value.toString()
        else -> null
    }
}
