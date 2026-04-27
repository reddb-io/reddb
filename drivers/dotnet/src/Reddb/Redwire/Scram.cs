using System;
using System.Security.Cryptography;
using System.Text;

namespace Reddb.Redwire;

/// <summary>
/// SCRAM-SHA-256 client primitives (RFC 5802 + RFC 7677).
///
/// Pure functions — no I/O. The state machine that calls these
/// lives in <see cref="RedWireConn"/>; testing is easier when the
/// crypto stays separable.
/// </summary>
public static class Scram
{
    /// <summary>Default iteration count when the server doesn't override.</summary>
    public const int DefaultIter = 16_384;

    /// <summary>Hard floor — verifiers below this are unsafe to use.</summary>
    public const int MinIter = 4096;

    /// <summary>Generate 24 random bytes and base64-encode them. Matches the engine's <c>new_server_nonce</c>.</summary>
    public static string NewClientNonce()
    {
        Span<byte> buf = stackalloc byte[24];
        RandomNumberGenerator.Fill(buf);
        return Convert.ToBase64String(buf);
    }

    /// <summary>Build the SCRAM <c>client-first-message</c> (no channel binding, no authzid).</summary>
    public static string ClientFirst(string username, string clientNonce)
    {
        // GS2 header `n,,` = no channel binding, no authzid.
        return "n,,n=" + SaslPrep(username) + ",r=" + clientNonce;
    }

    /// <summary>Strip the <c>n,,</c> GS2 header — server-first verification uses the bare form.</summary>
    public static string ClientFirstBare(string clientFirst)
    {
        if (!clientFirst.StartsWith("n,,", StringComparison.Ordinal))
        {
            throw new RedDBException.ProtocolError(
                "client-first must start with 'n,,' (no channel binding)");
        }
        return clientFirst.Substring(3);
    }

    /// <summary>Parsed shape of the server-first-message.</summary>
    public sealed class ServerFirst
    {
        public string CombinedNonce { get; }
        public byte[] Salt { get; }
        public int Iter { get; }
        public string Raw { get; }

        public ServerFirst(string combinedNonce, byte[] salt, int iter, string raw)
        {
            CombinedNonce = combinedNonce;
            Salt = salt;
            Iter = iter;
            Raw = raw;
        }
    }

    /// <summary>
    /// Parse <c>r=&lt;combined&gt;,s=&lt;b64salt&gt;,i=&lt;iter&gt;</c>. Verifies the
    /// combined nonce starts with the client's nonce — replay defence.
    /// </summary>
    public static ServerFirst ParseServerFirst(string serverFirst, string clientNonce)
    {
        string? combined = null;
        string? saltB64 = null;
        int? iter = null;
        foreach (string part in serverFirst.Split(','))
        {
            if (part.StartsWith("r=", StringComparison.Ordinal)) combined = part.Substring(2);
            else if (part.StartsWith("s=", StringComparison.Ordinal)) saltB64 = part.Substring(2);
            else if (part.StartsWith("i=", StringComparison.Ordinal))
            {
                if (!int.TryParse(part.AsSpan(2), out int parsed))
                    throw new RedDBException.ProtocolError($"server-first iter is not an int: {part}");
                iter = parsed;
            }
        }
        if (combined is null) throw new RedDBException.ProtocolError("server-first missing r=");
        if (saltB64 is null) throw new RedDBException.ProtocolError("server-first missing s=");
        if (iter is null) throw new RedDBException.ProtocolError("server-first missing i=");
        if (!combined.StartsWith(clientNonce, StringComparison.Ordinal))
        {
            throw new RedDBException.ProtocolError(
                "server-first nonce does not start with our client nonce — replay protection");
        }
        if (iter < MinIter)
        {
            throw new RedDBException.ProtocolError(
                $"server-first iter {iter} < MIN_ITER {MinIter}");
        }
        byte[] salt;
        try
        {
            salt = Convert.FromBase64String(saltB64);
        }
        catch (FormatException)
        {
            throw new RedDBException.ProtocolError("server-first salt is not base64");
        }
        return new ServerFirst(combined, salt, iter.Value, serverFirst);
    }

    /// <summary>Build the SCRAM <c>client-final-message-without-proof</c> (constant <c>c=biws</c>).</summary>
    public static string ClientFinalNoProof(string combinedNonce)
    {
        // c=biws is base64("n,,") — the canonical no-channel-binding header.
        return "c=biws,r=" + combinedNonce;
    }

    /// <summary>Build the canonical <c>AuthMessage</c> per RFC 5802 § 3.</summary>
    public static byte[] AuthMessage(string clientFirstBare, string serverFirst, string clientFinalNoProof)
    {
        return Encoding.UTF8.GetBytes(clientFirstBare + "," + serverFirst + "," + clientFinalNoProof);
    }

    /// <summary>PBKDF2-HMAC-SHA256 → 32 bytes.</summary>
    public static byte[] SaltedPassword(string password, byte[] salt, int iter)
    {
        using var pbkdf2 = new Rfc2898DeriveBytes(password, salt, iter, HashAlgorithmName.SHA256);
        return pbkdf2.GetBytes(32);
    }

    /// <summary>HMAC-SHA-256(key, data).</summary>
    public static byte[] HmacSha256(byte[] key, byte[] data)
    {
        using var hmac = new HMACSHA256(key);
        return hmac.ComputeHash(data);
    }

    /// <summary>SHA-256(data).</summary>
    public static byte[] Sha256Hash(byte[] data) => SHA256.HashData(data);

    /// <summary>Bytewise XOR of two equal-length arrays.</summary>
    public static byte[] Xor(byte[] a, byte[] b)
    {
        if (a.Length != b.Length)
            throw new ArgumentException($"xor length mismatch: {a.Length} vs {b.Length}");
        var output = new byte[a.Length];
        for (int i = 0; i < a.Length; i++) output[i] = (byte)(a[i] ^ b[i]);
        return output;
    }

    /// <summary>
    /// Compute the SCRAM client proof. Mirrors the formula in
    /// <c>src/auth/scram.rs</c>: <c>ClientKey XOR HMAC(StoredKey, AuthMessage)</c>.
    /// </summary>
    public static byte[] ClientProof(string password, byte[] salt, int iter, byte[] authMessage)
    {
        byte[] salted = SaltedPassword(password, salt, iter);
        byte[] clientKey = HmacSha256(salted, Encoding.UTF8.GetBytes("Client Key"));
        byte[] storedKey = Sha256Hash(clientKey);
        byte[] sig = HmacSha256(storedKey, authMessage);
        return Xor(clientKey, sig);
    }

    /// <summary>
    /// Verify the server signature. Returns true when the server
    /// also knew the verifier (defence against an active MITM).
    /// Constant-time comparison.
    /// </summary>
    public static bool VerifyServerSignature(string password, byte[] salt, int iter,
        byte[] authMessage, byte[]? presentedSignature)
    {
        if (presentedSignature is null || presentedSignature.Length != 32) return false;
        byte[] salted = SaltedPassword(password, salt, iter);
        byte[] serverKey = HmacSha256(salted, Encoding.UTF8.GetBytes("Server Key"));
        byte[] expected = HmacSha256(serverKey, authMessage);
        return ConstantTimeEq(expected, presentedSignature);
    }

    /// <summary>Constant-time equality.</summary>
    public static bool ConstantTimeEq(byte[]? a, byte[]? b)
    {
        if (a is null || b is null || a.Length != b.Length) return false;
        int diff = 0;
        for (int i = 0; i < a.Length; i++) diff |= a[i] ^ b[i];
        return diff == 0;
    }

    /// <summary>Build the SCRAM <c>client-final-message</c> (c=,r=,p=).</summary>
    public static string ClientFinal(string combinedNonce, byte[] proof)
    {
        return "c=biws,r=" + combinedNonce + ",p=" + Convert.ToBase64String(proof);
    }

    /// <summary>
    /// SASLprep is full Stringprep + a few extra rules. RedDB's
    /// server treats usernames as opaque byte strings; the JS / Rust
    /// drivers don't apply SASLprep either. We just forbid <c>,</c> and
    /// <c>=</c> because they break the wire format.
    /// </summary>
    internal static string SaslPrep(string input)
    {
        if (input.IndexOf(',') >= 0)
            throw new ArgumentException($"SCRAM username contains illegal ',': {input}");
        if (input.IndexOf('=') >= 0)
            throw new ArgumentException($"SCRAM username contains illegal '=': {input}");
        return input;
    }
}
