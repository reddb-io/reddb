using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;
using System.Threading.Tasks;
using Reddb;
using Xunit;

namespace Reddb.Tests;

/// <summary>
/// End-to-end smoke against a freshly-spawned RedDB binary. Gated on
/// <c>RED_SMOKE=1</c> so normal test runs don't drag in cargo build
/// time. The test discovers the bind port from stdout — the engine
/// prints <c>listening on tcp://127.0.0.1:&lt;port&gt;</c> once the
/// listener is up.
/// </summary>
public class SmokeTests
{
    private static readonly Regex PortRe = new(@"(?:tcp://|listening on .*?:|port=)(\d{2,5})", RegexOptions.Compiled);

    [Fact]
    public async Task RunsAgainstRealEngine()
    {
        if (Environment.GetEnvironmentVariable("RED_SMOKE") != "1")
        {
            return; // Skip when the smoke gate isn't set.
        }

        string repoRoot = FindRepoRoot();
        var psi = new ProcessStartInfo("cargo")
        {
            ArgumentList =
            {
                "run", "--release", "--bin", "red", "--",
                "serve", "--bind", "127.0.0.1:0", "--anon-ok",
            },
            WorkingDirectory = repoRoot,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
        };
        var proc = Process.Start(psi)
            ?? throw new InvalidOperationException("failed to spawn cargo");
        try
        {
            int port = await WaitForPortAsync(proc, TimeSpan.FromSeconds(60));

            await using IConn conn = await Reddb.ConnectAsync($"red://127.0.0.1:{port}");
            await conn.PingAsync();
            await conn.InsertAsync("smoke_users", new Dictionary<string, object?>
            {
                ["name"] = "alice",
                ["age"] = 30,
            });
            var result = await conn.QueryAsync("SELECT * FROM smoke_users WHERE name = 'alice'");
            string body = Encoding.UTF8.GetString(result.Span);
            Assert.Contains("alice", body);
            await conn.DeleteAsync("smoke_users", "alice");
        }
        finally
        {
            try { proc.Kill(true); } catch { }
            await proc.WaitForExitAsync();
        }
    }

    private static async Task<int> WaitForPortAsync(Process proc, TimeSpan timeout)
    {
        var deadline = DateTime.UtcNow + timeout;
        while (DateTime.UtcNow < deadline)
        {
            string? line = await proc.StandardOutput.ReadLineAsync();
            if (line is null) break;
            var m = PortRe.Match(line);
            if (m.Success)
            {
                return int.Parse(m.Groups[1].Value);
            }
        }
        throw new IOException("never saw a bind port in engine stdout");
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
