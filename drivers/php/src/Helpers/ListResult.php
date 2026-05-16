<?php

declare(strict_types=1);

namespace Reddb\Helpers;

final class ListResult
{
    /** @param list<array<string,mixed>> $items */
    public function __construct(
        public readonly array $items,
        public readonly ?string $nextCursor = null,
    ) {}
}
