# Determinism: temperature=0 + seed=hash + overrides [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/400

Labels: enhancement

GitHub issue number: #400

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Default `temperature=0` and `seed = hash(question + sources_fingerprint)` for providers that support it.

Per-query overrides: `ASK '...' TEMPERATURE 0.7 SEED 42`. Settings: `ask.default_temperature`.

Sources fingerprint is a stable hash over the URNs and content versions of the retrieved sources — ensures that same question + same data yields same seed even when no cache is involved.

Providers without seed support (per #396) use temperature only; audit row records what was actually sent.

## Acceptance criteria

- [ ] Default `temperature=0` applied to every ASK call.
- [ ] Default `seed` derived from question + sources_fingerprint for supporting providers.
- [ ] Per-query `TEMPERATURE` and `SEED` overrides parse and apply.
- [ ] Setting `ask.default_temperature` honored.
- [ ] Integration test: same question, same data, two calls → same answer string (within provider determinism guarantees) on OpenAI/Groq stubs.
- [ ] Audit row records the seed and temperature actually used.

## Blocked by

- #393

## Progress

Slice 1: `DeterminismDecider` deep module landed at
`crates/reddb-server/src/runtime/ai/determinism_decider.rs` with 19
unit tests. Pure — no I/O, no clock, no LLM calls. Exposes:

- `Inputs { question, sources_fingerprint }` — opaque fingerprint
  (decider doesn't recompute it; the retrieval layer owns the format).
- `Overrides { temperature: Option<f32>, seed: Option<u64> }` parsed
  from `ASK '...' TEMPERATURE x SEED n`.
- `Settings { default_temperature }` with `Default::default()` →
  `0.0` (the spec default).
- `Applied { temperature, seed }` — exactly what the caller should
  send to the provider AND what the audit row should record.
- `decide(inputs, caps, overrides, settings) -> Applied`.
- `derive_seed(question, fingerprint) -> u64` — `sha256(question ||
  0x1f || fingerprint)`, first 8 bytes little-endian. 0x1f (ASCII US)
  is used as the field delimiter to keep the concatenation injective
  without escaping; `derive_seed_is_injective_across_field_boundary`
  pins that `("ab","c") != ("a","bc")`.

Policy pinned by tests:
- temperature: override > settings.default > 0.0; if
  `caps.supports_temperature_zero == false` (Local-class endpoints
  that take no temperature at all), `temperature` is dropped to
  `None` even when overridden;
- seed: if `caps.supports_seed == false` (Anthropic, HuggingFace,
  Local, Custom — per #396), `seed` is dropped to `None` whether
  derived or overridden, so the audit row never lies about what the
  provider got;
- `Some(0.0)` and `Some(0)` overrides are preserved (guards against
  `unwrap_or(0)` regressions where override and default would be
  indistinguishable);
- determinism: `decide` and `derive_seed` are byte-equal across calls
  given the same inputs (`decide_is_deterministic_across_calls`,
  `derive_seed_is_deterministic_across_calls`).

Slice 2 (this commit): SQL surface for the overrides.

- Added `temperature: Option<f32>` and `seed: Option<u64>` to
  `AskQuery` (`crates/reddb-server/src/storage/query/core.rs`).
- Parser now accepts `TEMPERATURE <num>` and `SEED <int>` clauses
  alongside `USING / MODEL / DEPTH / LIMIT / COLLECTION`, in any
  order, each at most once (loop bound 5 → 7).
- gRPC ASK JSON payload binder forwards optional `temperature` /
  `seed` so MCP and gRPC drivers can pass them through without a
  second parser.
- Parser tests pin happy-path values (`0.7`, `42`), order-
  independence with `SEED 7 USING openai TEMPERATURE 0`, and
  negative cases (`ASK 'q' TEMPERATURE`, `ASK 'q' SEED`).
- `Some(0.0)` and `Some(0)` overrides preserved by the parser (no
  `unwrap_or(0)` collapse on the wire).

Deferred to follow-up slices (each independently shippable):

- Surface `ask.default_temperature` in runtime config (TOML/KV
  plumbing identical to #401 settings) and feed it into
  `determinism_decider::Settings`.
- Compute `sources_fingerprint` in the retrieval layer (suggested:
  lowercase hex sha256 over the canonical sorted list of
  `(urn, content_version_u64_be)` tuples post-fusion in #398) and
  pass it into `decide`. Same fingerprint should flow into the
  future answer-cache key (#403) so the two stay aligned.
- Wire `decide()` into `execute_ask`: replace the hard-coded
  `temperature: Some(0.3)` Anthropic branch, drop the literal seed
  off OpenAi-style branches, and pull both from `Applied`. Record
  `Applied` (not requested) in the audit row (#402).
- Integration test against OpenAI/Groq stubs verifying same
  question + same data → byte-equal answer (depends on the
  stubbable LLM transport refactor already deferred by #395/#396).

Verification (this slice):
- `cargo check -p reddb-io-server` clean.
- `cargo test -p reddb-io-server --lib storage::query::parser::tests::test_parse_dml`
  → 2 passed (existing cases plus the new TEMPERATURE/SEED block).

Deep module + SQL surface together unblock the wiring slice — at
that point a follow-up can land `decide()` into `execute_ask` in
one focused PR without re-touching the parser or the AST.

Slice 3 (this commit): `SourcesFingerprint` deep module landed at
`crates/reddb-server/src/runtime/ai/sources_fingerprint.rs` with 14
unit tests. Pure — no I/O, no clock. Pins the canonical format that
both `determinism_decider::derive_seed` and `answer_cache_key` (#403)
already treat as opaque, so changing it later is a one-way wire
break that the tests will catch.

Exposes:

- `Source<'a> { urn, content_version: u64 }` — one retrieved row.
- `fingerprint(&[Source]) -> String` — lowercase-hex SHA-256.

Canonical form pinned by tests:

- Tuples sorted by urn bytes ascending, then version ascending;
- `dedup` after sort so the same `(urn, version)` from two buckets
  (BM25 + vector) collapses;
- Per tuple: `urn || 0x1f || version.to_be_bytes() || 0x1e`;
- Empty input hashes to sha256("") — keeps the seed derivation
  total without an `Option<String>` thread.

Tests pin:

- empty-input hash equals the known sha256 of "";
- output is 64 lowercase-hex chars;
- deterministic across calls;
- order-independent across input permutations;
- duplicates collapse;
- version change flips the hash;
- different urns produce different hashes;
- `(urn, v1)` and `(urn, v2)` both contribute (no urn-only dedup);
- version width pinned to BE-8 bytes (1 vs 1<<56 differ);
- urn boundary is injective with the field delimiter;
- one hand-computed single-entry hash so the byte layout can't
  silently drift;
- bytewise sort, not lex (capital "Z" sorts before lowercase "a");
- empty-urn entry is distinct from empty-set.

Deferred to follow-up slices (each independently shippable):

- Compute `(urn, content_version)` pairs at the retrieval layer
  (post-#398 fusion) and call `fingerprint()` before invoking
  `determinism_decider::decide` in `execute_ask`. Same fingerprint
  flows into the future answer-cache key (#403) so the two stay
  aligned.
- Decide where the `content_version` for each retrieved row comes
  from — the unified record currently exposes one through the
  catalog, but vector/graph buckets need the same handle.
- Wire-up + integration test depend on the stubbable LLM transport
  refactor already deferred by #395/#396.

Verification (this slice):
- `cargo check -p reddb-io-server` clean.
- `cargo test -p reddb-io-server --lib runtime::ai::sources_fingerprint`
  → 14 passed.
