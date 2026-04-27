using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.IO;
using System.Net.Security;
using System.Net.Sockets;
using System.Security.Authentication;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Reddb.Redwire;

/// <summary>
/// RedWire client over a single socket.
///
/// One <see cref="RedWireConn"/> owns one stream, one
/// <see cref="SemaphoreSlim"/> mutex, and a monotonic correlation id.
/// All public ops are async and cancellable; concurrent calls
/// serialise through the mutex.
/// </summary>
public sealed class RedWireConn : IConn
{
    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = null,
    };

    private readonly Stream _stream;
    private readonly IDisposable? _owner;
    private readonly SemaphoreSlim _lock = new(1, 1);
    private long _nextCorrelation = 1;
    private readonly TimeSpan _timeout;
    private bool _closed;

    public string SessionId { get; }

    /// <summary>
    /// Raw constructor — caller already has a stream. Used by tests
    /// that drive the handshake on top of in-memory stream pairs.
    /// Production code should call <see cref="ConnectAsync"/>.
    /// </summary>
    public RedWireConn(Stream stream, IDisposable? owner, string sessionId, TimeSpan timeout)
    {
        _stream = stream;
        _owner = owner;
        SessionId = sessionId;
        _timeout = timeout <= TimeSpan.Zero ? TimeSpan.FromSeconds(30) : timeout;
    }

    /// <summary>Open a TCP / TLS connection and run the v2 handshake.</summary>
    public static async ValueTask<RedWireConn> ConnectAsync(RedUrl url, ConnectOptions opts, CancellationToken cancellationToken = default)
    {
        if (!url.IsRedwire)
            throw new ArgumentException(
                $"RedWireConn.ConnectAsync requires red:// or reds://, got {url.Scheme}", nameof(url));

        opts ??= ConnectOptions.Defaults;
        TimeSpan connectTimeout = opts.ConnectTimeout ?? opts.Timeout;
        TimeSpan opTimeout = opts.Timeout;

        var tcp = new TcpClient();
        Stream stream;
        IDisposable owner;
        try
        {
            using (var connectCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken))
            {
                if (connectTimeout > TimeSpan.Zero) connectCts.CancelAfter(connectTimeout);
                await tcp.ConnectAsync(url.Host!, url.Port, connectCts.Token).ConfigureAwait(false);
            }
            tcp.NoDelay = true;

            if (url.IsTls)
            {
                var ssl = new SslStream(tcp.GetStream(), leaveInnerStreamOpen: false);
                var authOpts = new SslClientAuthenticationOptions
                {
                    TargetHost = url.Host!,
                    EnabledSslProtocols = SslProtocols.Tls12 | SslProtocols.Tls13,
                    ApplicationProtocols = new List<SslApplicationProtocol>
                    {
                        new SslApplicationProtocol("redwire/1"),
                    },
                };
                await ssl.AuthenticateAsClientAsync(authOpts, cancellationToken).ConfigureAwait(false);
                stream = ssl;
                owner = new StreamAndSocket(ssl, tcp);
            }
            else
            {
                stream = tcp.GetStream();
                owner = tcp;
            }

            string? token = opts.Token ?? url.Token;
            string? username = opts.Username ?? url.Username;
            string? password = opts.Password ?? url.Password;
            string clientName = opts.ClientName ?? "reddb-dotnet/0.1";

            HandshakeResult handshake = await PerformHandshakeAsync(
                stream, username, password, token, clientName, cancellationToken).ConfigureAwait(false);

            return new RedWireConn(stream, owner, handshake.SessionId, opTimeout);
        }
        catch
        {
            try { tcp.Dispose(); } catch { /* swallow */ }
            throw;
        }
    }

    private sealed class StreamAndSocket : IDisposable
    {
        private readonly Stream _s;
        private readonly TcpClient _t;
        public StreamAndSocket(Stream s, TcpClient t) { _s = s; _t = t; }
        public void Dispose()
        {
            try { _s.Dispose(); } catch { }
            try { _t.Dispose(); } catch { }
        }
    }

    /// <summary>
    /// Drive the handshake on a raw stream. Static + public so tests
    /// can run it on top of an in-memory stream pair.
    /// </summary>
    public static async ValueTask<HandshakeResult> PerformHandshakeAsync(
        Stream stream,
        string? username,
        string? password,
        string? token,
        string? clientName,
        CancellationToken cancellationToken = default)
    {
        // 1. Magic preamble + minor version. Two bytes — written before any frame.
        await stream.WriteAsync(new byte[] { Frame.Magic, Frame.SupportedVersion }, cancellationToken).ConfigureAwait(false);
        await stream.FlushAsync(cancellationToken).ConfigureAwait(false);

        // 2. Hello — advertise every method this driver supports.
        List<string> methods;
        if (token is not null) methods = new List<string> { "bearer" };
        else if (username is not null && password is not null)
            methods = new List<string> { "scram-sha-256", "bearer" };
        else
            methods = new List<string> { "anonymous", "bearer" };

        var hello = new JsonObject
        {
            ["versions"] = new JsonArray(JsonValue.Create<int>(Frame.SupportedVersion & 0xff)),
            ["auth_methods"] = new JsonArray(),
            ["features"] = 0,
        };
        var arr = (JsonArray)hello["auth_methods"]!;
        foreach (var m in methods) arr.Add(m);
        if (clientName is not null) hello["client_name"] = clientName;

        byte[] helloBytes = JsonSerializer.SerializeToUtf8Bytes(hello, JsonOpts);
        await WriteFrameAsync(stream, new Frame(Frame.Kind.Hello, 1UL, helloBytes), cancellationToken).ConfigureAwait(false);

        // 3. HelloAck or AuthFail.
        Frame ack = await ReadFrameAsync(stream, cancellationToken).ConfigureAwait(false);
        if (ack.MessageKind == Frame.Kind.AuthFail)
        {
            throw new RedDBException.AuthRefused(Reason(ack.Payload, "AuthFail at HelloAck"));
        }
        if (ack.MessageKind != Frame.Kind.HelloAck)
        {
            throw new RedDBException.ProtocolError(
                $"expected HelloAck, got {Frame.Kind.Name(ack.MessageKind)}");
        }
        JsonNode? ackJson = ParseJson(ack.Payload, "HelloAck");
        string? chosen = TextField(ackJson, "auth");
        if (chosen is null)
        {
            throw new RedDBException.ProtocolError("HelloAck missing 'auth' field");
        }

        // 4. Auth dispatch.
        switch (chosen)
        {
            case "anonymous":
                await WriteFrameAsync(stream, new Frame(Frame.Kind.AuthResponse, 2UL, Array.Empty<byte>()), cancellationToken).ConfigureAwait(false);
                return await FinishOneRttAsync(stream, cancellationToken).ConfigureAwait(false);
            case "bearer":
            {
                if (token is null)
                {
                    throw new RedDBException.AuthRefused(
                        "server demanded bearer but no token was supplied");
                }
                var body = new JsonObject { ["token"] = token };
                await WriteFrameAsync(stream, new Frame(Frame.Kind.AuthResponse, 2UL, JsonSerializer.SerializeToUtf8Bytes(body, JsonOpts)), cancellationToken).ConfigureAwait(false);
                return await FinishOneRttAsync(stream, cancellationToken).ConfigureAwait(false);
            }
            case "scram-sha-256":
            {
                if (username is null || password is null)
                {
                    throw new RedDBException.AuthRefused(
                        "server picked scram-sha-256 but no username/password configured");
                }
                return await PerformScramAsync(stream, username, password, cancellationToken).ConfigureAwait(false);
            }
            case "oauth-jwt":
            {
                if (token is null)
                {
                    throw new RedDBException.AuthRefused(
                        "server picked oauth-jwt but no JWT token configured");
                }
                var body = new JsonObject { ["jwt"] = token };
                await WriteFrameAsync(stream, new Frame(Frame.Kind.AuthResponse, 2UL, JsonSerializer.SerializeToUtf8Bytes(body, JsonOpts)), cancellationToken).ConfigureAwait(false);
                return await FinishOneRttAsync(stream, cancellationToken).ConfigureAwait(false);
            }
            default:
                throw new RedDBException.ProtocolError(
                    $"server picked unsupported auth method: {chosen}");
        }
    }

    private static async ValueTask<HandshakeResult> FinishOneRttAsync(Stream stream, CancellationToken ct)
    {
        Frame f = await ReadFrameAsync(stream, ct).ConfigureAwait(false);
        if (f.MessageKind == Frame.Kind.AuthFail)
            throw new RedDBException.AuthRefused(Reason(f.Payload, "auth refused"));
        if (f.MessageKind != Frame.Kind.AuthOk)
            throw new RedDBException.ProtocolError(
                $"expected AuthOk, got {Frame.Kind.Name(f.MessageKind)}");
        JsonNode? j = ParseJson(f.Payload, "AuthOk");
        string sid = TextField(j, "session_id") ?? string.Empty;
        return new HandshakeResult(sid);
    }

    private static async ValueTask<HandshakeResult> PerformScramAsync(
        Stream stream, string username, string password, CancellationToken ct)
    {
        // RFC 5802 § 3 — three round trips after the version byte.
        string clientNonce = Scram.NewClientNonce();
        string clientFirst = Scram.ClientFirst(username, clientNonce);
        string clientFirstBare = Scram.ClientFirstBare(clientFirst);

        var cf = new JsonObject { ["client_first"] = clientFirst };
        await WriteFrameAsync(stream, new Frame(Frame.Kind.AuthResponse, 2UL, JsonSerializer.SerializeToUtf8Bytes(cf, JsonOpts)), ct).ConfigureAwait(false);

        Frame chall = await ReadFrameAsync(stream, ct).ConfigureAwait(false);
        if (chall.MessageKind == Frame.Kind.AuthFail)
            throw new RedDBException.AuthRefused(Reason(chall.Payload, "scram challenge refused"));
        if (chall.MessageKind != Frame.Kind.AuthRequest)
            throw new RedDBException.ProtocolError(
                $"scram: expected AuthRequest, got {Frame.Kind.Name(chall.MessageKind)}");

        string serverFirstStr = ScramServerFirst(chall.Payload);
        Scram.ServerFirst sf = Scram.ParseServerFirst(serverFirstStr, clientNonce);

        string clientFinalNoProof = Scram.ClientFinalNoProof(sf.CombinedNonce);
        byte[] authMessage = Scram.AuthMessage(clientFirstBare, sf.Raw, clientFinalNoProof);
        byte[] proof = Scram.ClientProof(password, sf.Salt, sf.Iter, authMessage);
        string clientFinal = Scram.ClientFinal(sf.CombinedNonce, proof);

        var cfin = new JsonObject { ["client_final"] = clientFinal };
        await WriteFrameAsync(stream, new Frame(Frame.Kind.AuthResponse, 3UL, JsonSerializer.SerializeToUtf8Bytes(cfin, JsonOpts)), ct).ConfigureAwait(false);

        Frame ok = await ReadFrameAsync(stream, ct).ConfigureAwait(false);
        if (ok.MessageKind == Frame.Kind.AuthFail)
            throw new RedDBException.AuthRefused(Reason(ok.Payload, "scram refused"));
        if (ok.MessageKind != Frame.Kind.AuthOk)
            throw new RedDBException.ProtocolError(
                $"scram: expected AuthOk, got {Frame.Kind.Name(ok.MessageKind)}");

        JsonNode? j = ParseJson(ok.Payload, "AuthOk");
        string sid = TextField(j, "session_id") ?? string.Empty;
        byte[]? sig = ParseServerSignature(j);
        if (sig is not null && !Scram.VerifyServerSignature(password, sf.Salt, sf.Iter, authMessage, sig))
        {
            throw new RedDBException.AuthRefused(
                "scram: server signature did not verify — possible MITM");
        }
        return new HandshakeResult(sid);
    }

    private static string ScramServerFirst(byte[] payload)
    {
        // Engine emits raw `r=...,s=...,i=...`; some drivers wrap it in JSON.
        if (payload.Length > 0 && payload[0] == (byte)'{')
        {
            JsonNode? j = ParseJson(payload, "AuthRequest");
            string? s = TextField(j, "server_first")
                ?? throw new RedDBException.ProtocolError("AuthRequest JSON missing 'server_first'");
            return s;
        }
        return Encoding.UTF8.GetString(payload);
    }

    private static byte[]? ParseServerSignature(JsonNode? authOk)
    {
        if (authOk is not JsonObject obj) return null;
        if (obj.TryGetPropertyValue("v", out JsonNode? v) && v is JsonValue vVal && vVal.TryGetValue(out string? s) && s is not null)
        {
            try { return Convert.FromBase64String(s); }
            catch (FormatException) { /* fall through */ }
        }
        if (obj.TryGetPropertyValue("server_signature", out JsonNode? hex) && hex is JsonValue hVal && hVal.TryGetValue(out string? hs) && hs is not null)
        {
            return DecodeHex(hs);
        }
        return null;
    }

    private static byte[]? DecodeHex(string s)
    {
        if (s.Length % 2 != 0) return null;
        var output = new byte[s.Length / 2];
        for (int i = 0; i < output.Length; i++)
        {
            int hi = HexNibble(s[i * 2]);
            int lo = HexNibble(s[i * 2 + 1]);
            if (hi < 0 || lo < 0) return null;
            output[i] = (byte)((hi << 4) | lo);
        }
        return output;
    }

    private static int HexNibble(char c) => c switch
    {
        >= '0' and <= '9' => c - '0',
        >= 'a' and <= 'f' => 10 + (c - 'a'),
        >= 'A' and <= 'F' => 10 + (c - 'A'),
        _ => -1,
    };

    // ---------------------------------------------------------------
    // IConn methods
    // ---------------------------------------------------------------

    public async ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, CancellationToken cancellationToken = default)
    {
        EnsureOpen();
        await _lock.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            ulong corr = NextCorr();
            byte[] payload = Encoding.UTF8.GetBytes(sql);
            using var ctx = WithTimeout(cancellationToken);
            await WriteFrameAsync(_stream, new Frame(Frame.Kind.Query, corr, payload), ctx.Token).ConfigureAwait(false);
            Frame resp = await ReadFrameAsync(_stream, ctx.Token).ConfigureAwait(false);
            if (resp.MessageKind == Frame.Kind.Result) return resp.Payload;
            if (resp.MessageKind == Frame.Kind.Error)
                throw new RedDBException.EngineError(Encoding.UTF8.GetString(resp.Payload));
            throw new RedDBException.ProtocolError(
                $"expected Result/Error, got {Frame.Kind.Name(resp.MessageKind)}");
        }
        finally { _lock.Release(); }
    }

    public ValueTask InsertAsync(string collection, object payload, CancellationToken cancellationToken = default)
    {
        var body = new JsonObject
        {
            ["collection"] = collection,
            ["payload"] = JsonSerializer.SerializeToNode(payload, payload?.GetType() ?? typeof(object), JsonOpts),
        };
        return SendInsertAsync(body, cancellationToken);
    }

    public ValueTask BulkInsertAsync(string collection, IReadOnlyList<object> rows, CancellationToken cancellationToken = default)
    {
        var arr = new JsonArray();
        foreach (var row in rows)
        {
            arr.Add(JsonSerializer.SerializeToNode(row, row?.GetType() ?? typeof(object), JsonOpts));
        }
        var body = new JsonObject
        {
            ["collection"] = collection,
            ["payloads"] = arr,
        };
        return SendInsertAsync(body, cancellationToken);
    }

    private async ValueTask SendInsertAsync(JsonObject body, CancellationToken cancellationToken)
    {
        EnsureOpen();
        await _lock.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            ulong corr = NextCorr();
            byte[] bytes = JsonSerializer.SerializeToUtf8Bytes(body, JsonOpts);
            using var ctx = WithTimeout(cancellationToken);
            await WriteFrameAsync(_stream, new Frame(Frame.Kind.BulkInsert, corr, bytes), ctx.Token).ConfigureAwait(false);
            Frame resp = await ReadFrameAsync(_stream, ctx.Token).ConfigureAwait(false);
            if (resp.MessageKind == Frame.Kind.BulkOk) return;
            if (resp.MessageKind == Frame.Kind.Error)
                throw new RedDBException.EngineError(Encoding.UTF8.GetString(resp.Payload));
            throw new RedDBException.ProtocolError(
                $"expected BulkOk/Error, got {Frame.Kind.Name(resp.MessageKind)}");
        }
        finally { _lock.Release(); }
    }

    public async ValueTask<ReadOnlyMemory<byte>> GetAsync(string collection, string id, CancellationToken cancellationToken = default)
    {
        EnsureOpen();
        await _lock.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            ulong corr = NextCorr();
            var body = new JsonObject { ["collection"] = collection, ["id"] = id };
            using var ctx = WithTimeout(cancellationToken);
            await WriteFrameAsync(_stream, new Frame(Frame.Kind.Get, corr, JsonSerializer.SerializeToUtf8Bytes(body, JsonOpts)), ctx.Token).ConfigureAwait(false);
            Frame resp = await ReadFrameAsync(_stream, ctx.Token).ConfigureAwait(false);
            if (resp.MessageKind == Frame.Kind.Result) return resp.Payload;
            if (resp.MessageKind == Frame.Kind.Error)
                throw new RedDBException.EngineError(Encoding.UTF8.GetString(resp.Payload));
            throw new RedDBException.ProtocolError(
                $"expected Result/Error, got {Frame.Kind.Name(resp.MessageKind)}");
        }
        finally { _lock.Release(); }
    }

    public async ValueTask DeleteAsync(string collection, string id, CancellationToken cancellationToken = default)
    {
        EnsureOpen();
        await _lock.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            ulong corr = NextCorr();
            var body = new JsonObject { ["collection"] = collection, ["id"] = id };
            using var ctx = WithTimeout(cancellationToken);
            await WriteFrameAsync(_stream, new Frame(Frame.Kind.Delete, corr, JsonSerializer.SerializeToUtf8Bytes(body, JsonOpts)), ctx.Token).ConfigureAwait(false);
            Frame resp = await ReadFrameAsync(_stream, ctx.Token).ConfigureAwait(false);
            if (resp.MessageKind == Frame.Kind.DeleteOk) return;
            if (resp.MessageKind == Frame.Kind.Error)
                throw new RedDBException.EngineError(Encoding.UTF8.GetString(resp.Payload));
            throw new RedDBException.ProtocolError(
                $"expected DeleteOk/Error, got {Frame.Kind.Name(resp.MessageKind)}");
        }
        finally { _lock.Release(); }
    }

    public async ValueTask PingAsync(CancellationToken cancellationToken = default)
    {
        EnsureOpen();
        await _lock.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            ulong corr = NextCorr();
            using var ctx = WithTimeout(cancellationToken);
            await WriteFrameAsync(_stream, new Frame(Frame.Kind.Ping, corr, Array.Empty<byte>()), ctx.Token).ConfigureAwait(false);
            Frame resp = await ReadFrameAsync(_stream, ctx.Token).ConfigureAwait(false);
            if (resp.MessageKind != Frame.Kind.Pong)
                throw new RedDBException.ProtocolError(
                    $"expected Pong, got {Frame.Kind.Name(resp.MessageKind)}");
        }
        finally { _lock.Release(); }
    }

    public async ValueTask DisposeAsync()
    {
        if (_closed) return;
        _closed = true;
        try
        {
            await _lock.WaitAsync().ConfigureAwait(false);
            try
            {
                ulong corr = NextCorr();
                await WriteFrameAsync(_stream, new Frame(Frame.Kind.Bye, corr, Array.Empty<byte>()), CancellationToken.None).ConfigureAwait(false);
            }
            catch
            {
                // best-effort; the socket may already be gone.
            }
            finally { _lock.Release(); }
        }
        catch { /* shutting down */ }

        try { _owner?.Dispose(); } catch { }
        _lock.Dispose();
    }

    // ---------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------

    private ulong NextCorr() => (ulong)Interlocked.Increment(ref _nextCorrelation);

    private void EnsureOpen()
    {
        if (_closed) throw new InvalidOperationException("RedWireConn is disposed");
    }

    private CancellationTokenSourceCtx WithTimeout(CancellationToken outer)
    {
        var cts = CancellationTokenSource.CreateLinkedTokenSource(outer);
        if (_timeout > TimeSpan.Zero) cts.CancelAfter(_timeout);
        return new CancellationTokenSourceCtx(cts);
    }

    private readonly struct CancellationTokenSourceCtx : IDisposable
    {
        private readonly CancellationTokenSource _cts;
        public CancellationTokenSourceCtx(CancellationTokenSource cts) { _cts = cts; }
        public CancellationToken Token => _cts.Token;
        public void Dispose() => _cts.Dispose();
    }

    /// <summary>Write a fully-encoded frame and flush.</summary>
    public static async ValueTask WriteFrameAsync(Stream stream, Frame frame, CancellationToken cancellationToken)
    {
        byte[] bytes = Codec.Encode(frame);
        await stream.WriteAsync(bytes, cancellationToken).ConfigureAwait(false);
        await stream.FlushAsync(cancellationToken).ConfigureAwait(false);
    }

    /// <summary>Read exactly one frame from the stream, blocking on partial reads.</summary>
    public static async ValueTask<Frame> ReadFrameAsync(Stream stream, CancellationToken cancellationToken)
    {
        var header = new byte[Frame.HeaderSize];
        await ReadExactAsync(stream, header, cancellationToken).ConfigureAwait(false);
        uint length = BinaryPrimitives.ReadUInt32LittleEndian(header);
        if (length < Frame.HeaderSize || length > Frame.MaxFrameSize)
            throw new RedDBException.FrameTooLarge($"frame length out of range: {length}");
        var full = new byte[length];
        Buffer.BlockCopy(header, 0, full, 0, Frame.HeaderSize);
        if (length > Frame.HeaderSize)
        {
            await ReadExactAsync(stream, full.AsMemory(Frame.HeaderSize, (int)length - Frame.HeaderSize), cancellationToken).ConfigureAwait(false);
        }
        return Codec.Decode(full);
    }

    private static async ValueTask ReadExactAsync(Stream stream, Memory<byte> buffer, CancellationToken cancellationToken)
    {
        int total = 0;
        while (total < buffer.Length)
        {
            int n = await stream.ReadAsync(buffer.Slice(total), cancellationToken).ConfigureAwait(false);
            if (n == 0) throw new EndOfStreamException("stream closed mid-frame");
            total += n;
        }
    }

    private static string Reason(byte[] payload, string fallback)
    {
        if (payload is null || payload.Length == 0) return fallback;
        try
        {
            JsonNode? n = JsonNode.Parse(payload);
            if (n is JsonObject obj && obj.TryGetPropertyValue("reason", out JsonNode? r)
                && r is JsonValue rVal && rVal.TryGetValue(out string? s) && s is not null)
            {
                return s;
            }
        }
        catch { /* not JSON */ }
        return Encoding.UTF8.GetString(payload);
    }

    private static JsonNode? ParseJson(byte[] payload, string label)
    {
        try { return JsonNode.Parse(payload); }
        catch (Exception ex)
        {
            throw new RedDBException.ProtocolError($"{label}: invalid JSON: {ex.Message}");
        }
    }

    private static string? TextField(JsonNode? node, string name)
    {
        if (node is not JsonObject obj) return null;
        if (!obj.TryGetPropertyValue(name, out JsonNode? v)) return null;
        if (v is JsonValue val && val.TryGetValue(out string? s)) return s;
        return null;
    }

    /// <summary>Outcome of a successful handshake — exposed mostly for tests.</summary>
    public sealed class HandshakeResult
    {
        public string SessionId { get; }
        public HandshakeResult(string sessionId) { SessionId = sessionId; }
    }
}
