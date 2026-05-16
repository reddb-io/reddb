using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Reddb.Helpers;
using Xunit;

namespace Reddb.Tests;

/// <summary>
/// Conformance tests mirroring drivers/go/helpers_test.go. Driven through
/// the public helper API against a <see cref="FakeQuerier"/> that records
/// SQL and replays scripted JSON replies.
/// </summary>
public class HelpersTests
{
    private sealed class FakeQuerier : IQuerier
    {
        public readonly List<string> Sqls = new();
        public readonly List<object?[]> Params = new();
        public readonly Queue<byte[]> Replies = new();
        public readonly Queue<Exception?> Errs = new();

        public ValueTask<ReadOnlyMemory<byte>> QueryAsync(
            string sql, object?[] args, CancellationToken ct = default)
        {
            Sqls.Add(sql);
            Params.Add(args);
            Exception? err = Errs.Count > 0 ? Errs.Dequeue() : null;
            byte[] body = Replies.Count > 0 ? Replies.Dequeue() : Array.Empty<byte>();
            if (err is not null) throw err;
            return new ValueTask<ReadOnlyMemory<byte>>(body);
        }

        public FakeQuerier Reply(object obj)
        {
            Replies.Enqueue(JsonSerializer.SerializeToUtf8Bytes(obj));
            return this;
        }
        public FakeQuerier Err(Exception e) { Errs.Enqueue(e); return this; }
    }

    private static Dictionary<string, object?> M(params (string, object?)[] kvs)
    {
        var m = new Dictionary<string, object?>();
        foreach (var (k, v) in kvs) m[k] = v;
        return m;
    }

    // --- KV path (covered via emitted SQL) --------------------------

    [Fact] public async Task Kv_Set_QuotesNamespacedKeys()
    {
        var fq = new FakeQuerier().Reply(M());
        await new Helpers.Helpers(fq).Kv().SetAsync("corpus:version", "v1");
        Assert.Contains("kv_default.'corpus:version'", fq.Sqls[0]);
    }

    [Fact] public async Task Kv_Set_PreservesDotsAndSlashes()
    {
        var fq = new FakeQuerier().Reply(M());
        await new Helpers.Helpers(fq).Kv().SetAsync("a/b.c", "v");
        Assert.Contains("kv_default.'a/b.c'", fq.Sqls[0]);
    }

    [Fact] public async Task Kv_Set_RejectsBadCollection()
    {
        var opts = new KvClient.SetOptions { Collection = "bad-name!" };
        await Assert.ThrowsAsync<HelperException.InvalidArgument>(
            async () => await new Helpers.Helpers(new FakeQuerier()).Kv().SetAsync("k", "v", opts));
    }

    [Fact] public async Task Kv_Set_EmitsExactKeyPath()
    {
        var fq = new FakeQuerier().Reply(M());
        await new Helpers.Helpers(fq).Kv().SetAsync("characters:hansel", "ok");
        var sql = fq.Sqls[0];
        Assert.Contains("kv_default.'characters:hansel'", sql);
        Assert.Contains("= 'ok'", sql);
    }

    [Fact] public async Task Kv_Set_EscapesQuotedValues()
    {
        var fq = new FakeQuerier().Reply(M());
        await new Helpers.Helpers(fq).Kv().SetAsync("k", "o'reilly");
        Assert.Contains("= 'o''reilly'", fq.Sqls[0]);
    }

    [Fact] public async Task Kv_Set_SerialisesObjects()
    {
        var fq = new FakeQuerier().Reply(M());
        await new Helpers.Helpers(fq).Kv().SetAsync("k", M(("a", 1)));
        Assert.Contains("= '{\"a\":1}'", fq.Sqls[0]);
    }

    [Fact] public async Task Kv_Get_ReturnsValueOrNull()
    {
        var fq = new FakeQuerier()
            .Reply(M(("rows", new[] { M(("value", "v")) })))
            .Reply(M(("rows", Array.Empty<object>())));
        var kv = new Helpers.Helpers(fq).Kv();
        Assert.Equal("v", await kv.GetAsync("k"));
        Assert.Null(await kv.GetAsync("k2"));
    }

    [Fact] public async Task Kv_Exists_UsesGet()
    {
        var fq = new FakeQuerier()
            .Reply(M(("rows", new[] { M(("value", "v")) })))
            .Reply(M(("rows", Array.Empty<object>())));
        var kv = new Helpers.Helpers(fq).Kv();
        Assert.True((await kv.ExistsAsync("k")).Exists);
        Assert.False((await kv.ExistsAsync("k2")).Exists);
    }

    [Fact] public async Task Kv_List_FiltersByPrefixWithoutRewriting()
    {
        var fq = new FakeQuerier().Reply(M(("rows", new[] {
            M(("key", "a:1"), ("value", 1)),
            M(("key", "b:1"), ("value", 2)),
            M(("key", "a:2"), ("value", 3)),
        })));
        var opts = new KvClient.ListOpts { Prefix = "a:" };
        var res = await new Helpers.Helpers(fq).Kv().ListAsync(opts);
        Assert.Equal(2, res.Items.Count);
        Assert.Equal("a:1", res.Items[0]["key"]);
        Assert.Equal("a:2", res.Items[1]["key"]);
    }

    [Fact] public async Task Kv_List_RejectsNegativeLimit()
    {
        var opts = new KvClient.ListOpts { Limit = -1 };
        await Assert.ThrowsAsync<HelperException.InvalidArgument>(
            async () => await new Helpers.Helpers(new FakeQuerier()).Kv().ListAsync(opts));
    }

    // --- Queue -------------------------------------------------------

    [Fact] public async Task Queue_Push_EmitsPriorityAndPayload()
    {
        var fq = new FakeQuerier().Reply(M(("affected", 1)));
        var opts = new QueueClient.PushOptions { Priority = 5 };
        await new Helpers.Helpers(fq).Queue().PushAsync("jobs", M(("id", 1)), opts);
        var sql = fq.Sqls[0];
        Assert.StartsWith("QUEUE PUSH jobs ", sql);
        Assert.Contains("PRIORITY 5", sql);
        Assert.Contains("{\"id\":1}", sql);
    }

    [Fact] public async Task Queue_Len_ReturnsInt()
    {
        var fq = new FakeQuerier().Reply(M(("rows", new[] { M(("len", 3)) })));
        Assert.Equal(3L, await new Helpers.Helpers(fq).Queue().LenAsync("jobs"));
    }

    [Fact] public async Task Queue_Pop_ReturnsPayloads()
    {
        var fq = new FakeQuerier().Reply(M(("rows", new[] {
            M(("payload", "a")), M(("payload", "b")) })));
        var out_ = await new Helpers.Helpers(fq).Queue().PopAsync("jobs", 2);
        Assert.Equal(new object?[] { "a", "b" }, out_.ToArray());
    }

    [Fact] public async Task Queue_Pop_RejectsNegativeCount()
    {
        await Assert.ThrowsAsync<HelperException.InvalidArgument>(
            async () => await new Helpers.Helpers(new FakeQuerier()).Queue().PopAsync("jobs", -1));
    }

    [Fact] public async Task Queue_Push_RejectsInvalidIdentifier()
    {
        await Assert.ThrowsAsync<HelperException.InvalidArgument>(
            async () => await new Helpers.Helpers(new FakeQuerier()).Queue().PushAsync("bad-name!", "x"));
    }

    // --- Documents ---------------------------------------------------

    [Fact] public async Task Documents_Insert_ReturnsRidEnvelope()
    {
        var fq = new FakeQuerier()
            .Reply(M(("rows", Array.Empty<object>()), ("affected", 0)))
            .Reply(M(("rows", new[] { M(("rid", "doc-1"), ("body", M(("name", "alice")))) }),
                     ("affected", 1)));
        var res = await new Helpers.Helpers(fq).Documents()
            .InsertAsync("people", M(("name", "alice")));
        Assert.Equal(1L, res.Affected);
        Assert.Equal("doc-1", res.Rid);
        Assert.Equal("doc-1", res.Item!["rid"]);
    }

    [Fact] public async Task Documents_Get_RaisesNotFoundOnMissing()
    {
        var fq = new FakeQuerier().Reply(M(("rows", Array.Empty<object>())));
        await Assert.ThrowsAsync<HelperException.NotFound>(
            async () => await new Helpers.Helpers(fq).Documents().GetAsync("people", "doc-1"));
    }

    [Fact] public async Task Documents_Patch_RejectsJsonPointerPaths()
    {
        await Assert.ThrowsAsync<HelperException.InvalidArgument>(
            async () => await new Helpers.Helpers(new FakeQuerier()).Documents()
                .PatchAsync("people", "doc-1", M(("a/b", 1))));
    }

    [Fact] public async Task Documents_List_OrdersByRidByDefault()
    {
        var fq = new FakeQuerier().Reply(M(("rows", new[] {
            M(("rid", "a")), M(("rid", "b")) })));
        var res = await new Helpers.Helpers(fq).Documents().ListAsync("people");
        Assert.Equal(2, res.Items.Count);
        Assert.Contains("ORDER BY rid ASC", fq.Sqls[0]);
    }

    [Fact] public async Task Documents_Insert_PassesThroughExistingCollection()
    {
        var fq = new FakeQuerier()
            .Err(new Exception("collection already exists"))
            .Reply(M())
            .Reply(M(("rows", new[] { M(("rid", "x")) }), ("affected", 1)));
        await new Helpers.Helpers(fq).Documents().InsertAsync("people", M(("a", 1)));
    }

    [Fact] public async Task Documents_List_HandlesNestedResultEnvelope()
    {
        var fq = new FakeQuerier().Reply(M(("result", M(("rows", new[] {
            M(("rid", "x")) })))));
        var res = await new Helpers.Helpers(fq).Documents().ListAsync("people");
        Assert.Single(res.Items);
        Assert.Equal("x", res.Items[0]["rid"]);
    }
}
