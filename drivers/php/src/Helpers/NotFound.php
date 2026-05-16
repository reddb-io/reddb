<?php

declare(strict_types=1);

namespace Reddb\Helpers;

/** `NOT_FOUND` — server replied empty for a lookup that required a row. */
final class NotFound extends HelperException {}
