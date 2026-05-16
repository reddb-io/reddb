<?php

declare(strict_types=1);

namespace Reddb\Helpers;

use Reddb\Conn;

/** Adapter wrapping a transport {@see Conn} as a {@see Querier}. */
final class ConnQuerier implements Querier
{
    public function __construct(private readonly Conn $conn) {}

    public function query(string $sql, array $params = []): string
    {
        return $this->conn->query($sql, $params);
    }
}
