<?php

declare(strict_types=1);

namespace Reddb\Helpers;

final class QueueHandlerTimeout extends HelperException
{
    public function __construct(string $message)
    {
        parent::__construct($message);
    }
}
