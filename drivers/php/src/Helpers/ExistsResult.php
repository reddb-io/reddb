<?php

declare(strict_types=1);

namespace Reddb\Helpers;

final class ExistsResult
{
    public function __construct(public readonly bool $exists) {}
}
