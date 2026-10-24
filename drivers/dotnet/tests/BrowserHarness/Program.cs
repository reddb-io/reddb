using Microsoft.AspNetCore.Components;
using Microsoft.AspNetCore.Components.Rendering;
using Microsoft.AspNetCore.Components.WebAssembly.Hosting;
using Microsoft.JSInterop;
using Reddb.Browser;

var builder = WebAssemblyHostBuilder.CreateDefault(args);
builder.RootComponents.Add<Probe>("#app");
await builder.Build().RunAsync();

public sealed class Probe : ComponentBase
{
    [Inject] public IJSRuntime JS { get; set; } = default!;

    protected override void BuildRenderTree(RenderTreeBuilder builder) =>
        builder.AddContent(0, "Browser interop conformance");

    protected override async Task OnAfterRenderAsync(bool firstRender)
    {
        if (!firstRender) return;
        try
        {
            await using var runtime = await BrowserRuntime.OpenAsync(JS, "/packages/browser/src/index.js");
            var info = await runtime.InfoAsync();
            await runtime.ValidateQueryAsync("SELECT 1");
            var scalar = await runtime.ExecuteScalarAsync("SELECT 6 * 7 AS answer");
            var exact = await runtime.ExecuteScalarAsync("SELECT 9007199254740993 AS exact");
            string? unsupported = null;
            try { await runtime.ExecuteScalarAsync("SELECT * FROM people"); }
            catch (BrowserRuntimeException error) { unsupported = error.Code; }
            await runtime.DisposeAsync();
            await runtime.DisposeAsync();
            string? closed = null;
            try { await runtime.InfoAsync(); }
            catch (BrowserRuntimeException error) { closed = error.Code; }
            await JS.InvokeVoidAsync("probeComplete", new
            {
                language = "blazor",
                abi = info.GetProperty("abiVersion").GetInt32(),
                storedQueries = info.GetProperty("capabilities").GetProperty("storedQueries").GetBoolean(),
                answer = scalar.GetProperty("rows")[0][0].GetInt32(),
                exact = exact.GetProperty("rows")[0][0].GetProperty("$int").GetString(),
                unsupported, closed,
            });
        }
        catch (Exception error) { await JS.InvokeVoidAsync("probeFailed", error.ToString()); }
    }
}
