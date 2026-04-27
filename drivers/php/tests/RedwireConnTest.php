<?php

declare(strict_types=1);

namespace Reddb\Tests;

use PHPUnit\Framework\TestCase;
use Reddb\RedDBException\AuthRefused;
use Reddb\RedDBException\ProtocolError;
use Reddb\Redwire\Frame;
use Reddb\Redwire\RedwireConn;

/**
 * Drive the handshake state machine over a pair of socketpair-bound
 * streams. The "server" runs in a forked child process so the
 * driver can do blocking reads/writes against a synthetic peer
 * without spawning the engine. PCNTL is part of the static-php-cli
 * build the project ships with; tests skip themselves on hosts
 * where it's missing.
 */
final class RedwireConnTest extends TestCase
{
    protected function setUp(): void
    {
        if (!function_exists('pcntl_fork')) {
            $this->markTestSkipped('pcntl_fork() not available');
        }
        if (!function_exists('stream_socket_pair')) {
            $this->markTestSkipped('stream_socket_pair() not available');
        }
    }

    public function test_handshake_anonymous_succeeds(): void
    {
        $session = $this->withFakeServer(static function ($serverSock): void {
            self::expectMagic($serverSock);
            $hello = self::readFrameFromSock($serverSock);
            self::assertSame(Frame::KIND_HELLO, $hello->kind);
            $helloJson = json_decode($hello->payload, true);
            self::assertIsArray($helloJson);
            self::assertContains('anonymous', $helloJson['auth_methods']);

            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_HELLO_ACK, $hello->correlationId, json_encode([
                    'auth' => 'anonymous', 'version' => 1, 'features' => 0,
                ])),
            );

            $resp = self::readFrameFromSock($serverSock);
            self::assertSame(Frame::KIND_AUTH_RESPONSE, $resp->kind);
            self::assertSame('', $resp->payload);

            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_AUTH_OK, $resp->correlationId, json_encode([
                    'session_id' => 'rwsess-test-anon', 'username' => 'anonymous', 'role' => 'read',
                ])),
            );
        }, function ($clientSock) {
            return RedwireConn::performHandshake($clientSock, null, null, null, 'test-driver');
        });
        $this->assertSame('rwsess-test-anon', $session['session_id']);
    }

    public function test_handshake_bearer_succeeds(): void
    {
        $session = $this->withFakeServer(static function ($serverSock): void {
            self::expectMagic($serverSock);
            $hello = self::readFrameFromSock($serverSock);
            $helloJson = json_decode($hello->payload, true);
            self::assertContains('bearer', $helloJson['auth_methods']);

            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_HELLO_ACK, $hello->correlationId, json_encode(['auth' => 'bearer'])),
            );

            $resp = self::readFrameFromSock($serverSock);
            $body = json_decode($resp->payload, true);
            self::assertSame('the-token', $body['token']);

            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_AUTH_OK, $resp->correlationId, json_encode([
                    'session_id' => 'rwsess-test-bearer',
                ])),
            );
        }, function ($clientSock) {
            return RedwireConn::performHandshake($clientSock, null, null, 'the-token', 'test-driver');
        });
        $this->assertSame('rwsess-test-bearer', $session['session_id']);
    }

    public function test_auth_fail_at_hello_ack_throws_auth_refused(): void
    {
        $this->expectException(AuthRefused::class);
        $this->expectExceptionMessageMatches('/no overlapping auth method/');
        $this->withFakeServer(static function ($serverSock): void {
            self::expectMagic($serverSock);
            $hello = self::readFrameFromSock($serverSock);
            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_AUTH_FAIL, $hello->correlationId, json_encode([
                    'reason' => 'no overlapping auth method',
                ])),
            );
        }, function ($clientSock): void {
            RedwireConn::performHandshake($clientSock, null, null, null, 'test-driver');
        });
    }

    public function test_auth_fail_at_auth_ok_throws_auth_refused(): void
    {
        $this->expectException(AuthRefused::class);
        $this->expectExceptionMessageMatches('/bearer token invalid/');
        $this->withFakeServer(static function ($serverSock): void {
            self::expectMagic($serverSock);
            $hello = self::readFrameFromSock($serverSock);
            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_HELLO_ACK, $hello->correlationId, json_encode(['auth' => 'bearer'])),
            );
            $resp = self::readFrameFromSock($serverSock);
            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_AUTH_FAIL, $resp->correlationId, json_encode([
                    'reason' => 'bearer token invalid',
                ])),
            );
        }, function ($clientSock): void {
            RedwireConn::performHandshake($clientSock, null, null, 'bad-token', 'test-driver');
        });
    }

    public function test_server_picks_unsupported_auth_method_throws_protocol_error(): void
    {
        $this->expectException(ProtocolError::class);
        $this->expectExceptionMessageMatches('/made-up-method/');
        $this->withFakeServer(static function ($serverSock): void {
            self::expectMagic($serverSock);
            $hello = self::readFrameFromSock($serverSock);
            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_HELLO_ACK, $hello->correlationId, json_encode(['auth' => 'made-up-method'])),
            );
        }, function ($clientSock): void {
            RedwireConn::performHandshake($clientSock, null, null, null, 'test-driver');
        });
    }

    public function test_malformed_hello_ack_json_raises_protocol_error(): void
    {
        $this->expectException(ProtocolError::class);
        $this->withFakeServer(static function ($serverSock): void {
            self::expectMagic($serverSock);
            $hello = self::readFrameFromSock($serverSock);
            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_HELLO_ACK, $hello->correlationId, 'not json'),
            );
        }, function ($clientSock): void {
            RedwireConn::performHandshake($clientSock, null, null, null, 'test-driver');
        });
    }

    public function test_query_round_trip_after_handshake(): void
    {
        // Fork once: child plays the server through both handshake + query.
        $sessionResult = $this->withFakeServer(static function ($serverSock): void {
            self::expectMagic($serverSock);
            $hello = self::readFrameFromSock($serverSock);
            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_HELLO_ACK, $hello->correlationId, json_encode(['auth' => 'anonymous'])),
            );
            $resp = self::readFrameFromSock($serverSock);
            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_AUTH_OK, $resp->correlationId, json_encode(['session_id' => 'rwsess-q'])),
            );

            // Query.
            $q = self::readFrameFromSock($serverSock);
            self::assertSame(Frame::KIND_QUERY, $q->kind);
            self::assertSame('SELECT 1', $q->payload);
            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_RESULT, $q->correlationId, json_encode([
                    'ok' => true, 'affected' => 1,
                ])),
            );

            // Drain the Bye frame so the child can exit cleanly.
            try {
                self::readFrameFromSock($serverSock);
            } catch (\Throwable) {
                // pipe close
            }
        }, function ($clientSock) {
            $session = RedwireConn::performHandshake($clientSock, null, null, null, 'test-driver');
            $conn = new RedwireConn($clientSock, $session);
            $body = json_decode($conn->query('SELECT 1'), true);
            self::assertTrue($body['ok']);
            self::assertSame(1, $body['affected']);
            $conn->close();
            return $session;
        });
        $this->assertSame('rwsess-q', $sessionResult['session_id']);
    }

    public function test_client_sends_magic_byte_first(): void
    {
        // The expectMagic helper already asserts the byte sequence; piggyback
        // by aborting right after with AuthFail so the handshake throws.
        $this->expectException(AuthRefused::class);
        $this->withFakeServer(static function ($serverSock): void {
            self::expectMagic($serverSock);
            $hello = self::readFrameFromSock($serverSock);
            self::writeFrameToSock(
                $serverSock,
                Frame::make(Frame::KIND_AUTH_FAIL, $hello->correlationId, json_encode(['reason' => 'stop here'])),
            );
        }, function ($clientSock): void {
            RedwireConn::performHandshake($clientSock, null, null, null, 'test-driver');
        });
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    /**
     * Run the supplied $serverFn in a forked child against one end of a
     * stream_socket_pair, and call $clientFn with the other end here in
     * the parent. Reaps the child and propagates any non-zero exit
     * status as an assertion failure.
     *
     * @template T
     * @param callable(resource): void $serverFn
     * @param callable(resource): T $clientFn
     * @return T
     */
    private function withFakeServer(callable $serverFn, callable $clientFn)
    {
        $pair = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, STREAM_IPPROTO_IP);
        if ($pair === false) {
            $this->fail('stream_socket_pair() failed');
        }
        [$a, $b] = $pair;
        stream_set_timeout($a, 5);
        stream_set_timeout($b, 5);

        $pid = pcntl_fork();
        if ($pid === -1) {
            fclose($a);
            fclose($b);
            $this->fail('pcntl_fork() failed');
        }
        if ($pid === 0) {
            // Child: be the server.
            fclose($a);
            try {
                $serverFn($b);
                $exit = 0;
            } catch (\Throwable $e) {
                fwrite(STDERR, 'fake-server error: ' . $e->getMessage() . PHP_EOL);
                $exit = 1;
            } finally {
                if (is_resource($b)) {
                    fclose($b);
                }
            }
            // exit() runs PHPUnit shutdown hooks which we don't want — kill
            // the process directly so the parent's pcntl_waitpid() unblocks
            // without scribbling another junit summary onto stdout.
            posix_kill(posix_getpid(), SIGKILL);
            exit($exit);
        }

        // Parent: drive the client.
        fclose($b);
        try {
            $result = $clientFn($a);
        } finally {
            // The client may have closed $a already (RedwireConn::close()
            // owns the resource); guard with is_resource to avoid the
            // PHP 8 TypeError on double-close.
            if (is_resource($a)) {
                fclose($a);
            }
            // Reap the child. The SIGKILL above guarantees a fast exit
            // even when the test threw — the child won't write further.
            $status = 0;
            pcntl_waitpid($pid, $status);
        }
        return $result;
    }

    /** @param resource $sock */
    private static function expectMagic($sock): void
    {
        $buf = '';
        while (strlen($buf) < 2) {
            $chunk = fread($sock, 2 - strlen($buf));
            if ($chunk === false || $chunk === '') {
                self::fail('short read on magic preamble');
            }
            $buf .= $chunk;
        }
        self::assertSame(chr(Frame::MAGIC) . chr(Frame::SUPPORTED_VERSION), $buf, 'magic preamble mismatch');
    }

    /** @param resource $sock */
    private static function readFrameFromSock($sock): Frame
    {
        $header = self::readN($sock, Frame::HEADER_SIZE);
        $length = Frame::encodedLength($header);
        $rest = $length === Frame::HEADER_SIZE ? '' : self::readN($sock, $length - Frame::HEADER_SIZE);
        return Frame::decode($header . $rest);
    }

    /** @param resource $sock */
    private static function writeFrameToSock($sock, Frame $frame): void
    {
        $bytes = Frame::encode($frame);
        $remaining = strlen($bytes);
        $offset = 0;
        while ($remaining > 0) {
            $n = fwrite($sock, substr($bytes, $offset, $remaining));
            if ($n === false || $n === 0) {
                self::fail('short write to fake-server pipe');
            }
            $offset += $n;
            $remaining -= $n;
        }
    }

    /** @param resource $sock */
    private static function readN($sock, int $n): string
    {
        $buf = '';
        while (strlen($buf) < $n) {
            $chunk = fread($sock, $n - strlen($buf));
            if ($chunk === false || $chunk === '') {
                self::fail("short read: wanted {$n}, got " . strlen($buf));
            }
            $buf .= $chunk;
        }
        return $buf;
    }
}
