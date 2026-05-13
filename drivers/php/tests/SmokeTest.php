<?php

declare(strict_types=1);

namespace Reddb\Tests;

use PHPUnit\Framework\TestCase;
use Reddb\Reddb;

/**
 * End-to-end smoke against a freshly-spawned RedDB binary. Gated on
 * `RED_SMOKE=1` so normal test runs don't drag in a cargo build.
 */
final class SmokeTest extends TestCase
{
    protected function setUp(): void
    {
        if (getenv('RED_SMOKE') !== '1') {
            $this->markTestSkipped('RED_SMOKE=1 not set — skipping engine smoke');
        }
    }

    public function test_runs_against_real_engine(): void
    {
        $repoRoot = $this->findRepoRoot();
        $dataDir = sys_get_temp_dir() . '/reddb-php-smoke-' . bin2hex(random_bytes(6));
        mkdir($dataDir);
        $port = $this->freePort();
        $cmd = $this->redCommand($dataDir . '/data.db', $port);
        $descriptors = [
            0 => ['pipe', 'r'],
            1 => ['file', '/dev/null', 'w'],
            2 => ['file', '/dev/null', 'w'],
        ];
        $proc = proc_open($cmd, $descriptors, $pipes, $repoRoot);
        if (!is_resource($proc)) {
            $this->fail('failed to spawn red server');
        }
        try {
            $conn = $this->waitForConnect("red://127.0.0.1:{$port}", 60);
            try {
                $conn->ping();
                $selectOne = $conn->query('SELECT 1');
                $this->assertStringContainsString('"ok":true', $selectOne, "expected ok result in: {$selectOne}");
                $conn->query('CREATE TABLE php_params (id INT, name TEXT)');
                $conn->query(
                    'INSERT INTO php_params (id, name) VALUES ($1, $2)',
                    [42, 'alice'],
                );
                $body = $conn->query(
                    'SELECT name FROM php_params WHERE id = $1 AND name = $2',
                    [42, 'alice'],
                );
                $this->assertStringContainsString('alice', $body, "expected alice in: {$body}");
                $conn->query(
                    'INSERT INTO smoke_embeddings VECTOR (dense, content) VALUES ($1, $2)',
                    [[0.7, 0.7], 'parameterized doc'],
                );
                $vectorBody = $conn->query(
                    'SEARCH SIMILAR $1 COLLECTION smoke_embeddings LIMIT 1',
                    [[0.7, 0.7]],
                );
                $this->assertStringContainsString('"record_count":1', $vectorBody, "expected vector match in: {$vectorBody}");
            } finally {
                $conn->close();
            }
        } finally {
            foreach ($pipes as $p) {
                if (is_resource($p)) {
                    @fclose($p);
                }
            }
            proc_terminate($proc);
            $deadline = microtime(true) + 10;
            while (microtime(true) < $deadline) {
                $status = proc_get_status($proc);
                if (!$status['running']) {
                    break;
                }
                usleep(100_000);
            }
            proc_close($proc);
        }
    }

    /** @return list<string> */
    private function redCommand(string $dataPath, int $port): array
    {
        $redBin = getenv('RED_BIN');
        if (is_string($redBin) && $redBin !== '') {
            return [$redBin, 'server', '--path', $dataPath, '--bind', "127.0.0.1:{$port}"];
        }
        return [
            'cargo', 'run', '--release', '--bin', 'red', '--',
            'server', '--path', $dataPath, '--bind', "127.0.0.1:{$port}",
        ];
    }

    private function waitForConnect(string $uri, int $timeoutSec): \Reddb\Conn
    {
        $deadline = microtime(true) + $timeoutSec;
        $last = null;
        while (microtime(true) < $deadline) {
            try {
                $conn = Reddb::connect($uri);
                try {
                    $conn->ping();
                    return $conn;
                } catch (\Throwable $e) {
                    $conn->close();
                    $last = $e;
                }
            } catch (\Throwable $e) {
                $last = $e;
            }
            usleep(50_000);
        }
        $message = 'server did not accept connections at ' . $uri;
        if ($last instanceof \Throwable) {
            $message .= ': ' . $last->getMessage();
        }
        $this->fail($message);
    }

    private function freePort(): int
    {
        $socket = stream_socket_server('tcp://127.0.0.1:0', $errno, $errstr);
        if ($socket === false) {
            $this->fail("could not allocate free port: {$errstr}");
        }
        $name = stream_socket_get_name($socket, false);
        fclose($socket);
        if (!is_string($name) || !preg_match('/:(\d+)$/', $name, $m)) {
            $this->fail('could not determine free port');
        }
        return (int) $m[1];
    }

    private function findRepoRoot(): string
    {
        $dir = __DIR__;
        while ($dir !== '/' && $dir !== '') {
            if (is_file($dir . '/Cargo.toml') && is_dir($dir . '/drivers')) {
                return $dir;
            }
            $dir = dirname($dir);
        }
        $this->fail('could not locate repo root with Cargo.toml + drivers/');
    }
}
