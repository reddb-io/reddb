using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;

namespace Reddb;

/// <summary>
/// Connection-shaped surface every transport (RedWire, HTTP) implements.
/// All methods are async; raw responses are returned as
/// <see cref="ReadOnlyMemory{T}"/> so callers can deserialise with
/// <c>System.Text.Json</c> (or anything else) without an extra copy.
/// </summary>
public interface IConn : IAsyncDisposable
{
    /// <summary>Run a SQL query. Returns the engine's JSON envelope as bytes.</summary>
    ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, CancellationToken cancellationToken = default);

    /// <summary>Insert a single row into a collection. <paramref name="payload"/> is anything <c>System.Text.Json</c> can serialise.</summary>
    ValueTask InsertAsync(string collection, object payload, CancellationToken cancellationToken = default);

    /// <summary>Insert many rows in one round trip. Each row is anything <c>System.Text.Json</c> can serialise.</summary>
    ValueTask BulkInsertAsync(string collection, IReadOnlyList<object> rows, CancellationToken cancellationToken = default);

    /// <summary>Fetch one row by id. Returns the JSON envelope (<c>{ ok, found, ... }</c>) as bytes.</summary>
    ValueTask<ReadOnlyMemory<byte>> GetAsync(string collection, string id, CancellationToken cancellationToken = default);

    /// <summary>Delete one row by id.</summary>
    ValueTask DeleteAsync(string collection, string id, CancellationToken cancellationToken = default);

    /// <summary>Round-trip a Ping → Pong (RedWire) or GET /admin/health (HTTP). Throws on protocol errors.</summary>
    ValueTask PingAsync(CancellationToken cancellationToken = default);
}
