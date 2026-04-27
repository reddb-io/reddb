<?php
/** Server returned an `Error` frame / HTTP 4xx-5xx with an engine-side reason. */

declare(strict_types=1);

namespace Reddb\RedDBException;

use Reddb\RedDBException;

class EngineError extends RedDBException
{
}
