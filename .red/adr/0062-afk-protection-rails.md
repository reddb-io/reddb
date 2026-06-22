# ADR 0062 — AFK protection rails (drift-guard, triage, instruction injection)

Status: accepted
Date: 2026-06-22

Implements the protection layer discussed in `.red/agents/domain.md` →
"reddb is a database engine; correctness is more important than convenience."
Layers on top of ADR 0059 (binding merge gate) without overlapping it.

## Context

The AFK harness runs every attempt inside a sandcastle subprocess, with
the policy layer (issue claim, handoff, feedback gate, merge, close)
controlled by `reddb-io/red-skills` and configured per-project from
`.red/config.yaml`. Three knobs accept custom commands:

- `--request` / `-r` — text injected into the agent prompt (cheap nudge).
- `plugins.dev.afk.backpressure` — shell commands that run after the
  agent's automatic feedback loop, before the merge. If any exit non-zero,
  the merge is BLOCKED and the issue routes to `ready-for-human` with
  `blocked:validation`. Command + output land in the envelope.
- `plugins.dev.afk.hooks.<point>` — lifecycle interceptors at named
  points: `pre_session`, `pre_pick`, `post_pick`, `pre_worktree`,
  `pre_attempt`, `post_attempt`, `on_attempt_error`, `pre_merge`,
  `post_merge`, `on_idle`, `post_session`, `on_session_error`.

What we already have (don't duplicate):

- ADR 0059 — `main` is protected; CI is a hard gate with `enforce_admins`.
- `[workspace.lints.clippy]` — `unwrap_used` (deny), `too_many_lines` warn
  at 200 (`clippy.toml`), `cast_possible_truncation` warn. House limits
  are 2000/file and 200/function; the 2000/file half has no clippy lint
  and is enforced by the drift-guard below.
- `scripts/lint-no-untyped-serialization.sh` + whitelist — six
  boundary-discipline categories.
- `scripts/check-afk-validation-diff.mjs` — AFK validation sidecar vs diff.
- `.githooks/pre-push` — `cargo fmt --check`.

The gap: these gates fire either at commit/push time (local, fast) or in
CI (slow, after the harness already merged). We want a **third tier**: a
local-fast fail that runs IN THE HARNESS, after the agent finishes but
before CI burns a round. That's the AFK backpressure + hooks surface.

## Decision

Wire four protection hooks via `.red/config.yaml` plus three shell
scripts in `scripts/`:

### 1. `hooks.pre_pick` → `scripts/afk-pre-pick.sh`

Triage filter. Reads the candidate queue from the context JSON, drops
issues carrying `do-not-pick` or `blocked:external`, requires both
`size:*` AND `priority:*` (the triage minimum per
`.red/agents/triage-labels.md`), and reorders ascending by `priority:*`.

Defensive: a broken pre_pick (jq/gh error, malformed JSON) MUST return
the original queue. A triage layer that starves the harness is worse
than no triage — that is the lesson of the admin-merge bypass
(ADR 0059).

### 2. `hooks.pre_attempt` → `scripts/afk-pre-attempt.sh`

Instruction injection. Adds a standard reminder to every attempt's
mutable context:

- Follow `STYLE.md` (TigerStyle subset, ADR 0056).
- Run `scripts/lint-no-untyped-serialization.sh` and `cargo fmt --all`
  before committing.
- Don't introduce bare `.unwrap()` outside the whitelist (ratchet).
- Don't touch `docs/conformance/`, `testdata/conformance/`, contract
  matrix scripts, or `.red/adr/`. CODEOWNERS blocks them and the
  pre_merge drift-guard rejects the PR.
- Don't add a Cargo dependency without a `# track: issue-XXXX`
  comment on the same line.
- Don't try `gh pr merge --admin` — `enforce_admins` is on.

Cheaper than `--request -r` on every /afk invocation. The
`--request` mechanism STILL wins (appended after), so per-run
overrides remain possible.

### 3. `hooks.pre_merge` → `scripts/afk-pre-merge.sh` (the drift-guard)

Hard gate before merge. Rejects the attempt on any of:

1. **Scope drift** — more than 25 files changed in the diff.
2. **Per-file size** — any file ADDED or MODIFIED in the diff whose
   post-merge line count exceeds **2000 lines** is blocked. This is
   the file-shape half of the house limit (2000 per file, 200 per
   function). The function half (200) is enforced by
   `clippy::too_many_lines` (warn at threshold 200 in `clippy.toml`)
   plus the backpressure `cargo clippy -D warnings` run; clippy
   doesn't have a per-file-size lint, so the drift-guard is the only
   enforcement point for the 2000/file half.
3. **Protected paths** — modifications (not additions) under
   `docs/conformance/`, `testdata/conformance/`, the contract matrix
   scripts, or `.red/adr/`. New ADRs and new conformance fixtures are
   legitimate flow; only edits to existing files trigger the gate
   (mirrors what CODEOWNERS rejects anyway, as a local fast-fail).
4. **New `.unwrap()`** — added `.unwrap()` in a file NOT on
   `scripts/lint-untyped-serialization-whitelist.txt`.
5. **New Cargo dep** — added `name = ...` in `Cargo.toml` without a
   `# track: issue-XXXX` comment on the same line.

Thresholds are deliberately conservative — a drift-guard that fires
too often gets disabled. Tune UP only after looking at a month of
false positives.

### 4. `backpressure` (list of gates)

Three commands run AFTER the agent's automatic feedback loop
(`pnpm test`/cargo check/etc. on touched packages), BEFORE merge:

```yaml
backpressure:
  - cargo fmt --all -- --check
  - scripts/lint-no-untyped-serialization.sh
  - cargo clippy --workspace --locked --all-targets -- -D warnings
```

The sidecar check (`check-afk-validation-diff.mjs`) is NOT in
backpressure: it's already in CI under `afk-validation-sidecar` and
adding it here would double-run. Keep CI's coverage of sidecar
contract as the canonical run; the harness-internal coverage from
backpressure is for the cheap local gates only.

### 5. `hooks.on_idle` (maintenance)

Prunes worktrees that lost their branch and deletes orphan `afk/*`
branches. Defaults handle `cargo`/`gradle` build-dir cleanup; this is
git-state cleanup. A broken `on_idle` only logs — by design.

### What we deliberately did NOT do

- **Pre-merge gate on `main` being green.** Tracked as a follow-up. It
  would freeze the AFK until the gate from ADR 0059 desgelates; given
  that at adoption `main` was red (#974), enabling this NOW kills the
  AFK. Adopt AFTER the green ratchet is unstuck.
- **External notifiers (`post_merge`, `on_session_error`).** Wired in
  config as commented-out templates; not enabled until we have a
  destination (Slack channel / pager rotation) the on-call actually
  reads.
- **Cargo-deny / gitleaks / trufflehog.** Useful, but each is an extra
  tool dependency. Add to backpressure once the install path is
  documented (`install.sh`) and CI also runs them — otherwise we
  duplicate the policy layer.
- **Modifying sandcastle.** All the substrate-level knobs (sandbox
  timeout, worktree isolation, stream capture) live in red-castle and
  are out of scope here.

## Consequences

- Per-issue cost goes UP slightly (one bash invocation per hook point
  per attempt) and DOWN significantly when a guard catches a bad
  attempt before CI burns a round.
- `pre_merge` rejection = the issue routes to `ready-for-human` with
  `blocked:validation`. The envelope contains the script output. No
  silent drop.
- `pre_pick` quietly filters the queue. If a triage label gets
  dropped from `.red/agents/triage-labels.md` without a matching
  update to `afk-pre-pick.sh`, issues will silently fail to be
  picked. Document the link.
- The drift-guard's thresholds live in `scripts/afk-pre-merge.sh`,
  not in `.red/config.yaml`. We chose script > config because the
  guard has regex and needs to be unit-testable without booting the
  harness.

## Failure mode hierarchy (the AFK loop contract)

| Layer | Failure means | Severity | Hook contract |
|---|---|---|---|
| `pre_pick` | parse / gh error | Low | Pass-through, never starve |
| `pre_attempt` | jq error | Low | Pass-through (return original ctx) |
| `pre_merge` | any check fails | High | Block merge, envelope output |
| `backpressure` | any command != 0 | High | Block merge, envelope output |
| `post_merge` / `on_*` | any command != 0 | None | Log only — never starve |

This is the same hierarchy as the harness itself (ADR 0059 lesson):
**a broken protection layer must not become the new outage mode.**

## Phasing

1. **Now (this ADR):** `pre_pick`, `pre_attempt`, `pre_merge`,
   `backpressure`, `on_idle`. All defaults ON.
2. **After main is green again:** enable `pre_worktree` check that
   skips issues when `origin/main` is red.
3. **After install.sh ships gitleaks/cargo-deny:** add them to
   `backpressure` and to CI in parallel.
4. **After a notification destination exists:** uncomment the
   `post_merge` / `on_session_error` notifier templates.

## Reversibility

Every hook and every backpressure entry is referenced by path in
`.red/config.yaml`. To roll back, revert that file (or comment the
entry). No harness change, no migration, no data loss.
