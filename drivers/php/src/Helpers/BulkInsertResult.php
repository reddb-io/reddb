<?php

declare(strict_types=1);

namespace Reddb\Helpers;

/**
 * Spec envelope for bulk inserts (SDK Helper Spec v1.0 §2.4 / §3.4).
 *
 * `$rids` preserves input order; `count($rids) === affected` for a
 * successful bulk insert.
 */
final class BulkInsertResult
{
    /** @param list<string> $rids */
    public function __construct(
        public readonly int $affected,
        public readonly array $rids,
    ) {}
}
