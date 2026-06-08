# ADR 0050 — red-ui distribution: pinned, checksummed, downloaded not embedded

Status: accepted
Date: 2026-06-08

Part of the integration batch 0047–0051.

## Decision

`red` ships **no UI assets**. The `red ui --server` path (and the browser
fallback of ADR 0051) downloads the red-ui static bundle at runtime:

- The **exact** red-ui version is pinned at `red` build time by CI that tests
  the pair, and the bundle's **SHA-256** is recorded alongside the pin.
- `red` downloads the pinned release — a GitHub release asset, e.g.
  `red-ui …/releases/download/v<X.Y.Z>/ui-dist.tgz` — over HTTPS into
  `~/.cache/reddb/ui/<version>/`, verifies the SHA-256, and **refuses on
  mismatch**. Cached bundles are reused.

## Why

The premise: "ship the complete experience, but not necessarily all the
assets — download dynamically." A feature-rich UI bundle is heavy (red-ui
pulls `@huggingface/transformers`) and embedding it in every `red` is
undesirable. The pin makes the served version deterministic and CI-tested; the
checksum prevents a tampered or MITM'd bundle from running JavaScript against
the user's database — including local `file://` data. Independence of repos
does not require independence of runtime resolution: the pin is just CI
recording "tested against red-ui X.Y.Z."

## Considered Options

- **Download + cache, pinned, checksummed (chosen).** Smallest binary;
  deterministic; integrity guaranteed.
- **Embed the pinned bundle in `red` (rejected).** Binary bloat for a heavy
  UI; a UI fix still needs a `red` rebuild (the pin already implies that).
- **Embed a minimal UI, download the rich one (rejected).** Two UIs to
  maintain.
- Integrity — **SHA-256 pin (chosen)** vs signature/cosign (deferred, for a
  many-publisher / key-rotation future) vs HTTPS-only trust (rejected — leaves
  the door the premise asked us to close wide open).

## Consequences

- Offline first-run of `--server` fails with a clear message when no bundle is
  cached.
- The desktop app (ADR 0048) manages its **own** version and is **not**
  governed by this pin; this pin governs the `--server`/browser path only.
- A UI-only fix requires a new `red` release to move the pin for the
  `--server` path (acceptable trade for determinism; can be loosened to an
  API-version range later if needed).

## Related

- ADR 0047 — `red ui` bridge: the UI is a single-transport client
- ADR 0048 — Desktop self-sufficiency via a bundled `red` sidecar
- ADR 0051 — `red ui` invocation: dispatch, surfaces, auth
