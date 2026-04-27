package dev.reddb.redwire;

import dev.reddb.RedDBException;

import javax.crypto.Mac;
import javax.crypto.SecretKeyFactory;
import javax.crypto.spec.PBEKeySpec;
import javax.crypto.spec.SecretKeySpec;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.security.SecureRandom;
import java.security.spec.InvalidKeySpecException;
import java.util.Base64;

/**
 * SCRAM-SHA-256 client primitives (RFC 5802 + RFC 7677).
 *
 * Pure functions — no I/O, no socket. The state machine that calls
 * these lives in {@link RedWireConn}; testing is easier when the
 * crypto stays separable.
 */
public final class Scram {
    /** Default iteration count when the server doesn't override. */
    public static final int DEFAULT_ITER = 16_384;
    /** Hard floor — verifiers below this are unsafe to use. */
    public static final int MIN_ITER = 4096;

    private Scram() {}

    /**
     * Generate 24 random bytes and base64-encode them. Matches the
     * server-side `new_server_nonce` shape (standard alphabet, no
     * URL-safe variant) so the combined nonce strings line up.
     */
    public static String newClientNonce() {
        byte[] raw = new byte[24];
        new SecureRandom().nextBytes(raw);
        return Base64.getEncoder().encodeToString(raw);
    }

    /** Build the SCRAM `client-first-message` (no channel binding, no authzid). */
    public static String clientFirst(String username, String clientNonce) {
        // GS2 header `n,,` = no channel binding, no authzid.
        return "n,,n=" + saslPrep(username) + ",r=" + clientNonce;
    }

    /** Strip the `n,,` GS2 header — server-first verification uses the bare form. */
    public static String clientFirstBare(String clientFirst) {
        if (!clientFirst.startsWith("n,,")) {
            throw new RedDBException.ProtocolError(
                "client-first must start with 'n,,' (no channel binding)");
        }
        return clientFirst.substring(3);
    }

    /** Parsed shape of the server-first-message. */
    public static final class ServerFirst {
        public final String combinedNonce;
        public final byte[] salt;
        public final int iter;
        public final String raw;
        ServerFirst(String combinedNonce, byte[] salt, int iter, String raw) {
            this.combinedNonce = combinedNonce;
            this.salt = salt;
            this.iter = iter;
            this.raw = raw;
        }
    }

    /**
     * Parse `r=<combined>,s=<b64salt>,i=<iter>`. Verifies the
     * combined nonce starts with the client's nonce — replay defence.
     */
    public static ServerFirst parseServerFirst(String serverFirst, String clientNonce) {
        String combined = null;
        String saltB64 = null;
        Integer iter = null;
        for (String part : serverFirst.split(",")) {
            if (part.startsWith("r=")) combined = part.substring(2);
            else if (part.startsWith("s=")) saltB64 = part.substring(2);
            else if (part.startsWith("i=")) {
                try {
                    iter = Integer.parseInt(part.substring(2));
                } catch (NumberFormatException e) {
                    throw new RedDBException.ProtocolError("server-first iter is not an int: " + part);
                }
            }
        }
        if (combined == null) throw new RedDBException.ProtocolError("server-first missing r=");
        if (saltB64 == null) throw new RedDBException.ProtocolError("server-first missing s=");
        if (iter == null) throw new RedDBException.ProtocolError("server-first missing i=");
        if (!combined.startsWith(clientNonce)) {
            throw new RedDBException.ProtocolError(
                "server-first nonce does not start with our client nonce — replay protection");
        }
        if (iter < MIN_ITER) {
            throw new RedDBException.ProtocolError(
                "server-first iter " + iter + " < MIN_ITER " + MIN_ITER);
        }
        byte[] salt;
        try {
            salt = Base64.getDecoder().decode(saltB64);
        } catch (IllegalArgumentException e) {
            throw new RedDBException.ProtocolError("server-first salt is not base64");
        }
        return new ServerFirst(combined, salt, iter, serverFirst);
    }

    /** Build the SCRAM `client-final-message-without-proof` (constant `c=biws`). */
    public static String clientFinalNoProof(String combinedNonce) {
        // c=biws is base64("n,,") — the canonical no-channel-binding header.
        return "c=biws,r=" + combinedNonce;
    }

    /** Build the canonical `AuthMessage` per RFC 5802 § 3. */
    public static byte[] authMessage(String clientFirstBare, String serverFirst, String clientFinalNoProof) {
        String joined = clientFirstBare + "," + serverFirst + "," + clientFinalNoProof;
        return joined.getBytes(StandardCharsets.UTF_8);
    }

    /** PBKDF2-HMAC-SHA256 → 32 bytes. */
    public static byte[] saltedPassword(String password, byte[] salt, int iter) {
        try {
            SecretKeyFactory f = SecretKeyFactory.getInstance("PBKDF2WithHmacSHA256");
            PBEKeySpec spec = new PBEKeySpec(password.toCharArray(), salt, iter, 256);
            return f.generateSecret(spec).getEncoded();
        } catch (NoSuchAlgorithmException | InvalidKeySpecException e) {
            throw new RedDBException.ProtocolError("PBKDF2WithHmacSHA256 unavailable: " + e.getMessage(), e);
        }
    }

    /** HMAC-SHA-256(key, data). */
    public static byte[] hmacSha256(byte[] key, byte[] data) {
        try {
            Mac mac = Mac.getInstance("HmacSHA256");
            mac.init(new SecretKeySpec(key, "HmacSHA256"));
            return mac.doFinal(data);
        } catch (Exception e) {
            throw new RedDBException.ProtocolError("HmacSHA256 unavailable: " + e.getMessage(), e);
        }
    }

    /** SHA-256(data). */
    public static byte[] sha256(byte[] data) {
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            return md.digest(data);
        } catch (NoSuchAlgorithmException e) {
            throw new RedDBException.ProtocolError("SHA-256 unavailable: " + e.getMessage(), e);
        }
    }

    /** Bytewise XOR of two equal-length arrays. */
    public static byte[] xor(byte[] a, byte[] b) {
        if (a.length != b.length) {
            throw new IllegalArgumentException("xor length mismatch: " + a.length + " vs " + b.length);
        }
        byte[] out = new byte[a.length];
        for (int i = 0; i < a.length; i++) out[i] = (byte) (a[i] ^ b[i]);
        return out;
    }

    /**
     * Compute the SCRAM client proof. Mirrors the formula in
     * `src/auth/scram.rs`: {@code ClientKey XOR HMAC(StoredKey, AuthMessage)}.
     */
    public static byte[] clientProof(String password, byte[] salt, int iter, byte[] authMessage) {
        byte[] salted = saltedPassword(password, salt, iter);
        byte[] clientKey = hmacSha256(salted, "Client Key".getBytes(StandardCharsets.UTF_8));
        byte[] storedKey = sha256(clientKey);
        byte[] sig = hmacSha256(storedKey, authMessage);
        return xor(clientKey, sig);
    }

    /**
     * Verify the server signature. Returns true when the server
     * also knew the verifier (defence against an active MITM).
     * Constant-time comparison.
     */
    public static boolean verifyServerSignature(String password, byte[] salt, int iter,
                                                byte[] authMessage, byte[] presentedSignature) {
        if (presentedSignature == null || presentedSignature.length != 32) return false;
        byte[] salted = saltedPassword(password, salt, iter);
        byte[] serverKey = hmacSha256(salted, "Server Key".getBytes(StandardCharsets.UTF_8));
        byte[] expected = hmacSha256(serverKey, authMessage);
        return constantTimeEq(expected, presentedSignature);
    }

    /** Constant-time equality. */
    public static boolean constantTimeEq(byte[] a, byte[] b) {
        if (a == null || b == null || a.length != b.length) return false;
        int diff = 0;
        for (int i = 0; i < a.length; i++) diff |= (a[i] ^ b[i]) & 0xff;
        return diff == 0;
    }

    /** Build the SCRAM `client-final-message` (c=,r=,p=). */
    public static String clientFinal(String combinedNonce, byte[] proof) {
        return "c=biws,r=" + combinedNonce + ",p=" + Base64.getEncoder().encodeToString(proof);
    }

    /**
     * SASLprep is full Stringprep + a few extra rules. RedDB's
     * server treats usernames as opaque byte strings; the JS / Rust
     * drivers don't apply SASLprep either. We just forbid `,` and
     * `=` because they break the wire format.
     */
    static String saslPrep(String input) {
        if (input.indexOf(',') >= 0) {
            throw new IllegalArgumentException("SCRAM username contains illegal ',': " + input);
        }
        if (input.indexOf('=') >= 0) {
            throw new IllegalArgumentException("SCRAM username contains illegal '=': " + input);
        }
        return input;
    }
}
