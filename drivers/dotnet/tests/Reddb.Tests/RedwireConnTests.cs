using System;
using System.Buffers.Binary;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;
using Reddb;
using Reddb.Redwire;
using Xunit;

namespace Reddb.Tests;

/// <summary>
/// Drives the handshake state machine over a loopback TCP socket pair
/// so we don't need a real engine. The "server" runs as a background
/// task on the listener side.
/// </summary>
public class RedwireConnTests
{
    /// <summary>Spin up a listener bound to 127.0.0.1:0 and connect a client.</summary>
    private static async Task<(NetworkStream client, NetworkStream server, TcpClient clientSock, TcpClient serverSock, TcpListener listener)>
        OpenSocketPairAsync()
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;

        var clientSock = new TcpClient();
        var connectTask = clientSock.ConnectAsync(IPAddress.Loopback, port);
        var serverSock = await listener.AcceptTcpClientAsync();
        await connectTask;

        return (clientSock.GetStream(), serverSock.GetStream(), clientSock, serverSock, listener);
    }

    private static async Task ReadExactAsync(Stream s, byte[] buf, CancellationToken ct = default)
    {
        int total = 0;
        while (total < buf.Length)
        {
            int n = await s.ReadAsync(buf.AsMemory(total), ct);
            if (n == 0) throw new EndOfStreamException();
            total += n;
        }
    }

    private static async Task ReadMagicAsync(Stream s)
    {
        var two = new byte[2];
        await ReadExactAsync(s, two);
        Assert.Equal(0xfe, two[0]);
        Assert.Equal(0x01, two[1]);
    }

    private static async Task<Frame> ReadClientFrameAsync(Stream s)
    {
        var header = new byte[Frame.HeaderSize];
        await ReadExactAsync(s, header);
        uint length = BinaryPrimitives.ReadUInt32LittleEndian(header);
        var full = new byte[length];
        Buffer.BlockCopy(header, 0, full, 0, Frame.HeaderSize);
        if (length > Frame.HeaderSize)
        {
            var rest = new byte[length - Frame.HeaderSize];
            await ReadExactAsync(s, rest);
            Buffer.BlockCopy(rest, 0, full, Frame.HeaderSize, rest.Length);
        }
        return Codec.Decode(full);
    }

    private static async Task WriteServerFrameAsync(Stream s, byte kind, ulong corr, byte[] payload)
    {
        var f = new Frame(kind, 0, 0, corr, payload);
        var bytes = Codec.Encode(f);
        await s.WriteAsync(bytes);
        await s.FlushAsync();
    }

    private static byte[] Json(JsonObject obj) => JsonSerializer.SerializeToUtf8Bytes(obj);

    [Fact]
    public async Task HandshakeAnonymous_Succeeds()
    {
        var (client, server, csock, ssock, listener) = await OpenSocketPairAsync();
        Exception? serverErr = null;

        var serverTask = Task.Run(async () =>
        {
            try
            {
                await ReadMagicAsync(server);
                Frame hello = await ReadClientFrameAsync(server);
                Assert.Equal(Frame.Kind.Hello, hello.MessageKind);
                var helloJson = JsonNode.Parse(hello.Payload)!.AsObject();
                Assert.Contains("anonymous", helloJson["auth_methods"]!.ToJsonString());

                var ack = new JsonObject { ["auth"] = "anonymous", ["version"] = 1, ["features"] = 0 };
                await WriteServerFrameAsync(server, Frame.Kind.HelloAck, hello.CorrelationId, Json(ack));

                Frame resp = await ReadClientFrameAsync(server);
                Assert.Equal(Frame.Kind.AuthResponse, resp.MessageKind);
                Assert.Empty(resp.Payload);

                var ok = new JsonObject
                {
                    ["session_id"] = "rwsess-test-anon",
                    ["username"] = "anonymous",
                    ["role"] = "read",
                };
                await WriteServerFrameAsync(server, Frame.Kind.AuthOk, resp.CorrelationId, Json(ok));
            }
            catch (Exception ex) { serverErr = ex; }
        });

        var res = await RedWireConn.PerformHandshakeAsync(client, null, null, null, "test-driver");
        await serverTask;
        if (serverErr is not null) throw new Exception("server thread", serverErr);

        Assert.Equal("rwsess-test-anon", res.SessionId);

        csock.Dispose(); ssock.Dispose(); listener.Stop();
    }

    [Fact]
    public async Task HandshakeBearer_Succeeds()
    {
        var (client, server, csock, ssock, listener) = await OpenSocketPairAsync();
        Exception? serverErr = null;

        var serverTask = Task.Run(async () =>
        {
            try
            {
                await ReadMagicAsync(server);
                Frame hello = await ReadClientFrameAsync(server);
                Assert.Contains("bearer", JsonNode.Parse(hello.Payload)!["auth_methods"]!.ToJsonString());

                var ack = new JsonObject { ["auth"] = "bearer" };
                await WriteServerFrameAsync(server, Frame.Kind.HelloAck, hello.CorrelationId, Json(ack));

                Frame resp = await ReadClientFrameAsync(server);
                var r = JsonNode.Parse(resp.Payload)!.AsObject();
                Assert.Equal("the-token", (string?)r["token"]);

                var ok = new JsonObject { ["session_id"] = "rwsess-test-bearer" };
                await WriteServerFrameAsync(server, Frame.Kind.AuthOk, resp.CorrelationId, Json(ok));
            }
            catch (Exception ex) { serverErr = ex; }
        });

        var res = await RedWireConn.PerformHandshakeAsync(client, null, null, "the-token", "test-driver");
        await serverTask;
        if (serverErr is not null) throw new Exception("server thread", serverErr);

        Assert.Equal("rwsess-test-bearer", res.SessionId);

        csock.Dispose(); ssock.Dispose(); listener.Stop();
    }

    [Fact]
    public async Task AuthFailAtHelloAck_ThrowsAuthRefused()
    {
        var (client, server, csock, ssock, listener) = await OpenSocketPairAsync();

        var serverTask = Task.Run(async () =>
        {
            try
            {
                await ReadMagicAsync(server);
                Frame hello = await ReadClientFrameAsync(server);
                var reason = new JsonObject { ["reason"] = "no overlapping auth method" };
                await WriteServerFrameAsync(server, Frame.Kind.AuthFail, hello.CorrelationId, Json(reason));
            }
            catch { /* peer closed */ }
        });

        var ex = await Assert.ThrowsAsync<RedDBException.AuthRefused>(async () =>
        {
            await RedWireConn.PerformHandshakeAsync(client, null, null, null, "test-driver");
        });
        Assert.Contains("no overlapping auth method", ex.Message);
        await serverTask;

        csock.Dispose(); ssock.Dispose(); listener.Stop();
    }

    [Fact]
    public async Task AuthFailAtAuthOk_ThrowsAuthRefused()
    {
        var (client, server, csock, ssock, listener) = await OpenSocketPairAsync();

        var serverTask = Task.Run(async () =>
        {
            try
            {
                await ReadMagicAsync(server);
                Frame hello = await ReadClientFrameAsync(server);
                var ack = new JsonObject { ["auth"] = "bearer" };
                await WriteServerFrameAsync(server, Frame.Kind.HelloAck, hello.CorrelationId, Json(ack));

                Frame resp = await ReadClientFrameAsync(server);
                var reason = new JsonObject { ["reason"] = "bearer token invalid" };
                await WriteServerFrameAsync(server, Frame.Kind.AuthFail, resp.CorrelationId, Json(reason));
            }
            catch { }
        });

        var ex = await Assert.ThrowsAsync<RedDBException.AuthRefused>(async () =>
        {
            await RedWireConn.PerformHandshakeAsync(client, null, null, "bad-token", "test-driver");
        });
        Assert.Contains("bearer token invalid", ex.Message);
        await serverTask;

        csock.Dispose(); ssock.Dispose(); listener.Stop();
    }

    [Fact]
    public async Task ServerPicksUnsupportedAuthMethod_ThrowsProtocol()
    {
        var (client, server, csock, ssock, listener) = await OpenSocketPairAsync();

        var serverTask = Task.Run(async () =>
        {
            try
            {
                await ReadMagicAsync(server);
                Frame hello = await ReadClientFrameAsync(server);
                var ack = new JsonObject { ["auth"] = "made-up-method" };
                await WriteServerFrameAsync(server, Frame.Kind.HelloAck, hello.CorrelationId, Json(ack));
            }
            catch { }
        });

        var ex = await Assert.ThrowsAsync<RedDBException.ProtocolError>(async () =>
        {
            await RedWireConn.PerformHandshakeAsync(client, null, null, null, "test-driver");
        });
        Assert.Contains("made-up-method", ex.Message);
        await serverTask;

        csock.Dispose(); ssock.Dispose(); listener.Stop();
    }

    [Fact]
    public async Task MalformedHelloAckJson_RaisesProtocolError()
    {
        var (client, server, csock, ssock, listener) = await OpenSocketPairAsync();

        var serverTask = Task.Run(async () =>
        {
            try
            {
                await ReadMagicAsync(server);
                Frame hello = await ReadClientFrameAsync(server);
                await WriteServerFrameAsync(server, Frame.Kind.HelloAck, hello.CorrelationId,
                    Encoding.UTF8.GetBytes("not json"));
            }
            catch { }
        });

        await Assert.ThrowsAsync<RedDBException.ProtocolError>(async () =>
        {
            await RedWireConn.PerformHandshakeAsync(client, null, null, null, "test-driver");
        });
        await serverTask;

        csock.Dispose(); ssock.Dispose(); listener.Stop();
    }

    [Fact]
    public async Task ClientSendsMagicByteFirst()
    {
        var (client, server, csock, ssock, listener) = await OpenSocketPairAsync();
        var prefix = new MemoryStream();

        var serverTask = Task.Run(async () =>
        {
            try
            {
                var two = new byte[2];
                await ReadExactAsync(server, two);
                prefix.Write(two);
                Frame hello = await ReadClientFrameAsync(server);
                var reason = new JsonObject { ["reason"] = "stop here" };
                await WriteServerFrameAsync(server, Frame.Kind.AuthFail, hello.CorrelationId, Json(reason));
            }
            catch { }
        });

        await Assert.ThrowsAsync<RedDBException.AuthRefused>(async () =>
        {
            await RedWireConn.PerformHandshakeAsync(client, null, null, null, "test-driver");
        });
        await serverTask;

        var header = prefix.ToArray();
        Assert.Equal((byte)0xfe, header[0]);
        Assert.Equal((byte)0x01, header[1]);

        csock.Dispose(); ssock.Dispose(); listener.Stop();
    }

    [Fact]
    public async Task QueryRoundTrip_AfterHandshake()
    {
        var (client, server, csock, ssock, listener) = await OpenSocketPairAsync();
        Exception? serverErr = null;

        var serverTask = Task.Run(async () =>
        {
            try
            {
                await ReadMagicAsync(server);
                Frame hello = await ReadClientFrameAsync(server);
                var ack = new JsonObject { ["auth"] = "anonymous" };
                await WriteServerFrameAsync(server, Frame.Kind.HelloAck, hello.CorrelationId, Json(ack));

                Frame resp = await ReadClientFrameAsync(server);
                var ok = new JsonObject { ["session_id"] = "rwsess-test-q" };
                await WriteServerFrameAsync(server, Frame.Kind.AuthOk, resp.CorrelationId, Json(ok));

                Frame q = await ReadClientFrameAsync(server);
                Assert.Equal(Frame.Kind.Query, q.MessageKind);
                Assert.Equal("SELECT 1", Encoding.UTF8.GetString(q.Payload));
                var result = new JsonObject { ["ok"] = true, ["affected"] = 1 };
                await WriteServerFrameAsync(server, Frame.Kind.Result, q.CorrelationId, Json(result));

                // Drain a Bye frame so the test cleans up.
                await ReadClientFrameAsync(server);
            }
            catch (Exception ex) { serverErr = ex; }
        });

        var res = await RedWireConn.PerformHandshakeAsync(client, null, null, null, "test-driver");
        Assert.Equal("rwsess-test-q", res.SessionId);

        var conn = new RedWireConn(client, csock, res.SessionId, TimeSpan.FromSeconds(5));
        var bytes = await conn.QueryAsync("SELECT 1");
        var parsed = JsonNode.Parse(bytes.Span)!.AsObject();
        Assert.True((bool)parsed["ok"]!);
        Assert.Equal(1, (int)parsed["affected"]!);

        // Dispose first so the server task drains the Bye frame and exits.
        await conn.DisposeAsync();
        await serverTask;
        if (serverErr is not null) throw new Exception("server thread", serverErr);

        ssock.Dispose(); listener.Stop();
    }
}
