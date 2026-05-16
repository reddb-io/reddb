<?php

declare(strict_types=1);

namespace Reddb\Helpers;

final class InsertResult
{
    public function __construct(
        public readonly int $affected,
        public readonly string $rid,
        /** @var array<string,mixed>|null */
        public readonly ?array $item = null,
    ) {}
}
