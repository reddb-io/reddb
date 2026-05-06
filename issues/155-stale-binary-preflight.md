# Bench: stale-binary preflight hardening + auto-rebuild [AFK]

GitHub: reddb-io/reddb#155
Parent: #152

Last mini-duel session was blocked silently by stale-binary guard. Make failure loud + add `BENCH_AUTOREBUILD=1` opt-in.

## Acceptance Criteria

- [ ] Preflight fails fast with clear actionable error when binary stale or missing.
- [ ] `BENCH_AUTOREBUILD=1` triggers `cargo build --release` then re-checks.
- [ ] `REDDB_BIN_PATH` honored, subject to same mtime check.
- [ ] Fixture tests cover four mtime states + auto-rebuild path.
- [ ] Both `make mini-duel` and `make duel-official` invoke preflight.
