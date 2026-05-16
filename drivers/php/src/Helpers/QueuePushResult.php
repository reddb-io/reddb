<?php

declare(strict_types=1);

namespace Reddb\Helpers;

final class QueuePushResult
{
    public function __construct(
        public readonly int $affected,
        public readonly ?string $rid = null,
    ) {}
}
