# September 7 diagnostic evidence

These are **diagnostic observations**, not an official performance scoreboard.
The host was shared; other projects and this audit's release build used CPU.
Default durability and native/subprocess/remote boundaries are not certified
as equivalent. Failed and incomplete probes remain visible.

`matrix.json` records 31 fixed comparison cells and their current decision states.
`manifest.json` records artifact hashes and final runner source hashes;
`summary.json` derives medians from measured (non-warmup), valid runs. Raw JSON
is compact to keep the bundle small; use `python3 -m json.tool FILE` to inspect.
Earlier runner revisions were recorded while tooling was dirty; the final
hashes do not retroactively certify exact earlier source bytes. The initial
and expanded runs therefore remain exploratory, even with ten repetitions.

## Artifact map

- `reddb-ext4.json`, `reddb-tmpfs.json`, `sqlite-{full,normal}-disk.json`:
  initial 200-row Bun SDK measurements, one warmup plus ten measured runs.
- `final-release--*.json`: ten-run release/SQLite FULL recheck after the audit
  build ended; the report leads with these rather than compilation-overlapped
  initial measurements.
- `initial-red-sqlite-full-analysis.json`: seeded run-level bootstrap; the
  only permitted verdict from this tool is diagnostic-only.
- `expanded--*.json`, `updates--*.json`: actual Bun sequential/bulk/point/update
  cases. Native Surreal warmups timed out at reopen; their supervisor entries
  remain invalid. Remaining repeated native cells were stopped rather than
  burning further cycles on the same reopen failure.
- `surreal-small.json`: one valid 20-row SurrealKV native engine 3.0.2 fixture,
  including reopen; this is not a 3.2.4 native baseline or statistically useful
  comparison by itself.
- `surface-release.json`, `cli-connect.trace`: real published CLI, HTTP,
  Bun wire, identifier and SDK transaction observations. CLI trace has no
  connection to the requested server. Bound wire query and HTTP see writes.
- `quickstarts-release.json`: complete SQL/results of eight quickstarts.
  Spatial returns IDs/distances where the checked-in expected output has names.
- `surreal-journeys.json`: six executed document/graph/rollback/index/vector/live
  smoke cases via the published JavaScript SDK and server 3.2.4. Earlier harness
  API mismatches were corrected before this final fixture; they are not engine
  defects. This is not an exhaustive transaction, quality or security test.
- `recovery-*.json`: process SIGKILL followed by 200-key/value readback, then
  logical dump/import into a fresh store. RedDB release recovery passes but
  logical restore fails content equality; its dump serializes strings with
  SQL-display quotes. SurrealDB 3.2.4 passes both fixture steps.
- `rpc-tmpfs-profile.{json,strace}`: **instrumented**, whole-child-lifetime
  counters; startup/DDL/shutdown are included. Futex waits are not CPU cost.
- `writers-reddb.json`: stopped exploratory sweep on an accumulating store;
  do not infer a concurrency scaling curve from it.
- `writers-final-reddb.json` and `writers-fresh-surreal.json`: the report
  curves; fresh owned server/store per run, separate HTTP
  socket per writer, 1/2/4/8/16 writers and full readback. Shared-host timing
  still does not prove group-commit efficiency or official leadership.
- `writers-fresh-reddb.json`: earlier build-overlapped version of the RedDB
  fresh-server sweep; superseded for the displayed curve by `writers-final-reddb.json`.
- `writers-fresh-surreal-basic.json`, `http-constant*.json`: the excluded
  per-request password-authentication experiment and the bearer-token control.
  No credential/token values were retained in result files.
- `native-main-*.json`, `main-sdk-*.json`: matching current-build optimized
  native/SDK boundary measurements with full readback/reopen.
- `native-scale-*.json`: individual 1,000/3,000-row current-native scaling
  probes; valid contents but no independent-run CI at those sizes.
- `rpc-tmpfs-uninstrumented.json`, `rpc-tmpfs-events.*`, `sync-phase-summary.json`:
  separate timing and phase-bounded sync observations. The INSERT window has
  202 fdatasync and 201 fsync starts, with zero in version/constant phases.
- `native-reopen-{node,bun}.json`: minimized 200-row native lifecycle case;
  Node exits 13 and Bun times out while awaiting reopen, after close returned.
- `studio-browser.json`: observed official login page; ephemeral auth state
  removed. Authenticated editing was not evaluated. Red UI release bundle
  fetch failed with a v0.0.0-dev 404; local embed-host override is not a released
  product-equivalence test.

Additional current-build, syscall-window and native artifacts, when present,
are explicitly named `main`, `native` or `uninstrumented`; their versions and
profiles must not be silently pooled with the release runs.

## Reproduce

Tool source: `reddb-io/reddb-benchmark`, [PR #2](https://github.com/reddb-io/reddb-benchmark/pull/2), branch
`bench/surrealdb-competitive-audit`, under `src/runners/bun/competitive/`.
The [runner README](https://github.com/reddb-io/reddb-benchmark/blob/bench/surrealdb-competitive-audit/src/runners/bun/competitive/README.md)
contains commands, dependencies, scopes and timeout behavior.

1. Download RedDB v1.23.2 and verify its published SHA256. Set `REDDB_BIN`.
2. Install the committed Bun lockfile, then `make competitive-audit-check`.
3. Run `suite.py` on ext4 and actual `/dev/shm` tmpfs paths, with ten runs.
   Never turn a failed reopen into a passing timing row.
4. Run `fresh_writers.py` for each binary on caller-owned unused localhost
   ports; it creates fresh server directories for every repetition.
5. Run `recovery.py`, `journeys.mjs` and `surreal_journeys.mjs` as documented.
6. Compile RedDB's `competitive_insert` example using the exact revision and
   profile, then use `native_sweep.py`; compare it to the same built binary
   through the SDK, not to a different release/profile.
7. Use `rpc_profile.py --syscall-events` for wall-clock phase boundaries;
   use its uninstrumented mode for timing. Keep those results separate.
8. Repeat on reserved hardware with a reviewed guarantee matrix before
   applying ADR 0076's public-lead decision rule.

No database files, dumps containing internal collections, access tokens or
runtime directories are included. The fixtures used only generated test data.
