# Reddb.Browser: experimental Blazor WebAssembly adapter

This separate project uses `Microsoft.JSInterop` to run the shared RedDB
Rust/WASM Worker locally in Blazor WebAssembly. It does not depend on the native
`Reddb` driver or launch a RedDB server. It is available as repository source;
no `Reddb.Browser` package has been published to NuGet.

Reference `Reddb.Browser.csproj` from your Blazor project. Install
`@reddb-io/browser@experimental` in your web asset build and copy its complete
`src/` directory to `wwwroot/vendor/reddb-browser/`, preserving the layout.

Use the runtime once JS interop is available, such as in `OnAfterRenderAsync`:

```csharp
using Microsoft.JSInterop;
using Reddb.Browser;

// JS is an injected IJSRuntime. Use an application-relative, locally served path.
await using var runtime = await BrowserRuntime.OpenAsync(
    JS, "./vendor/reddb-browser/index.js");
var info = await runtime.InfoAsync();
await runtime.ValidateQueryAsync("SELECT 1");
var result = await runtime.ExecuteScalarAsync("SELECT 6 * 7 AS answer");
Console.WriteLine(result.GetProperty("rows")[0][0].GetInt32()); // 42
```

The current surface is syntax validation and storage-free scalar SELECTs.
Persisted collection queries, application transactions and sync remain pending.
`InfoAsync` reports those capabilities explicitly. This adapter does not add
another evaluator or make the native TCP/process driver browser-compatible.
Blazor Server would need its own lifecycle/disconnection qualification.

Results are `JsonElement`, retaining canonical exact-value tags. For
`SELECT 9007199254740993 AS exact`, read
`result.GetProperty("rows")[0][0].GetProperty("$int").GetString()` rather than
converting to `double`. `BrowserRuntimeException.Code` exposes errors such as
`VERSION_MISMATCH`, `QUERY_UNSUPPORTED` and `CLOSED`. Dispose terminates the
Worker and releases JS references; repeated disposal is safe.

Include your Blazor framework, application assemblies, and all JS/Worker/WASM
assets in your application's versioned offline cache. The SDK registers no
service worker and loads no CDN dependency. The real Blazor test app runs on
Chromium, Firefox and Playwright WebKit with the network disabled and the HTTP
host stopped. Complete offline browser restarts are verified on Chromium/Firefox.
Playwright WPE loses CacheStorage on process restart; real Safari/iOS offline
restart qualification remains pending. This checks scalar execution,
not OPFS persistence or full database transactions.

From the repository root:

```sh
pnpm --filter @reddb-io/browser build
pnpm --filter @reddb-io/browser build:interop
pnpm --filter @reddb-io/browser test:interop
```

The harness requires .NET SDK 8.0.426 and Dart 3.13.7. `DOTNET`, `DART` and
`REDDB_BROWSER_INTEROP_DIR` can select local executables and a build directory.
