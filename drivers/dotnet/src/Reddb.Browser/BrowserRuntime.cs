using System.Text.Json;
using Microsoft.JSInterop;

namespace Reddb.Browser;

public sealed class BrowserRuntimeException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

/// <summary>Shared Rust/WASM scalar execution through a locally bundled JS module.</summary>
/// <remarks>Exact integers and decimals retain RedDB's tagged JSON representation.</remarks>
public sealed class BrowserRuntime : IAsyncDisposable
{
    private readonly IJSObjectReference module;
    private readonly IJSObjectReference bridge;
    private bool disposed;

    private BrowserRuntime(IJSObjectReference module, IJSObjectReference bridge)
    {
        this.module = module;
        this.bridge = bridge;
    }

    public static async ValueTask<BrowserRuntime> OpenAsync(IJSRuntime js, string modulePath)
    {
        var module = await js.InvokeAsync<IJSObjectReference>("import", modulePath);
        BrowserRuntime? runtime = null;
        try
        {
            runtime = new BrowserRuntime(module,
                await module.InvokeAsync<IJSObjectReference>("createInteropRuntime"));
            var info = await runtime.InfoAsync();
            if (info.GetProperty("abiVersion").GetInt32() != 1 ||
                info.GetProperty("valueEncoding").GetString() != "reddb-json-v1")
                throw new BrowserRuntimeException("VERSION_MISMATCH", "Unsupported runtime ABI");
            return runtime;
        }
        catch
        {
            if (runtime is not null) await runtime.DisposeAsync();
            else await module.DisposeAsync();
            throw;
        }
    }

    private async ValueTask<JsonElement> InvokeAsync(string method, object? arguments = null)
    {
        if (disposed) throw new BrowserRuntimeException("CLOSED", "This runtime handle is closed");
        var response = await bridge.InvokeAsync<JsonElement>("invoke", method, arguments ?? new { });
        if (!response.GetProperty("ok").GetBoolean())
        {
            var error = response.GetProperty("error");
            throw new BrowserRuntimeException(error.GetProperty("code").GetString()!,
                error.GetProperty("message").GetString()!);
        }
        return response.GetProperty("result");
    }

    public ValueTask<JsonElement> InfoAsync() => InvokeAsync("info");

    public async ValueTask ValidateQueryAsync(string sql)
    {
        await InvokeAsync("validateQuery", new { sql });
    }

    public ValueTask<JsonElement> ExecuteScalarAsync(string sql) =>
        InvokeAsync("executeScalar", new { sql });

    public async ValueTask DisposeAsync()
    {
        if (disposed) return;
        disposed = true;
        try { await bridge.InvokeVoidAsync("close"); }
        finally
        {
            try { await bridge.DisposeAsync(); }
            finally { await module.DisposeAsync(); }
        }
    }
}
