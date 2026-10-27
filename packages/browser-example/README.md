# RedDB Browser playground

[Open the deployed example](https://reddb-io.github.io/reddb/browser/).

This is a real Svelte 5/TypeScript/Vite application using the **published**
`@reddb-io/browser@0.4.0` package, including its Worker and WASM.
No Rust build, RedDB server, CDN, account or external API is required at runtime.
The build pins the registry distribution, not a link to unpublished workspace source.

The interface opens a local SQL database with `openDatabase`, creates an empty
`notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)` table if missing, and executes
CREATE TABLE, INSERT, UPDATE, DELETE and SELECT through the published `query` API.
Examples fill the editor; use **Executar consulta** or Ctrl/Command+Enter to run
one statement at a time. The INSERT example uses ID 1; update/delete it or supply
a new ID before inserting again.

INSERT, UPDATE and DELETE examples use positional `$1`/`$2` parameters. Edit the
**Parâmetros (JSON)** array alongside the SQL, or use numbered `?1`/`?2` positions.
Values stay data, including quotes, NUL and Unicode. Use `[]` for statements
without parameters. The editor accepts JSON scalar values (null, boolean, text
and finite numbers with safe integers), bounded at 10,000 source characters per
field. Objects and arrays are rejected by the engine. The SDK also accepts i64/u64
bigints directly from JS/TS; JSON cannot represent bigint values.

BEGIN, COMMIT and ROLLBACK controls send real SQL to the engine. Reads within a
transaction see staged writes; a successful COMMIT makes them durable. Statement
errors abort an active transaction until ROLLBACK. The UI displays the engine's
responses rather than inferring transaction state from SQL text. Reopening,
reloading, closing or updating the application discards changes without COMMIT.

The SQL database uses `reddb-browser-sql-playground-v1`, exclusively owned by one
tab. The previous example's `reddb-browser-playground-v1` marker file is preserved
and is not converted or overwritten. See the
[JS/TS guide](https://github.com/reddb-io/reddb/blob/main/docs/clients/browser.md)
for integration, supported table SQL and current engine limits.

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

Open the printed localhost URL. Run **Inserir**, then **Ler**. After the
cache-ready message, disable networking and reload. The committed note and SQL
execution should survive. Use BEGIN, edit a note and ROLLBACK to discard it, or
COMMIT to persist it. Reopen the database to recover only committed rows.
The development server intentionally does not register an offline service worker.
Close other tabs using this example if `FILE_BUSY` appears. Clearing this origin's
site data removes the test file and its cached application assets.

**Retenção dos dados** reports whether the browser granted persistent storage and
shows the estimated usage/quota for the entire origin, including application
caches. **Solicitar armazenamento persistente** calls `navigator.storage.persist()`;
a denial leaves the database usable and is reported explicitly. The browser makes
the decision. A grant does not prevent explicit site-data clearing, provide a
backup or synchronize data between browsers, profiles or devices. Refresh/F5
retains committed writes when reopening the same database name and origin.

## Download and restore a backup

**Baixar backup .rdb** downloads the complete native file, including committed
catalog, snapshot and WAL for all tables. It excludes pending transaction changes
without committing or rolling them back. Keep the file outside this site's
storage. A physical backup may contain old/deleted bytes and is sensitive; the
export adds no encryption or redaction.

Select a `.rdb` in **Arquivo de backup .rdb**, then choose **Restaurar em novo banco**.
The SDK restores into an unused generated name and validates through Rust. The
current database is closed only after successful restoration; invalid backups
leave it open, including its pending transaction. Switching after a successful
restore discards the old handle's uncommitted changes, as the inline notice
explains. Existing files are never overwritten. Both operations have a 256 MiB
physical-file bound.
Restoration also remains available when the original file cannot be opened;
the corrupt source is preserved while the valid backup opens under a new name.

The restored name is recorded in the page's `database` URL parameter, so refresh
and offline navigation reopen that same file. The URL contains a name, not the
database bytes, and cannot transfer the data to another browser/profile. Restored
files are not automatically bootstrapped or recreated if missing. The example
initially queries `notes`; for a backup containing different tables, use the SQL
editor to query them. **Abrir banco original do playground** returns to the
original, preserved database.

## Verify

```sh
pnpm --filter @reddb-io/browser-example build
pnpm --filter @reddb-io/browser-example exec playwright install chromium firefox
pnpm --filter @reddb-io/browser-example test
```

The tests serve the production build under `/reddb/browser/`, exercise query errors,
file ownership and mobile layout, then shut down the complete browser and HTTP
listener before two offline browser restarts. They verify CRUD, commit/rollback,
aborted transactions, own-write visibility, offline write recovery and discard
of uncommitted changes on reopen, close, refresh and service-worker activation.
Parameterized writes also recover after offline refresh/restart. The suite checks
the real parameter editor, invalid JSON/arity/types, quotes/NUL/Unicode and the
UI's response to granted/denied persistence requests.
Download/upload tests round-trip real `.rdb` files through the UI in both browsers,
reject invalid files while retaining the current transaction, recover restored
files after offline refresh and return to the unchanged original database.
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
the local database, discards uncommitted changes, activates the cached update
and reloads. The update notice asks users to COMMIT first. The normal browser
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

Results are the actual SQL engine response, rendered by the Data Table with
columns mapped by position (including repeated aliases). A JSON tab preserves the
original response. Ctrl/Command+Enter runs the focused query workspace. The local
SQL tokenizer comes from the pinned DS reference and only controls highlighting;
query semantics still belong to the WASM engine. Local highlighting extends the
reference keyword set for table DDL/DML and transactions. Query source remains bounded at
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
waits for explicit activation, updates through the real UI, retains a committed
note, discards a pending transaction and reloads a versioned page URL without
any HTTP request.
