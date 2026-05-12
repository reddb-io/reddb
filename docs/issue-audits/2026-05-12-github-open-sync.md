# GitHub Open Issue Sync Audit — 2026-05-12

Scope: GitHub issues that were open after the post-crash sync pass.

Result after sync:

- GitHub open count: 47.
- `issues/done` ∩ GitHub open: empty.
- local `issues/*.md` ∖ GitHub open: empty.
- Stale local open copies removed for #415, #420, #421, and #423.
- #404 was restored from `issues/done/` to `issues/` because its own progress
  note says the provider-failover work is only a kernel slice and still blocked
  by #396.

## Open Issue Matrix

| Issue | Audit status | Decision |
| --- | --- | --- |
| #351 PRD: Parameterized queries | GitHub-only parent PRD. | Keep open; parent tracker. |
| #352 ADR: parameterized queries | GitHub-only HITL ADR. | Keep open; requires review/acceptance. |
| #356 Value variants end-to-end | Local open issue. | Keep open; value-envelope work remains. |
| #360 Postgres-wire extended protocol | GitHub-only HITL protocol issue. | Keep open; extended protocol not complete. |
| #372 Bun verification | Local open issue. | Keep open; local Bun verification previously blocked by runtime crash/segfault. |
| #373 Cross-driver golden fixtures | Local open issue. | Keep open; blocked by remaining value/driver contract work. |
| #374 Docs sweep parameterized | Local open with progress notes. | Keep open; multiple docs slices landed, sweep not complete. |
| #377 ADR: parameter contract | GitHub-only ADR issue. | Keep open; no accepted ADR found under `docs/adr/`. |
| #378 Engine binder | GitHub-only issue; binder code exists in `user_params.rs`. | Keep open; contract/value coverage still incomplete. |
| #379 Embedded Rust API | GitHub-only issue; `query_with` exists. | Keep open; `execute_with`/tuple API acceptance not complete. |
| #380 HTTP params | GitHub-only issue; local #358 landed a tracer slice. | Keep open; all value variants/docs/DML gaps remain. |
| #381 RedWire QueryWithParams | GitHub-only issue; JS client code exists. | Keep open; server-side `crates/reddb-wire` frame/codec is not fully present in `main`. |
| #382 Postgres-wire Parse/Bind/Execute | GitHub-only issue. | Keep open; extended protocol remains unimplemented. |
| #383 JS/TS/Bun driver params | GitHub-only rollup. | Keep open; Bun verification #372 remains open. |
| #384 Python driver params | GitHub-only rollup; local #362 delivered `query`. | Keep open; rollup also asks `execute`/stubs/live integration. |
| #385 Go driver params | GitHub-only rollup; local #363/#359 delivered query paths. | Keep open; rollup also asks `Exec`. |
| #386 Rust external driver params | GitHub-only rollup; local #364 delivered client `query_with`. | Keep open; RedWire external crate path is still incomplete. |
| #387 Java/.NET/Kotlin params | GitHub-only rollup; child slices landed. | Keep open until live integration confidence is explicit. |
| #388 PHP/Dart/C++/Zig params | GitHub-only rollup; child slices landed. | Keep open until live integration confidence is explicit. |
| #389 CLI params + MCP + docs | GitHub-only rollup; #375/#376 landed. | Keep open; docs sweep #374 is still open. |
| #391 ASK PRD | GitHub-only parent PRD. | Keep open; parent tracker. |
| #395 Strict citation validation | Local open with progress. | Keep open; wiring remains. |
| #396 Provider capability registry | Local open with progress. | Keep open; blocked by #395. |
| #398 RRF hybrid retrieval | Local open with progress. | Keep open; not all retrieval slices landed. |
| #399 RLS-respecting ASK retrieval | Local open issue. | Keep open; blocked by #398. |
| #400 ASK determinism | Local open with progress. | Keep open; branch exists but conflicts/needs integration. |
| #401 Cost guards | Local open with progress. | Keep open; daily cap/follow-ups remain. |
| #402 ASK audit | Local open with progress. | Keep open; branch conflicts/needs integration. |
| #403 ASK cache | Local open with progress. | Keep open; blocked by #402. |
| #404 Provider failover | Local open restored from `done`. | Keep open; kernel slice only, blocked by #396. |
| #405 ASK stream SSE | Local open with progress. | Keep open; blocked by #395. |
| #406 Embedded stdio ASK | Local open with progress. | Keep open; JSON-RPC envelope landed, JS SDK/streaming remains. |
| #407 ASK via gRPC | Local open with progress. | Keep open; partial deep module only. |
| #408 ASK via Postgres-wire | Local open with progress. | Keep open; non-stream path not complete. |
| #409 ASK MCP tool | Local open with progress. | Keep open; partial parser/tool work only. |
| #410 Replica ASK audit forward | Local open issue. | Keep open; blocked by #402. |
| #411 EXPLAIN ASK | Local open with progress. | Keep open; partial plan builder only. |
| #412 ASK docs sweep | Local open issue. | Keep open; blocked by #405/#408/#409. |
| #419 Surface inserted ids | Local open; active Ralph worktree. | Keep open until Ralph result is reviewed and merged. |
| #422 GRAPH algorithm LIMIT/ORDER BY | Local open with progress; active Ralph worktree. | Keep open; centrality `LIMIT` slice landed, rest remains. |
| #424 Multi-row graph INSERT | Local open issue. | Keep open; depends on #419 id return semantics. |
| #425 Graph bulkInsert drivers | Local open issue. | Keep open; depends on #419/#424 semantics and driver work. |
| #426 HTTP client degraded health bug | GitHub-only new bug. | Keep open; no local issue/worktree yet. |
| #427 RedWire SELECT missing rows bug | GitHub-only new bug. | Keep open; no local issue/worktree yet. |
| #428 gRPC FRAME_INVALID_LENGTH bug | GitHub-only new bug. | Keep open; no local issue/worktree yet. |
| #429 SDK remote schemes contract bug | GitHub-only new bug. | Keep open; no local issue/worktree yet. |
| #430 GHCR image private | GitHub-only distribution/docs issue. | Keep open; needs visibility/docs decision. |

