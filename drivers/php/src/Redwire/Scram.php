<?php
/**
 * SCRAM-SHA-256 client primitives (RFC 5802 + RFC 7677).
 *
 * Pure functions, no I/O — the state machine that calls these
 * lives in {@see RedwireConn}. Mirrors the engine's
 * `src/auth/scram.rs` and the Java / Rust drivers byte-for-byte.
 */

declare(strict_types=1);

namespace Reddb\Redwire;

use Reddb\RedDBException\ProtocolError;

final class Scram
{
    /** Default iteration count when the server doesn't override. */
    public const DEFAULT_ITER = 16384;
    /** Hard floor — verifiers below this are unsafe to use. */
    public const MIN_ITER = 4096;

    /**
     * Generate 24 random bytes and base64-encode them. Matches the
     * engine-side `new_server_nonce` shape (standard alphabet).
     */
    public static function newClientNonce(): string
    {
        return base64_encode(random_bytes(24));
    }

    /** Build the SCRAM `client-first-message` (no channel binding, no authzid). */
    public static function clientFirst(string $username, string $clientNonce): string
    {
        // GS2 header `n,,` = no channel binding, no authzid.
        return 'n,,n=' . self::saslPrep($username) . ',r=' . $clientNonce;
    }

    /** Strip the `n,,` GS2 header — server-first verification uses the bare form. */
    public static function clientFirstBare(string $clientFirst): string
    {
        if (!str_starts_with($clientFirst, 'n,,')) {
            throw new ProtocolError("client-first must start with 'n,,' (no channel binding)");
        }
        return substr($clientFirst, 3);
    }

    /**
     * Parse `r=<combined>,s=<b64salt>,i=<iter>`. Verifies the
     * combined nonce starts with the client's nonce — replay defence.
     *
     * @return array{combinedNonce: string, salt: string, iter: int, raw: string}
     */
    public static function parseServerFirst(string $serverFirst, string $clientNonce): array
    {
        $combined = null;
        $saltB64 = null;
        $iter = null;
        foreach (explode(',', $serverFirst) as $part) {
            if (str_starts_with($part, 'r=')) {
                $combined = substr($part, 2);
            } elseif (str_starts_with($part, 's=')) {
                $saltB64 = substr($part, 2);
            } elseif (str_starts_with($part, 'i=')) {
                $raw = substr($part, 2);
                if ($raw === '' || !ctype_digit($raw)) {
                    throw new ProtocolError("server-first iter is not an int: {$part}");
                }
                $iter = (int) $raw;
            }
        }
        if ($combined === null) {
            throw new ProtocolError('server-first missing r=');
        }
        if ($saltB64 === null) {
            throw new ProtocolError('server-first missing s=');
        }
        if ($iter === null) {
            throw new ProtocolError('server-first missing i=');
        }
        if (!str_starts_with($combined, $clientNonce)) {
            throw new ProtocolError(
                'server-first nonce does not start with our client nonce — replay protection'
            );
        }
        if ($iter < self::MIN_ITER) {
            throw new ProtocolError(
                "server-first iter {$iter} < MIN_ITER " . self::MIN_ITER
            );
        }
        $salt = base64_decode($saltB64, true);
        if ($salt === false) {
            throw new ProtocolError('server-first salt is not base64');
        }
        return [
            'combinedNonce' => $combined,
            'salt' => $salt,
            'iter' => $iter,
            'raw' => $serverFirst,
        ];
    }

    /** Build the SCRAM `client-final-message-without-proof` (constant `c=biws`). */
    public static function clientFinalNoProof(string $combinedNonce): string
    {
        // c=biws is base64("n,,") — the canonical no-channel-binding header.
        return 'c=biws,r=' . $combinedNonce;
    }

    /** Build the canonical `AuthMessage` per RFC 5802 § 3. */
    public static function authMessage(string $clientFirstBare, string $serverFirst, string $clientFinalNoProof): string
    {
        return $clientFirstBare . ',' . $serverFirst . ',' . $clientFinalNoProof;
    }

    /** PBKDF2-HMAC-SHA256 → 32 bytes. */
    public static function saltedPassword(string $password, string $salt, int $iter): string
    {
        return hash_pbkdf2('sha256', $password, $salt, $iter, 32, true);
    }

    /** HMAC-SHA-256(key, data). */
    public static function hmacSha256(string $key, string $data): string
    {
        return hash_hmac('sha256', $data, $key, true);
    }

    /** SHA-256(data). */
    public static function sha256(string $data): string
    {
        return hash('sha256', $data, true);
    }

    /** Bytewise XOR of two equal-length strings. */
    public static function xor(string $a, string $b): string
    {
        $la = strlen($a);
        $lb = strlen($b);
        if ($la !== $lb) {
            throw new \InvalidArgumentException("xor length mismatch: {$la} vs {$lb}");
        }
        $out = '';
        for ($i = 0; $i < $la; $i++) {
            $out .= chr(ord($a[$i]) ^ ord($b[$i]));
        }
        return $out;
    }

    /**
     * Compute the SCRAM client proof. Mirrors the engine formula:
     * {@code ClientKey XOR HMAC(StoredKey, AuthMessage)}.
     */
    public static function clientProof(string $password, string $salt, int $iter, string $authMessage): string
    {
        $salted = self::saltedPassword($password, $salt, $iter);
        $clientKey = self::hmacSha256($salted, 'Client Key');
        $storedKey = self::sha256($clientKey);
        $sig = self::hmacSha256($storedKey, $authMessage);
        return self::xor($clientKey, $sig);
    }

    /**
     * Verify the server signature. Returns true when the server
     * also knew the verifier (defence against an active MITM).
     * Constant-time comparison via {@see hash_equals()}.
     */
    public static function verifyServerSignature(
        string $password,
        string $salt,
        int $iter,
        string $authMessage,
        string $presentedSignature,
    ): bool {
        if (strlen($presentedSignature) !== 32) {
            return false;
        }
        $salted = self::saltedPassword($password, $salt, $iter);
        $serverKey = self::hmacSha256($salted, 'Server Key');
        $expected = self::hmacSha256($serverKey, $authMessage);
        return hash_equals($expected, $presentedSignature);
    }

    /** Build the SCRAM `client-final-message` (c=,r=,p=). */
    public static function clientFinal(string $combinedNonce, string $proof): string
    {
        return 'c=biws,r=' . $combinedNonce . ',p=' . base64_encode($proof);
    }

    /**
     * Stripped-down SASLprep — RedDB treats usernames as opaque
     * byte strings. We reject `,` and `=` because they break the
     * SCRAM wire format.
     */
    public static function saslPrep(string $input): string
    {
        if (str_contains($input, ',')) {
            throw new \InvalidArgumentException("SCRAM username contains illegal ',': {$input}");
        }
        if (str_contains($input, '=')) {
            throw new \InvalidArgumentException("SCRAM username contains illegal '=': {$input}");
        }
        return $input;
    }
}
