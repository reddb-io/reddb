using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Reddb.Http;

/// <summary>
/// HTTP transport. A single <see cref="HttpClient"/> talks JSON to the
/// RedDB REST endpoints, carrying a bearer token in
/// <c>Authorization</c>. Login is automatic when
/// <see cref="ConnectOptions"/> / the URI carry username + password.
/// </summary>
public sealed class HttpConn : IConn
{
    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = null,
    };

    private readonly HttpClient _client;
    private readonly bool _ownsClient;
    private readonly string _baseUrl;
    private readonly TimeSpan _timeout;
    private string? _token;
    private bool _closed;

    public string? Token => _token;

    public HttpConn(HttpClient client, bool ownsClient, string baseUrl, string? token, TimeSpan timeout)
    {
        _client = client;
        _ownsClient = ownsClient;
        _baseUrl = StripTrailingSlash(baseUrl);
        _token = token;
        _timeout = timeout <= TimeSpan.Zero ? TimeSpan.FromSeconds(30) : timeout;
    }

    /// <summary>Open a fresh client, log in if credentials were supplied, return a ready connection.</summary>
    public static async ValueTask<HttpConn> ConnectAsync(RedUrl url, ConnectOptions opts, CancellationToken cancellationToken = default)
    {
        opts ??= ConnectOptions.Defaults;
        string scheme = url.Scheme == RedUrl.Kind.Https ? "https" : "http";
        string baseUrl = $"{scheme}://{url.Host}:{url.Port}";

        var handler = new SocketsHttpHandler
        {
            PooledConnectionLifetime = TimeSpan.FromMinutes(5),
            ConnectTimeout = opts.ConnectTimeout ?? opts.Timeout,
        };
        var client = new HttpClient(handler, disposeHandler: true);
        client.DefaultRequestHeaders.UserAgent.ParseAdd("reddb-dotnet/0.1");
        client.Timeout = opts.Timeout > TimeSpan.Zero ? opts.Timeout : TimeSpan.FromSeconds(30);

        string? token = opts.Token ?? url.Token;
        var conn = new HttpConn(client, ownsClient: true, baseUrl, token, opts.Timeout);

        if (token is null)
        {
            string? user = opts.Username ?? url.Username;
            string? pass = opts.Password ?? url.Password;
            if (user is not null && pass is not null)
            {
                await conn.LoginAsync(user, pass, cancellationToken).ConfigureAwait(false);
            }
        }
        return conn;
    }

    /// <summary>POST /auth/login → updates this connection's bearer token.</summary>
    public async ValueTask LoginAsync(string username, string password, CancellationToken cancellationToken = default)
    {
        var body = new JsonObject
        {
            ["username"] = username,
            ["password"] = password,
        };
        ReadOnlyMemory<byte> resp = await PostAsync("/auth/login", body, requireAuth: false, cancellationToken).ConfigureAwait(false);

        try
        {
            JsonNode? j = JsonNode.Parse(resp.Span);
            string? tok = TextField(j, "token");
            if (tok is null && j is JsonObject jo
                && jo.TryGetPropertyValue("result", out JsonNode? inner) && inner is JsonObject)
            {
                tok = TextField(inner, "token");
            }
            if (tok is null)
                throw new RedDBException.ProtocolError("auth/login response missing 'token'");
            _token = tok;
        }
        catch (JsonException ex)
        {
            throw new RedDBException.ProtocolError($"auth/login: invalid JSON: {ex.Message}", ex);
        }
    }

    public async ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, CancellationToken cancellationToken = default)
    {
        var body = new JsonObject { ["sql"] = sql };
        return await PostAsync("/query", body, requireAuth: true, cancellationToken).ConfigureAwait(false);
    }

    public async ValueTask InsertAsync(string collection, object payload, CancellationToken cancellationToken = default)
    {
        var body = new JsonObject
        {
            ["collection"] = collection,
            ["payload"] = JsonSerializer.SerializeToNode(payload, payload?.GetType() ?? typeof(object), JsonOpts),
        };
        await PostAsync("/insert", body, requireAuth: true, cancellationToken).ConfigureAwait(false);
    }

    public async ValueTask BulkInsertAsync(string collection, IReadOnlyList<object> rows, CancellationToken cancellationToken = default)
    {
        var arr = new JsonArray();
        foreach (var row in rows) arr.Add(JsonSerializer.SerializeToNode(row, row?.GetType() ?? typeof(object), JsonOpts));
        var body = new JsonObject
        {
            ["collection"] = collection,
            ["payloads"] = arr,
        };
        await PostAsync("/bulk_insert", body, requireAuth: true, cancellationToken).ConfigureAwait(false);
    }

    public async ValueTask<ReadOnlyMemory<byte>> GetAsync(string collection, string id, CancellationToken cancellationToken = default)
    {
        var body = new JsonObject { ["collection"] = collection, ["id"] = id };
        return await PostAsync("/get", body, requireAuth: true, cancellationToken).ConfigureAwait(false);
    }

    public async ValueTask DeleteAsync(string collection, string id, CancellationToken cancellationToken = default)
    {
        var body = new JsonObject { ["collection"] = collection, ["id"] = id };
        await PostAsync("/delete", body, requireAuth: true, cancellationToken).ConfigureAwait(false);
    }

    public async ValueTask PingAsync(CancellationToken cancellationToken = default)
    {
        // GET /admin/health — anything 2xx counts as healthy.
        using var req = new HttpRequestMessage(HttpMethod.Get, _baseUrl + "/admin/health");
        if (_token is not null)
            req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", _token);
        req.Headers.Accept.ParseAdd("application/json");

        using var resp = await _client.SendAsync(req, HttpCompletionOption.ResponseContentRead, cancellationToken).ConfigureAwait(false);
        if ((int)resp.StatusCode / 100 != 2)
        {
            string body = await resp.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false);
            throw new RedDBException.EngineError($"ping: HTTP {(int)resp.StatusCode}: {body}");
        }
    }

    public ValueTask DisposeAsync()
    {
        if (_closed) return ValueTask.CompletedTask;
        _closed = true;
        if (_ownsClient)
        {
            try { _client.Dispose(); } catch { }
        }
        return ValueTask.CompletedTask;
    }

    /// <summary>POST a JSON body; return the raw response bytes.</summary>
    private async ValueTask<ReadOnlyMemory<byte>> PostAsync(string path, JsonNode body, bool requireAuth, CancellationToken cancellationToken)
    {
        byte[] payload = JsonSerializer.SerializeToUtf8Bytes(body, JsonOpts);
        using var req = new HttpRequestMessage(HttpMethod.Post, _baseUrl + path)
        {
            Content = new ByteArrayContent(payload),
        };
        req.Content.Headers.ContentType = new MediaTypeHeaderValue("application/json");
        req.Headers.Accept.ParseAdd("application/json");
        if (requireAuth && _token is not null)
            req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", _token);

        try
        {
            using var resp = await _client.SendAsync(req, HttpCompletionOption.ResponseContentRead, cancellationToken).ConfigureAwait(false);
            byte[] respBody = await resp.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
            int sc = (int)resp.StatusCode;
            if (sc / 100 != 2)
            {
                string msg = Encoding.UTF8.GetString(respBody);
                if (sc == 401 || sc == 403)
                    throw new RedDBException.AuthRefused($"HTTP {sc} {path}: {msg}");
                throw new RedDBException.EngineError($"HTTP {sc} {path}: {msg}");
            }
            return respBody;
        }
        catch (HttpRequestException ex)
        {
            throw new RedDBException.ProtocolError($"{path} I/O: {ex.Message}", ex);
        }
    }

    private static string StripTrailingSlash(string s)
    {
        if (string.IsNullOrEmpty(s)) return s;
        return s.EndsWith('/') ? s.Substring(0, s.Length - 1) : s;
    }

    private static string? TextField(JsonNode? node, string name)
    {
        if (node is not JsonObject obj) return null;
        if (!obj.TryGetPropertyValue(name, out JsonNode? v)) return null;
        if (v is JsonValue val && val.TryGetValue(out string? s)) return s;
        return null;
    }
}
