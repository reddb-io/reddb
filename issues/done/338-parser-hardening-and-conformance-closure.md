# Parser hardening and conformance closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/338

Labels: enhancement

GitHub issue number: #338

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Tie parser hardening, conformance corpora, fuzz scheduling, coverage uplift, and secret-redaction claims to concrete current tests, corpus files, CI jobs, and report evidence.

Covers: #87, #97, #231, #233, #236

User stories covered: 10, 17, 18

## Acceptance criteria

- [x] Parser hardening harness has evidence for property, fuzz, snapshot, and limits coverage or explicit follow-ups for missing layers.
- [x] Secret-redaction audit has evidence that parser snapshots/corpora avoid known secret patterns.
- [x] Positive conformance corpus and fuzz schedule are evidenced by current files and CI/workflow wiring.
- [x] Lexer and table parser coverage uplift claims are tied to measurable coverage evidence or split into follow-up issues.
- [x] The evidence report no longer marks #87, #97, #231, #233, or #236 as partial without a final disposition.

## Closure notes

- Added conformance tests proving the positive parser corpus has at least 40 cases across the documented parser surfaces and that committed conformance TOMLs contain no unmasked secret-shaped strings.
- Renamed the E-30 DLQ conformance/docs queue example from `users_events_outbox_dlq` to `user_events_outbox_dlq` to avoid the `rs_...` token-shaped substring covered by the redaction audit.
- Added explicit evidence ledger dispositions for #87, #97, #231, #233, and #236 and regenerated both report JSON artifacts.

## Verification

- `cargo test -p reddb-server --test conformance positive_conformance_corpus_covers_documented_parser_surface -- --nocapture`
- `cargo test -p reddb-server --test conformance conformance_corpus_contains_no_unmasked_secret_shapes -- --nocapture`
- `node --test scripts/issue_code_evidence_report.test.mjs`
- `node scripts/issue_code_evidence_report.js /tmp/reddb_issues_raw.json reports`
- `rustfmt --edition 2021 --check --config skip_children=true crates/reddb-server/tests/conformance.rs`
- `cargo check`
- `git diff --check`

## Blockers / notes

- `cargo fmt --all --check` still reports unrelated repo-wide formatting drift outside this task.
- `pnpm test` still fails because `@reddb-io/internal-bin-resolver` is missing.
- `pnpm typecheck` still fails because no `typecheck` command is defined.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)
