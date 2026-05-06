# Bench: lock benchmark methodology to one canonical config schema [AFK]

GitHub: reddb-io/reddb#154
Parent: #152

Same RedDB commit produces ~2× different throughput numbers across "official partial" vs "focused-loop". That is methodology drift — what false regression #124 actually measured. Pick one canonical config; both `make mini-duel` and `make duel-official` resolve to the same schema. Extract `BenchConfigSchema` deep module.

## Acceptance Criteria

- [ ] One canonical configuration is named explicitly.
- [ ] `BenchConfigSchema` is single source of truth for both Make targets.
- [ ] Unit test asserts both targets resolve through the same schema (with documented intentional differences).
- [ ] Audit tooling refuses cross-config history rows.
- [ ] `rdb-benchmark/METHODOLOGY.md` documents canonical config + cross-config deprecation.
