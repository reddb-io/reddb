<?php
/** Embedded mode was requested but PHP FFI or the matching native RedDB library is unavailable. */

declare(strict_types=1);

namespace Reddb\RedDBException;

use Reddb\RedDBException;

class EmbeddedUnsupported extends RedDBException
{
}
