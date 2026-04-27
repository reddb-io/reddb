package dev.reddb

import dev.reddb.redwire.Scram
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.charset.StandardCharsets
import java.util.Base64
import java.util.HexFormat

class ScramTest {

    /** RFC 4231 § 4.2 — HMAC-SHA-256 test case 1. */
    @Test
    fun hmacSha256Rfc4231Case1() {
        val key = ByteArray(20) { 0x0b }
        val data = "Hi There".toByteArray(StandardCharsets.US_ASCII)
        val expected = HexFormat.of().parseHex(
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        )
        assertArrayEquals(expected, Scram.hmacSha256(key, data))
    }

    /** PBKDF2 deterministic check — same inputs always produce the same output. */
    @Test
    fun pbkdf2IsDeterministic() {
        val a = Scram.saltedPassword("hunter2", "salt".toByteArray(StandardCharsets.UTF_8), 4096)
        val b = Scram.saltedPassword("hunter2", "salt".toByteArray(StandardCharsets.UTF_8), 4096)
        assertArrayEquals(a, b)
        val c = Scram.saltedPassword("different", "salt".toByteArray(StandardCharsets.UTF_8), 4096)
        assertFalse(a.contentEquals(c))
        assertEquals(32, a.size)
    }

    @Test
    fun clientFirstShapeMatchesEngineParser() {
        val cf = Scram.clientFirst("alice", "nonce-A")
        assertTrue(cf.startsWith("n,,"))
        val bare = Scram.clientFirstBare(cf)
        assertEquals("n=alice,r=nonce-A", bare)
    }

    @Test
    fun rejectsClientFirstMissingGs2Header() {
        assertThrows(RedDBException.ProtocolError::class.java) {
            Scram.clientFirstBare("p=tls-unique,n=alice,r=x")
        }
    }

    @Test
    fun rejectsUsernameWithReservedCharacters() {
        assertThrows(IllegalArgumentException::class.java) { Scram.clientFirst("a,b", "n") }
        assertThrows(IllegalArgumentException::class.java) { Scram.clientFirst("a=b", "n") }
    }

    @Test
    fun parseServerFirstExtractsFields() {
        val salt = byteArrayOf(1, 2, 3, 4, 5)
        val sf = "r=cnonceSnonce,s=${Base64.getEncoder().encodeToString(salt)},i=4096"
        val parsed = Scram.parseServerFirst(sf, "cnonce")
        assertEquals("cnonceSnonce", parsed.combinedNonce)
        assertArrayEquals(salt, parsed.salt)
        assertEquals(4096, parsed.iter)
    }

    @Test
    fun parseServerFirstRejectsBadNoncePrefix() {
        val sf = "r=other,s=AAAA,i=4096"
        assertThrows(RedDBException.ProtocolError::class.java) { Scram.parseServerFirst(sf, "cnonce") }
    }

    @Test
    fun parseServerFirstRejectsLowIter() {
        val sf = "r=cnonce,s=AAAA,i=1024"
        assertThrows(RedDBException.ProtocolError::class.java) { Scram.parseServerFirst(sf, "cnonce") }
    }

    @Test
    fun clientFinalNoProofUsesBiwsHeader() {
        assertEquals("c=biws,r=COMBINED", Scram.clientFinalNoProof("COMBINED"))
    }

    @Test
    fun authMessageJoinsWithCommas() {
        val m = Scram.authMessage("a", "b", "c")
        assertEquals("a,b,c", String(m, StandardCharsets.UTF_8))
    }

    /** Round-trip the full proof against an engine-style verifier. */
    @Test
    fun clientProofRoundTripsAgainstStoredKey() {
        val salt = "reddb-rt-salt".toByteArray(StandardCharsets.UTF_8)
        val iter = 4096
        val password = "correct horse"

        // Server-side derivation.
        val salted = Scram.saltedPassword(password, salt, iter)
        val clientKey = Scram.hmacSha256(salted, "Client Key".toByteArray(StandardCharsets.UTF_8))
        val storedKey = Scram.sha256(clientKey)

        // Client-side proof.
        val clientFirstBare = "n=alice,r=cnonce"
        val serverFirst = "r=cnonceSnonce,s=${Base64.getEncoder().encodeToString(salt)},i=$iter"
        val clientFinalNoProof = "c=biws,r=cnonceSnonce"
        val authMessage = Scram.authMessage(clientFirstBare, serverFirst, clientFinalNoProof)
        val proof = Scram.clientProof(password, salt, iter, authMessage)

        // Server verifies.
        val sig = Scram.hmacSha256(storedKey, authMessage)
        val recoveredClientKey = Scram.xor(proof, sig)
        val derivedStored = Scram.sha256(recoveredClientKey)
        assertArrayEquals(storedKey, derivedStored)

        // Wrong password ⇒ verification fails.
        val wrongProof = Scram.clientProof("wrong", salt, iter, authMessage)
        val recoveredWrong = Scram.xor(wrongProof, sig)
        val derivedWrong = Scram.sha256(recoveredWrong)
        assertFalse(storedKey.contentEquals(derivedWrong))
    }

    @Test
    fun verifyServerSignatureRoundTrips() {
        val salt = "s".toByteArray(StandardCharsets.UTF_8)
        val iter = 4096
        val salted = Scram.saltedPassword("p", salt, iter)
        val serverKey = Scram.hmacSha256(salted, "Server Key".toByteArray(StandardCharsets.UTF_8))
        val am = "auth-message".toByteArray(StandardCharsets.UTF_8)
        val sig = Scram.hmacSha256(serverKey, am)

        assertTrue(Scram.verifyServerSignature("p", salt, iter, am, sig))
        assertFalse(Scram.verifyServerSignature("wrong", salt, iter, am, sig))
        // Wrong-length signature → false fast path.
        assertFalse(Scram.verifyServerSignature("p", salt, iter, am, ByteArray(10)))
    }

    @Test
    fun newClientNonceIsBase64AndUnique() {
        val a = Scram.newClientNonce()
        val b = Scram.newClientNonce()
        // base64(24 bytes) = 32 chars
        assertEquals(32, a.length)
        assertNotEquals(a, b)
        // Decodes to 24 bytes.
        assertEquals(24, Base64.getDecoder().decode(a).size)
    }

    @Test
    fun constantTimeEqMatchesEqualsForEqualInputs() {
        val a = HexFormat.of().parseHex("00112233445566778899aabbccddeeff")
        val b = HexFormat.of().parseHex("00112233445566778899aabbccddeeff")
        val c = HexFormat.of().parseHex("00112233445566778899aabbccddeefe")
        assertTrue(Scram.constantTimeEq(a, b))
        assertFalse(Scram.constantTimeEq(a, c))
        assertFalse(Scram.constantTimeEq(a, ByteArray(a.size - 1)))
    }
}
