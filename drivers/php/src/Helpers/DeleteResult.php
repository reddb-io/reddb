<?php

declare(strict_types=1);

namespace Reddb\Helpers;

/**
 * Spec envelope for delete helpers (SDK Helper Spec v1.0 §2.4).
 *
 * `$deleted` reports whether anything was actually removed (`affected > 0`).
 * Deleting a missing item returns `{ affected: 0, deleted: false }` rather
 * than a NOT_FOUND error, per §4.5 / §5.4.
 */
final class DeleteResult
{
    public readonly bool $deleted;

    public function __construct(public readonly int $affected, ?bool $deleted = null)
    {
        $this->deleted = $deleted ?? ($affected > 0);
    }
}
