using System;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;
using Reddb.Http;
using Xunit;

namespace Reddb.Tests;

public class HttpConnTests
{
    [Fact]
    public async Task QueryAsync_WithParams_PostsCanonicalQueryAndTypedParams()
    {
        HttpRequestMessage? captured = null;
        string? capturedBody = null;
        var handler = new CaptureHandler(async request =>
        {
            captured = request;
            capturedBody = await request.Content!.ReadAsStringAsync();
            return new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new ByteArrayContent(Encoding.UTF8.GetBytes("{\"ok\":true}")),
            };
        });
        using var client = new HttpClient(handler);
        var conn = new HttpConn(client, ownsClient: false, "http://example.test", "tok", TimeSpan.FromSeconds(5));

        await conn.QueryAsync(
            "SELECT * FROM docs WHERE id = $1 AND embedding <-> $2",
            42,
            new float[] { 1.0f, 2.0f });

        Assert.Equal(HttpMethod.Post, captured!.Method);
        Assert.Equal("http://example.test/query", captured.RequestUri!.ToString());
        Assert.Equal("Bearer", captured.Headers.Authorization!.Scheme);
        Assert.Equal("tok", captured.Headers.Authorization.Parameter);

        JsonObject body = JsonNode.Parse(capturedBody!)!.AsObject();
        Assert.Equal("SELECT * FROM docs WHERE id = $1 AND embedding <-> $2", (string)body["query"]!);
        Assert.False(body.ContainsKey("sql"));
        Assert.Equal(42, (int)body["params"]![0]!);
        Assert.Equal(1.0d, (double)body["params"]![1]![0]!);
        Assert.Equal(2.0d, (double)body["params"]![1]![1]!);
    }

    [Fact]
    public async Task QueryAsync_WithoutParams_OmitsParams()
    {
        string? capturedBody = null;
        var handler = new CaptureHandler(async request =>
        {
            capturedBody = await request.Content!.ReadAsStringAsync();
            return new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new ByteArrayContent(Encoding.UTF8.GetBytes("{}")),
            };
        });
        using var client = new HttpClient(handler);
        var conn = new HttpConn(client, ownsClient: false, "http://example.test", null, TimeSpan.FromSeconds(5));

        await conn.QueryAsync("SELECT 1");

        JsonObject body = JsonNode.Parse(capturedBody!)!.AsObject();
        Assert.Equal("SELECT 1", (string)body["query"]!);
        Assert.False(body.ContainsKey("params"));
    }

    private sealed class CaptureHandler : HttpMessageHandler
    {
        private readonly Func<HttpRequestMessage, Task<HttpResponseMessage>> _handler;

        public CaptureHandler(Func<HttpRequestMessage, Task<HttpResponseMessage>> handler)
        {
            _handler = handler;
        }

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
            => _handler(request);
    }
}
