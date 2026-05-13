# SDK KV: stop silent `:` rewrite; add `kv.get` / `getMany`

Status: done

Implemented:

- Replaced the JS SDK KV identifier rewrite with validation. Unsupported key characters now throw `INVALID_KV_KEY` and name the offending character instead of silently rewriting the key.
- Added `kv.get(key, options?)` to both JS driver packages, returning the stored value on hit and `null` on miss.
- Added `kv.getMany(keys, options?)` to both JS driver packages, preserving input order in the returned array.
- Updated TypeScript declarations for both driver packages.
- Added KV unit coverage for invalid characters, no silent rewrite, get hit/miss, dotted collection forms, and getMany order.

Verification:

- `node --test test/kv.test.mjs` in `drivers/js`
- `node --test test/kv.test.mjs` in `drivers/js-client`
- `node --test test/ask.test.mjs test/cache.test.mjs test/embedded-only.test.mjs test/kv.test.mjs test/params.test.mjs test/postinstall.test.mjs test/redwire.params.test.mjs` in `drivers/js`
- `node --test test/*.test.mjs` in `drivers/js-client`
- `git diff --check`

Notes:

- The engine KV parser accepts identifier path segments, not quoted arbitrary key segments, so the implemented behavior is explicit rejection rather than preservation for keys like `corpus:version`.
- GitHub issue `#460` does not exist in `reddb-io/reddb`; no remote comment or close was possible.
