package dev.reddb;

import java.util.List;

/**
 * Connection-shaped surface every transport (RedWire, HTTP) implements.
 * All methods are blocking; they return raw JSON bytes so the caller
 * can deserialise with whatever object mapper they prefer.
 */
public interface Conn extends AutoCloseable {
    /** Run a SQL query. Returns the engine's JSON envelope as bytes. */
    byte[] query(String sql);

    /** Insert a single row into a collection. `payload` is anything Jackson can serialise. */
    void insert(String collection, Object payload);

    /** Insert many rows in one round trip. Each row is anything Jackson can serialise. */
    void bulkInsert(String collection, List<?> rows);

    /** Fetch one row by id. Returns the JSON envelope (`{ ok, found, ... }`) as bytes. */
    byte[] get(String collection, String id);

    /** Delete one row by id. */
    void delete(String collection, String id);

    /** Round-trip a Ping → Pong. Throws on protocol errors. */
    void ping();

    /** Idempotent close. */
    @Override
    void close();
}
