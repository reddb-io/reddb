<?php
/**
 * Base type for every error the PHP driver surfaces. Subclasses
 * mirror the JS / Java / Rust drivers so user code can catch by
 * type without sniffing strings:
 *
 *   try {
 *       Reddb::connect('red://host:5050');
 *   } catch (\Reddb\RedDBException\AuthRefused $e) {
 *       // bad token, anonymous blocked, etc.
 *   }
 *
 * The subclasses live in the `Reddb\RedDBException` namespace so
 * PSR-4 autoloading lines up — see `src/RedDBException/*.php`.
 */

declare(strict_types=1);

namespace Reddb;

class RedDBException extends \RuntimeException
{
}
