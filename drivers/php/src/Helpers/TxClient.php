<?php
/**
 * Implements `tx.*` from the SDK Helper Spec (v1.0 §7) — imperative form
 * (`begin` / `commit` / `rollback`) plus the optional callback form
 * (`run`). Mirrors `drivers/go/helpers.go` (TxClient).
 *
 * The connection is session-stateful: a `begin` opens a transaction that the
 * next `commit` or `rollback` closes. Concurrent calls on the same client
 * during an open transaction MUST serialise (the underlying transport
 * already serialises one in-flight statement at a time).
 */

declare(strict_types=1);

namespace Reddb\Helpers;

final class TxClient
{
    public function __construct(
        private readonly Querier $q,
        private readonly bool $inTx = false,
    ) {}

    /** Start a transaction. Returns the raw JSON envelope. */
    public function begin(): string
    {
        return $this->q->query('BEGIN');
    }

    /** Commit the open transaction. Returns the raw JSON envelope. */
    public function commit(): string
    {
        return $this->q->query('COMMIT');
    }

    /** Roll back the open transaction. Returns the raw JSON envelope. */
    public function rollback(): string
    {
        return $this->q->query('ROLLBACK');
    }

    /**
     * Run a SQL statement inside the transaction session. Provided so a
     * `run` callback can issue savepoints or arbitrary statements without
     * reaching back to the connection.
     *
     * @param array<int,mixed> $params
     */
    public function query(string $sql, array $params = []): string
    {
        return $this->q->query($sql, $params);
    }

    /**
     * Callback form (spec §7.2): begin, invoke `$fn`, then commit on success
     * or rollback + re-throw on failure. Nested `run` is rejected with
     * INVALID_ARGUMENT — the PHP driver does NOT use savepoints; callers
     * wanting nested semantics issue `SAVEPOINT` directly via {@see query()}.
     *
     * @param callable(TxClient):void $fn
     */
    public function run(callable $fn): void
    {
        if ($this->inTx) {
            throw new InvalidArgument(
                'tx.run does not support nested transactions; use SAVEPOINT explicitly'
            );
        }
        $this->begin();
        $child = new self($this->q, true);
        try {
            $fn($child);
        } catch (\Throwable $e) {
            try {
                $this->rollback();
            } catch (\Throwable) {
                // Surface the original callback failure, not the rollback noise.
            }
            throw $e;
        }
        $this->commit();
    }
}
