<?php
/**
 * Minimal contract helpers need. {@see \Reddb\Conn} satisfies it via
 * {@see Conn::query()}; tests pass fakes that record SQL.
 */

declare(strict_types=1);

namespace Reddb\Helpers;

interface Querier
{
    /**
     * Run a SQL query with positional `$N` parameters. Returns the raw
     * JSON envelope as a string (mirrors {@see \Reddb\Conn::query()}).
     *
     * @param array<int,mixed> $params
     */
    public function query(string $sql, array $params = []): string;
}
