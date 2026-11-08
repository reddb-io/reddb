using System;
using System.Collections.Generic;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace Reddb.Embedded;

internal sealed class EmbeddedConn : IConn
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = null,
    };

    private readonly NativeAbi _abi;
    private readonly SemaphoreSlim _lock = new(1, 1);
    private IntPtr _handle;

    private EmbeddedConn(NativeAbi abi, IntPtr handle)
    {
        _abi = abi;
        _handle = handle;
    }

    internal static EmbeddedConn Open(RedUrl url)
    {
        if (!url.IsEmbedded) throw new ArgumentException("embedded URL required", nameof(url));
        NativeAbi abi = NativeAbi.Load();
        string? path = url.Scheme == RedUrl.Kind.EmbeddedFile ? url.Path : null;
        return new EmbeddedConn(abi, abi.Open(path));
    }

    public ValueTask<ReadOnlyMemory<byte>> QueryAsync(
        string sql,
        CancellationToken cancellationToken = default)
        => QueryCoreAsync(sql, Array.Empty<object?>(), cancellationToken);

    public ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, params object?[] args)
        => QueryCoreAsync(sql, args, CancellationToken.None);

    private ValueTask<ReadOnlyMemory<byte>> QueryCoreAsync(
        string sql,
        object?[] args,
        CancellationToken cancellationToken)
    {
        var parameters = new Dictionary<string, object?> { ["sql"] = sql };
        if (args.Length > 0) parameters["params"] = args;
        return InvokeAsync("query", parameters, cancellationToken);
    }

    public async ValueTask InsertAsync(
        string collection,
        object payload,
        CancellationToken cancellationToken = default)
    {
        _ = await InvokeAsync(
            "insert",
            new Dictionary<string, object?>
            {
                ["collection"] = collection,
                ["payload"] = payload,
            },
            cancellationToken).ConfigureAwait(false);
    }

    public async ValueTask BulkInsertAsync(
        string collection,
        IReadOnlyList<object> rows,
        CancellationToken cancellationToken = default)
    {
        _ = await InvokeAsync(
            "bulk_insert",
            new Dictionary<string, object?>
            {
                ["collection"] = collection,
                ["payloads"] = rows,
            },
            cancellationToken).ConfigureAwait(false);
    }

    public ValueTask<ReadOnlyMemory<byte>> GetAsync(
        string collection,
        string id,
        CancellationToken cancellationToken = default)
        => InvokeAsync(
            "get",
            new Dictionary<string, object?> { ["collection"] = collection, ["id"] = id },
            cancellationToken);

    public async ValueTask DeleteAsync(
        string collection,
        string id,
        CancellationToken cancellationToken = default)
    {
        _ = await InvokeAsync(
            "delete",
            new Dictionary<string, object?> { ["collection"] = collection, ["id"] = id },
            cancellationToken).ConfigureAwait(false);
    }

    public async ValueTask PingAsync(CancellationToken cancellationToken = default)
    {
        _ = await InvokeAsync("health", new Dictionary<string, object?>(), cancellationToken)
            .ConfigureAwait(false);
    }

    private async ValueTask<ReadOnlyMemory<byte>> InvokeAsync(
        string method,
        object parameters,
        CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        await _lock.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            EnsureOpen();
            string json = JsonSerializer.Serialize(parameters, JsonOptions);
            return _abi.Invoke(_handle, method, json);
        }
        finally
        {
            _lock.Release();
        }
    }

    public async ValueTask DisposeAsync()
    {
        await _lock.WaitAsync().ConfigureAwait(false);
        try
        {
            if (_handle == IntPtr.Zero) return;
            IntPtr handle = _handle;
            _handle = IntPtr.Zero;
            _abi.Close(handle);
        }
        finally
        {
            _lock.Release();
        }
    }

    private void EnsureOpen()
    {
        if (_handle == IntPtr.Zero)
        {
            throw new ObjectDisposedException(nameof(EmbeddedConn));
        }
    }
}
