# ADR 0047 — `red ui` bridge: the UI is a single-transport client

Status: accepted
Date: 2026-06-08

> Batch note: ADRs 0047–0051 came out of one integration design session for
> wiring `reddb` (the `red` binary) to `red-ui` (`reddb-io/red-ui`). They are
> siblings; implementation is greenfield. The two repos stay **separate and
> independently released**, integrating only through the `red` CLI and the
> RedWire/API contract — never a shared Rust crate (ADR 0048).

## Decision

`red ui <uri>` launches the red-ui app and guarantees it **one**
browser-reachable transport: RedWire-over-binary-WebSocket (ADR 0036). The
`red` binary — and the `@reddb-io/ui` node CLI — is the **launcher + bridge**.

- For any target the browser cannot reach natively — `file://` (embedded
  engine), `red://` / `reds://` (RedWire-over-TCP/TLS), and any future
  `anyscheme://` — `red` stands up a local RedWire-over-WS endpoint
  (`ws://127.0.0.1:<port>`) over that target and points the UI at it.
- When the target already exposes a browser-reachable surface (`red+wss://`,
  a `*.db.reddb.io` instance, a container's WS edge), `red ui` connects the UI
  **directly** — the same path the standalone `ui.reddb.io` uses. No local hop.

The UI never learns a second protocol. Every new scheme is a new translator
inside `red`; the UI is untouched.

## Why

The UI always runs in a browser/webview sandbox, which can only speak HTTP/WS.
Two alternatives fail:

- Teaching the UI every protocol (raw RedWire-over-TCP, the file engine) is
  impossible in-browser and couples the UI to engine internals.
- Pointing a remote/hosted page at a local `file://` cannot work — the browser
  has no file access, and there is no server to point it at.

Making `red` the universal bridge gives the UI exactly one transport to
support forever, makes `file://` work at all, and turns the original
`anyprotocolwehave://` requirement into extensibility for free.

## Considered Options

- **`red` is a universal bridge; UI is a single-transport client (chosen).**
  One UI transport; all scheme complexity absorbed by `red`.
- **Always interpose a local bridge, even for WS-native targets (rejected).**
  A pointless local hop; the bridged path stops sharing code with the
  standalone direct path.
- **Teach the UI all protocols (rejected).** Impossible in-browser; couples
  the UI to engine internals.

## Consequences

- The bridge process lives for the session; in `--server` and injected-auth
  modes `red` must stay alive while the UI window is open (ADR 0051).
- `file://` requires `red` — either the user's installed binary (via `red ui`)
  or the desktop app's bundled sidecar (ADR 0048).
- Direct-when-reachable means `red ui red+wss://…` exercises the exact path
  the standalone UI uses; the bridge is a narrow shim for genuinely
  unreachable targets only.
- The local bridge must serve RedWire-over-WS over the embedded engine, not
  just proxy an existing HTTP surface (ADR 0049).

## Related

- ADR 0036 — Unified async connection model (RedWire-over-WebSocket)
- ADR 0046 — Wire and file crate authority boundary
- ADR 0048 — Desktop self-sufficiency via bundled `red` sidecar
- ADR 0049 — UI canonical transport is RedWire-over-WebSocket
- ADR 0051 — `red ui` invocation: dispatch, surfaces, auth
