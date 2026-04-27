<?php
/**
 * Connection-shaped surface every transport (RedWire, HTTP)
 * implements. All methods are blocking. Methods that return data
 * yield the raw JSON envelope as a string so the caller can pick
 * its own decode strategy ({@see json_decode()} or otherwise).
 */

declare(strict_types=1);

namespace Reddb;

interface Conn
{
    /** Run a SQL query. Returns the engine's JSON envelope as a string. */
    public function query(string $sql): string;

    /**
     * Insert a single row into a collection.
     *
     * @param array<string,mixed>|object $payload Anything `json_encode` accepts.
     */
    public function insert(string $collection, array|object $payload): void;

    /**
     * Insert many rows in a single round-trip.
     *
     * @param iterable<int,array<string,mixed>|object> $rows
     */
    public function bulkInsert(string $collection, iterable $rows): void;

    /** Fetch one row by id. Returns the JSON envelope as a string. */
    public function get(string $collection, string $id): string;

    /** Delete one row by id. */
    public function delete(string $collection, string $id): void;

    /** Round-trip a Ping → Pong (or HTTP /admin/health). Throws on protocol errors. */
    public function ping(): void;

    /** Idempotent close. Safe to call multiple times. */
    public function close(): void;
}
