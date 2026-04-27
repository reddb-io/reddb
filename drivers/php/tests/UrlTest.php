<?php

declare(strict_types=1);

namespace Reddb\Tests;

use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;
use Reddb\Url;

final class UrlTest extends TestCase
{
    /**
     * @return iterable<string, array{string, string, string, int}>
     */
    public static function remoteCases(): iterable
    {
        yield 'red default port' => ['red://localhost', Url::KIND_REDWIRE, 'localhost', 5050];
        yield 'red explicit port' => ['red://localhost:5050', Url::KIND_REDWIRE, 'localhost', 5050];
        yield 'red custom port' => ['red://example.com:9999', Url::KIND_REDWIRE, 'example.com', 9999];
        yield 'red ipv4' => ['red://10.0.0.1:1234', Url::KIND_REDWIRE, '10.0.0.1', 1234];
        yield 'reds default' => ['reds://reddb.example.com', Url::KIND_REDWIRE_TLS, 'reddb.example.com', 5050];
        yield 'reds 8443' => ['reds://reddb.example.com:8443', Url::KIND_REDWIRE_TLS, 'reddb.example.com', 8443];
        yield 'http default' => ['http://localhost', Url::KIND_HTTP, 'localhost', 5050];
        yield 'http 8080' => ['http://localhost:8080', Url::KIND_HTTP, 'localhost', 8080];
        yield 'https default' => ['https://reddb.example.com', Url::KIND_HTTPS, 'reddb.example.com', 5050];
        yield 'https 8443' => ['https://reddb.example.com:8443', Url::KIND_HTTPS, 'reddb.example.com', 8443];
    }

    #[DataProvider('remoteCases')]
    public function test_parses_remote_shapes(string $uri, string $kind, string $host, int $port): void
    {
        $u = Url::parse($uri);
        $this->assertSame($kind, $u->kind);
        $this->assertSame($host, $u->host);
        $this->assertSame($port, $u->port);
        $this->assertSame($uri, $u->original);
    }

    public function test_extracts_user_info_from_authority(): void
    {
        $u = Url::parse('red://alice:secret@host:5050');
        $this->assertSame('alice', $u->username);
        $this->assertSame('secret', $u->password);
        $this->assertSame('host', $u->host);
        $this->assertSame(5050, $u->port);
    }

    public function test_decodes_percent_escapes_in_user_info(): void
    {
        $u = Url::parse('red://al%40ice:p%2Fass@host');
        $this->assertSame('al@ice', $u->username);
        $this->assertSame('p/ass', $u->password);
    }

    public function test_parses_user_only_without_password(): void
    {
        $u = Url::parse('red://alice@host');
        $this->assertSame('alice', $u->username);
        $this->assertNull($u->password);
    }

    public function test_parses_query_params_token_and_api_key(): void
    {
        $u = Url::parse('red://host?token=tok-abc&apiKey=ak-xyz');
        $this->assertSame('tok-abc', $u->token);
        $this->assertSame('ak-xyz', $u->apiKey);
        $this->assertSame('tok-abc', $u->params['token']);
    }

    public function test_api_key_accepts_snake_case_fallback(): void
    {
        $u = Url::parse('red://host?api_key=ak-xyz');
        $this->assertSame('ak-xyz', $u->apiKey);
    }

    public function test_decodes_percent_escapes_in_query(): void
    {
        $u = Url::parse('red://host?token=a%20b%2Bc');
        $this->assertSame('a b+c', $u->token);
    }

    /**
     * @return iterable<string, array{string}>
     */
    public static function memoryAliases(): iterable
    {
        foreach (['red:', 'red:/', 'red://', 'red://memory', 'red://memory/', 'red://:memory', 'red://:memory:'] as $s) {
            yield $s => [$s];
        }
    }

    #[DataProvider('memoryAliases')]
    public function test_recognises_embedded_in_memory_aliases(string $s): void
    {
        $u = Url::parse($s);
        $this->assertSame(Url::KIND_EMBEDDED_MEMORY, $u->kind);
        $this->assertTrue($u->isEmbedded());
    }

    public function test_recognises_embedded_file_triple(): void
    {
        $u = Url::parse('red:///var/lib/reddb/data.rdb');
        $this->assertSame(Url::KIND_EMBEDDED_FILE, $u->kind);
        $this->assertSame('/var/lib/reddb/data.rdb', $u->path);
    }

    /**
     * @return iterable<string, array{string}>
     */
    public static function unsupportedSchemes(): iterable
    {
        yield 'mongodb' => ['mongodb://localhost'];
        yield 'ftp' => ['ftp://host'];
        yield 'grpc' => ['grpc://host'];
        yield 'tcp' => ['tcp://host'];
    }

    #[DataProvider('unsupportedSchemes')]
    public function test_rejects_unsupported_schemes(string $uri): void
    {
        $this->expectException(\InvalidArgumentException::class);
        Url::parse($uri);
    }

    public function test_rejects_empty(): void
    {
        $this->expectException(\InvalidArgumentException::class);
        Url::parse('');
    }

    public function test_red_scheme_with_explicit_empty_authority_is_embedded_memory(): void
    {
        $this->assertSame(Url::KIND_EMBEDDED_MEMORY, Url::parse('red://')->kind);
    }

    public function test_scheme_is_case_insensitive(): void
    {
        $u = Url::parse('RED://host:1');
        $this->assertSame(Url::KIND_REDWIRE, $u->kind);
    }

    public function test_is_redwire_flags_correctly(): void
    {
        $this->assertTrue(Url::parse('red://h')->isRedwire());
        $this->assertTrue(Url::parse('reds://h')->isRedwire());
        $this->assertFalse(Url::parse('http://h')->isRedwire());
    }

    public function test_is_tls_flags_correctly(): void
    {
        $this->assertTrue(Url::parse('reds://h')->isTls());
        $this->assertTrue(Url::parse('https://h')->isTls());
        $this->assertFalse(Url::parse('red://h')->isTls());
        $this->assertFalse(Url::parse('http://h')->isTls());
    }

    public function test_port_defaults_to_5050(): void
    {
        $this->assertSame(5050, Url::parse('red://h')->port);
        $this->assertSame(5050, Url::parse('reds://h')->port);
        $this->assertSame(5050, Url::parse('http://h')->port);
        $this->assertSame(5050, Url::parse('https://h')->port);
    }

    public function test_preserves_original_uri(): void
    {
        $s = 'red://user:pw@host:5050?token=t';
        $this->assertSame($s, Url::parse($s)->original);
    }

    public function test_assert_not_embedded_throws_on_memory(): void
    {
        $this->expectException(\Reddb\RedDBException\EmbeddedUnsupported::class);
        Url::parse('red://')->assertNotEmbedded();
    }

    public function test_assert_not_embedded_passes_remote(): void
    {
        Url::parse('red://h')->assertNotEmbedded();
        $this->assertTrue(true);
    }

    public function test_token_falls_back_to_url_query(): void
    {
        $u = Url::parse('reds://host:8443?token=abc');
        $this->assertSame('abc', $u->token);
    }

    public function test_unknown_query_param_kept_in_params(): void
    {
        $u = Url::parse('red://host?proto=ignored&extra=yes');
        $this->assertSame('yes', $u->params['extra']);
        $this->assertSame('ignored', $u->params['proto']);
    }
}
