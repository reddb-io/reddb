<?php
/**
 * Conformance tests mirroring drivers/go/helpers_test.go. They run against
 * a FakeQuerier that records SQL and replays scripted JSON replies — no
 * server needed.
 */

declare(strict_types=1);

namespace Reddb\Tests\Helpers;

use PHPUnit\Framework\TestCase;
use Reddb\Helpers\Documents;
use Reddb\Helpers\Helpers;
use Reddb\Helpers\InvalidArgument;
use Reddb\Helpers\NotFound;
use Reddb\Helpers\Querier;
use Reddb\Helpers\Sql;

final class FakeQuerier implements Querier
{
    /** @var list<array{sql:string,params:array<int,mixed>}> */
    public array $calls = [];
    /** @var list<string> */
    public array $replies = [];
    /** @var list<\Throwable|null> */
    public array $errs = [];

    public function query(string $sql, array $params = []): string
    {
        $this->calls[] = ['sql' => $sql, 'params' => $params];
        $err = array_shift($this->errs);
        $body = array_shift($this->replies) ?? '';
        if ($err !== null && $err !== false) throw $err;
        return $body;
    }

    public function reply(mixed $obj): self
    {
        $this->replies[] = json_encode($obj, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
        return $this;
    }

    public function err(\Throwable $e): self
    {
        $this->errs[] = $e;
        return $this;
    }
}

final class HelpersTest extends TestCase
{
    public function testKvPathQuotesNamespacedKeys(): void
    {
        $this->assertSame("kv_default.'corpus:version'", Sql::kvPath('kv_default', 'corpus:version'));
    }

    public function testKvPathPreservesDotsAndSlashes(): void
    {
        $this->assertSame("kv_default.'a/b.c'", Sql::kvPath('kv_default', 'a/b.c'));
    }

    public function testKvPathRejectsBadCollection(): void
    {
        $this->expectException(InvalidArgument::class);
        Sql::kvPath('bad-name!', 'k');
    }

    /** @return iterable<string,array{mixed,string}> */
    public static function kvValueLiteralCases(): iterable
    {
        yield 'null' => [null, 'NULL'];
        yield 'true' => [true, 'true'];
        yield 'false' => [false, 'false'];
        yield 'int' => [42, '42'];
        yield 'text' => ['hi', "'hi'"];
        yield 'escape' => ["o'reilly", "'o''reilly'"];
        yield 'object' => [['a' => 1], "'{\"a\":1}'"];
    }

    /** @dataProvider kvValueLiteralCases */
    public function testKvValueLiteral(mixed $input, string $want): void
    {
        $this->assertSame($want, Sql::kvValueLiteral($input));
    }

    public function testKvSetEmitsExactKeyPath(): void
    {
        $fq = (new FakeQuerier())->reply(new \stdClass());
        (new Helpers($fq))->kv()->set('characters:hansel', 'ok');
        $sql = $fq->calls[0]['sql'];
        $this->assertStringContainsString("kv_default.'characters:hansel'", $sql);
        $this->assertStringContainsString("= 'ok'", $sql);
    }

    public function testKvGetReturnsValueOrNull(): void
    {
        $fq = (new FakeQuerier())
            ->reply(['rows' => [['value' => 'v']]])
            ->reply(['rows' => []]);
        $kv = (new Helpers($fq))->kv();
        $this->assertSame('v', $kv->get('k'));
        $this->assertNull($kv->get('k2'));
    }

    public function testKvExistsUsesGet(): void
    {
        $fq = (new FakeQuerier())
            ->reply(['rows' => [['value' => 'v']]])
            ->reply(['rows' => []]);
        $kv = (new Helpers($fq))->kv();
        $this->assertTrue($kv->exists('k')->exists);
        $this->assertFalse($kv->exists('k2')->exists);
    }

    public function testKvListFiltersByPrefixWithoutRewriting(): void
    {
        $fq = (new FakeQuerier())->reply(['rows' => [
            ['key' => 'a:1', 'value' => 1],
            ['key' => 'b:1', 'value' => 2],
            ['key' => 'a:2', 'value' => 3],
        ]]);
        $out = (new Helpers($fq))->kv()->list(['prefix' => 'a:']);
        $this->assertCount(2, $out->items);
        $this->assertSame('a:1', $out->items[0]['key']);
        $this->assertSame('a:2', $out->items[1]['key']);
    }

    public function testKvListRejectsNegativeLimit(): void
    {
        $this->expectException(InvalidArgument::class);
        (new Helpers(new FakeQuerier()))->kv()->list(['limit' => -1]);
    }

    public function testQueuePushEmitsPriorityAndPayload(): void
    {
        $fq = (new FakeQuerier())->reply(['affected' => 1]);
        (new Helpers($fq))->queue()->push('jobs', ['id' => 1], ['priority' => 5]);
        $sql = $fq->calls[0]['sql'];
        $this->assertStringStartsWith('QUEUE PUSH jobs ', $sql);
        $this->assertStringContainsString('PRIORITY 5', $sql);
        $this->assertStringContainsString('{"id":1}', $sql);
    }

    public function testQueueLenReturnsInt(): void
    {
        $fq = (new FakeQuerier())->reply(['rows' => [['len' => 3]]]);
        $this->assertSame(3, (new Helpers($fq))->queue()->len('jobs'));
    }

    public function testQueuePopReturnsPayloads(): void
    {
        $fq = (new FakeQuerier())->reply(['rows' => [
            ['payload' => 'a'], ['payload' => 'b']]]);
        $out = (new Helpers($fq))->queue()->pop('jobs', 2);
        $this->assertSame(['a', 'b'], $out);
    }

    public function testQueuePopRejectsNegativeCount(): void
    {
        $this->expectException(InvalidArgument::class);
        (new Helpers(new FakeQuerier()))->queue()->pop('jobs', -1);
    }

    public function testQueuePushRejectsInvalidIdentifier(): void
    {
        $this->expectException(InvalidArgument::class);
        (new Helpers(new FakeQuerier()))->queue()->push('bad-name!', 'x');
    }

    public function testDocumentsInsertReturnsRidEnvelope(): void
    {
        $fq = (new FakeQuerier())
            ->reply(['rows' => [], 'affected' => 0])
            ->reply(['rows' => [['rid' => 'doc-1', 'body' => ['name' => 'alice']]],
                     'affected' => 1]);
        $out = (new Helpers($fq))->documents()->insert('people', ['name' => 'alice']);
        $this->assertSame(1, $out->affected);
        $this->assertSame('doc-1', $out->rid);
        $this->assertSame('doc-1', $out->item['rid']);
        // ADR 0067 (#1709): inline JSON literal form — no (body) column list,
        // no quoted-string body coercion.
        $insertSql = '';
        foreach ($fq->calls as $call) {
            if (str_starts_with($call['sql'], 'INSERT')) {
                $insertSql = $call['sql'];
            }
        }
        $this->assertSame(
            'INSERT INTO people DOCUMENT VALUES ({"name":"alice"}) RETURNING *',
            $insertSql
        );
        $this->assertStringNotContainsString('(body)', $insertSql);
        $this->assertStringNotContainsString("('{", $insertSql);
    }

    public function testDocumentsGetRaisesNotFoundOnMissing(): void
    {
        $fq = (new FakeQuerier())->reply(['rows' => []]);
        $this->expectException(NotFound::class);
        (new Helpers($fq))->documents()->get('people', 'doc-1');
    }

    public function testDocumentsPatchRejectsJsonPointerPaths(): void
    {
        $this->expectException(InvalidArgument::class);
        (new Helpers(new FakeQuerier()))->documents()
            ->patch('people', 'doc-1', ['a/b' => 1]);
    }

    public function testDocumentsListOrdersByRidByDefault(): void
    {
        $fq = (new FakeQuerier())->reply(['rows' => [['rid' => 'a'], ['rid' => 'b']]]);
        $out = (new Helpers($fq))->documents()->list('people');
        $this->assertCount(2, $out->items);
        $this->assertStringContainsString('ORDER BY rid ASC', $fq->calls[0]['sql']);
    }

    public function testDocumentsInsertPassesThroughExistingCollection(): void
    {
        $fq = (new FakeQuerier())
            ->err(new \RuntimeException('collection already exists'))
            ->reply(new \stdClass())
            ->reply(['rows' => [['rid' => 'x']], 'affected' => 1]);
        // err array shorter than calls — second call has no err.
        (new Helpers($fq))->documents()->insert('people', ['a' => 1]);
        $this->assertTrue(true); // reached without throwing
    }

    public function testAffectedFromBodyHandlesNestedResult(): void
    {
        $body = (string) json_encode(['result' => ['affected' => 7]]);
        $this->assertSame(7, Sql::affectedFromBody($body));
    }

    // --- SDK Helper Spec v1.0 surface -----------------------------------

    public function testHelperSpecVersionConstant(): void
    {
        $this->assertSame('1.0', Helpers::HELPER_SPEC_VERSION);
        $this->assertSame('1.0', (new Helpers(new FakeQuerier()))->helperSpecVersion());
    }

    public function testQueuesIsPluralAliasOfQueue(): void
    {
        $h = new Helpers(new FakeQuerier());
        $this->assertInstanceOf(\Reddb\Helpers\Queue::class, $h->queues());
    }

    public function testQueueCreateEmitsIfNotExists(): void
    {
        $fq = (new FakeQuerier())->reply(new \stdClass());
        (new Helpers($fq))->queues()->create('jobs');
        $this->assertSame('CREATE QUEUE IF NOT EXISTS jobs', $fq->calls[0]['sql']);
    }

    public function testQueueCreateRejectsBadIdentifier(): void
    {
        $this->expectException(InvalidArgument::class);
        (new Helpers(new FakeQuerier()))->queues()->create('bad-name!');
    }

    public function testKvDeleteReturnsEnvelope(): void
    {
        $fq = (new FakeQuerier())
            ->reply(['affected' => 1])
            ->reply(['affected' => 0]);
        $kv = (new Helpers($fq))->kv();
        $hit = $kv->delete('k');
        $this->assertSame(1, $hit->affected);
        $this->assertTrue($hit->deleted);
        $miss = $kv->delete('k');
        $this->assertSame(0, $miss->affected);
        $this->assertFalse($miss->deleted);
    }

    public function testDocumentsDeleteReturnsEnvelope(): void
    {
        $fq = (new FakeQuerier())->reply(['affected' => 0]);
        $res = (new Helpers($fq))->documents()->delete('people', 'missing');
        $this->assertSame(0, $res->affected);
        $this->assertFalse($res->deleted);
    }

    public function testDocumentsPatchRejectsEmptyPatch(): void
    {
        $this->expectException(InvalidArgument::class);
        (new Helpers(new FakeQuerier()))->documents()->patch('people', 'doc-1', []);
    }

    public function testGenericQueryRejectsEmptySql(): void
    {
        $this->expectException(InvalidArgument::class);
        (new Helpers(new FakeQuerier()))->query('   ');
    }

    public function testBulkInsertEmptyIsNoOp(): void
    {
        $fq = new FakeQuerier();
        $res = (new Helpers($fq))->bulkInsert('people', []);
        $this->assertSame(0, $res->affected);
        $this->assertSame([], $res->rids);
        $this->assertCount(0, $fq->calls);
    }

    public function testBulkInsertPreservesOrder(): void
    {
        $fq = (new FakeQuerier())
            // first insert: ensureCollection + insert
            ->reply(new \stdClass())
            ->reply(['rows' => [['rid' => 'r-0']], 'affected' => 1])
            ->reply(new \stdClass())
            ->reply(['rows' => [['rid' => 'r-1']], 'affected' => 1]);
        $res = (new Helpers($fq))->bulkInsert('people', [['i' => 0], ['i' => 1]]);
        $this->assertSame(2, $res->affected);
        $this->assertSame(['r-0', 'r-1'], $res->rids);
    }

    public function testTxBeginCommitRollbackEmitSql(): void
    {
        $fq = (new FakeQuerier())->reply(new \stdClass())->reply(new \stdClass())->reply(new \stdClass());
        $tx = (new Helpers($fq))->tx();
        $tx->begin();
        $tx->commit();
        $tx->rollback();
        $this->assertSame('BEGIN', $fq->calls[0]['sql']);
        $this->assertSame('COMMIT', $fq->calls[1]['sql']);
        $this->assertSame('ROLLBACK', $fq->calls[2]['sql']);
    }

    public function testTxRunCommitsOnSuccess(): void
    {
        $fq = (new FakeQuerier())->reply(new \stdClass())->reply(new \stdClass());
        (new Helpers($fq))->tx()->run(function ($tx): void {
            // no-op body
        });
        $this->assertSame('BEGIN', $fq->calls[0]['sql']);
        $this->assertSame('COMMIT', $fq->calls[1]['sql']);
    }

    public function testTxRunRollsBackAndRethrowsOnFailure(): void
    {
        $fq = (new FakeQuerier())->reply(new \stdClass())->reply(new \stdClass());
        try {
            (new Helpers($fq))->tx()->run(function ($tx): void {
                throw new \RuntimeException('boom');
            });
            $this->fail('expected exception to propagate');
        } catch (\RuntimeException $e) {
            $this->assertSame('boom', $e->getMessage());
        }
        $this->assertSame('BEGIN', $fq->calls[0]['sql']);
        $this->assertSame('ROLLBACK', $fq->calls[1]['sql']);
    }

    public function testTxRunRejectsNesting(): void
    {
        $fq = (new FakeQuerier())->reply(new \stdClass())->reply(new \stdClass());
        $this->expectException(InvalidArgument::class);
        (new Helpers($fq))->tx()->run(function ($tx): void {
            $tx->run(function ($inner): void {});
        });
    }
}
