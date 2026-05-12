# Parser hardening and conformance closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/338

Labels: prd

GitHub issue number: #338

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Tie parser hardening, conformance corpora, fuzz scheduling, coverage uplift, and secret-redaction claims to concrete current tests, corpus files, CI jobs, and report evidence.

Covers: #87, #97, #231, #233, #236

User stories covered: 10, 17, 18

## Acceptance criteria

- [ ] Parser hardening harness has evidence for property, fuzz, snapshot, and limits coverage or explicit follow-ups for missing layers.
- [ ] Secret-redaction audit has evidence that parser snapshots/corpora avoid known secret patterns.
- [ ] Positive conformance corpus and fuzz schedule are evidenced by current files and CI/workflow wiring.
- [ ] Lexer and table parser coverage uplift claims are tied to measurable coverage evidence or split into follow-up issues.
- [ ] The evidence report no longer marks #87, #97, #231, #233, or #236 as partial without a final disposition.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)
