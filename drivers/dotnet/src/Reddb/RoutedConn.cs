using System.Net.Http;
using System.Net.Sockets;
using System.Text.Json.Nodes;

namespace Reddb;

internal sealed class RoutedConn : IConn
{
    private readonly Func<string, CancellationToken, ValueTask<IConn>> _dial;
    private readonly Dictionary<string, IConn> _clients = new();
    private readonly SemaphoreSlim _lock = new(1, 1);
    private readonly string _seed, _protocol;
    private readonly bool _forcePrimary;
    private string _primary;
    private List<(string Address, double Lag)> _replicas = new();
    private string? _pinned;
    private bool _session, _broken, _closed;
    private DateTimeOffset _refreshAt;
    private uint _next;

    internal RoutedConn(string seed, IConn initial, string protocol, bool forcePrimary,
        Func<string, CancellationToken, ValueTask<IConn>> dial)
    {
        _seed = _primary = seed; _clients[seed] = initial; _protocol = protocol;
        _forcePrimary = forcePrimary; _dial = dial;
    }

    internal async ValueTask InitializeAsync(CancellationToken cancellationToken) => await RefreshAsync(cancellationToken);

    private async ValueTask<IConn> EndpointAsync(string address, CancellationToken cancellationToken)
    {
        if (_clients.TryGetValue(address, out IConn? existing)) return existing;
        using var bounded = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        bounded.CancelAfter(TimeSpan.FromSeconds(5));
        IConn client = await _dial(address, bounded.Token);
        try { await client.PingAsync(bounded.Token); }
        catch { await client.DisposeAsync(); throw; }
        _clients[address] = client;
        return client;
    }

    private async ValueTask RefreshAsync(CancellationToken cancellationToken)
    {
        if (_pinned != null || _closed) return;
        _refreshAt = DateTimeOffset.UtcNow.AddSeconds(30);
        var candidates = new List<string> { _primary, _seed };
        candidates.AddRange(_replicas.Select(r => r.Address));
        for (int index = 0; index < candidates.Count && index < 258; index++)
        {
            IConn? control = null;
            try
            {
                using var bounded = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
                bounded.CancelAfter(TimeSpan.FromSeconds(5));
                control = await _dial(candidates[index], bounded.Token);
                if (control is not ITopologySource source) continue;
                JsonObject? topology = await source.TopologyAsync(_protocol, bounded.Token);
                if (topology?["version"]?.GetValue<int>() != 1) continue;
                string? primary = TopologyCodec.Address(topology["primary"]?["addr"]?.GetValue<string>(), _protocol);
                if (primary is null) continue;
                if (!candidates.Contains(primary)) candidates.Add(primary);
                await EndpointAsync(primary, cancellationToken);
                var replicas = new List<(string, double)>();
                foreach (JsonNode? replica in (topology["replicas"]?.AsArray() ?? new JsonArray()).Take(256))
                {
                    string? address = TopologyCodec.Address(replica?["addr"]?.GetValue<string>(), _protocol);
                    if (address is null || address == primary || replica?["healthy"]?.GetValue<bool>() != true
                        || replica["rebootstrapping"]?.GetValue<bool>() == true) continue;
                    try
                    {
                        await EndpointAsync(address, cancellationToken);
                        replicas.Add((address, replica["lag_ms"] is JsonNode lag ? lag.GetValue<uint>() : double.PositiveInfinity));
                    }
                    catch (Exception error) when (Network(error) && !cancellationToken.IsCancellationRequested) { }
                }
                _primary = primary; _replicas = replicas;
                var keep = new HashSet<string>(replicas.Select(r => r.Item1)) { _seed, primary };
                foreach (string retired in _clients.Keys.Where(a => !keep.Contains(a)).ToArray())
                { await _clients[retired].DisposeAsync(); _clients.Remove(retired); }
                if (primary == candidates[index]) return;
            }
            catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { throw; }
            catch (Exception error) when (error is not RedDBException.AuthRefused) { }
            finally { if (control != null) await control.DisposeAsync(); }
        }
    }

    private static bool Network(Exception error) => error is IOException or SocketException or HttpRequestException or OperationCanceledException;

    private async ValueTask<T> InvokeAsync<T>(Func<IConn, ValueTask<T>> operation, string? sql = null,
        bool read = false, ReadOptions? options = null, CancellationToken cancellationToken = default)
    {
        await _lock.WaitAsync(cancellationToken);
        try
        {
            if (_closed || _broken) throw new IOException("connection is closed");
            if (_pinned == null && DateTimeOffset.UtcNow >= _refreshAt) await RefreshAsync(cancellationToken);
            if (options != null && (!Enum.IsDefined(options.Consistency) || options.MaxLag < TimeSpan.Zero))
                throw new NotSupportedException("CONSISTENCY_UNSUPPORTED");
            var policy = sql is null ? ("primary", false) : RoutingSql.Policy(sql);
            read |= options != null && policy.Item2;
            var eligible = _forcePrimary || options?.Consistency == ReadConsistency.Strong
                ? new List<(string Address, double Lag)>()
                : _replicas.Where(r => options?.Consistency != ReadConsistency.BoundedStaleness || r.Lag <= options.MaxLag.TotalMilliseconds).ToList();
            string address = _pinned ?? (read && eligible.Count > 0 ? eligible[(int)(_next++ % (uint)eligible.Count)].Address : _primary);
            if (policy.Item1 is "begin" or "session") { _pinned = address; _session |= policy.Item1 == "session"; }
            try
            {
                T result = await operation(await EndpointAsync(address, cancellationToken));
                if (policy.Item1 == "end" && !_session) _pinned = null;
                return result;
            }
            catch (Exception error) when (Network(error))
            {
                _refreshAt = default;
                if (_clients.Remove(address, out IConn? failed)) await failed.DisposeAsync();
                if (_pinned != null) _broken = true;
                if (_pinned != null || !read || error is OperationCanceledException) throw;
                await RefreshAsync(cancellationToken);
                return await operation(await EndpointAsync(_primary, cancellationToken));
            }
        }
        finally { _lock.Release(); }
    }

    public ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, CancellationToken cancellationToken = default)
        => InvokeAsync(c => c.QueryAsync(sql, cancellationToken), sql, cancellationToken: cancellationToken);
    public ValueTask<ReadOnlyMemory<byte>> QueryAsync(string sql, params object?[] args)
        => InvokeAsync(c => c.QueryAsync(sql, args), sql);
    public ValueTask<ReadOnlyMemory<byte>> QueryWithOptionsAsync(string sql, ReadOptions options, params object?[] args)
        => InvokeAsync(c => c.QueryAsync(sql, args), sql, options: options);
    public async ValueTask InsertAsync(string collection, object payload, CancellationToken cancellationToken = default)
        => _ = await InvokeAsync(async c => { await c.InsertAsync(collection, payload, cancellationToken); return true; }, cancellationToken: cancellationToken);
    public async ValueTask BulkInsertAsync(string collection, IReadOnlyList<object> rows, CancellationToken cancellationToken = default)
        => _ = await InvokeAsync(async c => { await c.BulkInsertAsync(collection, rows, cancellationToken); return true; }, cancellationToken: cancellationToken);
    public ValueTask<ReadOnlyMemory<byte>> GetAsync(string collection, string id, CancellationToken cancellationToken = default)
        => InvokeAsync(c => c.GetAsync(collection, id, cancellationToken), read: true, cancellationToken: cancellationToken);
    public async ValueTask DeleteAsync(string collection, string id, CancellationToken cancellationToken = default)
        => _ = await InvokeAsync(async c => { await c.DeleteAsync(collection, id, cancellationToken); return true; }, cancellationToken: cancellationToken);
    public async ValueTask PingAsync(CancellationToken cancellationToken = default)
        => _ = await InvokeAsync(async c => { await c.PingAsync(cancellationToken); return true; }, cancellationToken: cancellationToken);
    public async ValueTask DisposeAsync()
    {
        await _lock.WaitAsync();
        try { _closed = true; foreach (IConn client in _clients.Values) await client.DisposeAsync(); _clients.Clear(); }
        finally { _lock.Release(); }
    }
}
