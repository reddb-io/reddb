<?php
/**
 * Implements `queues.*` from the SDK Helper Spec (v1.0 §6). The spec
 * namespace is plural; {@see Helpers::queues()} is the canonical accessor and
 * {@see Helpers::queue()} the singular alias.
 */

declare(strict_types=1);

namespace Reddb\Helpers;

final class Queue
{
    public function __construct(private readonly Querier $q) {}

    /**
     * Create the queue if it does not exist (idempotent). Wraps
     * `CREATE QUEUE IF NOT EXISTS`.
     */
    public function create(string $queue): void
    {
        Sql::assertIdentifier($queue, 'queue name');
        try {
            $this->q->query('CREATE QUEUE IF NOT EXISTS ' . Sql::identifier($queue));
        } catch (\Throwable $e) {
            if (str_contains($e->getMessage(), 'already exists')) return;
            throw $e;
        }
    }

    /** @param array{priority?:int} $opts */
    public function push(string $queue, mixed $value, array $opts = []): QueuePushResult
    {
        Sql::assertIdentifier($queue, 'queue name');
        $lit = Sql::queueValueLiteral($value);
        $priority = '';
        if (array_key_exists('priority', $opts) && $opts['priority'] !== null) {
            $priority = sprintf(' PRIORITY %d', (int) $opts['priority']);
        }
        $sql = sprintf('QUEUE PUSH %s %s%s', Sql::identifier($queue), $lit, $priority);
        $body = $this->q->query($sql);
        $affected = Sql::affectedFromBody($body);
        if ($affected === 0) $affected = 1;
        [$row] = Sql::firstRow($body);
        $rid = $row === null ? null : Sql::ridString($row['rid'] ?? null);
        return new QueuePushResult($affected, $rid);
    }

    /** @return list<mixed> */
    public function pop(string $queue, ?int $count = null): array
    {
        return $this->fetch('POP', $queue, $count);
    }

    /** @return list<mixed> */
    public function peek(string $queue, ?int $count = null): array
    {
        return $this->fetch('PEEK', $queue, $count);
    }

    /** @return list<mixed> */
    private function fetch(string $verb, string $queue, ?int $count): array
    {
        Sql::assertIdentifier($queue, 'queue name');
        $suffix = '';
        if ($count !== null) {
            if ($count < 0) {
                throw new InvalidArgument('queue count must be a non-negative integer');
            }
            $suffix = sprintf(' COUNT %d', $count);
        }
        $body = $this->q->query(sprintf('QUEUE %s %s%s', $verb, Sql::identifier($queue), $suffix));
        $rows = Sql::allRows($body);
        $out = [];
        foreach ($rows as $r) $out[] = $r['payload'] ?? null;
        return $out;
    }

    public function len(string $queue): int
    {
        Sql::assertIdentifier($queue, 'queue name');
        $body = $this->q->query('QUEUE LEN ' . Sql::identifier($queue));
        [$row] = Sql::firstRow($body);
        if ($row === null) return 0;
        $v = $row['len'] ?? null;
        if (is_int($v)) return $v;
        if (is_float($v)) return (int) $v;
        return 0;
    }

    public function purge(string $queue): DeleteResult
    {
        Sql::assertIdentifier($queue, 'queue name');
        $body = $this->q->query('QUEUE PURGE ' . Sql::identifier($queue));
        return new DeleteResult(Sql::affectedFromBody($body));
    }

    /**
     * Live `QUEUE READ … WAIT <ms>` helper (PRD #718 / #725). Blocks
     * until a message is available for `$consumer` on `$queue`, the
     * `waitMs` budget elapses, or the server cancels. Timeout returns
     * an empty list — same shape as an empty pop, never throws.
     * `waitMs` is required; no infinite-wait default. Cancellation and
     * cap rejection surface as exceptions from the transport.
     *
     * @param array{waitMs:int, group?:string, count?:int} $opts
     * @return list<mixed>
     */
    public function readWait(string $queue, string $consumer, array $opts): array
    {
        Sql::assertIdentifier($queue, 'queue name');
        Sql::assertIdentifier($consumer, 'consumer name');
        $waitMs = $opts['waitMs'] ?? null;
        if (!is_int($waitMs) || $waitMs < 0) {
            throw new InvalidArgument(
                'queue readWait requires a non-negative integer waitMs (no infinite wait)'
            );
        }
        $groupClause = '';
        if (!empty($opts['group'])) {
            Sql::assertIdentifier($opts['group'], 'group name');
            $groupClause = ' GROUP ' . Sql::identifier($opts['group']);
        }
        $countClause = '';
        if (array_key_exists('count', $opts) && $opts['count'] !== null) {
            $count = (int) $opts['count'];
            if ($count < 0) {
                throw new InvalidArgument('queue count must be a non-negative integer');
            }
            $countClause = sprintf(' COUNT %d', $count);
        }
        $sql = sprintf(
            'QUEUE READ %s%s CONSUMER %s%s WAIT %dms',
            Sql::identifier($queue),
            $groupClause,
            Sql::identifier($consumer),
            $countClause,
            $waitMs,
        );
        $body = $this->q->query($sql);
        $rows = Sql::allRows($body);
        $out = [];
        foreach ($rows as $r) $out[] = $r['payload'] ?? null;
        return $out;
    }
}
