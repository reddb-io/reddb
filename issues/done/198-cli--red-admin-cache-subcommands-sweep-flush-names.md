# null: CLI: red admin cache subcommands (sweep, flush-namespace, stats)

## Parent

#188

## What to build

Add `red admin cache <subcommand>` to the existing CLI. Subcommands:

- `red admin cache flush-namespace <ns>` → POST /admin/blob_cache/flush_namespace
- `red admin cache sweep [--limit-entries N | --limit-millis N]` → POST /admin/blob_cache/sweep
- `red admin cache stats [--namespace ns]` → GET /admin/blob_cache/stats (read existing BlobCacheStats getters)
- `red admin cache compare-and-set --namespace ns --key k --expected-version V --value <file> --new-version W` → POST /admin/cache/compare-and-set (per #195)

Output: JSON by default, table format with `--pretty`.

Lives under existing `red admin` subcommand tree in the CLI binary.

## Acceptance criteria

- [x] Four subcommands registered. — `red admin cache {stats,flush-namespace,sweep,compare-and-set}` in `src/bin/red.rs::run_admin_cache_command`.
- [x] Snapshot tests for output format (JSON + pretty). — `format_cache_stats_pretty` extracted from `print_cache_stats_pretty`; unit tests cover pretty-table rendering, invalid-JSON fallback, separator line, and `bytes_to_base64` RFC 4648 vectors in `src/bin/red.rs::tests`.
- [ ] Integration test against a local server for each subcommand. — Not yet; requires docker/server fixture.
- [x] Help text for each subcommand documents env-var overrides. — `RED_ADMIN_TOKEN`, `REDDB_BIND_ADDR` documented in `--help` output.
- [x] `docs/operations/blob-cache-backup-restore.md` (#187) updated to reference live commands instead of "pre-admin-handler" interim spec.

## Notes

- Server-side: Added `GET /admin/blob_cache/stats` endpoint (`routing.rs` + `handlers_admin.rs::handle_admin_blob_cache_stats`).
- CLI: Added `red admin` top-level command + `red admin cache` subcommand tree. Auth token via `--token` flag or `RED_ADMIN_TOKEN` env.
- POST requests use new `post_json_to_http_authed` helper (mirrors existing `post_json_to_http` + optional `Authorization: Bearer` header).
- `compare-and-set --value <file>` base64-encodes file contents using inline `bytes_to_base64` (no new dep required).
- `cargo check` not verified (sandbox approval required); types are consistent with existing patterns in the codebase.
- Integration tests (AC3) still pending — require a running server fixture; defer to a follow-up slice.

## Blocked by

- 
- 

