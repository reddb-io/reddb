<?php

declare(strict_types=1);

namespace Reddb\Tests;

use PHPUnit\Framework\TestCase;
use Reddb\RedDBException\ProtocolError;
use Reddb\Redwire\Scram;

final class ScramTest extends TestCase
{
    /** RFC 4231 § 4.2 — HMAC-SHA-256 test case 1. */
    public function test_hmac_sha256_rfc4231_case1(): void
    {
        $key = str_repeat("\x0b", 20);
        $data = 'Hi There';
        $expected = hex2bin(
            'b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7'
        );
        $this->assertSame($expected, Scram::hmacSha256($key, $data));
    }

    public function test_pbkdf2_is_deterministic(): void
    {
        $a = Scram::saltedPassword('hunter2', 'salt', 4096);
        $b = Scram::saltedPassword('hunter2', 'salt', 4096);
        $this->assertSame($a, $b);
        $c = Scram::saltedPassword('different', 'salt', 4096);
        $this->assertNotSame($a, $c);
        $this->assertSame(32, strlen($a));
    }

    public function test_client_first_shape_matches_engine_parser(): void
    {
        $cf = Scram::clientFirst('alice', 'nonce-A');
        $this->assertStringStartsWith('n,,', $cf);
        $this->assertSame('n=alice,r=nonce-A', Scram::clientFirstBare($cf));
    }

    public function test_rejects_client_first_missing_gs2_header(): void
    {
        $this->expectException(ProtocolError::class);
        Scram::clientFirstBare('p=tls-unique,n=alice,r=x');
    }

    public function test_rejects_username_with_reserved_characters(): void
    {
        $this->expectException(\InvalidArgumentException::class);
        Scram::clientFirst('a,b', 'n');
    }

    public function test_rejects_username_with_equal_sign(): void
    {
        $this->expectException(\InvalidArgumentException::class);
        Scram::clientFirst('a=b', 'n');
    }

    public function test_parse_server_first_extracts_fields(): void
    {
        $salt = "\x01\x02\x03\x04\x05";
        $sf = 'r=cnonceSnonce,s=' . base64_encode($salt) . ',i=4096';
        $parsed = Scram::parseServerFirst($sf, 'cnonce');
        $this->assertSame('cnonceSnonce', $parsed['combinedNonce']);
        $this->assertSame($salt, $parsed['salt']);
        $this->assertSame(4096, $parsed['iter']);
    }

    public function test_parse_server_first_rejects_bad_nonce_prefix(): void
    {
        $this->expectException(ProtocolError::class);
        Scram::parseServerFirst('r=other,s=AAAA,i=4096', 'cnonce');
    }

    public function test_parse_server_first_rejects_low_iter(): void
    {
        $this->expectException(ProtocolError::class);
        Scram::parseServerFirst('r=cnonce,s=AAAA,i=1024', 'cnonce');
    }

    public function test_client_final_no_proof_uses_biws_header(): void
    {
        $this->assertSame('c=biws,r=COMBINED', Scram::clientFinalNoProof('COMBINED'));
    }

    public function test_auth_message_joins_with_commas(): void
    {
        $this->assertSame('a,b,c', Scram::authMessage('a', 'b', 'c'));
    }

    public function test_client_proof_round_trips_against_stored_key(): void
    {
        $salt = 'reddb-rt-salt';
        $iter = 4096;
        $password = 'correct horse';

        // Server-side derivation (mirror of ScramVerifier::from_password).
        $salted = Scram::saltedPassword($password, $salt, $iter);
        $clientKey = Scram::hmacSha256($salted, 'Client Key');
        $storedKey = Scram::sha256($clientKey);

        // Client-side proof.
        $cfb = 'n=alice,r=cnonce';
        $serverFirst = 'r=cnonceSnonce,s=' . base64_encode($salt) . ',i=' . $iter;
        $cfnp = 'c=biws,r=cnonceSnonce';
        $am = Scram::authMessage($cfb, $serverFirst, $cfnp);
        $proof = Scram::clientProof($password, $salt, $iter, $am);

        // Server verifies — XOR proof with HMAC(storedKey, am), then SHA-256 == storedKey.
        $sig = Scram::hmacSha256($storedKey, $am);
        $recoveredClientKey = Scram::xor($proof, $sig);
        $this->assertSame($storedKey, Scram::sha256($recoveredClientKey));

        // Wrong password ⇒ verification fails.
        $wrongProof = Scram::clientProof('wrong', $salt, $iter, $am);
        $recoveredWrong = Scram::xor($wrongProof, $sig);
        $this->assertNotSame($storedKey, Scram::sha256($recoveredWrong));
    }

    public function test_verify_server_signature_round_trips(): void
    {
        $salt = 's';
        $iter = 4096;
        $salted = Scram::saltedPassword('p', $salt, $iter);
        $serverKey = Scram::hmacSha256($salted, 'Server Key');
        $am = 'auth-message';
        $sig = Scram::hmacSha256($serverKey, $am);

        $this->assertTrue(Scram::verifyServerSignature('p', $salt, $iter, $am, $sig));
        $this->assertFalse(Scram::verifyServerSignature('wrong', $salt, $iter, $am, $sig));
        $this->assertFalse(Scram::verifyServerSignature('p', $salt, $iter, $am, str_repeat("\x00", 10)));
    }

    public function test_new_client_nonce_is_base64_and_unique(): void
    {
        $a = Scram::newClientNonce();
        $b = Scram::newClientNonce();
        $this->assertSame(32, strlen($a)); // base64(24 bytes) = 32 chars
        $this->assertNotSame($a, $b);
        $this->assertSame(24, strlen(base64_decode($a, true)));
    }

    public function test_xor_rejects_length_mismatch(): void
    {
        $this->expectException(\InvalidArgumentException::class);
        Scram::xor('abc', 'ab');
    }

    public function test_client_final_carries_base64_proof(): void
    {
        $proof = "\x00\x11\x22\x33";
        $msg = Scram::clientFinal('combined', $proof);
        $this->assertStringContainsString(',p=' . base64_encode($proof), $msg);
        $this->assertStringStartsWith('c=biws,r=combined', $msg);
    }
}
