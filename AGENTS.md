# Repository Guidelines

Behavioral guidelines to reduce common LLM coding mistakes. Merge these with explicit user instructions and repository-specific constraints.

Tradeoff: these guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

Do not assume. Do not hide confusion. Surface tradeoffs before implementing.

- State assumptions explicitly.
- If multiple interpretations exist, present them instead of choosing silently.
- If a simpler approach exists, say so and push back when warranted.
- If something is unclear, stop, name the ambiguity, and ask.

For RedDB, be especially careful with transaction semantics, WAL behavior, locking, persistence ordering, tenancy, RLS, and wire compatibility. This is a database engine; correctness is more important than convenience.

## 2. Simplicity First

Write the minimum code that solves the problem. Nothing speculative.

- No features beyond what was requested.
- No abstractions for single-use code.
- No extra flexibility or configurability unless asked for.
- No error handling for impossible scenarios.
- If 200 lines can be 50, rewrite it.

Ask: would a senior engineer call this overcomplicated? If yes, simplify. Prefer explicit, boring code in storage and query paths over clever compactness.

## 3. Surgical Changes

Touch only what you must. Clean up only your own mess.

When editing existing code:

- Do not improve adjacent code, comments, or formatting without a task-driven reason.
- Do not refactor code that is not part of the request.
- Match the local style, even if you would choose differently.
- If you notice unrelated dead code, mention it; do not delete it.

When your change creates orphans:

- Remove imports, variables, and functions made unused by your edit.
- Do not remove pre-existing dead code unless asked.

Test every diff line against the request: each changed line should trace directly to the task.

## 4. Goal-Driven Execution

Define success criteria and loop until verified.

- "Add validation" -> add tests for invalid inputs, then make them pass.
- "Fix the bug" -> reproduce it with a test, then make it pass.
- "Refactor X" -> verify behavior before and after.

For multi-step work, state a short plan with verification points:

1. Step -> verify with a concrete check.
2. Step -> verify with a concrete check.
3. Step -> verify with a concrete check.

Strong success criteria enable independent execution. Weak requests like "make it work" require clarification.

## Project Structure & Commands

`src/` contains the Rust engine and server surfaces. Focus areas include `src/storage/`, `src/application/`, `src/presentation/`, `src/cli/`, and `src/grpc/`. Cross-module and end-to-end coverage lives in `tests/`, including `integration_*.rs`, `e2e_*.rs`, `smoke_*.rs`, and `tests/regress/`. Supporting material lives in `docs/`, `examples/`, `testdata/compose/`, `drivers/`, and `sdk/`.

Prefer the `Makefile` wrappers:

- `make check` for a quick compile check.
- `make test` or `make test-fast` for the default local test layer.
- `make test-persistent` for ignored persistent multimodel tests.
- `make test-env PROFILE=replica` for Docker-backed environment validation.
- `make fmt` and `make lint` for formatting and clippy.
- `make run ARGS='--path ./data.db --bind 127.0.0.1:8080'` to run locally.

Run `cargo check` after non-trivial edits. Add unit tests next to local behavior and use `tests/` for cross-module or persistence scenarios. If `graphify-out/` is present and code changed, update it before finishing.

## Style, Commits, and PRs

Rust uses edition 2021 and `rustfmt.toml` sets `max_width = 100`. Use `snake_case` for files, modules, and functions; `PascalCase` for types and traits; and `SCREAMING_SNAKE_CASE` for constants.

Recent commits follow Conventional Commit style such as `feat(views): ...`, `fix(timeseries): ...`, `docs: ...`, and `chore: ...`. Keep commit subjects imperative and scoped when useful. PRs should describe the behavioral change, note storage or compatibility risks, link the issue when available, and list the verification commands that were run.

## Security & Configuration

Never commit provider secrets or local credentials. Use environment variables such as `REDDB_AI_PROVIDER` and `REDDB_{PROVIDER}_API_KEY`. Avoid editing generated or runtime artifacts like `target/`, `wal/`, and ad hoc local database files unless the task explicitly requires it.
