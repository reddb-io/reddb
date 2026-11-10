<?php
declare(strict_types=1);
namespace Reddb;

final class ReadOptions
{
    public function __construct(
        public readonly string $consistency = 'strong',
        public readonly int $maxLagMs = 0,
    ) {
        if (!in_array($consistency, ['strong', 'local', 'bounded_staleness'], true) || $maxLagMs < 0) {
            throw new \InvalidArgumentException('CONSISTENCY_UNSUPPORTED');
        }
    }
}
