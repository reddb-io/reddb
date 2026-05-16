<?php
/**
 * Groups the rich namespaces ({@see Documents}, {@see Kv}, {@see Queue})
 * bound to a single transport. Stateless — safe to construct per call.
 * Mirrors `drivers/go/helpers.go`.
 */

declare(strict_types=1);

namespace Reddb\Helpers;

use Reddb\Conn;

final class Helpers
{
    public function __construct(private readonly Querier $q) {}

    /** Wrap a {@see Conn} (or any {@see Querier}) with the helper surface. */
    public static function for(Querier|Conn $target): self
    {
        if ($target instanceof Querier) return new self($target);
        return new self(new ConnQuerier($target));
    }

    public function documents(): Documents { return new Documents($this->q); }

    public function kv(string $collection = 'kv_default'): Kv { return new Kv($this->q, $collection); }

    public function queue(): Queue { return new Queue($this->q); }
}
