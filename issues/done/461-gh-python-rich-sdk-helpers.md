# Implement rich SDK helpers for Python [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/461
Parent: #449

## What to build

Implement the SDK Helper Spec for the Python driver surface, keeping embedded and
remote behavior truthful. Python users should get rich document and KV helpers
and clear unsupported errors where a transport cannot provide a helper.

## Acceptance criteria

- [x] Python exposes helpers required by the SDK Helper Spec where supported.
- [x] Document helpers include insert, get, list/filter, patch, and delete.
- [x] KV helpers support namespaced keys and exact key round-trip.
- [x] Embedded and remote transport limitations are explicit and tested
      (PyO3 `grpc://` document/KV helpers raise `NOT_SUPPORTED`; asyncio
      `red://` embedded URIs still raise `NotImplementedError`; HTTP-only
      helpers like `kv.watch` raise `UNSUPPORTED_TRANSPORT` on RedWire).
- [x] Python conformance tests pass for supported helpers
      (new unit tests in `tests/test_helpers.py`).
- [x] README and type hints match the implemented helper surface.

## Notes for next iteration

- The PyO3 embedded driver (`drivers/python/`) now exposes document and KV
  helpers, `rid`/`rids` result aliases, and conformance smoke coverage.
- Probabilistic helpers (`HLL`, `Bloom`, `CMS`) are still expressed
  through `db.query` until the Rust server stabilizes their wire shape.

## Verification

- `cargo fmt --all --check`
- `git diff --check`
- `cargo check` in `drivers/python`
- `CARGO_TARGET_DIR=.target-py461 cargo check` in `drivers/python`
- `maturin develop` against a local venv, with isolated `CARGO_TARGET_DIR`
- `.venv-py461/bin/python -m pytest drivers/python/tests/test_helpers.py drivers/python/tests/test_smoke.py`
- `PYTHONPATH=drivers/python-asyncio/src uv run --with pytest --with pytest-asyncio --with httpx python -m pytest drivers/python-asyncio/tests/test_helpers.py`
- `bash scripts/check-versions.sh`
- `CARGO_TARGET_DIR=.target-gh461 make check`
- `CARGO_TARGET_DIR=.target-gh461 cargo build --bin red`

Known pre-existing failures:

- `pnpm typecheck` exits `1` while printing `TypeScript: No errors found`.
- `cargo clippy -p reddb-io-server --all-targets -- -D warnings` fails on
  existing server-wide clippy backlog outside this slice.

## Blocked by

- #459
- #452
- #454
- #456
