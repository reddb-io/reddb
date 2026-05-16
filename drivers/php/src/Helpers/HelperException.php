<?php
/**
 * Typed helper errors mirroring Go/Java/Python drivers. Subclasses encode
 * the spec error codes (`INVALID_ARGUMENT`, `NOT_FOUND`, `INVALID_RESPONSE`).
 */

declare(strict_types=1);

namespace Reddb\Helpers;

class HelperException extends \RuntimeException {}
