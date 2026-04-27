using System;
using Reddb;
using Xunit;

namespace Reddb.Tests;

public class UrlTests
{
    [Theory]
    [InlineData("red://localhost", RedUrl.Kind.Redwire, "localhost", 5050)]
    [InlineData("red://localhost:5050", RedUrl.Kind.Redwire, "localhost", 5050)]
    [InlineData("red://example.com:9999", RedUrl.Kind.Redwire, "example.com", 9999)]
    [InlineData("red://10.0.0.1:1234", RedUrl.Kind.Redwire, "10.0.0.1", 1234)]
    [InlineData("reds://reddb.example.com", RedUrl.Kind.RedwireTls, "reddb.example.com", 5050)]
    [InlineData("reds://reddb.example.com:8443", RedUrl.Kind.RedwireTls, "reddb.example.com", 8443)]
    [InlineData("http://localhost", RedUrl.Kind.Http, "localhost", 5050)]
    [InlineData("http://localhost:8080", RedUrl.Kind.Http, "localhost", 8080)]
    [InlineData("https://reddb.example.com", RedUrl.Kind.Https, "reddb.example.com", 5050)]
    [InlineData("https://reddb.example.com:8443", RedUrl.Kind.Https, "reddb.example.com", 8443)]
    [InlineData("RED://host:1", RedUrl.Kind.Redwire, "host", 1)]
    [InlineData("red://h", RedUrl.Kind.Redwire, "h", 5050)]
    public void ParsesRemoteShapes(string uri, RedUrl.Kind expectedKind, string expectedHost, int expectedPort)
    {
        var u = RedUrl.Parse(uri);
        Assert.Equal(expectedKind, u.Scheme);
        Assert.Equal(expectedHost, u.Host);
        Assert.Equal(expectedPort, u.Port);
        Assert.Equal(uri, u.Original);
    }

    [Fact]
    public void ExtractsUserInfoFromAuthority()
    {
        var u = RedUrl.Parse("red://alice:secret@host:5050");
        Assert.Equal("alice", u.Username);
        Assert.Equal("secret", u.Password);
        Assert.Equal("host", u.Host);
        Assert.Equal(5050, u.Port);
    }

    [Fact]
    public void DecodesPercentEscapesInUserInfo()
    {
        var u = RedUrl.Parse("red://al%40ice:p%2Fass@host");
        Assert.Equal("al@ice", u.Username);
        Assert.Equal("p/ass", u.Password);
    }

    [Fact]
    public void ParsesUserOnlyWithoutPassword()
    {
        var u = RedUrl.Parse("red://alice@host");
        Assert.Equal("alice", u.Username);
        Assert.Null(u.Password);
    }

    [Fact]
    public void ParsesQueryParamsTokenAndApiKey()
    {
        var u = RedUrl.Parse("red://host?token=tok-abc&apiKey=ak-xyz");
        Assert.Equal("tok-abc", u.Token);
        Assert.Equal("ak-xyz", u.ApiKey);
        Assert.Equal("tok-abc", u.Params["token"]);
    }

    [Fact]
    public void ApiKeyAcceptsSnakeCaseFallback()
    {
        var u = RedUrl.Parse("red://host?api_key=ak-xyz");
        Assert.Equal("ak-xyz", u.ApiKey);
    }

    [Fact]
    public void DecodesPercentEscapesInQuery()
    {
        var u = RedUrl.Parse("red://host?token=a%20b%2Bc");
        Assert.Equal("a b+c", u.Token);
    }

    [Theory]
    [InlineData("red:")]
    [InlineData("red:/")]
    [InlineData("red://")]
    [InlineData("red://memory")]
    [InlineData("red://memory/")]
    [InlineData("red://:memory")]
    [InlineData("red://:memory:")]
    public void RecognisesEmbeddedInMemoryAliases(string s)
    {
        var u = RedUrl.Parse(s);
        Assert.Equal(RedUrl.Kind.EmbeddedMemory, u.Scheme);
        Assert.True(u.IsEmbedded);
    }

    [Fact]
    public void RecognisesEmbeddedFileTriple()
    {
        var u = RedUrl.Parse("red:///var/lib/reddb/data.rdb");
        Assert.Equal(RedUrl.Kind.EmbeddedFile, u.Scheme);
        Assert.Equal("/var/lib/reddb/data.rdb", u.Path);
    }

    [Theory]
    [InlineData("mongodb://localhost")]
    [InlineData("ftp://host")]
    [InlineData("grpc://host")]
    [InlineData("tcp://host")]
    public void RejectsUnsupportedSchemes(string uri)
    {
        Assert.Throws<ArgumentException>(() => RedUrl.Parse(uri));
    }

    [Fact]
    public void RejectsEmpty()
    {
        Assert.Throws<ArgumentException>(() => RedUrl.Parse(""));
        Assert.Throws<ArgumentException>(() => RedUrl.Parse(null!));
    }

    [Fact]
    public void RedSchemeWithExplicitEmptyAuthorityIsEmbeddedMemory()
    {
        Assert.Equal(RedUrl.Kind.EmbeddedMemory, RedUrl.Parse("red://").Scheme);
    }

    [Fact]
    public void IsRedwireFlagsCorrectly()
    {
        Assert.True(RedUrl.Parse("red://h").IsRedwire);
        Assert.True(RedUrl.Parse("reds://h").IsRedwire);
        Assert.False(RedUrl.Parse("http://h").IsRedwire);
    }

    [Fact]
    public void IsTlsFlagsCorrectly()
    {
        Assert.True(RedUrl.Parse("reds://h").IsTls);
        Assert.True(RedUrl.Parse("https://h").IsTls);
        Assert.False(RedUrl.Parse("red://h").IsTls);
        Assert.False(RedUrl.Parse("http://h").IsTls);
    }

    [Fact]
    public void PortDefaultsTo5050()
    {
        Assert.Equal(5050, RedUrl.Parse("red://h").Port);
        Assert.Equal(5050, RedUrl.Parse("reds://h").Port);
        Assert.Equal(5050, RedUrl.Parse("http://h").Port);
        Assert.Equal(5050, RedUrl.Parse("https://h").Port);
    }

    [Fact]
    public void ExplicitDefaultPortStillWorks()
    {
        // http://h:80 should keep 80, not coerce to 5050.
        Assert.Equal(80, RedUrl.Parse("http://h:80").Port);
        Assert.Equal(443, RedUrl.Parse("https://h:443").Port);
    }

    [Fact]
    public void PreservesOriginalUri()
    {
        const string s = "red://user:pw@host:5050?token=t";
        Assert.Equal(s, RedUrl.Parse(s).Original);
    }
}
