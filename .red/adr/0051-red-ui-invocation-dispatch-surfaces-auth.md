# ADR 0051 — `red ui` invocation: dispatch, surfaces, and auth

Status: accepted
Date: 2026-06-08

Part of the integration batch 0047–0051.

## Decision

### Two entry points

- **`red ui <uri>`** opens a UI client *here* for the target.
- **`red server … --ui`** makes the running reddb server also serve the UI
  (the "everything on one box / reach it from the network" case).

### `red ui <uri>` dispatch (the deep-link seam)

`red ui <uri>` **canonicalizes** `<uri>` (resolving a relative `file://` to an
absolute path — the OS handler runs with a different cwd) and dispatches via
`xdg-open redui://?connect=<canonical-uri>` (or the OS equivalent).

- **Desktop app installed** (the `redui://` handler is registered) → the OS
  opens it; the app is self-sufficient (ADR 0048).
- **Not installed** → `red ui` falls back to the **`--server` path** (serve the
  pinned bundle on `127.0.0.1`, open the default browser) and nudges the user
  to install the desktop app. First-run always works.
- `red ui --server` forces the browser path; `red ui --desktop` forces the
  desktop download/install.

### `red server --ui` exposure

Serves the same pinned bundle (ADR 0050) on the server's HTTP surface for a
remote browser. Network reach is **opt-in** via the bind address (default
localhost); the served bundle is inert static assets, so exposing it is safe by
construction — auth lives on the data endpoint, not the asset path.

### Auth model — the database is the source of truth

- A DB **with** auth → the UI prompts (its own connect flow), wherever it runs.
- A DB **without** auth → no prompt.
- **Local-launch convenience:** when the user passes `red ui <uri> --token …`,
  `red` owns the credential and **injects** it — the UI runs in injected-auth
  mode (ADR 0036 bearer/JWT in the handshake), never sees the token, never
  persists it. The token **never** rides the deep-link URL; for the desktop
  path it crosses via a local secret channel (OS keychain / one-time loopback
  fetch), never `ps` / shell history / URL logs.

## Why

One magic command must cover file/local/remote without the user knowing the
mechanism, while never landing a secret somewhere greppable and never
re-asking for credentials the CLI already has.

- The deep-link seam lets `red ui` defer to the rich desktop app when present
  without re-implementing launch.
- The browser fallback guarantees first contact works: auto-installing a
  **native** app on first run hits OS security friction (Gatekeeper,
  SmartScreen, `sudo`/`apt`) — exactly where "magic" dies, and it can fail
  headless. The native app stays an opt-in upgrade we still incentivise
  (preferred whenever installed; upsell otherwise).
- DB-as-source-of-truth keeps the auth model uniform across local,
  server-served, and standalone, and matches red-ui's existing injected-client
  / connection-provider design (`injected-client-provider.ts`,
  `tauri-encrypted-store.ts`).

## Considered Options

- Dispatch — **deep-link + browser fallback (chosen)** vs auto-install the
  native app on first run (rejected — OS security friction, headless failure)
  vs prompt-and-stop (rejected — least magic).
- Auth handoff — **`red`-owned / injected + keychain-nonce (chosen)** vs token
  in the deep-link URL (rejected — leaks via `ps` / history / URL logs) vs
  UI-always-prompts (rejected — re-types credentials already given, kills the
  magic for authed targets).

## Consequences

- The desktop app owns `redui://` URL-scheme registration.
- `red ui` must canonicalize relative `file://` paths before handoff.
- In `--server` / injected-auth modes the bridge (ADR 0047) stays alive for
  the session.
- Two auth models coexist — injected (local launch with a token) and the UI's
  own connect flow (everything else) — each matching its trust context.

## Related

- ADR 0036 — Unified async connection model (browser auth in the handshake)
- ADR 0047 — `red ui` bridge: the UI is a single-transport client
- ADR 0048 — Desktop self-sufficiency via a bundled `red` sidecar
- ADR 0050 — red-ui distribution: pinned, checksummed, downloaded
