# SDK: propagate `id` / `ids` from `insert` / `bulkInsert` to public types

Status: done

Implemented:

- Made `InsertResult.id` required in both JS driver type surfaces.
- Made `BulkInsertResult.ids` required in both JS driver type surfaces.
- Added runtime guards for `insert` and `bulkInsert` so older engines without id fields throw `RedDBError('ENGINE_TOO_OLD', ...)`.
- Added a bulk id count guard that throws `INVALID_RESPONSE` if the server returns the wrong number of ids.
- Preserved the remote client's existing `affected: 1` shim for insert responses that contain `id` but not `affected`.
- Added unit tests for required id fields, ordered bulk ids, older-engine errors, and malformed bulk id counts.
- Extended embedded smoke assertions to check real `insert` ids and `bulkInsert` ids.

Verification:

- `node --test test/insert-ids.test.mjs` in `drivers/js`
- `node --test test/insert-ids.test.mjs` in `drivers/js-client`
- `node --test test/ask.test.mjs test/cache.test.mjs test/db-helpers.test.mjs test/embedded-only.test.mjs test/insert-ids.test.mjs test/kv.test.mjs test/params.test.mjs test/postinstall.test.mjs test/queue.test.mjs test/redwire.params.test.mjs` in `drivers/js`
- `node --test test/*.test.mjs` in `drivers/js-client`
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red node --input-type=module <insert ids stdio smoke>`
- `npx -y -p typescript tsc --noEmit --strict --module Node16 --moduleResolution node16 --target ES2022 --allowImportingTsExtensions <temporary typecheck>`
- `git diff --check`

Notes:

- The runtime guard message names minimum engine version `1.0.9`, the first expected release line after the #458 engine-side id support.
- GitHub issue `#464` does not exist in `reddb-io/reddb`; no remote comment or close was possible.
