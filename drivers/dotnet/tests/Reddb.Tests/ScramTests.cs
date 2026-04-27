using System;
using System.Linq;
using System.Text;
using Reddb;
using Reddb.Redwire;
using Xunit;

namespace Reddb.Tests;

public class ScramTests
{
    /// <summary>RFC 4231 § 4.2 — HMAC-SHA-256 test case 1.</summary>
    [Fact]
    public void HmacSha256_Rfc4231_Case1()
    {
        var key = Enumerable.Repeat((byte)0x0b, 20).ToArray();
        var data = Encoding.ASCII.GetBytes("Hi There");
        var expected = Convert.FromHexString(
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        Assert.Equal(expected, Scram.HmacSha256(key, data));
    }

    [Fact]
    public void Pbkdf2_IsDeterministic()
    {
        var a = Scram.SaltedPassword("hunter2", Encoding.UTF8.GetBytes("salt"), 4096);
        var b = Scram.SaltedPassword("hunter2", Encoding.UTF8.GetBytes("salt"), 4096);
        Assert.Equal(a, b);
        var c = Scram.SaltedPassword("different", Encoding.UTF8.GetBytes("salt"), 4096);
        Assert.NotEqual(a, c);
        Assert.Equal(32, a.Length);
    }

    [Fact]
    public void ClientFirst_ShapeMatchesEngineParser()
    {
        string cf = Scram.ClientFirst("alice", "nonce-A");
        Assert.StartsWith("n,,", cf);
        string bare = Scram.ClientFirstBare(cf);
        Assert.Equal("n=alice,r=nonce-A", bare);
    }

    [Fact]
    public void RejectsClientFirst_MissingGs2Header()
    {
        Assert.Throws<RedDBException.ProtocolError>(
            () => Scram.ClientFirstBare("p=tls-unique,n=alice,r=x"));
    }

    [Fact]
    public void RejectsUsername_WithReservedCharacters()
    {
        Assert.Throws<ArgumentException>(() => Scram.ClientFirst("a,b", "n"));
        Assert.Throws<ArgumentException>(() => Scram.ClientFirst("a=b", "n"));
    }

    [Fact]
    public void ParseServerFirst_ExtractsFields()
    {
        var salt = new byte[] { 1, 2, 3, 4, 5 };
        string sf = "r=cnonceSnonce,s=" + Convert.ToBase64String(salt) + ",i=4096";
        var parsed = Scram.ParseServerFirst(sf, "cnonce");
        Assert.Equal("cnonceSnonce", parsed.CombinedNonce);
        Assert.Equal(salt, parsed.Salt);
        Assert.Equal(4096, parsed.Iter);
    }

    [Fact]
    public void ParseServerFirst_RejectsBadNoncePrefix()
    {
        string sf = "r=other,s=AAAA,i=4096";
        Assert.Throws<RedDBException.ProtocolError>(
            () => Scram.ParseServerFirst(sf, "cnonce"));
    }

    [Fact]
    public void ParseServerFirst_RejectsLowIter()
    {
        string sf = "r=cnonce,s=AAAA,i=1024";
        Assert.Throws<RedDBException.ProtocolError>(
            () => Scram.ParseServerFirst(sf, "cnonce"));
    }

    [Fact]
    public void ClientFinalNoProof_UsesBiwsHeader()
    {
        Assert.Equal("c=biws,r=COMBINED", Scram.ClientFinalNoProof("COMBINED"));
    }

    [Fact]
    public void AuthMessage_JoinsWithCommas()
    {
        var m = Scram.AuthMessage("a", "b", "c");
        Assert.Equal("a,b,c", Encoding.UTF8.GetString(m));
    }

    /// <summary>Round-trip the full proof against an engine-style verifier.</summary>
    [Fact]
    public void ClientProof_RoundTripsAgainstStoredKey()
    {
        var salt = Encoding.UTF8.GetBytes("reddb-rt-salt");
        const int iter = 4096;
        const string password = "correct horse";

        // Server-side derivation (mirror of ScramVerifier::from_password).
        var salted = Scram.SaltedPassword(password, salt, iter);
        var clientKey = Scram.HmacSha256(salted, Encoding.UTF8.GetBytes("Client Key"));
        var storedKey = Scram.Sha256Hash(clientKey);

        // Client-side proof.
        const string clientFirstBare = "n=alice,r=cnonce";
        string serverFirst = "r=cnonceSnonce,s=" + Convert.ToBase64String(salt) + ",i=" + iter;
        const string clientFinalNoProof = "c=biws,r=cnonceSnonce";
        var authMessage = Scram.AuthMessage(clientFirstBare, serverFirst, clientFinalNoProof);
        var proof = Scram.ClientProof(password, salt, iter, authMessage);

        // Server verifies — recover ClientKey via XOR with HMAC(storedKey, am), then SHA-256 == storedKey.
        var sig = Scram.HmacSha256(storedKey, authMessage);
        var recoveredClientKey = Scram.Xor(proof, sig);
        var derivedStored = Scram.Sha256Hash(recoveredClientKey);
        Assert.Equal(storedKey, derivedStored);

        // Wrong password ⇒ verification fails.
        var wrongProof = Scram.ClientProof("wrong", salt, iter, authMessage);
        var recoveredWrong = Scram.Xor(wrongProof, sig);
        var derivedWrong = Scram.Sha256Hash(recoveredWrong);
        Assert.NotEqual(storedKey, derivedWrong);
    }

    [Fact]
    public void VerifyServerSignature_RoundTrips()
    {
        var salt = Encoding.UTF8.GetBytes("s");
        const int iter = 4096;
        var salted = Scram.SaltedPassword("p", salt, iter);
        var serverKey = Scram.HmacSha256(salted, Encoding.UTF8.GetBytes("Server Key"));
        var am = Encoding.UTF8.GetBytes("auth-message");
        var sig = Scram.HmacSha256(serverKey, am);

        Assert.True(Scram.VerifyServerSignature("p", salt, iter, am, sig));
        Assert.False(Scram.VerifyServerSignature("wrong", salt, iter, am, sig));
        // Wrong-length signature → false fast path.
        Assert.False(Scram.VerifyServerSignature("p", salt, iter, am, new byte[10]));
    }

    [Fact]
    public void NewClientNonce_IsBase64AndUnique()
    {
        string a = Scram.NewClientNonce();
        string b = Scram.NewClientNonce();
        // base64(24 bytes) = 32 chars
        Assert.Equal(32, a.Length);
        Assert.NotEqual(a, b);
        Assert.Equal(24, Convert.FromBase64String(a).Length);
    }

    [Fact]
    public void ConstantTimeEq_MatchesEqualsForEqualInputs()
    {
        var a = Convert.FromHexString("00112233445566778899aabbccddeeff");
        var b = Convert.FromHexString("00112233445566778899aabbccddeeff");
        var c = Convert.FromHexString("00112233445566778899aabbccddeefe");
        Assert.True(Scram.ConstantTimeEq(a, b));
        Assert.False(Scram.ConstantTimeEq(a, c));
        Assert.False(Scram.ConstantTimeEq(a, new byte[a.Length - 1]));
    }
}
