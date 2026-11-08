<?php

declare(strict_types=1);

namespace Reddb\Tests;

use PHPUnit\Framework\TestCase;
use Reddb\Reddb;
use Reddb\RedDBException\EngineError;

final class EmbeddedTest extends TestCase
{
    protected function setUp(): void
    {
        if (getenv('REDDB_EMBEDDED_SMOKE') !== '1') {
            $this->markTestSkipped('REDDB_EMBEDDED_SMOKE=1 not set');
        }
    }

    public function test_memory_and_single_file_journey(): void
    {
        $memory = Reddb::connect('memory://');
        $memory->query('CREATE TABLE php_embedded (id INTEGER, name TEXT)');
        $memory->query('INSERT INTO php_embedded (id, name) VALUES ($1, $2)', [7, 'embedded']);
        self::assertStringContainsString(
            'embedded',
            $memory->query('SELECT name FROM php_embedded WHERE id = $1', [7]),
        );
        $memory->close();

        $directory = sys_get_temp_dir() . '/reddb-php-' . bin2hex(random_bytes(8));
        self::assertTrue(mkdir($directory));
        $database = $directory . '/driver-contract.rdb';
        $uri = 'file://' . $database;
        try {
            $first = Reddb::connect($uri);
            $first->query('CREATE TABLE durable (id INTEGER, name TEXT)');
            $first->query("INSERT INTO durable (id, name) VALUES (1, 'persisted')");
            $this->expectExceptionForSecondWriter($uri);
            $first->close();

            $reopened = Reddb::connect($uri);
            self::assertStringContainsString(
                'persisted',
                $reopened->query('SELECT name FROM durable WHERE id = 1'),
            );
            $reopened->close();
            self::assertSame(['driver-contract.rdb'], array_values(array_diff(scandir($directory), ['.', '..'])));
        } finally {
            if (is_file($database)) unlink($database);
            if (is_dir($directory)) rmdir($directory);
        }
    }

    private function expectExceptionForSecondWriter(string $uri): void
    {
        try {
            $second = Reddb::connect($uri);
            $second->close();
            self::fail('second writer unexpectedly opened');
        } catch (EngineError) {
            self::assertTrue(true);
        }
    }
}
