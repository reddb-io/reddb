# SDK Queue client (`db.queue.{push,pop,peek,len,purge}`)

Status: done

Implemented:

- Added `QueueClient` to `@reddb-io/sdk` and `@reddb-io/client`.
- Exposed `db.queue` on both public `RedDB` handles.
- Added `push`, `pop`, `peek`, `len`, and `purge` methods.
- Serialized string, number, boolean, null, and JSON object queue payloads into the engine queue DSL.
- Added optional `priority` support for `push`.
- Updated TypeScript declarations in both driver packages.
- Added JS unit tests for generated SQL, payload array normalization, count/priority validation, and public exports.
- Added embedded stdio smoke coverage in `drivers/js/test/smoke.test.mjs`.
- Extended the Rust queue parser so `QUEUE PEEK <queue> COUNT <n>` works while preserving the existing `QUEUE PEEK <queue> <n>` form.

Verification:

- `node --test test/queue.test.mjs` in `drivers/js`
- `node --test test/queue.test.mjs` in `drivers/js-client`
- `node --test test/ask.test.mjs test/cache.test.mjs test/embedded-only.test.mjs test/kv.test.mjs test/params.test.mjs test/postinstall.test.mjs test/queue.test.mjs test/redwire.params.test.mjs` in `drivers/js`
- `node --test test/*.test.mjs` in `drivers/js-client`
- `cargo test -q -p reddb-io-server test_parse_queue_control_and_group_command_forms --lib -- --test-threads=1`
- `cargo build -q --bin red`
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red node --input-type=module <queue stdio smoke>`
- `cargo check -q -p reddb-io-server`
- `git diff --check`

Notes:

- The full `drivers/js/test/smoke.test.mjs` was also run with `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red`. The queue smoke passed, then an unrelated ASK assertion failed later: expected default cost `0`, got `0.000014`.
- GitHub issue `#461` does not exist in `reddb-io/reddb`; no remote comment or close was possible.
