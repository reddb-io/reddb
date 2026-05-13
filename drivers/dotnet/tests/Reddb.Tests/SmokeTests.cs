using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;
using System.Threading.Tasks;
using Reddb;
using Xunit;

namespace Reddb.Tests;

/// <summary>
/// End-to-end smoke against a freshly-spawned RedDB binary. Gated on
/// <c>RED_SMOKE=1</c> so normal test runs don't drag in cargo build
/// time.
/// </summary>
public class SmokeTests
{
    [Fact]
    public async Task RunsAgainstRealEngine()
    {
        if (Environment.GetEnvironmentVariable("RED_SMOKE") != "1")
        {
            return; // Skip when the smoke gate isn't set.
        }

        string repoRoot = FindRepoRoot();
        string dataDir = Directory.CreateTempSubdirectory("reddb-dotnet-smoke-").FullName;
        int port = FreePort();
        IReadOnlyList<string> command = RedCommand(Path.Combine(dataDir, "data.db"), port);
        var psi = new ProcessStartInfo(command[0])
        {
            WorkingDirectory = repoRoot,
            RedirectStandardOutput = false,
            RedirectStandardError = false,
            UseShellExecute = false,
        };
        foreach (string arg in command.Skip(1))
            psi.ArgumentList.Add(arg);
        var proc = Process.Start(psi)
            ?? throw new InvalidOperationException("failed to spawn red server");
        try
        {
            await using IConn conn = await WaitForConnectAsync($"red://127.0.0.1:{port}", TimeSpan.FromSeconds(60));
            await conn.PingAsync();
            await conn.QueryAsync("CREATE TABLE smoke_params (id INT, name TEXT)");
            await conn.QueryAsync("INSERT INTO smoke_params (id, name) VALUES ($1, $2)", 42, "alice");
            var result = await conn.QueryAsync("SELECT 1");
            string body = Encoding.UTF8.GetString(result.Span);
            Assert.Contains("\"ok\":true", body);
            var paramResult = await conn.QueryAsync(
                "SELECT name FROM smoke_params WHERE id = $1 AND name = $2",
                42,
                "alice");
            string paramBody = Encoding.UTF8.GetString(paramResult.Span);
            Assert.Contains("alice", paramBody);
            JsonNode? genericBody = await conn.QueryAsync<JsonNode>(
                "SELECT name FROM smoke_params WHERE id = $1 AND name = $2",
                42,
                "alice");
            Assert.Contains("alice", genericBody!.ToJsonString());
        }
        finally
        {
            try { proc.Kill(true); } catch { }
            await proc.WaitForExitAsync();
        }
    }

    private static IReadOnlyList<string> RedCommand(string dataPath, int port)
    {
        string? redBin = Environment.GetEnvironmentVariable("RED_BIN");
        var command = new List<string>();
        if (!string.IsNullOrWhiteSpace(redBin))
        {
            command.Add(redBin);
        }
        else
        {
            command.AddRange(new[] { "cargo", "run", "--release", "--bin", "red", "--" });
        }
        command.AddRange(new[] { "server", "--path", dataPath, "--bind", $"127.0.0.1:{port}" });
        return command;
    }

    private static async Task<IConn> WaitForConnectAsync(string uri, TimeSpan timeout)
    {
        var deadline = DateTime.UtcNow + timeout;
        Exception? last = null;
        while (DateTime.UtcNow < deadline)
        {
            try
            {
                IConn conn = await Reddb.ConnectAsync(uri);
                try
                {
                    await conn.PingAsync();
                    return conn;
                }
                catch (Exception ex)
                {
                    await conn.DisposeAsync();
                    last = ex;
                }
            }
            catch (Exception ex)
            {
                last = ex;
            }
            await Task.Delay(50);
        }
        throw new IOException($"server did not accept connections at {uri}", last);
    }

    private static int FreePort()
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        try
        {
            return ((IPEndPoint)listener.LocalEndpoint).Port;
        }
        finally
        {
            listener.Stop();
        }
    }

    private static string FindRepoRoot()
    {
        var f = new DirectoryInfo(Directory.GetCurrentDirectory());
        while (f is not null)
        {
            if (File.Exists(Path.Combine(f.FullName, "Cargo.toml"))
                && Directory.Exists(Path.Combine(f.FullName, "drivers")))
            {
                return f.FullName;
            }
            f = f.Parent;
        }
        throw new InvalidOperationException("could not locate repo root with Cargo.toml + drivers/");
    }
}
