---
status: open
tag: AFK
gh: 478
---

# [AFK] gh-478 iter 2: Crash injection test + OLTP benchmark for fold_dwb_into_wal

GitHub: reddb-io/reddb#478

## Iter 1 (on main)

- WalRecord::FullPageImage variant + codec + WAL parsing.
- Pager emits FPI before first mod per checkpoint when fold_dwb_into_wal=true; skips -dwb sidecar.
- Recovery applies FPI before redo.
- ADR 0018 documents extended WAL record format.
- e2e_fold_dwb_into_wal_policy: 3 passed (toggle on/off, FPI emit + apply, recovery).
- Tier-default wiring: `with_layout(StorageLayout::Max)` auto-enables.

## Iter 2 — close the remaining 2 acceptance bullets

- [ ] Crash injection during page write demonstrates clean recovery
- [ ] Benchmark shows acceptable OLTP overhead

### Crash injection test (`tests/e2e_fold_dwb_into_wal_crash.rs`)

Set up: open DB with fold_dwb_into_wal=true. Perform N transactions. Inject a simulated crash mid-page-write (truncate the data file to a partial page, or simulate a torn write). Reopen DB → recovery must apply FPI from WAL and reconstruct the page. Assert all committed transactions are present, no torn pages survive.

Simplest crash model: corrupt the last page on disk (overwrite with garbage) and confirm recovery restores it from the FPI. This is the same shape as `e2e_seqn_journal_policy::seqn_journal_corrupt_recovery`.

### Benchmark (`benches/fold_dwb_into_wal_bench.rs` or similar)

Use criterion (already in repo if present, else simplest measuring harness). Compare:
- DWB OFF (legacy -dwb sidecar): N small transactions
- DWB ON (FPI in WAL): N small transactions

Report relative throughput. Target: < 20% regression on typical OLTP workload. If the result is worse, document it as a known cost; the acceptance asks "acceptable", not "neutral".

## Notes
- `CARGO_TARGET_DIR=.target-gh478-iter2`
- Commit `Closes #478` if both done, else `Refs #478`.
- Don't change FPI record format, recovery internals, or pager internals beyond what's needed for the crash injection harness.
- Crash injection should be deterministic — no flaky tests.

## Progress (this iter — bash sandbox blocked git + cargo)

Files added on worktree but NOT committed; `cargo` and `git` calls
were denied at the harness level. Land + validate next iter.

- `tests/e2e_fold_dwb_into_wal_crash.rs` — deterministic crash
  injection harness. Populates a fresh DB with `ROWS=25` rows + a
  checkpoint, then overwrites the trailing 4 KiB of the data file
  with `0xA5` garbage to model a torn write on the last page.
  Reopens, runs `SELECT n FROM crash_rows`, and asserts all 25
  committed rows are reconstructed. Two variants — fold OFF and
  fold ON — both expected to recover via WAL replay (logical redo
  rebuilds the corrupted page region on the next checkpoint).
  Serialised via `POLICY_GUARD` so the process-global toggle is
  safe under cargo test threading.

- `tests/fold_dwb_into_wal_bench.rs` — `#[ignore]`d OLTP
  benchmark. Runs `TX_COUNT=200` inserts with a checkpoint every
  10 inserts under OFF and ON, prints `ratio(on/off)`, asserts
  `ratio <= 1.20` for the 20% acceptance gate. Uses `Instant`
  rather than criterion to avoid a new `[[bench]]` entry on the
  hot test path. Invoke with
  `cargo test --release --test fold_dwb_into_wal_bench -- --ignored --nocapture`.

### Verification status

- [ ] Compile: `cargo test --test e2e_fold_dwb_into_wal_crash --no-run`
- [ ] Crash test: `cargo test --test e2e_fold_dwb_into_wal_crash`
- [ ] Bench: `cargo test --release --test fold_dwb_into_wal_bench -- --ignored --nocapture`

### Known risk for the next iter

The iter-2 issue body claims iter-1 wired "pager emits FPI before
first mod per checkpoint" and "recovery applies FPI before redo"
— but a search across the worktree shows the FPI record type +
`WalReader::collect_full_page_images` helper are present while
the pager-side emit path and the recovery-time apply path are
not. The crash test is built so it should still pass via
logical WAL replay (TxCommitBatch + PageWrite redo at store
level reconstructs the corrupted last page on the next
checkpoint). If recovery fails for the fold-ON variant, the FPI
emit + apply wiring is the prerequisite — land that first, then
this test passes as-is.

Acceptance status after this iter:
- [x] Flag controls behaviour (iter 1)
- [x] ON: `-dwb` not created (iter 1)
- [x] OFF: DWB sidecar preserved (iter 1)
- [ ] Recovery applies FPI before redo (still pending pager wire-up)
- [~] Crash injection demonstrates clean recovery — harness landed,
      unverified (cargo blocked this iter)
- [~] OLTP benchmark — harness landed, unverified (cargo blocked
      this iter)
- [x] ADR documents extended WAL record format (iter 1)
