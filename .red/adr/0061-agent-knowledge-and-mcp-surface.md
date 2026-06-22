# Agent-Facing Knowledge & MCP Surface

Status: accepted

## Context

AI agents consistently misunderstand RedDB. They assume the connection model is
"only gRPC" or "only WebSocket" and miss that **RedWire (`red://` / `reds://`) is
the principal transport**. They cannot learn the RQL dialect (RedDB has its own
SQL surface) without reading the entire codebase, and they have a weak grasp of
the value type system, the multi-model surface (documents, key-value, queues,
graph, vault, config, RQL-tabular), and the deployment topologies
(embedded/standalone, serverless, primary-replica, cluster).

A `red mcp` server already exists (built into the `red` Rust binary, stdio, ~30
**operational** tools). Extensive `docs/` exist but agents don't consume them
(they read source instead). The gap is **conceptual understanding + discovery**,
not operational tooling.

## Decision

**One authoritative MCP surface = the native Rust `red mcp`.** It stays in
lockstep with the engine and is the single home for both operation (the existing
tools) and the new **knowledge** surface. We do **not** re-implement an MCP in
JS — there must be exactly one tool/knowledge vocabulary.

1. **Distribution — `@reddb-io/mcp` npx launcher.** A thin package whose only job
   is to resolve/download the matching `red` binary (reusing the existing
   `@reddb-io/internal-bin-resolver` / `internal-asset-fetcher`, which fetch from
   GitHub Releases) and spawn `red mcp`. `npx -y @reddb-io/mcp` ⇒ always the
   latest engine, zero-install, any Node host. Other languages use the native
   binary directly. This avoids stale local installs and duplicated logic.

2. **One connection knob — `--uri` / `REDDB_MCP_URI`.** The scheme decides the
   mode, and that mapping *is* the connection lesson:

   | URI | Mode | Path |
   |---|---|---|
   | `memory://` (default) | embedded, ephemeral | `red` binary (the engine itself) |
   | `file:///path` | embedded, persisted | `red` binary (`--path`) |
   | `red://` / `reds://` | remote RedWire | `red mcp --url` (client via `reddb-io-client`) |
   | `grpc://` / `http(s)://` | remote (other transports) | `red mcp --url` |

   Because the launcher spawns the real binary, `memory://`/`file://` are genuine
   embedded engine instances (not mocks) — which is why `memory://` is the
   zero-config default. `red mcp` gains a remote `--url` client mode so one tool
   surface spans embedded↔cluster.

3. **Knowledge content — hybrid, generated-from-source where it can drift.** The
   volatile facts are emitted from the engine's own authorities — RQL
   keyword/function catalog from `reddb-io-rql` (lexer/parser authority), type
   catalog from `reddb-io-types`, supported schemes/transports from the connection
   layer — so they are provably exact and auto-update. The conceptual narrative
   (topologies, "when `red://` vs `grpc://`", mental models) is hand-authored
   because it is judgment, not extractable. The data surface is layered: a
   multi-model map (documents/KV/queue/graph/vault/config/RQL-tabular) over the
   per-model type catalog.

4. **Delivery — resources + active tools + a single generated `llms.txt`.**
   - MCP **resources** for breadth (RQL grammar, type catalog, connection guide,
     topology guide).
   - MCP **active tools** for the high-friction spots: validate/explain an RQL
     query against the *real* `reddb-io-rql` parser, decode a connection URI →
     transport/topology, look up a type. The agent submits and the engine
     answers, instead of "read and hope."
   - A generated **`docs/llms.txt`**: because `reddb.io` is published from
     `docs/`, the same file is both in-repo and at `reddb.io/llms.txt`, with an
     `AGENTS.md` pointer. One generated source feeds MCP + `llms.txt` ⇒ no drift.

5. **Config — env-first, no secrets in committed config.** `.mcp.json` uses
   `${VAR:-default}` expansion and host-env inheritance, e.g.
   `"env": { "REDDB_MCP_URI": "${REDDB_MCP_URI:-memory://}" }`. Default is
   embedded `memory://`; a host-exported `reds://user:pass@…` enables remote
   without leaking credentials into the repo.

## Considered options (rejected)

- **JS-reimplemented MCP in the SDK** — two tool surfaces that drift apart; the
  very confusion we're fighting.
- **MCP only in `@reddb-io/sdk`** — that package is embedded-only (spawns the
  binary); it would teach embedded, not `red://`. Remote RedWire lives in
  `@reddb-io/client`.
- **Hand-authored docs only** — drift is the root cause of the problem.
- **MCP only, or `llms.txt` only** — each misses an audience (no-MCP agents vs
  MCP hosts). We serve both from one source.

## Consequences

- `red mcp` must grow: a remote `--url` client mode, knowledge resources +
  active tools, and the source-emission generators. The npx launcher is a new
  package (`@reddb-io/mcp`).
- **Phasing:** v1 = knowledge surface + embedded operation (already teaches
  `red://` via knowledge); the remote `--url` operation mode follows immediately
  after.
- **Deferred / not yet decided:** remote auth methods beyond `anonymous` +
  `bearer` (SCRAM/mTLS/OAuth); the precise definition of "serverless" among the
  taught topologies; the exact v1 cut.
- Related: ADR 0001 (RedWire TCP protocol), ADR 0036 (unified async connection
  model), ADR 0060 (operational telemetry substrate — same generate-from-engine
  discipline).
