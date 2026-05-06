# Backup/Restore Drill History

The nightly drill workflow appends one row per run through `scripts/drill-nightly.sh`.

| timestamp UTC | command | result |
|---------------|---------|--------|
| 2026-05-06T06:43:24Z | `cargo test --locked --test 'drill_*' --no-fail-fast` | PASS |
| 2026-05-06T06:46:42Z | `cargo test --locked --test 'drill_*' --no-fail-fast` | PASS |
| 2026-05-06T06:46:52Z | `cargo test --locked --test 'drill_*' --no-fail-fast` | PASS |
| 2026-05-06T06:46:55Z | `cargo test --locked --test 'drill_*' --no-fail-fast` | PASS |
| 2026-05-06T06:46:57Z | `cargo test --locked --test 'drill_*' --no-fail-fast` | PASS |
