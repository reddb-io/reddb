# SDK helpers: `db.exists`, `db.list`, generic `db.from<T>`

Status: done

Implemented:

- Added `db.exists(collection)` on both JS driver packages.
- Added `db.list()` on both JS driver packages, returning visible `SHOW COLLECTIONS` rows with a stable `capabilities` array field.
- Added caller-typed `db.from<T>(collection)` with a focused `TypedQueryBuilder`.
- The builder supports `.select(...)`, `.where(...)`, and `.run()`, passes query params through `db.query`, and returns only explicitly selected columns when a projection is provided.
- Updated TypeScript declarations, including generic projection narrowing for `.select(...)`.
- Added JS unit coverage for existence, list shape, typed builder SQL, runtime row projection, and identifier validation.
- Added embedded stdio smoke coverage for `exists`, `list`, and `from`.

Verification:

- `node --test test/db-helpers.test.mjs` in `drivers/js`
- `node --test test/db-helpers.test.mjs` in `drivers/js-client`
- `node --test test/ask.test.mjs test/cache.test.mjs test/db-helpers.test.mjs test/embedded-only.test.mjs test/kv.test.mjs test/params.test.mjs test/postinstall.test.mjs test/queue.test.mjs test/redwire.params.test.mjs` in `drivers/js`
- `node --test test/*.test.mjs` in `drivers/js-client`
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red node --input-type=module <db helpers stdio smoke>`
- `npx -y -p typescript tsc --noEmit --strict --module Node16 --moduleResolution node16 --target ES2022 --allowImportingTsExtensions <temporary typecheck>`
- `git diff --check`

Notes:

- The TypeScript check used a temporary `.mts` file importing `drivers/js/index.d.ts` and asserted that `db.from<Row>().select('id', 'name').run()` narrows away unselected `age`.
- GitHub issue `#463` does not exist in `reddb-io/reddb`; no remote comment or close was possible.
