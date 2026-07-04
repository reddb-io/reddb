<?php
/**
 * Implements `documents.*` from the SDK Helper Spec.
 */

declare(strict_types=1);

namespace Reddb\Helpers;

final class Documents
{
    public function __construct(private readonly Querier $q) {}

    /** @param array<string,mixed> $document */
    public function insert(string $collection, array $document): InsertResult
    {
        $this->ensureCollection($collection);
        $sql = sprintf(
            'INSERT INTO %s DOCUMENT VALUES (%s) RETURNING *',
            Sql::identifierPath($collection),
            Sql::jsonInlineLiteral($document)
        );
        $body = $this->q->query($sql);
        [$row, $affected] = Sql::firstRow($body);
        if ($row === null || !isset($row['rid'])) {
            throw new InvalidResponse('documents.insert expected one returned item with rid');
        }
        if ($affected === 0) $affected = 1;
        return new InsertResult($affected, Sql::ridString($row['rid']) ?? '', $row);
    }

    /** @return array<string,mixed> */
    public function get(string $collection, string $rid): array
    {
        $sql = sprintf(
            'SELECT * FROM %s WHERE rid = $1 LIMIT 1',
            Sql::identifierPath($collection)
        );
        $body = $this->q->query($sql, [$rid]);
        [$row] = Sql::firstRow($body);
        if ($row === null) {
            throw new NotFound(sprintf('document "%s" was not found', $rid));
        }
        return $row;
    }

    /**
     * @param array{limit?:int,orderBy?:string,filter?:string} $opts
     */
    public function list(string $collection, array $opts = []): ListResult
    {
        $limit = Sql::normalizeLimit((int)($opts['limit'] ?? 0));
        $order = (string)($opts['orderBy'] ?? '');
        if ($order === '') $order = 'rid ASC';
        $filter = (string)($opts['filter'] ?? '');
        $where = $filter === '' ? '' : ' WHERE ' . $filter;
        $sql = sprintf(
            'SELECT * FROM %s%s ORDER BY %s LIMIT %d',
            Sql::identifierPath($collection), $where, $order, $limit
        );
        $body = $this->q->query($sql);
        return new ListResult(Sql::allRows($body));
    }

    /**
     * @param array<string,mixed> $patch
     * @return array<string,mixed>
     */
    public function patch(string $collection, string $rid, array $patch): array
    {
        if (count($patch) === 0) {
            throw new InvalidArgument('documents.patch patch must be a non-empty object');
        }
        $parts = [];
        foreach ($patch as $field => $value) {
            if (!is_string($field) || str_contains($field, '/')) {
                throw new InvalidArgument(
                    'documents.patch currently accepts top-level document fields'
                );
            }
            $parts[] = sprintf('%s = %s', Sql::identifier($field), Sql::valueLiteral($value));
        }
        $sql = sprintf(
            'UPDATE %s DOCUMENTS SET %s WHERE rid = $1 RETURNING *',
            Sql::identifierPath($collection), implode(', ', $parts)
        );
        $body = $this->q->query($sql, [$rid]);
        [$row] = Sql::firstRow($body);
        if ($row === null) {
            throw new NotFound(sprintf('document "%s" was not found', $rid));
        }
        return $row;
    }

    public function delete(string $collection, string $rid): DeleteResult
    {
        $sql = sprintf(
            'DELETE FROM %s WHERE rid = $1',
            Sql::identifierPath($collection)
        );
        $body = $this->q->query($sql, [$rid]);
        return new DeleteResult(Sql::affectedFromBody($body));
    }

    private function ensureCollection(string $collection): void
    {
        try {
            $this->q->query('CREATE DOCUMENT ' . Sql::identifierPath($collection));
        } catch (\Throwable $e) {
            if (str_contains($e->getMessage(), 'already exists')) return;
            throw $e;
        }
    }
}
