# ADR 0007 — npm package matrix (`cli` / `sdk` / `client`) and pinned-binary rationale

**Status:** Accepted
**Date:** 2026-05-06
**Supersedes:** —
**Superseded by:** —
**Related issues:**
[#132](https://github.com/reddb-io/reddb/issues/132) (this ADR),
[#133](https://github.com/reddb-io/reddb/issues/133) (asset-fetcher),
[#134](https://github.com/reddb-io/reddb/issues/134) (bin-resolver),
[#135](https://github.com/reddb-io/reddb/issues/135) (version-compare),
[#136](https://github.com/reddb-io/reddb/issues/136) (client package),
[#137](https://github.com/reddb-io/reddb/issues/137) (release pipeline),
[#138](https://github.com/reddb-io/reddb/issues/138) (docs).

## Context

Today the npm side of RedDB ships a single root package — `@reddb-io/cli`
(formerly squat-blocked `reddb-cli`) — and a sibling driver package —
`@reddb-io/sdk` (formerly `reddb`) — that lives in `drivers/js/`. Both
packages run a `postinstall` script that downloads the matching `red`
binary from the GitHub release whose tag equals the package version
and drops it under the package's own `bin/` directory.

That single-script-shared-by-both-packages shape conflates three
distinct install scenarios:

1. **Operator installing the server CLI** (`npm i -g @reddb-io/cli`).
   Wants `red` on `PATH` and is fine sharing one global binary across
   versions. Will frequently re-install over an existing `red` already
   on `PATH` — possibly newer than the package being installed.
2. **App developer adding the full SDK with embedded engine**
   (`pnpm add @reddb-io/sdk`). Talks to the binary over an in-process
   stdio JSON-RPC channel whose framing/version handshake assumes the
   binary speaks the *same* wire revision as the JS code. A `PATH`
   binary at the wrong version silently misframes messages.
3. **Edge / serverless app needing only a thin RPC client** (target
   future package `@reddb-io/client`). Talks to a *remote* RedDB over
   the network; the only "binary" it carries is `red_client`, much
   smaller than the embedded server. Bundling the full server binary
   into a Lambda layer or a Cloudflare Worker container is wasteful.

The single shared `postinstall.js` cannot serve all three. It always
downloads regardless of `PATH` (#1 wastes a re-download on every
upgrade), it assumes the embedded server (#3 has no use for the server
binary), and it has no version-comparison logic (#1 has no way to
"upgrade if older / skip if newer").

The wire-format coupling for #2 is load-bearing. The embedded factory
spawns the binary as a child process and exchanges length-prefixed
JSON-RPC frames whose schema evolves with the server. A user with
`/usr/local/bin/red` from a previous release on `PATH` and a freshly
installed `@reddb-io/sdk` would see the SDK pick up the stale `PATH`
binary if any fallback path exists. The failure mode is silent — RPC
calls hang or return malformed responses — and bisecting it back to
"wrong binary on `PATH`" is a multi-hour support ticket.

## Decision

Ship **three** npm packages under the `@reddb-io/` scope, each with a
single, distinct contract for how it acquires the `red` binary on
install and how it locates it at runtime.

### Package matrix

| Package              | Audience                                          | Binary acquired       | Install size budget | Runtime lookup                                                                             |
| -------------------- | ------------------------------------------------- | --------------------- | ------------------- | ------------------------------------------------------------------------------------------ |
| `@reddb-io/cli`      | Operators, dev laptops, CI runners                | `red` to *global PATH* | ≤ 12 MB compressed  | Global `PATH` (delegated to `npm`/`pnpm` global bin dir)                                   |
| `@reddb-io/sdk`      | App devs running embedded engine in-process       | `red` pinned in pkg    | ≤ 12 MB compressed  | `REDDB_BIN` env → `node_modules/@reddb-io/sdk/bin/red`. **`PATH` not consulted.**          |
| `@reddb-io/client`   | Edge / serverless / sidecar talking to remote RedDB | `red_client` pinned in pkg | ≤ 5 MB compressed   | `REDDB_CLIENT_BIN` env → `node_modules/@reddb-io/client/bin/red_client`. **`PATH` not consulted.** |

The three packages share three workspace-only deep modules (private,
not published) that encapsulate the install-time and runtime-time
disciplines:

- `@reddb-io/internal-asset-fetcher` (#133) — platform/arch detection,
  GitHub release URL composition, redirect-following download,
  optional sha256 verification. One pure function:
  `fetchReleaseAsset({ repo, tag, platform, arch, binName, sha256? }) → Buffer`.
- `@reddb-io/internal-bin-resolver` (#134) — runtime lookup for
  SDK/client. One pure function: `resolveBin({ name, packageRoot, envVar }) → string`.
  Honours `env → <packageRoot>/bin/<name>` and **never** consults
  `PATH`.
- `@reddb-io/internal-version-compare` (#135) — install-time decision
  for CLI. One pure function:
  `compareInstalled({ packageVersion, exec }) → { action, reason }`
  returning `install` | `upgrade` | `skip`.

### CLI postinstall: install / upgrade / skip

`@reddb-io/cli` is allowed to consult `PATH` because it *targets*
`PATH`. Its postinstall consults `compareInstalled`:

```
PATH `red` absent          → action: install    (download to global bin dir)
PATH `red` older than pkg  → action: upgrade    (download, overwrite, log one line)
PATH `red` equal           → action: skip       (log "already up to date")
PATH `red` newer than pkg  → action: skip       (log "PATH binary is newer; leaving in place")
PATH `red` malformed       → action: install    (log warning, treat as absent)
exec failure               → action: install    (log warning, treat as absent)
```

The asymmetry vs SDK/client is intentional: the operator scenario
shares one `red` across many CLI versions, and silently re-downloading
on each `npm i -g` upgrade churns disk and network for no behaviour
change. The version comparison gates the network call.

`REDDB_SKIP_POSTINSTALL=1` short-circuits the whole flow (CI cache
warming, air-gapped installs). `REDDB_BIN` is honoured as an explicit
override that suppresses both the version compare and the download.

### SDK / client runtime lookup: env → local, no PATH

`@reddb-io/sdk` and `@reddb-io/client` *pin* their binary inside
`node_modules` and resolve it through `@reddb-io/internal-bin-resolver`
with this precedence:

1. The package-specific env var (`REDDB_BIN` for SDK, `REDDB_CLIENT_BIN`
   for client). If set, the value is used verbatim — no existence
   probe, no fallback. The env var is the user's "I know what I'm
   doing" override.
2. `<packageRoot>/bin/<binName>` — where `packageRoot` is the on-disk
   location of the consuming package (`node_modules/@reddb-io/sdk`,
   `node_modules/@reddb-io/client`) and `binName` is `red` or
   `red_client`.
3. **No further fallback.** If neither (1) nor (2) yields a usable
   path, `resolveBin` throws an actionable error naming the env var,
   the expected local path, and a one-line `pnpm install` hint.

`PATH` is deliberately omitted. The wire-format coupling between the
SDK and the embedded engine (and between the client and the remote
server's wire revision, indirectly via the released
`red_client` binary) is too tight to silently fall back to whatever
`red` happens to live on the machine. A version-mismatched binary
fails in misframed-RPC mode, not in "command not found" mode — making
silent fallback actively harmful.

## Alternatives considered

### A. One package with conditional behaviour

Keep `@reddb-io/sdk` only and have it expose a CLI bin alongside the
runtime API. Rejected:

- Drags the full embedded server binary into edge/serverless deployments
  whose only need is the thin client. Lambda layers and Worker
  containers blow past size budgets.
- Forces the operator-CLI install scenario through a flow whose only
  legitimate use case is "downloaded, and pinned forever to this
  install" — there is no `PATH`-binary-already-newer branch.
- Makes `npm i -g` semantics fight `pnpm add` semantics: one wants a
  global `PATH` placement, the other wants a `node_modules`
  placement. The same package cannot do both honestly.

### B. Bundle binaries into all three packages but vary lookup order

Have all three packages ship the binary, with SDK/client preferring
local and CLI preferring `PATH`. Rejected: still wastes the operator
scenario's bandwidth on every `npm i -g` upgrade (no version-compare),
and still gives edge/serverless the full server binary (`red`) when
they only want `red_client`. The asymmetry between operator and
app-dev install scenarios is a fundamental one — pretending it is
not just pushes the asymmetry into runtime config.

### C. Allow `PATH` fallback in SDK/client

Cheap "do what the operator means" ergonomics. Rejected on the
grounds spelled out in Context above: silent wire-format mismatch is
worse than a clear error message, and the env-var override
(`REDDB_BIN` / `REDDB_CLIENT_BIN`) is already the documented escape
hatch for environments that block postinstall scripts.

### D. Publish binaries to npm directly (per-platform packages with
`optionalDependencies`, esbuild-style)

Eliminates the postinstall entirely; npm picks the right
`@reddb-io/sdk-linux-x64` automatically. Rejected for this slice — the
infra cost (six new published packages per release, six new release
jobs, npm scope rate limits) does not yet pay for itself given our
release cadence. Reconsider if the GitHub-release download path
becomes a sustained reliability problem.

## Consequences

**Wins.**

- App developers can never silently bind to a stale `PATH` binary.
  The SDK and client lookups are deterministic: env, then local,
  then a clear error.
- Operators upgrading the CLI no longer pay a network round-trip when
  their `PATH` binary is already at or above the package version.
- The edge/serverless scenario gets a `< 5 MB` package shipping only
  the thin client.
- The three workspace-only deep modules (asset-fetcher, bin-resolver,
  version-compare) are individually testable. Today's 130-line
  inlined `postinstall.js` is not.

**Costs.**

- Three published packages per release instead of two. The release
  pipeline (#137) gains a `publish-client` job and a fan-out matrix
  for the `internal-*` packages (which stay private).
- Two env vars (`REDDB_BIN`, `REDDB_CLIENT_BIN`) instead of one
  catch-all (`REDDB_BINARY_PATH`). The legacy name remains honoured
  by SDK during a deprecation window — see #134.
- Documentation churn: README, getting-started, and the JS/TS driver
  guide all need a "which package do I want?" section (#138).

**No public API change for SDK runtime callers.** `connect()` and
related helpers keep the same signatures; only the binary-lookup
plumbing changes. The CLI `bin` entry (`reddb-cli`) is unchanged.

## Validation

- ADR review against #133, #134, #135 acceptance criteria for
  consistency on env-var names, lookup precedence, and error messages.
- Cross-link check: every implementation slice (#133–#138) references
  back to this ADR.
- Format match against `0004-red-client-container-image.md` and
  `0005-entity-cache-sharded-lru.md` (Status / Date / Context /
  Decision / Alternatives / Consequences / Validation).
