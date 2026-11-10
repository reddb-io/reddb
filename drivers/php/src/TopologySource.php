<?php
declare(strict_types=1);
namespace Reddb;

interface TopologySource
{
    public function topology(string $protocol): ?array;
}
