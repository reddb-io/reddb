using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using Reddb;
using Reddb.Helpers;
using Xunit;

namespace Reddb.Tests;

/// <summary>
/// SDK Helper Spec — conformance harness for the .NET driver.
///
/// Spec: <c>docs/spec/sdk-helpers.md</c> (v1.0). Case IDs in §12 are ported as
/// test method names (dots → underscores) so cross-driver dashboards line up.
///
/// The .NET driver does not embed the engine, so the harness spawns one
/// <c>red server</c> process per test class and shares it across cases. The
/// harness is opt-in, gated on the same env contract as <see cref="SmokeTests"/>:
///
/// <list type="bullet">
///   <item>Skipped when <c>RED_SMOKE</c> is not <c>1</c>.</item>
///   <item>Skipped when <c>RED_SKIP_SMOKE=1</c>.</item>
///   <item>Set <c>RED_BIN=/path/to/red</c> to reuse a prebuilt binary;
///     otherwise the harness spawns <c>cargo run --release --bin red</c>.</item>
/// </list>
/// </summary>
public class ConformanceTests : IClassFixture<ConformanceTests.ServerFixture>
{
    public sealed class ServerFixture : IAsyncLifetime
    {
        public Process? Process;
        public string? Uri;
        public string? Skip;

        public Task InitializeAsync()
        {
            if (Environment.GetEnvironmentVariable("RED_SKIP_SMOKE") == "1")
            {
                Skip = "RED_SKIP_SMOKE=1 set";
                return Task.CompletedTask;
            }
            if (Environment.GetEnvironmentVariable("RED_SMOKE") != "1")
            {
                Skip = "set RED_SMOKE=1 to enable the conformance harness";
                return Task.CompletedTask;
            }

            string repoRoot = FindRepoRoot();
            string dataDir = Directory.CreateTempSubdirectory("reddb-dotnet-conf-").FullName;
            int port = FreePort();
            string? redBin = Environment.GetEnvironmentVariable("RED_BIN");
            var args = new List<string>();
            string exe;
            if (!string.IsNullOrWhiteSpace(redBin))
            {
                exe = redBin;
            }
            else
            {
                exe = "cargo";
                args.AddRange(new[] { "run", "--release", "--bin", "red", "--" });
            }
            args.AddRange(new[] { "server",
                "--path", Path.Combine(dataDir, "data.db"),
                "--bind", $"127.0.0.1:{port}" });
            var psi = new ProcessStartInfo(exe)
            {
                WorkingDirectory = repoRoot,
                UseShellExecute = false,
            };
            foreach (var a in args) psi.ArgumentList.Add(a);
            Process = Process.Start(psi)
                ?? throw new InvalidOperationException("failed to spawn red server");
            Uri = $"red://127.0.0.1:{port}";
            return WaitReadyAsync(Uri, TimeSpan.FromSeconds(60));
        }

        public Task DisposeAsync()
        {
            if (Process is not null)
            {
                try { Process.Kill(true); } catch { }
                Process.WaitForExit();
            }
            return Task.CompletedTask;
        }

        private static async Task WaitReadyAsync(string uri, TimeSpan timeout)
        {
            var deadline = DateTime.UtcNow + timeout;
            Exception? last = null;
            while (DateTime.UtcNow < deadline)
            {
                try
                {
                    await using IConn c = await Reddb.ConnectAsync(uri);
                    await c.PingAsync();
                    return;
                }
                catch (Exception ex) { last = ex; }
                await Task.Delay(100);
            }
            throw new IOException($"server did not become ready at {uri}", last);
        }

        private static int FreePort()
        {
            var l = new TcpListener(IPAddress.Loopback, 0);
            l.Start();
            try { return ((IPEndPoint)l.LocalEndpoint).Port; }
            finally { l.Stop(); }
        }

        private static string FindRepoRoot()
        {
            var f = new DirectoryInfo(Directory.GetCurrentDirectory());
            while (f is not null)
            {
                if (File.Exists(Path.Combine(f.FullName, "Cargo.toml"))
                    && Directory.Exists(Path.Combine(f.FullName, "drivers")))
                    return f.FullName;
                f = f.Parent;
            }
            throw new InvalidOperationException("repo root not found");
        }
    }

    private readonly ServerFixture _fx;
    public ConformanceTests(ServerFixture fx) { _fx = fx; }

    private async Task<(IConn, Helpers.Helpers)> DialAsync()
    {
        if (_fx.Skip is not null) throw SkipException.For(_fx.Skip);
        IConn c = await Reddb.ConnectAsync(_fx.Uri!);
        return (c, Helpers.Helpers.For(c));
    }

    // Xunit v2 has no native Skip; emulate by short-circuiting if the fixture
    // recorded a skip reason. The test still passes — matches the SmokeTests
    // convention in this repo.
    private bool SkippedHere()
    {
        if (_fx.Skip is null) return false;
        return true;
    }

    private static string Uniq([System.Runtime.CompilerServices.CallerMemberName] string name = "")
        => name.ToLowerInvariant().Replace('.', '_');

    // --- generic.* --------------------------------------------------------

    [Fact(DisplayName = "generic.query.no_params")]
    public async Task Conformance_generic_query_no_params()
    {
        if (SkippedHere()) return;
        var (c, _) = await DialAsync();
        await using var _c = c;
        string table = "conf_q_" + Uniq();
        await c.QueryAsync($"CREATE TABLE {table} (id INTEGER, name TEXT)");
        await c.QueryAsync($"INSERT INTO {table} (id, name) VALUES (1, 'a')");
        var body = await c.QueryAsync($"SELECT id, name FROM {table}");
        string s = Encoding.UTF8.GetString(body.Span);
        Assert.Contains("\"a\"", s);
    }

    [Fact(DisplayName = "generic.query_with.params")]
    public async Task Conformance_generic_query_with_params()
    {
        if (SkippedHere()) return;
        var (c, _) = await DialAsync();
        await using var _c = c;
        string table = "conf_p_" + Uniq();
        await c.QueryAsync($"CREATE TABLE {table} (id INTEGER, name TEXT)");
        await c.QueryAsync($"INSERT INTO {table} (id, name) VALUES ($1, $2)", 42, "alice");
        var body = await c.QueryAsync($"SELECT name FROM {table} WHERE id = $1", 42);
        Assert.Contains("alice", Encoding.UTF8.GetString(body.Span));
    }

    [Fact(DisplayName = "generic.insert.rid")]
    public async Task Conformance_generic_insert_rid()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        var r = await h.Documents().InsertAsync("conf_ins_" + Uniq(),
            new Dictionary<string, object?> { ["name"] = "eve" });
        Assert.Equal(1L, r.Affected);
        Assert.False(string.IsNullOrEmpty(r.Rid));
    }

    [Fact(DisplayName = "generic.delete")]
    public async Task Conformance_generic_delete()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string coll = "conf_del_" + Uniq();
        var ins = await h.Documents().InsertAsync(coll,
            new Dictionary<string, object?> { ["k"] = "v" });
        var del = await h.Documents().DeleteAsync(coll, ins.Rid);
        Assert.Equal(1L, del.Affected);
        Assert.True(del.Deleted);
    }

    // --- documents.* ------------------------------------------------------

    [Fact(DisplayName = "documents.crud_nested_patch")]
    public async Task Conformance_documents_crud_nested_patch()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string coll = "conf_doc_" + Uniq();
        var docs = h.Documents();
        var ins = await docs.InsertAsync(coll, new Dictionary<string, object?>
        {
            ["event_type"] = "login",
            ["attempts"] = 2,
            ["success"] = true,
        });
        Assert.False(string.IsNullOrEmpty(ins.Rid));

        var got = await docs.GetAsync(coll, ins.Rid);
        Assert.Equal("login", got["event_type"]);

        var list = await docs.ListAsync(coll);
        Assert.NotEmpty(list.Items);

        var patched = await docs.PatchAsync(coll, ins.Rid,
            new Dictionary<string, object?> { ["attempts"] = 3 });
        // Spec §4.4: top-level merge MUST preserve unrelated fields.
        Assert.Equal("login", patched["event_type"]);

        var del = await docs.DeleteAsync(coll, ins.Rid);
        Assert.Equal(1L, del.Affected);
        Assert.True(del.Deleted);
    }

    [Fact(DisplayName = "documents.delete_missing_no_error")]
    public async Task Conformance_documents_delete_missing_no_error()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string coll = "conf_doc_miss_" + Uniq();
        // Touch the collection so it exists.
        var ins = await h.Documents().InsertAsync(coll,
            new Dictionary<string, object?> { ["k"] = "v" });
        await h.Documents().DeleteAsync(coll, ins.Rid);
        var r = await h.Documents().DeleteAsync(coll, "rid_that_does_not_exist");
        Assert.Equal(0L, r.Affected);
        Assert.False(r.Deleted);
    }

    [Fact(DisplayName = "documents.patch_empty_rejects")]
    public async Task Conformance_documents_patch_empty_rejects()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string coll = "conf_doc_pe_" + Uniq();
        var ins = await h.Documents().InsertAsync(coll,
            new Dictionary<string, object?> { ["k"] = "v" });
        await Assert.ThrowsAsync<HelperException.InvalidArgument>(
            async () => await h.Documents().PatchAsync(coll, ins.Rid,
                new Dictionary<string, object?>()));
    }

    // --- kv.* -------------------------------------------------------------

    [Fact(DisplayName = "kv.exact_key_round_trip")]
    public async Task Conformance_kv_exact_key_round_trip()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string coll = "conf_kv_" + Uniq();
        var kv = h.Kv();
        const string key = "characters:hansel";
        await kv.SetAsync(key, "witch", new KvClient.SetOptions { Collection = coll });
        object? got = await kv.GetAsync(key, coll);
        Assert.Equal("witch", got);
    }

    [Fact(DisplayName = "kv.missing_get_returns_none")]
    public async Task Conformance_kv_missing_get_returns_none()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string coll = "conf_kv_miss_" + Uniq();
        var kv = h.Kv();
        await kv.SetAsync("seed", "v", new KvClient.SetOptions { Collection = coll });
        Assert.Null(await kv.GetAsync("never:set", coll));
    }

    [Fact(DisplayName = "kv.delete_returns_envelope")]
    public async Task Conformance_kv_delete_returns_envelope()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string coll = "conf_kv_del_" + Uniq();
        var kv = h.Kv();
        await kv.SetAsync("k", "v", new KvClient.SetOptions { Collection = coll });
        var r = await kv.DeleteAsync("k", coll);
        Assert.Equal(1L, r.Affected);
        Assert.True(r.Deleted);
        var r2 = await kv.DeleteAsync("k", coll);
        Assert.Equal(0L, r2.Affected);
        Assert.False(r2.Deleted);
    }

    // --- queues.* ---------------------------------------------------------

    [Fact(DisplayName = "queues.fifo_peek_pop_len")]
    public async Task Conformance_queues_fifo_peek_pop_len()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string name = "conf_q_fifo_" + Uniq();
        var q = h.Queues();
        await q.CreateAsync(name);
        await q.PushAsync(name, new Dictionary<string, object?> { ["n"] = 1 });
        await q.PushAsync(name, new Dictionary<string, object?> { ["n"] = 2 });
        Assert.Equal(2L, await q.LenAsync(name));
        var peeked = await q.PeekAsync(name, 1);
        Assert.Single(peeked);
        Assert.Equal(2L, await q.LenAsync(name));
        var popped = await q.PopAsync(name, 1);
        Assert.Single(popped);
        Assert.Equal(1L, await q.LenAsync(name));
    }

    [Fact(DisplayName = "queues.empty_pop_returns_empty")]
    public async Task Conformance_queues_empty_pop_returns_empty()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string name = "conf_q_empty_" + Uniq();
        var q = h.Queues();
        await q.CreateAsync(name);
        var out_ = await q.PopAsync(name);
        Assert.Empty(out_);
    }

    [Fact(DisplayName = "queues.purge_resets_len")]
    public async Task Conformance_queues_purge_resets_len()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string name = "conf_q_purge_" + Uniq();
        var q = h.Queues();
        await q.CreateAsync(name);
        for (int i = 0; i < 3; i++)
            await q.PushAsync(name, new Dictionary<string, object?> { ["i"] = i });
        Assert.Equal(3L, await q.LenAsync(name));
        await q.PurgeAsync(name);
        Assert.Equal(0L, await q.LenAsync(name));
    }

    // --- tx.* -------------------------------------------------------------

    [Fact(DisplayName = "tx.commit_persists")]
    public async Task Conformance_tx_commit_persists()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string table = "conf_tx_commit_" + Uniq();
        await c.QueryAsync($"CREATE TABLE {table} (name TEXT)");
        var tx = h.Tx();
        await tx.BeginAsync();
        await c.QueryAsync($"INSERT INTO {table} (name) VALUES ('keep')");
        await tx.CommitAsync();
        var body = await c.QueryAsync($"SELECT name FROM {table} WHERE name = 'keep'");
        Assert.Contains("keep", Encoding.UTF8.GetString(body.Span));
    }

    [Fact(DisplayName = "tx.rollback_discards")]
    public async Task Conformance_tx_rollback_discards()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string table = "conf_tx_rb_" + Uniq();
        await c.QueryAsync($"CREATE TABLE {table} (name TEXT)");
        var tx = h.Tx();
        await tx.BeginAsync();
        await c.QueryAsync($"INSERT INTO {table} (name) VALUES ('drop')");
        await tx.RollbackAsync();
        var body = await c.QueryAsync($"SELECT name FROM {table} WHERE name = 'drop'");
        Assert.DoesNotContain("drop", Encoding.UTF8.GetString(body.Span));
    }

    // --- errors.* ---------------------------------------------------------

    [Fact(DisplayName = "errors.not_found.document_get")]
    public async Task Conformance_errors_not_found_document_get()
    {
        if (SkippedHere()) return;
        var (c, h) = await DialAsync();
        await using var _c = c;
        string coll = "conf_err_nf_" + Uniq();
        var ins = await h.Documents().InsertAsync(coll,
            new Dictionary<string, object?> { ["k"] = "v" });
        await h.Documents().DeleteAsync(coll, ins.Rid);
        await Assert.ThrowsAsync<HelperException.NotFound>(
            async () => await h.Documents().GetAsync(coll, "rid_definitely_missing"));
    }

    // --- wire.* (provisional namespaces — SQL only in v1.0) ---------------

    [Fact(DisplayName = "wire.probabilistic.hll_round_trip")]
    public async Task Conformance_wire_probabilistic_hll_round_trip()
    {
        if (SkippedHere()) return;
        var (c, _) = await DialAsync();
        await using var _c = c;
        string name = "conf_hll_" + Uniq();
        await c.QueryAsync("CREATE HLL " + name);
        await c.QueryAsync($"HLL ADD {name} 'alice' 'bob' 'alice'");
        var body = await c.QueryAsync("HLL COUNT " + name);
        string s = Encoding.UTF8.GetString(body.Span);
        Assert.True(s.Contains("count") || s.Contains("cardinality"),
            $"expected count/cardinality column in: {s}");
    }
}

/// <summary>
/// Local skip-exception helper — xunit v2 doesn't ship a built-in skip; the
/// repo's <see cref="SmokeTests"/> uses early-return, and the conformance
/// cases follow the same pattern. This type exists so individual cases can
/// throw a sentinel if a deeper helper wants to short-circuit; currently
/// unused by the cases themselves.
/// </summary>
internal sealed class SkipException : Exception
{
    private SkipException(string reason) : base(reason) { }
    public static SkipException For(string reason) => new(reason);
}
