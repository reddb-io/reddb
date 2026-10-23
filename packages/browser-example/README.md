# RedDB Browser playground

[Open the deployed example](https://reddb-io.github.io/reddb/browser/).

This is a real Svelte 5/TypeScript/Vite application using the **published**
`@reddb-io/browser@0.1.0-alpha.0` package, including its Worker and WASM.
No Rust build, RedDB server, CDN, account or external API is required at runtime.
The build pins the registry distribution, not a link to unpublished workspace source.

The interface demonstrates scalar SELECTs, explicit unsupported stored-data queries,
exclusive artifact ownership and an opaque binary test marker persisted in OPFS.
The marker exercises `.rdb` artifact publication; it is not a collection record,
SQL command or application transaction. Full stored-data execution remains pending.

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
other applications' caches. Service worker updates use the normal browser lifecycle,
so a new build takes over after existing tabs close.

## Vendored Design System

The UI composes the official Svelte Base and App Kits through canonical
`@reddb-io/design-system/base` and `/app` imports. The root `design-system.manifest.json`
pins `v2026.10.30`, selecting both Kits and Application styles. Official Sync lands
the package in `vendor/ds` with `design-system.receipt.json`; do not hand-edit
those files. App CSS owns page arrangement only, using DS spatial/color/font roles.

ApplicationShell owns the page landmark and skip navigation. Navbar owns the
product navigation; SidebarLayout and SidebarPanel place artifact/offline controls
beside the query workspace, stacking after it in narrow containers. The editor and
result split follows a DS container step, independently of viewport width.

The page declares Application, light and comfortable as its initial appearance axes.
The Navbar exposes a dark-scheme switch.
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
checks workspace placement at desktop/phone/container widths and keyboard navigation.
Axe and import checks complement visual review of rendered screenshots; they do not
establish visual adherence on their own.

Rendered examples of the application composition:

[Desktop, light](docs/playground-desktop.png) · [Mobile, dark](docs/playground-mobile-dark.png).
