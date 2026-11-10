using System.Text;
using System.Text.Json.Nodes;
using Xunit;

namespace Reddb.Tests;

public class RoutingTests
{
    [Fact]
    public void CanonicalTopologyFixtures()
    {
        DirectoryInfo? root = new(AppContext.BaseDirectory);
        while (root != null && !File.Exists(Path.Combine(root.FullName, "testdata/conformance/topology/manifest.json"))) root = root.Parent;
        Assert.NotNull(root);
        JsonObject manifest = JsonNode.Parse(File.ReadAllText(Path.Combine(root!.FullName, "testdata/conformance/topology/manifest.json")))!.AsObject();
        foreach (JsonNode? policy in JsonNode.Parse(File.ReadAllText(Path.Combine(root.FullName, "testdata/conformance/topology/sql-policy.json")))!.AsArray())
            Assert.Equal(policy!["replica_read"]!.GetValue<bool>(), RoutingSql.Policy(policy["sql"]!.GetValue<string>()).Item2);
        foreach (JsonNode? fixture in manifest["cases"]!.AsArray())
        {
            byte[] bytes = Convert.FromHexString(fixture!["hex"]!.GetValue<string>());
            if (fixture["error"]?.GetValue<bool>() == true)
                Assert.ThrowsAny<Exception>(() => TopologyCodec.Decode(bytes));
            else Assert.True(JsonNode.DeepEquals(fixture["expected"], TopologyCodec.Decode(bytes)), fixture["name"]!.GetValue<string>());
        }
    }

    private sealed class Peer(string address, Func<JsonObject> topology, List<(string, string)> calls) : IConn, ITopologySource
    {
        public ValueTask<JsonObject?> TopologyAsync(string protocol, CancellationToken cancellationToken) => ValueTask.FromResult<JsonObject?>(topology());
        public ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, CancellationToken cancellationToken = default)
        {
            calls.Add((address, sql));
            if (sql == "UPDATE lost") throw new IOException("lost commit reply");
            if (sql.Contains("effectful")) throw new RedDBException.ProtocolError("HTTP I/O", new IOException("lost reply"));
            return ValueTask.FromResult<ReadOnlyMemory<byte>>(Encoding.UTF8.GetBytes(address));
        }
        public ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, params object?[] args) => QueryAsync(sql);
        public ValueTask InsertAsync(string collection, object payload, CancellationToken cancellationToken = default) => ValueTask.CompletedTask;
        public ValueTask BulkInsertAsync(string collection, IReadOnlyList<object> rows, CancellationToken cancellationToken = default) => ValueTask.CompletedTask;
        public ValueTask<ReadOnlyMemory<byte>> GetAsync(string collection, string id, CancellationToken cancellationToken = default) => QueryAsync("get");
        public ValueTask DeleteAsync(string collection, string id, CancellationToken cancellationToken = default) => ValueTask.CompletedTask;
        public ValueTask PingAsync(CancellationToken cancellationToken = default) => ValueTask.CompletedTask;
        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    [Fact]
    public async Task RoutesReadsPinsTransactionsAndDoesNotReplayWrites()
    {
        string primary = "https://primary:5000", replica = "https://replica:5000", active = primary;
        var calls = new List<(string Address, string Sql)>();
        JsonObject Membership() => new() { ["version"] = 1, ["primary"] = new JsonObject { ["addr"] = active },
            ["replicas"] = new JsonArray(new JsonObject { ["addr"] = replica, ["healthy"] = true, ["lag_ms"] = 10u }) };
        ValueTask<IConn> Dial(string address, CancellationToken cancellationToken) => ValueTask.FromResult<IConn>(new Peer(address, Membership, calls));
        var client = new RoutedConn(primary, await Dial(primary, default), "https", false, Dial);
        await client.InitializeAsync(default);
        await client.QueryAsync("SELECT 1");
        Assert.Equal(primary, calls[^1].Address);
        await client.QueryWithOptionsAsync("SELECT 1", new(ReadConsistency.Local));
        Assert.Equal(replica, calls[^1].Address);
        await client.QueryWithOptionsAsync("SELECT 1", new(ReadConsistency.Strong));
        Assert.Equal(primary, calls[^1].Address);
        await client.QueryAsync("/* begin */ BEGIN");
        active = "https://promoted:5000";
        await client.InitializeAsync(default);
        await client.QueryWithOptionsAsync("SELECT 1", new(ReadConsistency.Local));
        Assert.Equal(primary, calls[^1].Address);
        await client.QueryAsync("ROLLBACK TO SAVEPOINT nested");
        await client.QueryAsync("ROLLBACK");
        await client.InitializeAsync(default);
        await client.QueryAsync("UPDATE users SET id = 1");
        Assert.Equal(active, calls[^1].Address);
        await Assert.ThrowsAsync<IOException>(async () => await client.QueryAsync("UPDATE lost"));
        Assert.Single(calls.Where(c => c.Sql == "UPDATE lost"));
        int before = calls.Count;
        await Assert.ThrowsAsync<RedDBException.ProtocolError>(async () => await client.QueryWithOptionsAsync("SELECT \"effectful\"()", new(ReadConsistency.Local)));
        Assert.Equal(before + 1, calls.Count);
        Assert.Equal(active, calls[^1].Address);
        await client.DisposeAsync();
        await Assert.ThrowsAsync<IOException>(async () => await client.QueryAsync("SELECT 1"));
    }
}
