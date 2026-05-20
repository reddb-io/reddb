<?php
/**
 * SDK Helper Spec — conformance harness (PHP driver).
 *
 * Spec: `docs/spec/sdk-helpers.md` (v1.0). Each §12 case ID is ported as a
 * test method (dots → underscores) so cross-driver dashboards line up; the
 * case ID is preserved in a comment above each method.
 *
 * The PHP driver does not embed the engine, so the harness needs a real
 * `red://` server. It is gated on the same env contract as
 * {@see SmokeTest}, keeping CI policy uniform:
 *
 *   - skipped by default,
 *   - skipped when RED_SKIP_SMOKE=1,
 *   - runs only when RED_SMOKE=1 *and* RED_BIN=/path/to/red are set.
 *
 * Cases use per-test unique collection/queue names so they stay independent
 * on the shared engine and failures isolate cleanly.
 */

declare(strict_types=1);

namespace Reddb\Tests;

use PHPUnit\Framework\TestCase;
use Reddb\Conn;
use Reddb\Helpers\Helpers;
use Reddb\Helpers\InvalidArgument;
use Reddb\Helpers\NotFound;
use Reddb\Reddb;

final class ConformanceTest extends TestCase
{
    /** @var resource|null */
    private static $proc = null;
    /** @var array<int,resource> */
    private static array $pipes = [];
    private static ?string $uri = null;
    private static string $skip = '';
    private static bool $started = false;
    private static int $seq = 0;

    private Conn $conn;
    private Helpers $h;

    public static function setUpBeforeClass(): void
    {
        if (getenv('RED_SKIP_SMOKE') === '1') {
            self::$skip = 'RED_SKIP_SMOKE=1 set — skipping conformance harness';
            return;
        }
        if (getenv('RED_SMOKE') !== '1') {
            self::$skip = 'set RED_SMOKE=1 to enable the conformance harness; off by default';
            return;
        }
        $bin = getenv('RED_BIN');
        if (!is_string($bin) || $bin === '') {
            self::$skip = 'set RED_BIN=/path/to/red to run the conformance harness';
            return;
        }
        if (!is_file($bin)) {
            self::$skip = "RED_BIN \"{$bin}\" not found";
            return;
        }

        $dataDir = sys_get_temp_dir() . '/reddb-php-conformance-' . bin2hex(random_bytes(6));
        if (!mkdir($dataDir) && !is_dir($dataDir)) {
            self::$skip = "could not create temp dir {$dataDir}";
            return;
        }
        $port = self::freePort();
        if ($port === 0) {
            self::$skip = 'could not allocate a free port';
            return;
        }
        $cmd = [$bin, 'server', '--path', $dataDir . '/data.db', '--bind', "127.0.0.1:{$port}"];
        $descriptors = [
            0 => ['pipe', 'r'],
            1 => ['file', '/dev/null', 'w'],
            2 => ['file', '/dev/null', 'w'],
        ];
        $proc = proc_open($cmd, $descriptors, $pipes);
        if (!is_resource($proc)) {
            self::$skip = 'failed to spawn red server';
            return;
        }
        self::$proc = $proc;
        self::$pipes = $pipes;
        self::$uri = "red://127.0.0.1:{$port}";
        self::$started = true;

        if (!self::waitForReady(self::$uri, 60)) {
            self::$skip = 'engine never became ready at ' . self::$uri;
        }
    }

    public static function tearDownAfterClass(): void
    {
        foreach (self::$pipes as $p) {
            if (is_resource($p)) {
                @fclose($p);
            }
        }
        if (is_resource(self::$proc)) {
            proc_terminate(self::$proc);
            $deadline = microtime(true) + 10;
            while (microtime(true) < $deadline) {
                $status = proc_get_status(self::$proc);
                if (!$status['running']) {
                    break;
                }
                usleep(100_000);
            }
            proc_close(self::$proc);
        }
        self::$proc = null;
        self::$pipes = [];
    }

    protected function setUp(): void
    {
        if (self::$skip !== '') {
            $this->markTestSkipped(self::$skip);
        }
        $conn = Reddb::connect((string) self::$uri);
        $this->conn = $conn;
        $this->h = Helpers::for($conn);
    }

    protected function tearDown(): void
    {
        if (isset($this->conn)) {
            $this->conn->close();
        }
    }

    /** Per-test unique suffix so cases don't collide on the shared engine. */
    private function uniq(): string
    {
        return strtolower(str_replace(['\\', ':', '.'], '_', $this->name())) . '_' . (++self::$seq);
    }

    // === generic.* ======================================================

    // Case ID: generic.query.no_params
    public function test_generic_query_no_params(): void
    {
        $table = 'conf_q_' . $this->uniq();
        $this->conn->query("CREATE TABLE {$table} (id INTEGER, name TEXT)");
        $this->conn->query("INSERT INTO {$table} (id, name) VALUES (1, 'a')");
        $body = $this->h->query("SELECT id, name FROM {$table}");
        $this->assertStringContainsString('"a"', $body, "missing row in: {$body}");
    }

    // Case ID: generic.query_with.params
    public function test_generic_query_with_params(): void
    {
        $table = 'conf_p_' . $this->uniq();
        $this->conn->query("CREATE TABLE {$table} (id INTEGER, name TEXT)");
        $this->conn->query("INSERT INTO {$table} (id, name) VALUES ($1, $2)", [42, 'alice']);
        $body = $this->h->query("SELECT name FROM {$table} WHERE id = $1", [42]);
        $this->assertStringContainsString('alice', $body, "expected alice row: {$body}");
    }

    // Case ID: generic.insert.rid
    public function test_generic_insert_rid(): void
    {
        $coll = 'conf_ins_' . $this->uniq();
        $res = $this->h->insert($coll, ['name' => 'eve']);
        $this->assertSame(1, $res->affected);
        $this->assertNotSame('', $res->rid);
    }

    // Case ID: generic.bulk_insert.rids
    public function test_generic_bulk_insert_rids(): void
    {
        $coll = 'conf_bulk_' . $this->uniq();
        $empty = $this->h->bulkInsert($coll, []);
        $this->assertSame(0, $empty->affected);
        $this->assertSame([], $empty->rids);

        $got = $this->h->bulkInsert($coll, [['idx' => 0], ['idx' => 1], ['idx' => 2]]);
        $this->assertSame(3, $got->affected);
        $this->assertCount(3, $got->rids);
        $this->assertCount(3, array_unique($got->rids), 'rids must be distinct');
    }

    // Case ID: generic.delete
    public function test_generic_delete(): void
    {
        $coll = 'conf_del_' . $this->uniq();
        $ins = $this->h->documents()->insert($coll, ['k' => 'v']);
        $res = $this->h->delete($coll, $ins->rid);
        $this->assertSame(1, $res->affected);
        $this->assertTrue($res->deleted);
    }

    // === documents.* ====================================================

    // Case ID: documents.crud_nested_patch
    public function test_documents_crud_nested_patch(): void
    {
        $coll = 'conf_doc_' . $this->uniq();
        $docs = $this->h->documents();
        $ins = $docs->insert($coll, ['event_type' => 'login', 'attempts' => 2, 'success' => true]);
        $this->assertNotSame('', $ins->rid);

        $got = $docs->get($coll, $ins->rid);
        $this->assertSame('login', $got['event_type'] ?? null);

        $list = $docs->list($coll);
        $this->assertNotCount(0, $list->items);

        $patched = $docs->patch($coll, $ins->rid, ['attempts' => 3]);
        // Spec §4.4: top-level merge MUST preserve unrelated fields.
        $this->assertSame('login', $patched['event_type'] ?? null);

        $del = $docs->delete($coll, $ins->rid);
        $this->assertSame(1, $del->affected);
        $this->assertTrue($del->deleted);
    }

    // Case ID: documents.delete_missing_no_error
    public function test_documents_delete_missing_no_error(): void
    {
        $coll = 'conf_doc_miss_' . $this->uniq();
        $ins = $this->h->documents()->insert($coll, ['k' => 'v']);
        $this->h->documents()->delete($coll, $ins->rid);
        $res = $this->h->documents()->delete($coll, 'rid_that_does_not_exist');
        $this->assertSame(0, $res->affected);
        $this->assertFalse($res->deleted);
    }

    // Case ID: documents.patch_empty_rejects
    public function test_documents_patch_empty_rejects(): void
    {
        $coll = 'conf_doc_pe_' . $this->uniq();
        $ins = $this->h->documents()->insert($coll, ['k' => 'v']);
        $this->expectException(InvalidArgument::class);
        $this->h->documents()->patch($coll, $ins->rid, []);
    }

    // === kv.* ===========================================================

    // Case ID: kv.exact_key_round_trip
    public function test_kv_exact_key_round_trip(): void
    {
        $coll = 'conf_kv_' . $this->uniq();
        $kv = $this->h->kv($coll);
        $kv->set('characters:hansel', 'witch');
        $this->assertSame('witch', $kv->get('characters:hansel'));
    }

    // Case ID: kv.missing_get_returns_none
    public function test_kv_missing_get_returns_none(): void
    {
        $coll = 'conf_kv_miss_' . $this->uniq();
        $kv = $this->h->kv($coll);
        $kv->set('seed', 'v');
        $this->assertNull($kv->get('never:set'));
    }

    // Case ID: kv.delete_returns_envelope
    public function test_kv_delete_returns_envelope(): void
    {
        $coll = 'conf_kv_del_' . $this->uniq();
        $kv = $this->h->kv($coll);
        $kv->set('k', 'v');
        $hit = $kv->delete('k');
        $this->assertSame(1, $hit->affected);
        $this->assertTrue($hit->deleted);
        $miss = $kv->delete('k');
        $this->assertSame(0, $miss->affected);
        $this->assertFalse($miss->deleted);
    }

    // === queues.* =======================================================

    // Case ID: queues.fifo_peek_pop_len
    public function test_queues_fifo_peek_pop_len(): void
    {
        $name = 'conf_q_fifo_' . $this->uniq();
        $q = $this->h->queues();
        $q->create($name);
        $q->push($name, ['n' => 1]);
        $q->push($name, ['n' => 2]);
        $this->assertSame(2, $q->len($name));
        $peeked = $q->peek($name, 1);
        $this->assertCount(1, $peeked);
        // peek MUST NOT decrement length.
        $this->assertSame(2, $q->len($name));
        $popped = $q->pop($name, 1);
        $this->assertCount(1, $popped);
        $this->assertSame(1, $q->len($name));
    }

    // Case ID: queues.empty_pop_returns_empty
    public function test_queues_empty_pop_returns_empty(): void
    {
        $name = 'conf_q_empty_' . $this->uniq();
        $q = $this->h->queues();
        $q->create($name);
        $this->assertSame([], $q->pop($name));
    }

    // Case ID: queues.purge_resets_len
    public function test_queues_purge_resets_len(): void
    {
        $name = 'conf_q_purge_' . $this->uniq();
        $q = $this->h->queues();
        $q->create($name);
        for ($i = 0; $i < 3; $i++) {
            $q->push($name, ['i' => $i]);
        }
        $this->assertSame(3, $q->len($name));
        $q->purge($name);
        $this->assertSame(0, $q->len($name));
    }

    // === tx.* ===========================================================

    // Case ID: tx.commit_persists
    public function test_tx_commit_persists(): void
    {
        $table = 'conf_tx_commit_' . $this->uniq();
        $this->conn->query("CREATE TABLE {$table} (name TEXT)");
        $tx = $this->h->tx();
        $tx->begin();
        $this->conn->query("INSERT INTO {$table} (name) VALUES ('keep')");
        $tx->commit();
        $body = $this->conn->query("SELECT name FROM {$table} WHERE name = 'keep'");
        $this->assertStringContainsString('keep', $body, "commit did not persist: {$body}");
    }

    // Case ID: tx.rollback_discards
    public function test_tx_rollback_discards(): void
    {
        $table = 'conf_tx_rb_' . $this->uniq();
        $this->conn->query("CREATE TABLE {$table} (name TEXT)");
        $tx = $this->h->tx();
        $tx->begin();
        $this->conn->query("INSERT INTO {$table} (name) VALUES ('drop')");
        $tx->rollback();
        $body = $this->conn->query("SELECT name FROM {$table} WHERE name = 'drop'");
        $this->assertStringNotContainsString('drop', $body, "rollback did not discard: {$body}");
    }

    // === errors.* =======================================================

    // Case ID: errors.invalid_argument.empty_sql
    public function test_errors_invalid_argument_empty_sql(): void
    {
        $this->expectException(InvalidArgument::class);
        $this->h->query('');
    }

    // Case ID: errors.not_found.document_get
    public function test_errors_not_found_document_get(): void
    {
        $coll = 'conf_err_nf_' . $this->uniq();
        $ins = $this->h->documents()->insert($coll, ['k' => 'v']);
        $this->h->documents()->delete($coll, $ins->rid);
        $this->expectException(NotFound::class);
        $this->h->documents()->get($coll, 'rid_definitely_missing');
    }

    // === wire.* (provisional namespaces — SQL only in v1.0) =============

    // Case ID: wire.probabilistic.hll_round_trip
    public function test_wire_probabilistic_hll_round_trip(): void
    {
        $name = 'conf_hll_' . $this->uniq();
        $this->conn->query("CREATE HLL {$name}");
        $this->conn->query("HLL ADD {$name} 'alice' 'bob' 'alice'");
        $body = $this->conn->query("HLL COUNT {$name}");
        // Spec accepts either `count` or `cardinality` as the projected column.
        $this->assertTrue(
            str_contains($body, 'count') || str_contains($body, 'cardinality'),
            "expected count/cardinality column in: {$body}",
        );
    }

    // --- harness wiring -------------------------------------------------

    private static function waitForReady(string $uri, int $timeoutSec): bool
    {
        $deadline = microtime(true) + $timeoutSec;
        while (microtime(true) < $deadline) {
            try {
                $conn = Reddb::connect($uri);
                try {
                    $conn->ping();
                    $conn->close();
                    return true;
                } catch (\Throwable) {
                    $conn->close();
                }
            } catch (\Throwable) {
                // not up yet
            }
            usleep(50_000);
        }
        return false;
    }

    private static function freePort(): int
    {
        $socket = @stream_socket_server('tcp://127.0.0.1:0', $errno, $errstr);
        if ($socket === false) {
            return 0;
        }
        $name = stream_socket_get_name($socket, false);
        fclose($socket);
        if (!is_string($name) || !preg_match('/:(\d+)$/', $name, $m)) {
            return 0;
        }
        return (int) $m[1];
    }
}
