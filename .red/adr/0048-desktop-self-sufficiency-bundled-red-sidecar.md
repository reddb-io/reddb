# ADR 0048 — Desktop self-sufficiency via a bundled `red` sidecar

Status: accepted
Date: 2026-06-08

Part of the integration batch 0047–0051.

## Decision

The red-ui desktop app (Tauri 2) is **self-sufficient** for every target,
including `file://` — but it achieves this by **bundling the `red` binary as a
Tauri sidecar**, not by linking reddb's engine crate.

- To open `file://` (or any non-WS target) the desktop app spawns its bundled
  `red` sidecar, which stands up the local RedWire-over-WS bridge (ADR 0047);
  the app then connects to it like any WS endpoint.
- For WS-native targets the app connects directly.

reddb and red-ui remain separate repos with independent release cycles. Their
only coupling is **packaging**: a desktop release embeds a pinned `red` build.
There is no shared Rust crate.

## Why

"Self-sufficient like a browser" for `file://` requires engine code somewhere.
Linking reddb's engine crate into the desktop's Rust backend would couple the
two repos at compile time and force red-ui to track engine internals — against
the project independence premise. A sidecar gives the same self-sufficiency
(the app ships everything it needs, no external install required) while keeping
the coupling loose, versioned, and at the packaging layer. The `red` binary is
already the "all-powerful" bridge (ADR 0047); reuse it rather than re-link its
engine into a second Rust app kept in lockstep.

## Considered Options

- **Bundle `red` as a Tauri sidecar (chosen).** Self-sufficient, no crate
  coupling, integrates via the same CLI + API contract.
- **Link reddb's engine crate (rejected).** Slightly leaner (no child
  process) but compile-time couples the repos and tracks engine internals.
- **Require an external `red` on `PATH` (rejected).** Not self-sufficient; the
  desktop app would silently depend on a separate install.

## Consequences

- A desktop release carries a `red` binary per platform; its size and the
  sidecar's engine version are owned by red-ui's release.
- When the app is opened **standalone** on a local file, the bundled sidecar's
  engine version reads it; when launched via `red ui` (ADR 0051) the user's
  installed `red` may host the bridge instead. File-format compatibility
  between the two `red` versions is a separate concern (the `.rdb` format
  carries its own versioning).
- No shared crate means no lockstep build dependency between the repos.

## Related

- ADR 0047 — `red ui` bridge: the UI is a single-transport client
- ADR 0050 — red-ui distribution: pinned, checksummed, downloaded
- ADR 0051 — `red ui` invocation: dispatch, surfaces, auth
