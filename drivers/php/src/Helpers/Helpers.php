<?php
/**
 * Groups the rich namespaces ({@see Documents}, {@see Kv}, {@see Queue},
 * {@see TxClient}) bound to a single transport, plus the generic top-level
 * helpers from SDK Helper Spec v1.0 §3. Stateless — safe to construct per
 * call. Mirrors `drivers/go/helpers.go`.
 */

declare(strict_types=1);

namespace Reddb\Helpers;

use Reddb\Conn;

final class Helpers
{
    /**
     * SDK Helper Spec revision this driver satisfies. Cross-driver CI
     * dashboards assert against this constant per spec §14.
     */
    public const HELPER_SPEC_VERSION = '1.0';

    public function __construct(private readonly Querier $q) {}

    /** Wrap a {@see Conn} (or any {@see Querier}) with the helper surface. */
    public static function for(Querier|Conn $target): self
    {
        if ($target instanceof Querier) return new self($target);
        return new self(new ConnQuerier($target));
    }

    /** The {@see self::HELPER_SPEC_VERSION} this driver satisfies. */
    public function helperSpecVersion(): string { return self::HELPER_SPEC_VERSION; }

    public function documents(): Documents { return new Documents($this->q); }

    public function kv(string $collection = 'kv_default'): Kv { return new Kv($this->q, $collection); }

    public function queue(): Queue { return new Queue($this->q); }

    /**
     * Spec-canonical plural alias of {@see queue()}. The namespace name in
     * the spec is `queues.*`; both forms call into the same client.
     */
    public function queues(): Queue { return $this->queue(); }

    public function tx(): TxClient { return new TxClient($this->q); }

    // --- generic helpers (spec §3) ---------------------------------------

    /**
     * Run a SQL statement (spec §3.1 / §3.2). Empty SQL is rejected locally
     * with INVALID_ARGUMENT before the request is sent.
     *
     * @param array<int,mixed> $params
     */
    public function query(string $sql, array $params = []): string
    {
        if (trim($sql) === '') {
            throw new InvalidArgument('query SQL must not be empty');
        }
        return $this->q->query($sql, $params);
    }

    /**
     * Insert one row-like item (spec §3.3), returning the spec InsertResult.
     *
     * @param array<string,mixed> $payload
     */
    public function insert(string $collection, array $payload): InsertResult
    {
        return $this->documents()->insert($collection, $payload);
    }

    /**
     * Insert many row-like items (spec §3.4). Empty input is a no-op
     * returning `{ affected: 0, rids: [] }`; otherwise per-row identity is
     * preserved in input order.
     *
     * @param list<array<string,mixed>> $payloads
     */
    public function bulkInsert(string $collection, array $payloads): BulkInsertResult
    {
        if (count($payloads) === 0) {
            return new BulkInsertResult(0, []);
        }
        $docs = $this->documents();
        $rids = [];
        foreach ($payloads as $payload) {
            if (!is_array($payload)) {
                throw new InvalidArgument('bulk_insert entries must be objects');
            }
            $rids[] = $docs->insert($collection, $payload)->rid;
        }
        return new BulkInsertResult(count($rids), $rids);
    }

    /** Delete one document by rid (spec §3.5), returning the DeleteResult. */
    public function delete(string $collection, string $rid): DeleteResult
    {
        return $this->documents()->delete($collection, $rid);
    }
}
