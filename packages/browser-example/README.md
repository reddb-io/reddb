# RedDB Browser playground

[Open the deployed example](https://reddb-io.github.io/reddb/browser/).

This is a real TypeScript/Vite application using the **published**
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
pnpm install --frozen-lockfile --ignore-scripts --filter @reddb-io/browser-example
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
