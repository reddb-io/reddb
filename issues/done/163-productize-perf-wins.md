# Docs: productize RedDB perf wins (typed_insert 16×, disk_usage 1.5×) [AFK]

GitHub: reddb-io/reddb#163
Parent: #152
Blocked by: #154

Surface measured wins in user-facing docs. New `docs/perf/wins.md` with reproducible bench commands citing canonical-config session ids. Cross-link from README + JS/TS driver guide. Include "when not to use RedDB" complement.

## Acceptance Criteria

- [ ] `docs/perf/wins.md` exists with typed_insert + disk_usage tables, each citing reproducible bench command + session id.
- [ ] README perf section links to `docs/perf/wins.md`.
- [ ] JS/TS driver guide carries perf-wins callout linking to same page.
- [ ] "When not to use RedDB" section documents concurrent/bulk_update/aggregate_group gaps with links to closure issues (#157, #159, #161).
- [ ] Doc avoids embedding raw numbers; cites session sidecars.
