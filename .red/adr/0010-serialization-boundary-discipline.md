# ADR 0010 — Serialization boundary discipline: typed guards over ad-hoc formatting

**Status:** Accepted
**Date:** 2026-05-06
**Supersedes:** —
**Superseded by:** —
**Related issues:**
[#173](https://github.com/reddb-io/reddb/issues/173) (parent PRD: serialization boundary discipline),
[#174](https://github.com/reddb-io/reddb/issues/174) (audit slice),
[#175](https://github.com/reddb-io/reddb/issues/175) (this ADR),
[#176](https://github.com/reddb-io/reddb/issues/176) (HeaderEscapeGuard fix slice),
[#177](https://github.com/reddb-io/reddb/issues/177) (AuditFieldEscaper fix slice),
[#178](https://github.com/reddb-io/reddb/issues/178) (SerializedJsonField fix slice),
[#179](https://github.com/reddb-io/reddb/issues/179) (ConnStringSanitizer fix slice),
[#180](https://github.com/reddb-io/reddb/issues/180) (CI lint slice).

## Context

The Whiz / GitHub Babeld disclosure (March 2026) demonstrated a generic
class of bug: untrusted input concatenated into a structured
serialization format — an HTTP header, a log line, an audit entry, a
JSON payload, a frame field, a gRPC metadata key — without escaping the
format's delimiter lets an attacker smuggle "authoritative" control
structures past the producing process and into the parser at the other
end. The smuggled structure is indistinguishable from one the producer
itself emitted. Whatever trust the downstream parser places in the
producer transfers, unearned, to the attacker.

RedDB has the same shape on several surfaces. The HelloAck JSON
envelope and the topology base64 field defined in #166 both round-trip
caller-influenced strings. The audit emission in
`runtime/audit_log.rs` writes structured records that today still take
some fields through `format!`. The HTTP response handlers in
`server/handlers_*.rs` set header values whose source is request-side
input. The connection-string forwarding path in `reddb-wire::conn_string`
flows user-supplied strings into log lines and into gRPC metadata. The
gRPC interceptor chain and the PayloadReply assembly both interpolate
caller-influenced strings into framing the receiver trusts.

PRD #173 hardens these surfaces against the Babeld pattern. This ADR
records the architectural rule the PRD enforces, so future contributors
who add a new surface inherit the rule without re-deriving it.

## Decision

The producing side of every serialization boundary is owned by a typed
guard whose only job is to know the boundary's escape contract. User
input crosses the boundary by passing through the guard or it does not
cross at all. Four guards, one per boundary:

### 1. `HeaderEscapeGuard` — HTTP header values

`HeaderEscapeGuard` wraps the act of writing a header value. It rejects
CR, LF, NUL, and tab — the four bytes that let an attacker terminate
the current header and inject a second one — and surfaces a typed
error when a caller tries to set a value that contains them. Header
setters in `server/handlers_*.rs` and the gRPC metadata path do not
accept raw strings; they accept `HeaderEscapeGuard`-validated values.

### 2. `AuditFieldEscaper` — structured-only audit emission

The audit log emits structured records, not formatted strings. The
guard exposes typed-field setters; it does not expose a `format!`-style
sink. The compile-time consequence is that an audit field cannot
absorb an interpolated user-supplied substring — the field is a typed
value, the serializer owns the framing, and the framing is not
addressable from the call site.

### 3. `SerializedJsonField` — HelloAck / PayloadReply / topology JSON

Caller-influenced JSON fragments do not get concatenated into the outer
envelope as raw bytes. They round-trip through `serde_json::Value`
first: parsed, validated, re-serialized by serde. The
`SerializedJsonField` wrapper enforces this round-trip on the producing
side of HelloAck, PayloadReply, and the topology payload. A caller that
hands the wrapper a malformed or unexpected fragment surfaces a typed
error; a caller that hands it a well-formed fragment gets the bytes
serde would have written, not the bytes the caller might have hoped to
smuggle.

### 4. `ConnStringSanitizer` — `Tainted<String>` for connection strings

User-supplied connection strings enter the system inside a
`Tainted<String>` wrapper. The wrapper is not `Display`. It cannot be
written to a log macro, a header setter, a frame field, or a gRPC
metadata value directly. The only way out is `escape_for(boundary)`,
which re-serializes the string under the escape contract of the named
boundary and yields a value the boundary's guard accepts. The taint
propagates through the type system; a contributor who forgets to
sanitize gets a compile error, not a runtime smuggling bug.

### CI lint: `scripts/lint-no-untyped-serialization.sh`

A grep-based CI lint bans new `format!()` and `write!()` invocations
inside log macros, header setters, audit emissions, and frame-field
assembly. The lint runs on every PR. It is not perfect — grep is
syntactic, not semantic — but it catches the common shape, and every
hit forces the contributor either to switch to a typed guard or to
explain themselves on the whitelist.

## Whitelist process

`scripts/lint-untyped-serialization-whitelist.txt` is the escape valve.
Every entry requires:

- A comment explaining why the call site cannot use a typed guard
  today.
- A tracking issue number for the retrofit.

The whitelist is intentionally short. Every entry is a bug that has not
been fixed yet, not a sanctioned exception. Reviewers treat additions
to the whitelist as additions to a known-debt list: the entry merges,
the issue stays open, and the slice that closes it removes the entry.

## Consequences

**Benefits.**

- The Babeld pattern cannot regress on a guarded surface. The producer
  side does not have a path that lets an attacker-controlled byte
  reach a parser at the other end without crossing the boundary's
  escape contract first.
- New contributors inherit the rule by encountering the guard. A
  contributor who writes user input into a header, an audit field, a
  JSON envelope, or a connection-string forward gets a typed error or
  a CI failure, not a silent vulnerability.
- The four guards localise the escape contracts. There is one place to
  read for header escaping, one place for audit fields, one place for
  JSON round-trip, one place for connection-string taint. Auditing the
  cluster's escape posture is a four-file read.
- The whitelist makes residual debt visible. A surface that cannot yet
  use a guard is a tracked issue, not an invisible exception.

**Costs.**

- Existing call sites need to be retrofitted to the guards. The fix
  slices #176–#179 carry that work; until they land, the whitelist
  carries the residue.
- Contributors who want to interpolate a user-supplied string pay
  friction: wrap it in a guard, surface a typed escape error, or add
  to the whitelist with a tracking issue. The friction is intentional.
- The grep-based lint produces false positives on `format!` and
  `write!` calls that are obviously safe (constant strings, internal
  tracing). Whitelisting those is cheap, but it is a recurring cost.

## Open questions

- Whether the grep-based lint should graduate to a clippy custom rule
  once the maintenance burden of the grep approach proves insufficient.
  Grep is fine while the surface is small; a semantic rule scales
  better as more guards land. Not yet — revisit when the whitelist
  churn outpaces the cost of authoring a clippy lint.
- Whether the rule extends to `println!` and `eprintln!`. Probably yes
  for production code paths — those macros end up in operator-visible
  logs and inherit the same smuggling shape — and probably no for
  tests, where the cost of the discipline is not paid back. The lint
  slice (#180) settles the exact grep predicate.
- Whether the `Tainted<String>` pattern from `ConnStringSanitizer`
  should be extended to other user-input parsers — SQL identifiers,
  collection names, capability strings, anything else that a caller
  can shape and a downstream parser can be tricked by. Deferred to a
  future ADR; this ADR scopes the discipline to the four boundaries
  enumerated in PRD #173.

## Cross-links

- Parent PRD: [#173](https://github.com/reddb-io/reddb/issues/173)
- Audit slice: [#174](https://github.com/reddb-io/reddb/issues/174)
- Fix slices: [#176](https://github.com/reddb-io/reddb/issues/176),
  [#177](https://github.com/reddb-io/reddb/issues/177),
  [#178](https://github.com/reddb-io/reddb/issues/178),
  [#179](https://github.com/reddb-io/reddb/issues/179)
- CI lint slice: [#180](https://github.com/reddb-io/reddb/issues/180)
- Topology wire payload (HelloAck / topology JSON surface):
  [#166](https://github.com/reddb-io/reddb/issues/166)
- Prior security ADR: `.red/adr/0008-topology-advertisement-security.md`
