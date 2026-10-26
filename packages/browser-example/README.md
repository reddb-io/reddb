# RedDB Browser playground

[Open the deployed example](https://reddb-io.github.io/reddb/browser/).

This is a real Svelte 5/TypeScript/Vite application using the **published**
`@reddb-io/browser@0.2.0` package, including its Worker and WASM.
No Rust build, RedDB server, CDN, account or external API is required at runtime.
The build pins the registry distribution, not a link to unpublished workspace source.

The interface demonstrates scalar SELECTs, explicit unsupported stored-data queries,
exclusive artifact ownership and an opaque binary test marker persisted in OPFS.
The marker exercises `.rdb` artifact publication; it is not a collection record,
SQL command or application transaction. For local table CRUD and transactions,
use `openDatabase` as shown in the [JS/TS guide](https://github.com/reddb-io/reddb/blob/main/docs/clients/browser.md).
This page exercises the artifact and scalar APIs.

## Run locally

From the repository root:

```sh
pnpm install --frozen-lockfile --ignore-scripts --filter @reddb-io/browser-example...
pnpm --filter @reddb-io/browser-example dev
```

For the same offline behavior as the published application:

```sh
pnpm --filter @reddb-io/browser-example build
pnpm --filter @reddb-io/browser-example exec vite preview --host 127.0.0.1
```

Open the printed localhost URL. After the cache-ready message, write a marker,
disable networking and reload. The count and scalar execution should survive.
The development server intentionally does not register an offline service worker.
Close other tabs using this example if `FILE_BUSY` appears. Clearing this origin's
site data removes the test file and its cached application assets.

## Verify

```sh
pnpm --filter @reddb-io/browser-example build
pnpm --filter @reddb-io/browser-example exec playwright install chromium firefox
pnpm --filter @reddb-io/browser-example test
```

The tests serve the production build under `/reddb/browser/`, exercise query errors,
file ownership and mobile layout, then shut down the complete browser and HTTP
listener before reopening offline. They also write and reopen another marker offline.
Storage-unsupported hosts must show an error and keep query/write controls disabled.

The same tests can target the deployed application:

```sh
REDDB_BROWSER_EXAMPLE_URL=https://reddb-io.github.io/reddb/browser/ \
  pnpm --filter @reddb-io/browser-example test
```

Deployment is part of the existing GitHub Pages documentation workflow. It builds
these sources into `docs/browser/` for the upload; generated output is not committed.
The example's versioned offline cache is restricted to its own path and never clears
other applications' caches. Service worker updates are offered through an explicit
"Atualizar aplicação" action. The control waits for current work to finish, closes
the local artifact, activates the cached update and reloads. The normal browser
lifecycle also activates an update after existing tabs close. Version parameters
on the page URL still support offline reload after the current worker is active.

## Vendored Design System

The UI composes the official Svelte Base, App and Data Kits through canonical
`@reddb-io/design-system/base`, `/app`, `/data/CodeField` and `/data/Table` imports.
The root `design-system.manifest.json` pins `v2026.10.30`. Official Sync lands the
Kits and Application styles in `vendor/ds` with `design-system.receipt.json`;
do not hand-edit those files.

The composition follows the DS's [red-ui query editor reference](https://github.com/reddb-io/design-system/blob/c43cc0341a26418135d83c20435908ea2e8ba2d3/apps/showcase/src/routes/examples/red-ui/query/+page.svelte):
a compact application frame, local artifact panel, line-numbered CodeField,
results under the editor, and a runtime status bar. ApplicationShell owns the main
landmark and skip navigation; Navbar owns product navigation. On narrow containers,
artifact controls follow the query/result regions. Page chrome follows the viewport.

Results are the actual scalar engine response, rendered by the Data Table with
columns mapped by position (including repeated aliases). A JSON tab preserves the
original response. Ctrl/Command+Enter runs the focused query workspace. The local
SQL tokenizer comes from the pinned DS reference and only controls highlighting;
query semantics still belong to the WASM engine. Query source remains bounded at
10,000 characters.

The initial appearance is Application, dark and compact, matching the canonical
reference. The Navbar exposes the dark-scheme switch.
Both schemes and all densities remain available through the DS. The Logo places
the original Brand mark; Space Grotesk and JetBrains Mono are emitted as local
assets and included in the offline cache. No external font service is used.

To repeat or update Sync, use the producer from the pinned release of the
sibling `../design-system` checkout, with its producer dependencies installed,
and run from the RedDB repository root:

```sh
pnpm dlx tsx ../design-system/scripts/producer/src/cli.ts
```

Review and commit the manifest, generated package and receipt together. The
consumer build verifies every receipt hash before typechecking. The browser suite
compares served marks and fonts to the vendored bytes and audits WCAG AA in light
and dark at desktop/mobile widths, alongside the existing offline tests. It also
checks the code-aware editor, actual result columns/cells, keyboard execution,
status-bar placement and desktop/phone/container layouts.
Axe and import checks complement visual review of rendered screenshots; they do not
establish visual adherence on their own.

Rendered examples of the application composition:

[Desktop, dark](docs/playground-desktop-dark.png) · [Desktop, light](docs/playground-desktop.png) · [Mobile, dark](docs/playground-mobile-dark.png).

The local browser suite also installs a changed worker revision, verifies that it
waits for explicit activation, updates through the real UI, retains a persisted
marker and reloads a versioned page URL without any HTTP request.
