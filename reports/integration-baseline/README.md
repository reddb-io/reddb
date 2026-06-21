# Integration-test baseline (Issue #974)

Published baseline of the RedDB integration (e2e) test lane, measured with
`cargo-nextest` (see `docs/testing/nextest-lanes.md`). This directory records the
real pass/fail picture so the true scope beyond unit tests is known and
remediation can be sliced from it.

## Lanes

- **integration** (`integration.md`) — the e2e / integration-test-binary lane,
  the required deliverable:
  `cargo nextest run --workspace --locked -E 'kind(test)'`
- **unit** (`unit.md`, optional/bonus) — the fast lib lane:
  `cargo nextest run --workspace --locked --lib`

Each lane report holds:

- total pass / fail / skipped counts,
- the explicit list of failing tests,
- the wall-clock the lane took,
- the remediation scope the failures imply.

> Measured incrementally: the lane is run once, its result committed
> immediately, so the run is never lost to a progress guard.
