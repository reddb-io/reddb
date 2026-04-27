<?php
/** Wire-level error: malformed frame, unexpected message kind, JSON decode failure. */

declare(strict_types=1);

namespace Reddb\RedDBException;

use Reddb\RedDBException;

class ProtocolError extends RedDBException
{
}
