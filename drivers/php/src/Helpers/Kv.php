<?php
/**
 * Implements `kv.*` from the SDK Helper Spec.
 */

declare(strict_types=1);

namespace Reddb\Helpers;

final class Kv
{
    public function __construct(
        private readonly Querier $q,
        public readonly string $collection = 'kv_default',
    ) {}

    /**
     * @param array{collection?:string,tags?:list<string>,expireMs?:int} $opts
     */
    public function set(string $key, mixed $value, array $opts = []): void
    {
        $this->put($key, $value, $opts);
    }

    /**
     * @param array{collection?:string,tags?:list<string>,expireMs?:int} $opts
     */
    public function put(string $key, mixed $value, array $opts = []): void
    {
        $coll = (string)($opts['collection'] ?? '');
        if ($coll === '') $coll = $this->collection;
        $lit = Sql::kvValueLiteral($value);
        $expireMs = (int)($opts['expireMs'] ?? 0);
        $expire = $expireMs > 0 ? sprintf(' EXPIRE %d ms', $expireMs) : '';
        $tagClause = '';
        $tags = $opts['tags'] ?? null;
        if (is_array($tags) && count($tags) > 0) {
            $parts = array_map([Sql::class, 'kvTagLiteral'], $tags);
            $tagClause = ' TAGS [' . implode(', ', $parts) . ']';
        }
        $path = Sql::kvPath($coll, $key);
        $this->q->query(sprintf('KV PUT %s = %s%s%s', $path, $lit, $expire, $tagClause));
    }

    public function get(string $key, ?string $collection = null): mixed
    {
        $coll = $collection !== null && $collection !== '' ? $collection : $this->collection;
        $path = Sql::kvPath($coll, $key);
        $body = $this->q->query('KV GET ' . $path);
        [$row] = Sql::firstRow($body);
        if ($row === null) return null;
        return $row['value'] ?? null;
    }

    public function exists(string $key, ?string $collection = null): ExistsResult
    {
        return new ExistsResult($this->get($key, $collection) !== null);
    }

    public function delete(string $key, ?string $collection = null): DeleteResult
    {
        $coll = $collection !== null && $collection !== '' ? $collection : $this->collection;
        $path = Sql::kvPath($coll, $key);
        $body = $this->q->query('KV DELETE ' . $path);
        return new DeleteResult(Sql::affectedFromBody($body));
    }

    /**
     * @param array{collection?:string,limit?:int,prefix?:string} $opts
     */
    public function list(array $opts = []): ListResult
    {
        $coll = (string)($opts['collection'] ?? '');
        if ($coll === '') $coll = $this->collection;
        $limit = Sql::normalizeLimit((int)($opts['limit'] ?? 0));
        $sql = sprintf(
            'SELECT key, value FROM %s ORDER BY key ASC LIMIT %d',
            Sql::identifier($coll), $limit
        );
        $body = $this->q->query($sql);
        $rows = Sql::allRows($body);
        $prefix = (string)($opts['prefix'] ?? '');
        if ($prefix !== '') {
            $rows = array_values(array_filter($rows, static function ($r) use ($prefix) {
                $k = $r['key'] ?? null;
                return is_string($k) && str_starts_with($k, $prefix);
            }));
        }
        return new ListResult($rows);
    }
}
