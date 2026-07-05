using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace Reddb.Helpers;

/// <summary>
/// Minimal contract helpers need. Tests pass fakes that record SQL.
/// IConn instances are adapted via <see cref="Helpers.For(IConn)"/>.
/// </summary>
public interface IQuerier
{
    ValueTask<ReadOnlyMemory<byte>> QueryAsync(
        string sql, object?[] args, CancellationToken cancellationToken = default);
}

/// <summary>Typed helper errors mirroring Go/Java/Python.</summary>
public class HelperException : Exception
{
    public HelperException(string message) : base(message) { }

    public sealed class InvalidArgument : HelperException
    {
        public InvalidArgument(string message) : base(message) { }
    }
    public sealed class NotFound : HelperException
    {
        public NotFound(string message) : base(message) { }
    }
    public sealed class InvalidResponse : HelperException
    {
        public InvalidResponse(string message) : base(message) { }
    }
}

public sealed record InsertResult(long Affected, string Rid, IReadOnlyDictionary<string, object?>? Item);

/// <summary>
/// Spec envelope for delete helpers (SDK Helper Spec v1.0 §4.5 / §5.4).
/// <see cref="Deleted"/> reports whether anything was actually removed
/// (<see cref="Affected"/> &gt; 0). A delete of a missing item returns
/// <c>{ Affected = 0, Deleted = false }</c> rather than NOT_FOUND.
/// </summary>
public sealed record DeleteResult(long Affected, bool Deleted)
{
    public DeleteResult(long Affected) : this(Affected, Affected > 0) { }
}

public sealed record ExistsResult(bool Exists);
public sealed record ListResult(IReadOnlyList<IReadOnlyDictionary<string, object?>> Items, string? NextCursor = null);
public sealed record QueuePushResult(long Affected, string? Rid);

/// <summary>
/// Groups the rich namespaces (<see cref="Documents"/>, <see cref="Kv"/>,
/// <see cref="Queue"/>, <see cref="Tx"/>) bound to a single transport.
/// Stateless — safe to construct per call. Mirrors
/// <c>drivers/go/helpers.go</c>.
/// </summary>
public sealed class Helpers
{
    /// <summary>
    /// SDK Helper Spec revision this driver satisfies. Cross-driver CI
    /// dashboards assert against this constant per spec §14.
    /// </summary>
    public const string HelperSpecVersion = "1.0";

    private readonly IQuerier _q;

    public Helpers(IQuerier q) { _q = q; }

    /// <summary>Wrap an <see cref="IConn"/> with the helper surface.</summary>
    public static Helpers For(IConn conn) => new(new ConnQuerier(conn));

    public DocumentClient Documents() => new(_q);
    public KvClient Kv(string collection = "kv_default") => new(_q, collection);
    public QueueClient Queue() => new(_q);

    /// <summary>
    /// Alias for <see cref="Queue"/> matching the spec namespace name
    /// (<c>queues.*</c>). Both forms return the same client.
    /// </summary>
    public QueueClient Queues() => Queue();

    /// <summary>
    /// Transaction namespace client implementing <c>tx.begin</c>,
    /// <c>tx.commit</c>, <c>tx.rollback</c> (SDK Helper Spec §7).
    /// </summary>
    public TxClient Tx() => new(_q);

    private sealed class ConnQuerier : IQuerier
    {
        private readonly IConn _c;
        public ConnQuerier(IConn c) { _c = c; }
        public ValueTask<ReadOnlyMemory<byte>> QueryAsync(
            string sql, object?[] args, CancellationToken ct = default)
            => args is null || args.Length == 0
                ? _c.QueryAsync(sql, ct)
                : _c.QueryAsync(sql, args);
    }
}

public sealed class DocumentClient
{
    private readonly IQuerier _q;
    internal DocumentClient(IQuerier q) { _q = q; }

    public sealed class ListOptions
    {
        public int Limit { get; set; }
        public string? OrderBy { get; set; }
        public string? Filter { get; set; }
    }

    public async ValueTask<InsertResult> InsertAsync(
        string collection, IDictionary<string, object?> document,
        CancellationToken ct = default)
    {
        if (document is null)
            throw new HelperException.InvalidArgument("documents.insert document must be an object");
        await EnsureCollectionAsync(collection, ct).ConfigureAwait(false);
        string sql = $"INSERT INTO {Sql.IdentifierPath(collection)} DOCUMENT (body) VALUES ({Sql.JsonLiteral(document)}) RETURNING *";
        var body = await _q.QueryAsync(sql, Array.Empty<object?>(), ct).ConfigureAwait(false);
        var (row, affected) = Sql.FirstRow(body.Span);
        if (row is null || !row.TryGetValue("rid", out var rid) || rid is null)
            throw new HelperException.InvalidResponse("documents.insert expected one returned item with rid");
        if (affected == 0L) affected = 1L;
        return new InsertResult(affected, Sql.RidString(rid)!, row);
    }

    public async ValueTask<IReadOnlyDictionary<string, object?>> GetAsync(
        string collection, string rid, CancellationToken ct = default)
    {
        string sql = $"SELECT * FROM {Sql.IdentifierPath(collection)} WHERE rid = $1 LIMIT 1";
        var body = await _q.QueryAsync(sql, new object?[] { rid }, ct).ConfigureAwait(false);
        var (row, _) = Sql.FirstRow(body.Span);
        if (row is null) throw new HelperException.NotFound($"document \"{rid}\" was not found");
        return row;
    }

    public async ValueTask<ListResult> ListAsync(
        string collection, ListOptions? opts = null, CancellationToken ct = default)
    {
        opts ??= new ListOptions();
        int limit = Sql.NormalizeLimit(opts.Limit);
        string order = string.IsNullOrEmpty(opts.OrderBy) ? "rid ASC" : opts.OrderBy!;
        string where = string.IsNullOrEmpty(opts.Filter) ? "" : " WHERE " + opts.Filter;
        string sql = $"SELECT * FROM {Sql.IdentifierPath(collection)}{where} ORDER BY {order} LIMIT {limit}";
        var body = await _q.QueryAsync(sql, Array.Empty<object?>(), ct).ConfigureAwait(false);
        return new ListResult(Sql.AllRows(body.Span));
    }

    public async ValueTask<IReadOnlyDictionary<string, object?>> PatchAsync(
        string collection, string rid, IDictionary<string, object?> patch,
        CancellationToken ct = default)
    {
        if (patch is null)
            throw new HelperException.InvalidArgument("documents.patch patch must be an object");
        if (patch.Count == 0)
            throw new HelperException.InvalidArgument("documents.patch patch must be a non-empty object");
        var parts = new List<string>(patch.Count);
        foreach (var kv in patch)
        {
            if (kv.Key.Contains('/'))
                throw new HelperException.InvalidArgument(
                    "documents.patch currently accepts top-level document fields");
            parts.Add($"{Sql.Identifier(kv.Key)} = {Sql.ValueLiteral(kv.Value)}");
        }
        string sql = $"UPDATE {Sql.IdentifierPath(collection)} SET {string.Join(", ", parts)} WHERE rid = $1 RETURNING *";
        var body = await _q.QueryAsync(sql, new object?[] { rid }, ct).ConfigureAwait(false);
        var (row, _) = Sql.FirstRow(body.Span);
        if (row is null) throw new HelperException.NotFound($"document \"{rid}\" was not found");
        return row;
    }

    public async ValueTask<DeleteResult> DeleteAsync(
        string collection, string rid, CancellationToken ct = default)
    {
        string sql = $"DELETE FROM {Sql.IdentifierPath(collection)} WHERE rid = $1";
        var body = await _q.QueryAsync(sql, new object?[] { rid }, ct).ConfigureAwait(false);
        return new DeleteResult(Sql.AffectedFromBody(body.Span));
    }

    private async ValueTask EnsureCollectionAsync(string collection, CancellationToken ct)
    {
        try
        {
            await _q.QueryAsync($"CREATE DOCUMENT {Sql.IdentifierPath(collection)}",
                Array.Empty<object?>(), ct).ConfigureAwait(false);
        }
        catch (Exception e) when (e.Message?.Contains("already exists") == true)
        {
            // already exists is fine
        }
    }
}

public sealed class KvClient
{
    private readonly IQuerier _q;
    public string Collection { get; }

    internal KvClient(IQuerier q, string collection) { _q = q; Collection = collection; }

    public sealed class SetOptions
    {
        public string? Collection { get; set; }
        public IReadOnlyList<string>? Tags { get; set; }
        public long ExpireMs { get; set; }
    }

    public sealed class ListOpts
    {
        public string? Collection { get; set; }
        public int Limit { get; set; }
        public string? Prefix { get; set; }
    }

    public ValueTask SetAsync(string key, object? value, SetOptions? opts = null, CancellationToken ct = default)
        => PutAsync(key, value, opts, ct);

    public async ValueTask PutAsync(string key, object? value, SetOptions? opts = null, CancellationToken ct = default)
    {
        opts ??= new SetOptions();
        string coll = string.IsNullOrEmpty(opts.Collection) ? Collection : opts.Collection!;
        string lit = Sql.KvValueLiteral(value);
        string expire = opts.ExpireMs > 0 ? $" EXPIRE {opts.ExpireMs} ms" : "";
        string tagClause = "";
        if (opts.Tags is { Count: > 0 } tags)
            tagClause = " TAGS [" + string.Join(", ", tags.Select(Sql.KvTagLiteral)) + "]";
        string path = Sql.KvPath(coll, key);
        await _q.QueryAsync($"KV PUT {path} = {lit}{expire}{tagClause}", Array.Empty<object?>(), ct)
            .ConfigureAwait(false);
    }

    public async ValueTask<object?> GetAsync(string key, string? collection = null, CancellationToken ct = default)
    {
        string coll = string.IsNullOrEmpty(collection) ? Collection : collection!;
        string path = Sql.KvPath(coll, key);
        var body = await _q.QueryAsync($"KV GET {path}", Array.Empty<object?>(), ct).ConfigureAwait(false);
        var (row, _) = Sql.FirstRow(body.Span);
        if (row is null) return null;
        row.TryGetValue("value", out var v);
        return v;
    }

    public async ValueTask<ExistsResult> ExistsAsync(string key, string? collection = null, CancellationToken ct = default)
        => new(await GetAsync(key, collection, ct).ConfigureAwait(false) is not null);

    public async ValueTask<DeleteResult> DeleteAsync(string key, string? collection = null, CancellationToken ct = default)
    {
        string coll = string.IsNullOrEmpty(collection) ? Collection : collection!;
        string path = Sql.KvPath(coll, key);
        var body = await _q.QueryAsync($"KV DELETE {path}", Array.Empty<object?>(), ct).ConfigureAwait(false);
        return new DeleteResult(Sql.AffectedFromBody(body.Span));
    }

    public async ValueTask<ListResult> ListAsync(ListOpts? opts = null, CancellationToken ct = default)
    {
        opts ??= new ListOpts();
        string coll = string.IsNullOrEmpty(opts.Collection) ? Collection : opts.Collection!;
        int limit = Sql.NormalizeLimit(opts.Limit);
        string sql = $"SELECT key, value FROM {Sql.Identifier(coll)} ORDER BY key ASC LIMIT {limit}";
        var body = await _q.QueryAsync(sql, Array.Empty<object?>(), ct).ConfigureAwait(false);
        var rows = Sql.AllRows(body.Span);
        if (!string.IsNullOrEmpty(opts.Prefix))
        {
            rows = rows.Where(r => r.TryGetValue("key", out var k) && k is string s && s.StartsWith(opts.Prefix!)).ToList();
        }
        return new ListResult(rows);
    }
}

public sealed class QueueClient
{
    private readonly IQuerier _q;
    internal QueueClient(IQuerier q) { _q = q; }

    public sealed class PushOptions
    {
        public int? Priority { get; set; }
    }

    /// <summary>
    /// Create a queue if it doesn't already exist (spec §6.1, idempotent).
    /// </summary>
    public async ValueTask CreateAsync(string name, CancellationToken ct = default)
    {
        Sql.AssertIdentifier(name, "queue name");
        await _q.QueryAsync($"CREATE QUEUE IF NOT EXISTS {Sql.Identifier(name)}",
            Array.Empty<object?>(), ct).ConfigureAwait(false);
    }

    public async ValueTask<QueuePushResult> PushAsync(
        string queue, object? value, PushOptions? opts = null, CancellationToken ct = default)
    {
        Sql.AssertIdentifier(queue, "queue name");
        opts ??= new PushOptions();
        string lit = Sql.QueueValueLiteral(value);
        string priority = opts.Priority is null ? "" : $" PRIORITY {opts.Priority}";
        string sql = $"QUEUE PUSH {Sql.Identifier(queue)} {lit}{priority}";
        var body = await _q.QueryAsync(sql, Array.Empty<object?>(), ct).ConfigureAwait(false);
        long affected = Sql.AffectedFromBody(body.Span);
        if (affected == 0L) affected = 1L;
        var (row, _) = Sql.FirstRow(body.Span);
        string? rid = row is null ? null : (row.TryGetValue("rid", out var r) ? Sql.RidString(r) : null);
        return new QueuePushResult(affected, rid);
    }

    public ValueTask<IReadOnlyList<object?>> PopAsync(string queue, int? count = null, CancellationToken ct = default)
        => FetchAsync("POP", queue, count, ct);

    public ValueTask<IReadOnlyList<object?>> PeekAsync(string queue, int? count = null, CancellationToken ct = default)
        => FetchAsync("PEEK", queue, count, ct);

    private async ValueTask<IReadOnlyList<object?>> FetchAsync(string verb, string queue, int? count, CancellationToken ct)
    {
        Sql.AssertIdentifier(queue, "queue name");
        string suffix = "";
        if (count is not null)
        {
            if (count < 0)
                throw new HelperException.InvalidArgument("queue count must be a non-negative integer");
            suffix = $" COUNT {count}";
        }
        var body = await _q.QueryAsync($"QUEUE {verb} {Sql.Identifier(queue)}{suffix}",
            Array.Empty<object?>(), ct).ConfigureAwait(false);
        var rows = Sql.AllRows(body.Span);
        var out_ = new List<object?>(rows.Count);
        foreach (var row in rows)
        {
            row.TryGetValue("payload", out var p);
            out_.Add(p);
        }
        return out_;
    }

    public async ValueTask<long> LenAsync(string queue, CancellationToken ct = default)
    {
        Sql.AssertIdentifier(queue, "queue name");
        var body = await _q.QueryAsync($"QUEUE LEN {Sql.Identifier(queue)}",
            Array.Empty<object?>(), ct).ConfigureAwait(false);
        var (row, _) = Sql.FirstRow(body.Span);
        if (row is null) return 0L;
        if (!row.TryGetValue("len", out var v) || v is null) return 0L;
        return v switch
        {
            long l => l,
            int i => i,
            double d => (long)d,
            float f => (long)f,
            decimal m => (long)m,
            JsonElement je => je.ValueKind == JsonValueKind.Number ? je.GetInt64() : 0L,
            _ => 0L,
        };
    }

    public async ValueTask<DeleteResult> PurgeAsync(string queue, CancellationToken ct = default)
    {
        Sql.AssertIdentifier(queue, "queue name");
        var body = await _q.QueryAsync($"QUEUE PURGE {Sql.Identifier(queue)}",
            Array.Empty<object?>(), ct).ConfigureAwait(false);
        return new DeleteResult(Sql.AffectedFromBody(body.Span));
    }

    /// <summary>
    /// Options for <see cref="ReadWaitAsync"/>.
    /// </summary>
    public sealed class ReadWaitOptions
    {
        /// <summary>Optional consumer group name.</summary>
        public string? Group { get; set; }
        /// <summary>Optional max messages to deliver. Server default = 1.</summary>
        public int? Count { get; set; }
    }

    /// <summary>
    /// Live <c>QUEUE READ … WAIT &lt;ms&gt;</c> helper (PRD #718 / #725).
    /// Blocks until a message is available for <paramref name="consumer"/>
    /// on <paramref name="queue"/>, the wait budget elapses, or the
    /// server cancels. Timeout returns an empty list — same shape as
    /// an empty <see cref="PopAsync"/>; never throws. <paramref name="wait"/>
    /// must be specified — there is no infinite-wait default.
    /// Cancellation and cap rejection surface as transport exceptions.
    /// </summary>
    public async ValueTask<IReadOnlyList<object?>> ReadWaitAsync(
        string queue,
        string consumer,
        TimeSpan wait,
        ReadWaitOptions? opts = null,
        CancellationToken ct = default)
    {
        Sql.AssertIdentifier(queue, "queue name");
        Sql.AssertIdentifier(consumer, "consumer name");
        if (wait < TimeSpan.Zero)
        {
            throw new HelperException.InvalidArgument(
                "queue ReadWait requires a non-negative wait duration (no infinite wait)");
        }
        opts ??= new ReadWaitOptions();
        string groupClause = "";
        if (!string.IsNullOrEmpty(opts.Group))
        {
            Sql.AssertIdentifier(opts.Group!, "group name");
            groupClause = $" GROUP {Sql.Identifier(opts.Group!)}";
        }
        string countClause = "";
        if (opts.Count is not null)
        {
            if (opts.Count < 0)
                throw new HelperException.InvalidArgument(
                    "queue count must be a non-negative integer");
            countClause = $" COUNT {opts.Count}";
        }
        long waitMs = (long)wait.TotalMilliseconds;
        string sql = $"QUEUE READ {Sql.Identifier(queue)}{groupClause} CONSUMER {Sql.Identifier(consumer)}{countClause} WAIT {waitMs}ms";
        var body = await _q.QueryAsync(sql, Array.Empty<object?>(), ct).ConfigureAwait(false);
        var rows = Sql.AllRows(body.Span);
        var out_ = new List<object?>(rows.Count);
        foreach (var row in rows)
        {
            row.TryGetValue("payload", out var p);
            out_.Add(p);
        }
        return out_;
    }
}

/// <summary>
/// Transaction namespace (SDK Helper Spec §7). Imperative
/// <see cref="BeginAsync"/> / <see cref="CommitAsync"/> /
/// <see cref="RollbackAsync"/> plus an optional callback form
/// <see cref="RunAsync"/>. Nested <see cref="RunAsync"/> rejects with
/// INVALID_ARGUMENT — callers needing savepoints should issue them
/// directly via <c>conn.QueryAsync</c>.
/// </summary>
public sealed class TxClient
{
    private readonly IQuerier _q;
    private bool _inRun;

    internal TxClient(IQuerier q) { _q = q; }

    public async ValueTask BeginAsync(CancellationToken ct = default)
        => await _q.QueryAsync("BEGIN", Array.Empty<object?>(), ct).ConfigureAwait(false);

    public async ValueTask CommitAsync(CancellationToken ct = default)
        => await _q.QueryAsync("COMMIT", Array.Empty<object?>(), ct).ConfigureAwait(false);

    public async ValueTask RollbackAsync(CancellationToken ct = default)
        => await _q.QueryAsync("ROLLBACK", Array.Empty<object?>(), ct).ConfigureAwait(false);

    /// <summary>
    /// Callback form: opens a transaction, runs <paramref name="body"/>,
    /// commits on success, rolls back and re-throws on failure. Nested
    /// invocation rejects with INVALID_ARGUMENT (spec §7.2).
    /// </summary>
    public async ValueTask RunAsync(Func<TxClient, ValueTask> body, CancellationToken ct = default)
    {
        if (body is null)
            throw new HelperException.InvalidArgument("tx.run requires a callback");
        if (_inRun)
            throw new HelperException.InvalidArgument(
                "tx.run does not support nesting; use raw SAVEPOINT via conn.QueryAsync");
        _inRun = true;
        try
        {
            await BeginAsync(ct).ConfigureAwait(false);
            try
            {
                await body(this).ConfigureAwait(false);
            }
            catch
            {
                try { await RollbackAsync(ct).ConfigureAwait(false); }
                catch { /* preserve original exception */ }
                throw;
            }
            await CommitAsync(ct).ConfigureAwait(false);
        }
        finally
        {
            _inRun = false;
        }
    }
}

internal static class Sql
{
    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
    };

    public static string KvPath(string collection, string key)
    {
        foreach (char ch in collection)
        {
            if (!IsIdentChar(ch))
                throw new HelperException.InvalidArgument(
                    $"invalid KV collection \"{collection}\": character \"{ch}\" is not supported");
        }
        return collection + "." + KvKeySegment(key);
    }

    public static string KvKeySegment(string value)
        => !string.IsNullOrEmpty(value) && AllIdentChars(value)
            ? value
            : "'" + value.Replace("'", "''") + "'";

    public static string KvValueLiteral(object? value)
    {
        return value switch
        {
            null => "NULL",
            bool b => b ? "true" : "false",
            string s => "'" + s.Replace("'", "''") + "'",
            sbyte or byte or short or ushort or int or uint or long or ulong => value.ToString()!,
            float f => f.ToString(System.Globalization.CultureInfo.InvariantCulture),
            double d => d.ToString(System.Globalization.CultureInfo.InvariantCulture),
            decimal m => m.ToString(System.Globalization.CultureInfo.InvariantCulture),
            _ => "'" + JsonSerializer.Serialize(value, JsonOpts).Replace("'", "''") + "'",
        };
    }

    public static string KvTagLiteral(string tag) => "'" + tag.Replace("'", "''") + "'";

    public static string QueueValueLiteral(object? value)
    {
        return value switch
        {
            null => "NULL",
            bool b => b ? "true" : "false",
            string s => "'" + s.Replace("'", "''") + "'",
            sbyte or byte or short or ushort or int or uint or long or ulong => value.ToString()!,
            float f => f.ToString(System.Globalization.CultureInfo.InvariantCulture),
            double d => d.ToString(System.Globalization.CultureInfo.InvariantCulture),
            decimal m => m.ToString(System.Globalization.CultureInfo.InvariantCulture),
            _ => JsonSerializer.Serialize(value, JsonOpts),
        };
    }

    public static string ValueLiteral(object? value) => KvValueLiteral(value);

    public static string JsonLiteral(object? value)
        => "'" + JsonSerializer.Serialize(value, JsonOpts).Replace("'", "''") + "'";

    public static string Identifier(string value)
        => !string.IsNullOrEmpty(value) && AllIdentChars(value)
            ? value
            : "\"" + value.Replace("\"", "\"\"") + "\"";

    public static string IdentifierPath(string value)
        => !value.Contains('.') ? Identifier(value)
            : string.Join('.', value.Split('.').Select(Identifier));

    public static void AssertIdentifier(string value, string label)
    {
        if (string.IsNullOrEmpty(value) || !AllIdentChars(value))
            throw new HelperException.InvalidArgument(
                $"invalid {label} \"{value}\": must match [A-Za-z0-9_]+");
    }

    public static int NormalizeLimit(int value)
    {
        if (value == 0) return 100;
        if (value < 0) throw new HelperException.InvalidArgument("limit must be a positive integer");
        return value;
    }

    public static bool IsIdentChar(char c)
        => (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
            || (c >= '0' && c <= '9') || c == '_';

    public static bool AllIdentChars(string s)
    {
        foreach (char c in s) if (!IsIdentChar(c)) return false;
        return true;
    }

    // --- response parsing ----------------------------------------------

    public static Dictionary<string, object?>? DecodeBody(ReadOnlySpan<byte> body)
    {
        if (body.IsEmpty) return null;
        try
        {
            using var doc = JsonDocument.Parse(body.ToArray());
            if (doc.RootElement.ValueKind != JsonValueKind.Object) return null;
            return JsonElementToDict(doc.RootElement);
        }
        catch
        {
            return null;
        }
    }

    private static Dictionary<string, object?> JsonElementToDict(JsonElement el)
    {
        var dict = new Dictionary<string, object?>();
        foreach (var prop in el.EnumerateObject())
            dict[prop.Name] = JsonElementToObject(prop.Value);
        return dict;
    }

    private static object? JsonElementToObject(JsonElement el) => el.ValueKind switch
    {
        JsonValueKind.Null => null,
        JsonValueKind.True => true,
        JsonValueKind.False => false,
        JsonValueKind.String => el.GetString(),
        JsonValueKind.Number => el.TryGetInt64(out long l) ? l : el.GetDouble(),
        JsonValueKind.Array => el.EnumerateArray().Select(JsonElementToObject).ToList(),
        JsonValueKind.Object => JsonElementToDict(el),
        _ => null,
    };

    public static long AffectedFromMap(IReadOnlyDictionary<string, object?> obj)
    {
        if (!obj.TryGetValue("affected", out var v) || v is null) return 0L;
        return v switch
        {
            long l => l,
            int i => i,
            double d => (long)d,
            float f => (long)f,
            decimal m => (long)m,
            _ => 0L,
        };
    }

    public static (IReadOnlyDictionary<string, object?>? row, long affected) FirstRow(ReadOnlySpan<byte> body)
    {
        var obj = DecodeBody(body);
        if (obj is null) return (null, 0L);
        long affected = AffectedFromMap(obj);
        List<object?>? rows = obj.TryGetValue("rows", out var raw) ? raw as List<object?> : null;
        if (rows is null || rows.Count == 0)
        {
            if (obj.TryGetValue("result", out var nested) && nested is Dictionary<string, object?> nm)
            {
                rows = nm.TryGetValue("rows", out var nr) ? nr as List<object?> : null;
                if (affected == 0L) affected = AffectedFromMap(nm);
            }
        }
        if (rows is null || rows.Count == 0) return (null, affected);
        if (rows[0] is Dictionary<string, object?> first) return (first, affected);
        return (null, affected);
    }

    public static IReadOnlyList<IReadOnlyDictionary<string, object?>> AllRows(ReadOnlySpan<byte> body)
    {
        var obj = DecodeBody(body);
        if (obj is null) return Array.Empty<IReadOnlyDictionary<string, object?>>();
        List<object?>? raw = obj.TryGetValue("rows", out var r) ? r as List<object?> : null;
        if (raw is null)
        {
            if (obj.TryGetValue("result", out var nested) && nested is Dictionary<string, object?> nm)
                raw = nm.TryGetValue("rows", out var nr) ? nr as List<object?> : null;
        }
        if (raw is null) return Array.Empty<IReadOnlyDictionary<string, object?>>();
        var rows = new List<IReadOnlyDictionary<string, object?>>(raw.Count);
        foreach (var item in raw)
            if (item is Dictionary<string, object?> m) rows.Add(m);
        return rows;
    }

    public static long AffectedFromBody(ReadOnlySpan<byte> body)
    {
        var obj = DecodeBody(body);
        if (obj is null) return 0L;
        long direct = AffectedFromMap(obj);
        if (direct > 0L) return direct;
        if (obj.TryGetValue("result", out var nested) && nested is Dictionary<string, object?> nm)
            return AffectedFromMap(nm);
        return 0L;
    }

    public static string? RidString(object? value) => value switch
    {
        null => null,
        string s => s,
        long l => l.ToString(System.Globalization.CultureInfo.InvariantCulture),
        int i => i.ToString(System.Globalization.CultureInfo.InvariantCulture),
        double d => d.ToString(System.Globalization.CultureInfo.InvariantCulture),
        _ => null,
    };
}
