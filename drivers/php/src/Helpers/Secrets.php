<?php

declare(strict_types=1);

namespace Reddb\Helpers;

/** Vault helper bound to one explicit collection. */
final readonly class Secrets
{
    public function __construct(
        private Querier $q,
        public string $collection,
    ) {
        self::validatePart($collection, 'vault collection');
    }

    public function put(string $key, mixed $value): void
    {
        $this->q->query(sprintf(
            'VAULT PUT %s = %s',
            $this->path($key),
            Sql::kvValueLiteral($value),
        ));
    }

    /** @return array<string,mixed>|null */
    public function get(string $key): ?array { return $this->read('GET', $key); }
    /** @return array<string,mixed>|null */
    public function reveal(string $key): ?array { return $this->read('REVEAL', $key); }

    /** @return array<string,mixed>|null */
    private function read(string $verb, string $key): ?array
    {
        [$row] = Sql::firstRow($this->q->query("VAULT $verb " . $this->path($key)));
        return $row;
    }

    private function path(string $key): string
    {
        self::validatePart($key, 'vault key');
        return $this->collection . '.' . $key;
    }

    private static function validatePart(string $value, string $label): void
    {
        if (preg_match('/^[A-Za-z0-9_.]+$/D', $value) !== 1) {
            throw new InvalidArgument("invalid $label");
        }
    }
}
