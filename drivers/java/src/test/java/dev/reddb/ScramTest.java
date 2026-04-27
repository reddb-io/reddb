package dev.reddb;

import dev.reddb.redwire.Scram;
import org.junit.jupiter.api.Test;

import java.nio.charset.StandardCharsets;
import java.util.Base64;
import java.util.HexFormat;

import static org.junit.jupiter.api.Assertions.*;

class ScramTest {

    /** RFC 4231 § 4.2 — HMAC-SHA-256 test case 1. */
    @Test
    void hmacSha256Rfc4231Case1() {
        byte[] key = new byte[20];
        for (int i = 0; i < 20; i++) key[i] = 0x0b;
        byte[] data = "Hi There".getBytes(StandardCharsets.US_ASCII);
        byte[] expected = HexFormat.of().parseHex(
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        assertArrayEquals(expected, Scram.hmacSha256(key, data));
    }

    /**
     * RFC 7677 § 3 — single PBKDF2 iteration deterministic check
     * (we don't rely on the canonical SCRAM vector since the exact
     * salt depends on the test set; just verify determinism).
     */
    @Test
    void pbkdf2IsDeterministic() {
        byte[] a = Scram.saltedPassword("hunter2", "salt".getBytes(StandardCharsets.UTF_8), 4096);
        byte[] b = Scram.saltedPassword("hunter2", "salt".getBytes(StandardCharsets.UTF_8), 4096);
        assertArrayEquals(a, b);
        byte[] c = Scram.saltedPassword("different", "salt".getBytes(StandardCharsets.UTF_8), 4096);
        assertFalse(java.util.Arrays.equals(a, c));
        assertEquals(32, a.length);
    }

    @Test
    void clientFirstShapeMatchesEngineParser() {
        // Engine expects "n,,n=<user>,r=<nonce>" — strip and verify.
        String cf = Scram.clientFirst("alice", "nonce-A");
        assertTrue(cf.startsWith("n,,"));
        String bare = Scram.clientFirstBare(cf);
        assertEquals("n=alice,r=nonce-A", bare);
    }

    @Test
    void rejectsClientFirstMissingGs2Header() {
        assertThrows(RedDBException.ProtocolError.class, () -> Scram.clientFirstBare("p=tls-unique,n=alice,r=x"));
    }

    @Test
    void rejectsUsernameWithReservedCharacters() {
        assertThrows(IllegalArgumentException.class, () -> Scram.clientFirst("a,b", "n"));
        assertThrows(IllegalArgumentException.class, () -> Scram.clientFirst("a=b", "n"));
    }

    @Test
    void parseServerFirstExtractsFields() {
        byte[] salt = new byte[]{1, 2, 3, 4, 5};
        String sf = "r=cnonceSnonce,s=" + Base64.getEncoder().encodeToString(salt) + ",i=4096";
        Scram.ServerFirst parsed = Scram.parseServerFirst(sf, "cnonce");
        assertEquals("cnonceSnonce", parsed.combinedNonce);
        assertArrayEquals(salt, parsed.salt);
        assertEquals(4096, parsed.iter);
    }

    @Test
    void parseServerFirstRejectsBadNoncePrefix() {
        String sf = "r=other,s=AAAA,i=4096";
        assertThrows(RedDBException.ProtocolError.class, () -> Scram.parseServerFirst(sf, "cnonce"));
    }

    @Test
    void parseServerFirstRejectsLowIter() {
        String sf = "r=cnonce,s=AAAA,i=1024";
        assertThrows(RedDBException.ProtocolError.class, () -> Scram.parseServerFirst(sf, "cnonce"));
    }

    @Test
    void clientFinalNoProofUsesBiwsHeader() {
        assertEquals("c=biws,r=COMBINED", Scram.clientFinalNoProof("COMBINED"));
    }

    @Test
    void authMessageJoinsWithCommas() {
        byte[] m = Scram.authMessage("a", "b", "c");
        assertEquals("a,b,c", new String(m, StandardCharsets.UTF_8));
    }

    /**
     * Round-trip the full proof against an engine-style verifier.
     * Replicates `src/auth/scram.rs::full_round_trip`.
     */
    @Test
    void clientProofRoundTripsAgainstStoredKey() {
        byte[] salt = "reddb-rt-salt".getBytes(StandardCharsets.UTF_8);
        int iter = 4096;
        String password = "correct horse";

        // Server-side derivation (mirror of ScramVerifier::from_password).
        byte[] salted = Scram.saltedPassword(password, salt, iter);
        byte[] clientKey = Scram.hmacSha256(salted, "Client Key".getBytes(StandardCharsets.UTF_8));
        byte[] storedKey = Scram.sha256(clientKey);

        // Client-side proof.
        String clientFirstBare = "n=alice,r=cnonce";
        String serverFirst = "r=cnonceSnonce,s=" + Base64.getEncoder().encodeToString(salt) + ",i=" + iter;
        String clientFinalNoProof = "c=biws,r=cnonceSnonce";
        byte[] authMessage = Scram.authMessage(clientFirstBare, serverFirst, clientFinalNoProof);
        byte[] proof = Scram.clientProof(password, salt, iter, authMessage);

        // Server verifies — recover ClientKey via XOR with HMAC(storedKey, am), then SHA-256 == storedKey.
        byte[] sig = Scram.hmacSha256(storedKey, authMessage);
        byte[] recoveredClientKey = Scram.xor(proof, sig);
        byte[] derivedStored = Scram.sha256(recoveredClientKey);
        assertArrayEquals(storedKey, derivedStored);

        // Wrong password ⇒ verification fails.
        byte[] wrongProof = Scram.clientProof("wrong", salt, iter, authMessage);
        byte[] recoveredWrong = Scram.xor(wrongProof, sig);
        byte[] derivedWrong = Scram.sha256(recoveredWrong);
        assertFalse(java.util.Arrays.equals(storedKey, derivedWrong));
    }

    @Test
    void verifyServerSignatureRoundTrips() {
        byte[] salt = "s".getBytes(StandardCharsets.UTF_8);
        int iter = 4096;
        byte[] salted = Scram.saltedPassword("p", salt, iter);
        byte[] serverKey = Scram.hmacSha256(salted, "Server Key".getBytes(StandardCharsets.UTF_8));
        byte[] am = "auth-message".getBytes(StandardCharsets.UTF_8);
        byte[] sig = Scram.hmacSha256(serverKey, am);

        assertTrue(Scram.verifyServerSignature("p", salt, iter, am, sig));
        assertFalse(Scram.verifyServerSignature("wrong", salt, iter, am, sig));
        // Wrong-length signature → false fast path.
        assertFalse(Scram.verifyServerSignature("p", salt, iter, am, new byte[10]));
    }

    @Test
    void newClientNonceIsBase64AndUnique() {
        String a = Scram.newClientNonce();
        String b = Scram.newClientNonce();
        // base64(24 bytes) = 32 chars
        assertEquals(32, a.length());
        assertNotEquals(a, b);
        // Decodes to 24 bytes.
        assertEquals(24, Base64.getDecoder().decode(a).length);
    }

    @Test
    void constantTimeEqMatchesEqualsForEqualInputs() {
        byte[] a = HexFormat.of().parseHex("00112233445566778899aabbccddeeff");
        byte[] b = HexFormat.of().parseHex("00112233445566778899aabbccddeeff");
        byte[] c = HexFormat.of().parseHex("00112233445566778899aabbccddeefe");
        assertTrue(Scram.constantTimeEq(a, b));
        assertFalse(Scram.constantTimeEq(a, c));
        assertFalse(Scram.constantTimeEq(a, new byte[a.length - 1]));
    }
}
