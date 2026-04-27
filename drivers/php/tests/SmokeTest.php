<?php

declare(strict_types=1);

namespace Reddb\Tests;

use PHPUnit\Framework\TestCase;
use Reddb\Reddb;

/**
 * End-to-end smoke against a freshly-spawned RedDB binary. Gated on
 * `RED_SMOKE=1` so normal test runs don't drag in a cargo build.
 * The test discovers the bind port from stdout — the engine prints
 * `listening on tcp://127.0.0.1:<port>` once the listener is up.
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
        $cmd = ['cargo', 'run', '--release', '--bin', 'red', '--', 'serve', '--bind', '127.0.0.1:0', '--anon-ok'];
        $descriptors = [
            0 => ['pipe', 'r'],
            1 => ['pipe', 'w'],
            2 => ['redirect', 1],
        ];
        $proc = proc_open($cmd, $descriptors, $pipes, $repoRoot);
        if (!is_resource($proc)) {
            $this->fail('failed to spawn cargo run');
        }
        try {
            $port = $this->waitForPort($pipes[1], 60);
            $conn = Reddb::connect("red://127.0.0.1:{$port}");
            try {
                $conn->ping();
                $conn->insert('smoke_users', ['name' => 'alice', 'age' => 30]);
                $body = $conn->query("SELECT * FROM smoke_users WHERE name = 'alice'");
                $this->assertStringContainsString('alice', $body, "expected alice in: {$body}");
                $conn->delete('smoke_users', 'alice');
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

    /** @param resource $stream */
    private function waitForPort($stream, int $timeoutSec): int
    {
        $deadline = microtime(true) + $timeoutSec;
        $buf = '';
        stream_set_blocking($stream, false);
        while (microtime(true) < $deadline) {
            $chunk = fread($stream, 8192);
            if ($chunk !== false && $chunk !== '') {
                $buf .= $chunk;
                if (preg_match('#(?:tcp://|listening on .*?:|port=)(\d{2,5})#', $buf, $m)) {
                    return (int) $m[1];
                }
            } else {
                usleep(50_000);
            }
        }
        $this->fail('never saw a bind port in engine stdout');
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
