<?php

declare(strict_types=1);

namespace Reddb\Helpers;

final class DeleteResult
{
    public function __construct(public readonly int $affected) {}
}
