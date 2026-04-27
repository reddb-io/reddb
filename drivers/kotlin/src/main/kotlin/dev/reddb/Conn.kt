package dev.reddb

/**
 * Connection-shaped surface every transport (RedWire, HTTP) implements.
 * Every operation is a `suspend fun`; the implementation owns one socket
 * (or one HTTP client) and serialises concurrent calls internally.
 *
 * Methods that return result rows hand back the engine's raw JSON envelope
 * as a [ByteArray] — callers deserialise with whichever object mapper they
 * prefer (Jackson, kotlinx-serialization, etc.).
 */
public interface Conn : AutoCloseable {
    /** Run a SQL query. Returns the engine's JSON envelope as bytes. */
    public suspend fun query(sql: String): ByteArray

    /** Insert a single row into a collection. `payload` is anything Jackson can serialise. */
    public suspend fun insert(collection: String, payload: Any)

    /** Insert many rows in one round trip. */
    public suspend fun bulkInsert(collection: String, rows: List<Any?>)

    /** Fetch one row by id. Returns the JSON envelope (`{ ok, found, ... }`) as bytes. */
    public suspend fun get(collection: String, id: String): ByteArray

    /** Delete one row by id. */
    public suspend fun delete(collection: String, id: String)

    /** Round-trip a Ping → Pong (or GET /admin/health on HTTP). Throws on protocol errors. */
    public suspend fun ping()

    /** Idempotent close — second call is a no-op. */
    public override fun close()
}
