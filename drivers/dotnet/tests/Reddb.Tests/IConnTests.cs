using System;
using System.Collections.Generic;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Reddb;
using Xunit;

namespace Reddb.Tests;

public class IConnTests
{
    [Fact]
    public async Task GenericQueryDeserializesJsonAndPassesParams()
    {
        var conn = new RecordingConn("""{"Ok":true,"Value":42}""");
        IConn db = conn;

        QueryEnvelope? result = await db.QueryAsync<QueryEnvelope>("SELECT $1", 42);

        Assert.NotNull(result);
        Assert.True(result!.Ok);
        Assert.Equal(42, result.Value);
        Assert.Equal("SELECT $1", conn.Sql);
        Assert.Equal(new object?[] { 42 }, conn.Args);
    }

    private sealed record QueryEnvelope(bool Ok, int Value);

    private sealed class RecordingConn : IConn
    {
        private readonly ReadOnlyMemory<byte> rows;

        public RecordingConn(string json)
        {
            rows = Encoding.UTF8.GetBytes(json);
        }

        public string? Sql { get; private set; }
        public object?[] Args { get; private set; } = Array.Empty<object?>();

        public ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, CancellationToken cancellationToken = default)
        {
            Sql = sql;
            Args = Array.Empty<object?>();
            return ValueTask.FromResult(rows);
        }

        public ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, params object?[] args)
        {
            Sql = sql;
            Args = args;
            return ValueTask.FromResult(rows);
        }

        public ValueTask InsertAsync(string collection, object payload, CancellationToken cancellationToken = default) =>
            ValueTask.CompletedTask;

        public ValueTask BulkInsertAsync(string collection, IReadOnlyList<object> rows, CancellationToken cancellationToken = default) =>
            ValueTask.CompletedTask;

        public ValueTask<ReadOnlyMemory<byte>> GetAsync(string collection, string id, CancellationToken cancellationToken = default) =>
            ValueTask.FromResult(ReadOnlyMemory<byte>.Empty);

        public ValueTask DeleteAsync(string collection, string id, CancellationToken cancellationToken = default) =>
            ValueTask.CompletedTask;

        public ValueTask PingAsync(CancellationToken cancellationToken = default) =>
            ValueTask.CompletedTask;

        public ValueTask DisposeAsync() =>
            ValueTask.CompletedTask;
    }
}
