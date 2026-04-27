package dev.reddb.redwire

import dev.reddb.RedDBException
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import java.security.NoSuchAlgorithmException
import java.security.SecureRandom
import java.security.spec.InvalidKeySpecException
import java.util.Base64
import javax.crypto.Mac
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.PBEKeySpec
import javax.crypto.spec.SecretKeySpec

/**
 * SCRAM-SHA-256 client primitives (RFC 5802 + RFC 7677).
 *
 * Pure functions — no I/O, no socket. The state machine that calls
 * these lives in [RedwireConn]; testing is easier when the crypto
 * stays separable.
 */
public object Scram {
    /** Default iteration count when the server doesn't override. */
    public const val DEFAULT_ITER: Int = 16_384

    /** Hard floor — verifiers below this are unsafe to use. */
    public const val MIN_ITER: Int = 4096

    /**
     * Generate 24 random bytes and base64-encode them. Matches the
     * server-side `new_server_nonce` shape (standard alphabet, no
     * URL-safe variant) so the combined nonce strings line up.
     */
    public fun newClientNonce(): String {
        val raw = ByteArray(24)
        SecureRandom().nextBytes(raw)
        return Base64.getEncoder().encodeToString(raw)
    }

    /** Build the SCRAM `client-first-message` (no channel binding, no authzid). */
    public fun clientFirst(username: String, clientNonce: String): String {
        // GS2 header `n,,` = no channel binding, no authzid.
        return "n,,n=${saslPrep(username)},r=$clientNonce"
    }

    /** Strip the `n,,` GS2 header — server-first verification uses the bare form. */
    public fun clientFirstBare(clientFirst: String): String {
        if (!clientFirst.startsWith("n,,")) {
            throw RedDBException.ProtocolError(
                "client-first must start with 'n,,' (no channel binding)"
            )
        }
        return clientFirst.substring(3)
    }

    /** Parsed shape of the server-first-message. */
    public class ServerFirst internal constructor(
        public val combinedNonce: String,
        public val salt: ByteArray,
        public val iter: Int,
        public val raw: String,
    )

    /**
     * Parse `r=<combined>,s=<b64salt>,i=<iter>`. Verifies the
     * combined nonce starts with the client's nonce — replay defence.
     */
    public fun parseServerFirst(serverFirst: String, clientNonce: String): ServerFirst {
        var combined: String? = null
        var saltB64: String? = null
        var iter: Int? = null
        for (part in serverFirst.split(",")) {
            when {
                part.startsWith("r=") -> combined = part.substring(2)
                part.startsWith("s=") -> saltB64 = part.substring(2)
                part.startsWith("i=") -> iter = try {
                    part.substring(2).toInt()
                } catch (e: NumberFormatException) {
                    throw RedDBException.ProtocolError("server-first iter is not an int: $part")
                }
            }
        }
        if (combined == null) throw RedDBException.ProtocolError("server-first missing r=")
        if (saltB64 == null) throw RedDBException.ProtocolError("server-first missing s=")
        if (iter == null) throw RedDBException.ProtocolError("server-first missing i=")
        if (!combined.startsWith(clientNonce)) {
            throw RedDBException.ProtocolError(
                "server-first nonce does not start with our client nonce — replay protection"
            )
        }
        if (iter < MIN_ITER) {
            throw RedDBException.ProtocolError("server-first iter $iter < MIN_ITER $MIN_ITER")
        }
        val salt = try {
            Base64.getDecoder().decode(saltB64)
        } catch (e: IllegalArgumentException) {
            throw RedDBException.ProtocolError("server-first salt is not base64")
        }
        return ServerFirst(combined, salt, iter, serverFirst)
    }

    /** Build the SCRAM `client-final-message-without-proof` (constant `c=biws`). */
    public fun clientFinalNoProof(combinedNonce: String): String {
        // c=biws is base64("n,,") — the canonical no-channel-binding header.
        return "c=biws,r=$combinedNonce"
    }

    /** Build the canonical `AuthMessage` per RFC 5802 § 3. */
    public fun authMessage(clientFirstBare: String, serverFirst: String, clientFinalNoProof: String): ByteArray {
        return "$clientFirstBare,$serverFirst,$clientFinalNoProof".toByteArray(StandardCharsets.UTF_8)
    }

    /** PBKDF2-HMAC-SHA256 → 32 bytes. */
    public fun saltedPassword(password: String, salt: ByteArray, iter: Int): ByteArray {
        try {
            val factory = SecretKeyFactory.getInstance("PBKDF2WithHmacSHA256")
            val spec = PBEKeySpec(password.toCharArray(), salt, iter, 256)
            return factory.generateSecret(spec).encoded
        } catch (e: NoSuchAlgorithmException) {
            throw RedDBException.ProtocolError("PBKDF2WithHmacSHA256 unavailable: ${e.message}", e)
        } catch (e: InvalidKeySpecException) {
            throw RedDBException.ProtocolError("PBKDF2WithHmacSHA256 unavailable: ${e.message}", e)
        }
    }

    /** HMAC-SHA-256(key, data). */
    public fun hmacSha256(key: ByteArray, data: ByteArray): ByteArray {
        try {
            val mac = Mac.getInstance("HmacSHA256")
            mac.init(SecretKeySpec(key, "HmacSHA256"))
            return mac.doFinal(data)
        } catch (e: Exception) {
            throw RedDBException.ProtocolError("HmacSHA256 unavailable: ${e.message}", e)
        }
    }

    /** SHA-256(data). */
    public fun sha256(data: ByteArray): ByteArray {
        try {
            return MessageDigest.getInstance("SHA-256").digest(data)
        } catch (e: NoSuchAlgorithmException) {
            throw RedDBException.ProtocolError("SHA-256 unavailable: ${e.message}", e)
        }
    }

    /** Bytewise XOR of two equal-length arrays. */
    public fun xor(a: ByteArray, b: ByteArray): ByteArray {
        require(a.size == b.size) { "xor length mismatch: ${a.size} vs ${b.size}" }
        val out = ByteArray(a.size)
        for (i in a.indices) out[i] = (a[i].toInt() xor b[i].toInt()).toByte()
        return out
    }

    /**
     * Compute the SCRAM client proof. Mirrors the formula in
     * `src/auth/scram.rs`: `ClientKey XOR HMAC(StoredKey, AuthMessage)`.
     */
    public fun clientProof(password: String, salt: ByteArray, iter: Int, authMessage: ByteArray): ByteArray {
        val salted = saltedPassword(password, salt, iter)
        val clientKey = hmacSha256(salted, "Client Key".toByteArray(StandardCharsets.UTF_8))
        val storedKey = sha256(clientKey)
        val sig = hmacSha256(storedKey, authMessage)
        return xor(clientKey, sig)
    }

    /**
     * Verify the server signature. Returns true when the server
     * also knew the verifier (defence against an active MITM).
     * Constant-time comparison.
     */
    public fun verifyServerSignature(
        password: String,
        salt: ByteArray,
        iter: Int,
        authMessage: ByteArray,
        presentedSignature: ByteArray?,
    ): Boolean {
        if (presentedSignature == null || presentedSignature.size != 32) return false
        val salted = saltedPassword(password, salt, iter)
        val serverKey = hmacSha256(salted, "Server Key".toByteArray(StandardCharsets.UTF_8))
        val expected = hmacSha256(serverKey, authMessage)
        return constantTimeEq(expected, presentedSignature)
    }

    /** Constant-time equality. */
    public fun constantTimeEq(a: ByteArray?, b: ByteArray?): Boolean {
        if (a == null || b == null || a.size != b.size) return false
        var diff = 0
        for (i in a.indices) diff = diff or ((a[i].toInt() xor b[i].toInt()) and 0xff)
        return diff == 0
    }

    /** Build the SCRAM `client-final-message` (c=,r=,p=). */
    public fun clientFinal(combinedNonce: String, proof: ByteArray): String {
        return "c=biws,r=$combinedNonce,p=${Base64.getEncoder().encodeToString(proof)}"
    }

    /**
     * SASLprep is full Stringprep + a few extra rules. RedDB's server
     * treats usernames as opaque byte strings; the JS / Rust drivers
     * don't apply SASLprep either. We just forbid `,` and `=` because
     * they break the wire format.
     */
    internal fun saslPrep(input: String): String {
        require(input.indexOf(',') < 0) { "SCRAM username contains illegal ',': $input" }
        require(input.indexOf('=') < 0) { "SCRAM username contains illegal '=': $input" }
        return input
    }
}
