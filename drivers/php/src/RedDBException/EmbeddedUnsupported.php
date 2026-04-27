<?php
/** User asked for `red://`, `red:///path`, or `red://memory` but no embedded engine ships in this driver. */

declare(strict_types=1);

namespace Reddb\RedDBException;

use Reddb\RedDBException;

class EmbeddedUnsupported extends RedDBException
{
}
