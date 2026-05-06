# Serialization-Boundary Audit — 2026-05-06

Status: Findings draft (read-only)

Parent: #173. This slice: #174. Sibling fix slices: #175, #176, #177, #178 (typed guards) plus follow-up issues filed from the high-severity findings below.

## Scope and methodology

The Whiz / GitHub Babeld vulnerability (March 2026) exposed a generic
class of bug: when an upstream component concatenates **trusted** and
**untrusted** strings into a structured serialization format whose
delimiter the untrusted side controls, downstream parsers see forged
fields. The shape is `serialize(trusted ++ untrusted)` without escape
of the format's delimiter.

This audit walks every `user-input → serialization-output` path in
the RedDB workspace and asks four questions per surface:

1. **What format does the surface produce?** (HTTP header line, JSON
   body, JSONL audit row, gRPC `grpc-message` trailer, RedWire frame,
   PG backend message, Prometheus exposition, structured tracing
   field, base64 envelope.)
2. **What escape rule does the surface rely on today?** (`HeaderValue`
   constructor, `serde_json::Value`, hand-rolled `escape_string`,
   length-prefixed binary, percent-encoding from a vendored library,
   `sanitize_label`, "none — vulnerable".)
3. **Does the surface accept user-controlled bytes that include the
   format's delimiter (CRLF, NUL, `"`, `;`, `,`, `\t`, etc.)?**
4. **If a delimiter slips through, what does the downstream parser
   do?** (Forge a field, truncate a record, split a log line into
   two, escalate trust into a sibling field, …)

Methodology was read-only: no code was changed during the audit.
File and symbol pointers below are stable references; line numbers
are intentionally omitted because they rot.

The biggest single discovery is a **codebase-wide JSON-string
encoder mismatch** (F-01): the `crate::serde_json::Value::escape_string`
hand-rolled escaper used by the audit log, the RedWire HelloAck/AuthOk
JSON envelopes, gRPC `PayloadReply.payload`, the `compact_entity_json`
presentation path, and every `json_response` reply on the HTTP surface
**does not escape ASCII control characters U+0000 through U+001F
except `\n`, `\r`, `\t`**. A separate, correct escaper
(`utils::json::JsonValue::write_json`, plus `grpc::scan_json::write_json_string`)
exists in the codebase; the two diverge silently. Most surfaces
import `crate::json::Value`, which re-exports the buggy one.

## Summary

| Severity | Count |
|---|---:|
| Critical | 1 |
| High     | 5 |
| Medium   | 6 |
| Low      | 3 |
| Total    | 15 |

Severity rubric (per #173):
- **Critical**: RCE-ish, auth bypass, capability/role escalation.
- **High**: log forging, audit forging, info-disclosure of credentials,
  forged fields read by downstream policy code.
- **Medium**: protocol-spec violation, downstream tool confusion,
  cosmetic record corruption that survives compression / archival.
- **Low**: cosmetic, no parser hits the malformed bytes today.

## Findings

### F-01 [Critical] — `serde_json::Value::escape_string` does not escape ASCII control characters below 0x20 (except `\n`, `\r`, `\t`)

**Surface:** Every JSON-serialized output that imports `crate::json`
or `crate::serde_json` and routes a user-supplied `String` through
`JsonValue::String(s) → Value::to_string_compact()`. Concretely, this
covers:
- HTTP `json_response` / `json_error` / `json_ok` (every handler under
  `crates/reddb-server/src/server/handlers_*.rs`).
- gRPC `PayloadReply.payload` (`grpc::control_support::json_payload_reply`
  → `serde_json::to_string`).
- gRPC scan / query reply slow path
  (`presentation::entity_json::compact_entity_json_string`
  → `json_to_string`).
- Audit log JSONL emission (`AuditEvent::to_json_line` →
  `JsonValue::Object(_).to_string_compact`).
- RedWire HelloAck JSON envelope (`wire::redwire::auth::build_hello_ack`).
- RedWire AuthOk / AuthFail JSON (`wire::redwire::auth::build_auth_ok`,
  `build_auth_fail`).
- MCP server output (`mcp::server` — multiple call sites).

**Boundary:** JSON-encoded UTF-8 string, framed by surrounding `"..."`
plus `,` / `:` / `}` / `]` outside the string.

**Escape rule today:** `crate::serde_json::escape_string`:

```text
input.replace('\\', "\\\\")
     .replace('"',  "\\\"")
     .replace('\n', "\\n")
     .replace('\r', "\\r")
     .replace('\t', "\\t")
```

This is not RFC 8259 / ECMA-404 compliant. The spec requires every
`U+0000`–`U+001F` codepoint to be escaped (typically `\uXXXX`).
Backspace `U+0008`, form-feed `U+000C`, vertical-tab `U+000B`, and
NUL `U+0000` are passed through verbatim.

A separate escaper (`utils::json::JsonValue::write_json`) is correct,
and `grpc::scan_json::write_json_string` is also correct. The
codebase uses the buggy one for nearly every external boundary.

**Hypothetical exploit chain:**
- A tenant supplies a collection name, a SQL alias, or a JWT-derived
  username containing a literal `U+0008` (backspace).
- The runtime emits an audit log line via `AuditEvent::to_json_line`,
  embedding the value through the buggy escaper.
- The audit log line is shipped to a SIEM (`stream_post`) and to disk.
  The SIEM's JSON parser may accept it (most strict parsers reject;
  many tolerant parsers accept).
- Downstream tooling that ingests the file via `awk`/`grep`/`split`
  on NUL or that re-decodes via a strict parser sees record-shape
  ambiguity. Splunk's `INDEXED_EXTRACTIONS` for JSONL specifically
  rejects unescaped 0x00–0x1F, dropping forensic events on the floor
  precisely when a hostile tenant is active.
- The same pattern applies to RedWire HelloAck — a hostile peer
  receiving HelloAck with a NUL inside `chosen_auth` (today
  server-controlled, but tomorrow when v2.2 admits caller-supplied
  feature negotiation strings, it becomes tenant-reachable) treats
  the envelope as malformed.
- The same payload returned via gRPC `PayloadReply.payload` to a
  client whose JSON parser is strict will be rejected after the
  RedDB write succeeded, leaving the client in a "did the write
  apply or not?" split-brain state.

The reason this rises to **Critical** rather than High: there is at
least one path where the bytes round-trip back into a *different*
parser. Specifically, audit log lines are read back by
`AuditEvent::parse_line` (the audit query endpoint) using
`crate::json::from_str`. If a tenant's input causes the writer-side
JSON to be written with a raw NUL, the reader-side JSON parser may
disagree about field boundaries with whatever Splunk or the auditor's
`jq` saw. An auditor reviewing a forensic event sees one thing;
RedDB's own audit query endpoint sees another. That is a
**self-disagreeing audit** — the worst possible state for a SOC 2
artifact.

The exploit class is "spec-violating output that may be silently
mangled by any one of N downstream consumers, where the set of
consumers is open-ended." That matches the Whiz/Babeld severity
profile: the bug is benign in isolation; it composes into auth /
audit-bypass when chained with any one downstream that interprets
the malformed bytes as control structure.

**Call site:** `crates/reddb-server/src/serde_json.rs::escape_string`
(the buggy implementation). Re-exported via
`crates/reddb-server/src/json.rs` and used by every surface listed
above.

**Fix slice:** **#177** (typed `SerializedJsonField` guard, per #173
PRD §"Construction discipline"). The guard's contract is "values
must round-trip through `serde_json::Value` rather than format-string
concatenation"; this finding upgrades the guard's deliverable to
"replace the buggy `escape_string` with the spec-compliant version
and delete one of the two encoders."

---

### F-02 [High] — Postgres wire `ErrorResponse.M` field is null-terminated, message bytes are concatenated unescaped

**Surface:** PG wire `BackendMessage::ErrorResponse { message }`
(also `NoticeResponse`, `CommandComplete`, `RowDescription` column
names). Each PG backend message field is built by writing a tag byte
then `value.as_bytes()` then a `0` (NUL) terminator.

**Boundary:** PG3 wire protocol, NUL-terminated C-strings inside a
length-prefixed frame.

**Escape rule today:** None. `message.as_bytes()` is appended raw,
trusting the upstream `RedDBError::to_string()`.

**Hypothetical exploit chain:**
- A tenant submits SQL that triggers a parser error embedding their
  raw input — e.g., `SELECT * FROM "evil\0M\0fake-message\0"`.
- The runtime wraps the input into `RedDBError::Query(format!("…
  near {input}"))`, the PG wire layer maps it to
  `BackendMessage::ErrorResponse { severity: "ERROR", code: "42601",
  message: "syntax error near …<NUL>M<NUL>fake-message<NUL>" }`.
- The encoded bytes are: `S` + "ERROR" + `\0` + `V` + "ERROR" + `\0`
  + `C` + "42601" + `\0` + `M` + "syntax error near " + `\0` (here
  the **first** NUL terminates `M` early) + `M` + "fake-message" +
  `\0` + `\0` (the trailing terminator).
- A standard PG client (libpq, JDBC PG driver, etc.) parses this
  as: severity=ERROR, sqlstate=42601, M="syntax error near ",
  M="fake-message". The second `M` field overwrites the first,
  meaning the displayed error in the operator's psql or the
  exception text in the application driver becomes "fake-message"
  — entirely tenant-controlled.
- Worse, a tenant can inject a fake `H` (HINT) field or a fake
  `S` (SEVERITY) field. A fake `S` of `FATAL` causes most clients
  to drop the connection, which DoSes other queries multiplexed
  on the same channel.
- RowDescription column names suffer the same risk if the runtime
  ever surfaces a tenant-supplied alias verbatim
  (`SELECT 1 AS "x\0type_oid\0…"`).

**Call site:** `crates/reddb-server/src/wire/postgres/protocol.rs`
in the `BackendMessage::ErrorResponse` branch of
`encode_backend_message`. Same shape applies to the
`BackendMessage::NoticeResponse`, `CommandComplete`, and
`RowDescription` branches in the same file.

**Fix slice:** Follow-up issue (filed). Reject any user-supplied
field that contains NUL at the surface boundary; or escape via
the same approach gRPC uses (reject + structured 4xx, since PG3
has no in-band escape for embedded NUL).

---

### F-03 [High] — Audit log fields embed user-supplied collection / username / tenant verbatim (control-byte forging via F-01)

**Surface:** `AuditEvent::to_json_line` is called with `principal`,
`tenant`, `resource`, `action`, `detail` fields, all of which can
be tenant-derived in some call sites
(see `crates/reddb-server/src/runtime/lease_lifecycle.rs`,
`crates/reddb-server/src/runtime/impl_core.rs`,
`crates/reddb-server/src/server/handlers_admin.rs` × 7,
`crates/reddb-server/src/server/handlers_auth.rs` × 6, and
several DDL / DML emit sites). Every emit site passes the values
as `JsonValue::String(value.to_string())` and relies on the JSON
encoder.

**Boundary:** JSONL audit log, one event per line, hash-chained
via `prev_hash = sha256(prev_line)`.

**Escape rule today:** Inherits the buggy
`crate::serde_json::escape_string` (F-01). `\n` and `\r` *are*
escaped, so a literal newline cannot break out of a JSONL record
via this path. NUL, 0x01–0x08, 0x0B, 0x0C, 0x0E–0x1F, and 0x7F are
not escaped.

**Hypothetical exploit chain:**
- Tenant `acme` creates a user with username
  `attackersystem`. The
  runtime accepts the username (UTF-8, < 256 bytes, no `\n`).
- A privileged action by that user emits an audit row:
  `{"principal":"attacker\u{08}\u{08}\u{08}\u{08}\u{08}\u{08}\u{08}system",
  "action":"db/grant", "outcome":"success"}`.
- The audit log file is shipped to a terminal-rendering tool (a
  forensic analyst tailing the file, a syslog viewer with VT100
  cursor handling, an `less`/`tail` paging session). Backspace
  characters cause the analyst to see `"principal":"system"` —
  the literal `attacker` prefix is over-typed by the cursor.
- The hash chain is intact (the bytes on disk are correct), but
  the *displayed* representation of the audit log is forged.
  Compliance auditors who use terminal tools (most do) see a
  fabricated principal.
- Variant: `` (BEL) tortures the on-call analyst's terminal
  during incident response.

The hash chain *protects* against this in principle (a forensic
auditor can recompute hashes and observe that the on-disk bytes
match), but the routine review path — operators who `tail -f` the
file — is where the exploit lands.

This is a separate finding from F-01 because the audit log surface
has additional invariants: it is signed, replicated, and treated
by SOC 2 as load-bearing. The fix slice is also different: F-01's
guard rewrites the encoder; F-03 adds a per-field reject rule
(no control bytes in `principal` / `tenant` / `action`).

**Call site:** `crates/reddb-server/src/runtime/audit_log.rs::AuditEvent::to_json_line`
plus every caller per `grep -rn "audit_log\." crates/reddb-server/src/`
(15 caller sites today).

**Fix slice:** **#176** (typed `AuditFieldEscaper`). Per #173 PRD,
this guard's contract is "audit entries are always structured and
can never be forged by a tenant-supplied collection name or SQL
fragment". The fix is reject-on-emit for control-byte content in
the principal / tenant / action / resource fields plus reuse of
the F-01 spec-compliant encoder for everything else.

---

### F-04 [High] — Tracing macros render user-supplied fields with `Display`; CRLF / NUL injection forges log lines

**Surface:** Every `tracing::info!` / `warn!` / `error!` / `debug!`
macro that interpolates a user-supplied string via the `%value`
(`Display`) syntax. Approximately 141 call sites across
`crates/reddb-server/src/`. Concrete examples:
- `crates/reddb-server/src/server/handlers_auth.rs`:
  `tracing::warn!(target: "reddb::http_auth", principal = %caller_id,
   "login refused")`. `caller_id` is `UserId::from_parts(tenant_id,
   username)`, both attacker-controlled.
- `crates/reddb-server/src/server/routing.rs::resolve_bearer`:
  `tracing::info!(user = %username, …, "JWT accepted")`. `username`
  comes from a JWT claim, attacker-controllable in the federated
  case.
- `crates/reddb-server/src/storage/unified/store/impl_pages.rs`:
  `collection = %name`. Collection names are tenant-supplied in
  every multi-tenant deployment.
- `crates/reddb-server/src/grpc/control_support.rs`:
  `username = %username` in two places.
- `crates/reddb-server/src/auth/store.rs`:
  `username = %username`.

**Boundary:** Plain-text log line emitted by tracing's default
`fmt::format::DefaultFields` formatter, which writes
`field=value` separated by spaces. `Display` does no escaping at
all.

**Escape rule today:** None for `%`-formatted values. (`?value`
goes through `Debug`, which *does* escape control characters and
quote-wraps the value — so the right idiom is `?username` not
`%username`. The codebase uses `%` ubiquitously.)

**Hypothetical exploit chain:**
- Attacker registers (or coerces a JWT issuer to mint) a username
  `alice\nlevel=ERROR cluster_breach=true target="reddb::secrets"`.
- They authenticate. The HTTP auth path emits
  `tracing::info!(principal = %caller_id, "login ok")`.
- The default tracing-subscriber renders this as one line. Because
  `%caller_id` writes bytes verbatim, the actual file output is:

  ```
  2026-05-06T12:00:00 INFO reddb::http_auth principal=alice
  level=ERROR cluster_breach=true target="reddb::secrets" "login ok"
  ```

- A log shipper that splits on `\n` (Filebeat, Fluentd, journald
  stdin reader) treats the second line as an independent log
  event. Severity-aware alerting interprets `level=ERROR
  cluster_breach=true` as a real critical event and pages SRE.
- Compounding: if the SRE's alert pipeline indexes by `target=`,
  the forged line gets indexed under `reddb::secrets`. A subsequent
  query "show me secrets-related events" returns the forged line,
  burying real signal.
- This is the classic Babeld pattern: an unsanitized
  Display-concatenation crosses a delimiter (newline), and the
  downstream pipeline treats the smuggled bytes as authoritative
  control structure.

**Call site:** Pervasive. Highest-risk subset (caller-controlled
strings rendered via `Display`):
- `crates/reddb-server/src/server/handlers_auth.rs::handle_auth_login`
  (and 5 sibling handlers)
- `crates/reddb-server/src/server/routing.rs::resolve_bearer`
- `crates/reddb-server/src/grpc/control_support.rs` (2 sites)
- `crates/reddb-server/src/storage/unified/store/impl_pages.rs`
  (2 sites with `collection = %name`)
- `crates/reddb-server/src/auth/store.rs::record_failed_attempt`
- `crates/reddb-server/src/application/ports_impls_entity.rs`
  (`collection = %applied.collection`)

**Fix slice:** Follow-up issue (filed). The simplest fix is a
codebase-wide `tracing` interpolation lint: ban `%value` for any
string that crossed an untrusted-input boundary. The mechanical
fix is `%value` → `?value`, which forces `Debug` quoting and
control-byte escaping. The structural fix is a wrapper type
`Tainted<String>` that only implements `Debug`, never `Display`.

---

### F-05 [High] — `RedDBError::Query(msg)` reflects unsanitized SQL fragments into HTTP / gRPC / PG error replies

**Surface:** SQL parser error messages that embed the offending
input verbatim:
- `parser/dml.rs::EXPIRES AT requires a unix timestamp in
  milliseconds, got '{trimmed}'`
- `parser/auth_ddl.rs::resource must be \`kind:name\`, got \`{raw}\``
- `parser/expr.rs::unknown type name \`{type_name}\` in CAST`
- `parser/tree.rs::invalid tree entity id '{value}'`
- `parser/timeseries.rs::unknown duration unit '{}', expected
  s/m/h/d`
- `parser/ddl.rs::WITH tenant_by expects a text literal, got
  {other:?}` (uses `?` so this one is OK)
- `parser/dml.rs::invalid JSON object literal: {err}` (delegates
  to the JSON parser's error, which also embeds input)

**Boundary:**
- HTTP: `json_error(400, msg)` → `JsonValue::String(msg)` →
  buggy F-01 encoder. Same control-byte risk.
- gRPC: `Status::invalid_argument(msg)`. tonic percent-encodes the
  description into the `grpc-message` HTTP/2 trailer, so this
  surface is **safe** (assuming tonic 0.14 is correct).
- PG3: `ErrorResponse.message`. F-02 risk applies (NUL truncation,
  field forgery).

**Escape rule today:** None at the parser-error layer; safety is
delegated to the per-surface encoder. The HTTP and PG paths
inherit the F-01 / F-02 weaknesses.

**Hypothetical exploit chain:**
- Tenant submits SQL: `CAST(x AS "evil M fake")`
  via the PG wire. The parser produces
  `RedDBError::Query("unknown type name `evil\0M\0fake` in CAST")`.
- Mapped to `BackendMessage::ErrorResponse { code: "42601",
  message: "unknown type name `evil\0M\0fake` in CAST" }`.
- Encoded bytes carry an embedded NUL → F-02 exploit chain
  (forged `M` field).
- Same input via HTTP `POST /query` returns
  `{"ok":false,"error":"unknown type name `evil\0M\0fake` in CAST"}`
  — JSON spec-violating because of the raw NUL (F-01).

**Call site:** Every parser file under
`crates/reddb-server/src/storage/query/parser/` that uses the
`format!("…{user_input}…")` shape with bare `{` (not `{:?}`).
About 12 call sites today.

**Fix slice:** **#178** (downstream guards) plus the lint script
in #179 — flag any `format!()` inside an error-construction path
that uses bare `{}` interpolation for a user-supplied value.

---

### F-06 [High] — `parse_with_limits` error messages reflect raw URI fragments into wrapped error chain

**Surface:**
`crates/reddb-wire/src/conn_string.rs::parse_with_limits` builds
`ParseError::new(InvalidUri, format!("{e}: {uri}"))` where `uri`
is the original (post-scheme-lowercase) input. `ConnStringLimits`
errors also embed the offending count: `format!("max_uri_bytes
exceeded: limit={} actual={}")`. Cluster-host parsing emits
`"unsupported scheme: {other}"` with `other` = raw scheme.

**Boundary:**
- These errors travel back via `ConnStringSanitizer`
  (#177-tier work) consumers — the gRPC connector, server-side
  dispatch, and the CLI's `red-cli connect` command which prints
  errors to stderr.
- Stderr is captured by service supervisors (systemd, k8s, fly.io)
  and emitted to journald / cloud logging without escape.

**Escape rule today:** None. `Display::fmt` for `ParseError`
writes `"{kind_str}: {message}"` where `message` carries the raw
URI bytes.

**Hypothetical exploit chain:**
- A user (or a misconfigured client) supplies a URI string
  containing `\n` or NUL.
- The CLI prints the parser error to stderr.
- journald / cloud logging sees a multi-line "log entry" because
  stderr is line-split.
- A monitoring rule "alert when stderr contains 'panic'" misfires
  if the malicious URI contains the literal substring "panic".

This is lower-severity than F-04 because the conn-string parser
is invoked in the operator path, not the tenant path. But because
some clients (the gRPC bootstrap path, the replica
`primary_addr` env-resolution) accept conn-string-shaped input
from configuration sources outside the operator's direct review
(env injection, k8s ConfigMap, fly.io secret), the surface is not
fully operator-controlled.

**Call site:**
`crates/reddb-wire/src/conn_string.rs::parse_with_limits`,
`crates/reddb-wire/src/conn_string.rs::try_parse_grpc_cluster`.

**Fix slice:** **#177** typed `ConnStringSanitizer` per the #173
PRD. The PRD already calls out a `Tainted<String>` wrapper that
requires explicit `escape_for(boundary)` re-serialization;
parser-error embedding becomes the first consumer of that
wrapper.

---

### F-07 [Medium] — Prometheus exposition embeds replica IDs and principals via `sanitize_label`; missing tab-byte coverage

**Surface:**
`crates/reddb-server/src/server/handlers_admin.rs` exports
`/metrics` Prometheus scrape output. User-controlled labels:
- `replica_id="{}"` (registered replica identity, operator-trusted
  in normal flows but cluster-membership-attacker-reachable via
  malicious replica registration)
- `principal="{}"` from the per-caller quota rejection map. The
  principal string is hashed for bearer / HMAC keys but the
  `replica:<id>` and `system` lanes pass through verbatim.

**Boundary:** Prometheus exposition format. Labels are
`name="value"`, with `\\`, `\n`, and `"` requiring backslash
escape per the Prometheus spec.

**Escape rule today:** `sanitize_label`:

```text
'"'  -> \"
'\\' -> \\
'\n' -> \n
'\r' -> \r
```

Per Prometheus spec, `\r` is not actually a special label-value
character — but `\t` and other control bytes are also not special.
So the current rule covers the spec'd characters correctly. The
**spec itself permits unescaped control bytes**, which a label
parser tolerates but a downstream metrics-pipeline tool
(VictoriaMetrics, Mimir, Cortex) may flag as warnings.

**Hypothetical exploit chain:**
- A malicious replica registers itself as id `attacker\tregion="…"`.
- The metrics scrape exports
  `reddb_replica_ack_lsn{replica_id="attacker\tregion=\"prod\""} 0`
  (literal tab-byte inside the value).
- Prometheus accepts the line. VictoriaMetrics rejects it with a
  parse warning.
- The scrape pipeline drops the metric for that scrape — observability
  goes dark for the attacker's replica precisely when an operator
  most needs to see it.

This is **medium** because the spec permits the bytes and most
operators won't be using a strict downstream. It is not
critical/high because no field is actually forged — the worst-case
outcome is a dropped metric, not a forged claim.

**Call site:**
`crates/reddb-server/src/server/handlers_admin.rs::sanitize_label`
plus its callers in the `handle_metrics` (admin metrics endpoint).

**Fix slice:** Follow-up TBD. Minor. Track inside #178 as part of
the broader audit-of-output-escapers slice.

---

### F-08 [Medium] — Audit log SIEM streaming POST embeds JSONL line into HTTP body; relies on F-01 correctness

**Surface:** When `RED_AUDIT_STREAM_URL` is set,
`stream_post(url, line)` POSTs each audit line to a SIEM endpoint
fire-and-forget. The line is the same bytes that hit the local
JSONL file.

**Boundary:** HTTP body, content-type `application/x-ndjson`. The
body is shipped to an operator-controlled URL.

**Escape rule today:** Inherits F-01. The body is the literal
JSONL line.

**Hypothetical exploit chain:**
- Same as F-03 — a user-supplied control byte rides into the
  audit log.
- The SIEM endpoint's NDJSON parser rejects the line because of
  the unescaped control byte.
- The audit event is silently dropped from the SIEM index. The
  on-disk `.audit.log` retains it (good), but the SIEM-driven
  dashboard / alerting does not see it (bad).

**Call site:**
`crates/reddb-server/src/runtime/audit_log.rs::stream_post`.

**Fix slice:** Resolved transitively by **#177** (F-01 fix).

---

### F-09 [Medium] — `RowDescription` column names in PG wire and gRPC `QueryReply.columns` accept tenant-supplied aliases verbatim

**Surface:**
- `crates/reddb-server/src/wire/postgres/protocol.rs::encode_backend_message`
  in the `RowDescription` branch writes `col.name.as_bytes()` then
  NUL terminator.
- `crates/reddb-server/src/grpc/scan_json.rs::query_reply` passes
  `result.columns` directly into `QueryReply.columns`. The columns
  come from `UnifiedResult::columns`, which originates in the
  parser / executor.

**Boundary:**
- PG: NUL-terminated C-string per column.
- gRPC: protobuf `repeated string columns`. Protobuf string fields
  must be valid UTF-8; protoc accepts NUL but downstream consumers
  may not.

**Escape rule today:** None at the wire layer. Validation, if
any, happens during SQL parsing — but `SELECT 1 AS "name with
\0"` is *not* prohibited by the current SQL parser (quote-delimited
identifiers with NULs slip through if the lexer does not
explicitly reject them — confirm in #178 fix).

**Hypothetical exploit chain:**
- Tenant submits `SELECT 1 AS "x\0type_oid\0\0\0\0"` (PG3
  RowDescription column entry has `name<NUL>table_oid<u32>
  column_attr<u16> type_oid<u32>…`). With NULs inside the
  alias, the encoded bytes look like a malformed
  RowDescription with one extra phantom column.
- Most PG clients reject the row description and drop the
  connection. Some accept it and return phantom columns to the
  application — leading to schema-confusion exploitation in the
  application layer.

**Call site:** Same as F-02 (the PG wire encoder) plus
`grpc::scan_json::query_reply`.

**Fix slice:** Follow-up — joins F-02 in the new issue.

---

### F-10 [Medium] — `JsonValue::Number` is rendered with `{}` (Display) for non-integer floats; `NaN`/`Inf` produce non-JSON tokens

**Surface:** `crate::serde_json::Value::write_compact`:

```rust
JsonValue::Number(n) => {
    if n.fract() == 0.0 {
        out.push_str(&format!("{}", *n as i64));
    } else {
        out.push_str(&format!("{}", n));
    }
}
```

If `n` is `f64::NAN`, `f64::INFINITY`, or `f64::NEG_INFINITY`,
the formatter emits `NaN` / `inf` / `-inf` — none of which are
valid JSON. If the integer cast `*n as i64` saturates (n > i64::MAX
or n is NaN), the output is `i64::MAX` (`as i64` saturating cast),
silently changing the value.

This is not a *boundary* injection per the Whiz pattern, but it
*is* a JSON-spec violation that downstream parsers handle
inconsistently (some accept `NaN`, some reject).

**Boundary:** Same as F-01 — every JSON output.

**Escape rule today:** None — `format!("{}", n)` for `f64` does
not produce valid JSON for non-finite values.

**Hypothetical exploit chain:**
- A query result includes a float column where some rows are
  `NaN` (e.g., from a `0.0/0.0` aggregation).
- The HTTP / gRPC reply contains literal `NaN` tokens.
- A strict downstream JSON parser rejects the response after the
  query already executed → tenant sees a 500 from RedDB but
  RedDB's logs say 200. (This is more a correctness bug than a
  security bug.)

**Call site:**
`crates/reddb-server/src/serde_json.rs::Value::write_compact`.

**Fix slice:** Resolved by **#177** (F-01 fix slice — the new
encoder is required to emit `null` for non-finite floats per
`grpc::scan_json::write_value_json` precedent).

---

### F-11 [Medium] — RedWire `build_auth_fail(reason)` embeds `reason` from caller-formatted strings that can include user input

**Surface:** `wire::redwire::auth::build_auth_fail` is called with
strings such as `format!("scram client-first: {e}")`,
`format!("scram client-final: {e}")`,
`format!("auth method '{other}' is not supported in v2.1")` where
`e` and `other` come from the client-supplied AuthRequest payload.

**Boundary:** JSON envelope inside an `AuthFail` frame. Inherits
F-01.

**Escape rule today:** F-01 buggy escaper.

**Hypothetical exploit chain:** Same as F-01 with a control byte
in the auth method name or SCRAM error reason. Effects:
log-pipeline confusion when the AuthFail JSON is logged on the
client side; minor protocol-spec violation. No escalation to RCE
or auth bypass — the AuthFail frame is the *last* frame on the
session and there is no further trust transfer.

**Call site:**
`crates/reddb-server/src/wire/redwire/auth.rs::build_auth_fail`,
called from `auth.rs::validate_auth_response` and
`crates/reddb-server/src/wire/redwire/session.rs` SCRAM path.

**Fix slice:** Resolved transitively by **#177**.

---

### F-12 [Medium] — `replica_apply_health()` and `last_error` config strings are echoed unsanitized into JSON / metrics

**Surface:**
- `crates/reddb-server/src/server/handlers_replication.rs::handle_replication_status`
  emits `"last_error": <runtime config string>` via JsonValue::String.
- `crates/reddb-server/src/server/handlers_admin.rs::handle_metrics`
  emits `reddb_replica_apply_health{state="<sanitized>"} 1`
  using `sanitize_label`. The metrics path is OK (sanitize_label
  handles `\\`, `\n`, `\r`, `"`); the JSON path is not (F-01).

**Boundary:** JSON body for `/replication/status`, Prometheus
exposition for `/metrics`.

**Escape rule today:**
- JSON: F-01 buggy escaper.
- Metrics: `sanitize_label` (correct for the spec'd characters).

**Hypothetical exploit chain:** A WAL apply error embeds tenant
table or column names verbatim into `RED_REPLICATION_LAST_ERROR`.
A replica failing to apply a tenant-crafted DDL surfaces a
`last_error` string with control bytes.
- Status endpoint JSON: F-01 fallout.
- Metrics: clean.

**Call site:**
`crates/reddb-server/src/server/handlers_replication.rs::handle_replication_status`.

**Fix slice:** Resolved transitively by **#177**.

---

### F-13 [Low] — RedWire HelloAck `chosen_auth` is server-controlled today, but path is JSON-encoded via the buggy escaper

**Surface:** `wire::redwire::auth::build_hello_ack` writes the
`auth` field as `JsonValue::String(chosen_auth.to_string())`.
`chosen_auth` is selected by `pick_auth_method` from a static set
of `&'static str` values: `"anonymous"`, `"scram-sha-256"`,
`"oauth-jwt"`, `"bearer"`. None are user-controlled today.

**Boundary:** RedWire HelloAck JSON envelope. Subject to F-01.

**Escape rule today:** F-01.

**Hypothetical exploit chain:** None today — `chosen_auth` is
selected from a hard-coded list. Recorded as **Low** because a
future v2.2 negotiation extension that admits caller-supplied
strings into the chosen-method label promotes this to medium.

**Call site:**
`crates/reddb-server/src/wire/redwire/auth.rs::build_hello_ack`.

**Fix slice:** Resolved transitively by **#177**. Also: avoid
calling `chosen_auth.to_string()` — the `&'static str` should be
preserved through to `JsonValue::String` directly.

---

### F-14 [Low] — `default_holder_id` embeds `HOSTNAME`/`HOST` env into a string used as a lease holder identifier

**Surface:**
`crates/reddb-server/src/server/handlers_admin.rs::default_holder_id`
returns `format!("{host}-{}", std::process::id())` where `host`
comes from `$HOSTNAME` or `$HOST` env. The lease holder id then
flows into audit log records and into the lease/promotion HTTP
response.

**Boundary:** JSON body fields, audit log JSONL fields.

**Escape rule today:** Inherits F-01 / F-03.

**Hypothetical exploit chain:** An operator with `HOSTNAME`-set
control (e.g., a Docker / k8s deployment with a
unicode-control-char hostname) surfaces non-ASCII bytes through
this path. Operator-controlled, low severity. No tenant reach.

**Call site:**
`crates/reddb-server/src/server/handlers_admin.rs::default_holder_id`.

**Fix slice:** Resolved transitively by **#177** / **#176**.

---

### F-15 [Low] — RedWire frame protocol, replication WAL spool, and `topology` envelope are length-prefixed binary; confirmed safe

**Surface:**
- RedWire frame:
  `crates/reddb-wire/src/redwire/frame.rs` — 16-byte header
  (`length:u32 | kind:u8 | flags:u8 | stream_id:u16 | corr:u64`)
  followed by length-bounded payload.
- Replication WAL spool:
  `crates/reddb-server/src/replication/primary.rs::append_with_timestamp`
  — header is `version | lsn | timestamp_ms | length | payload | crc32`.
- Topology canonical encoding:
  `crates/reddb-wire/src/topology.rs::encode_topology` —
  `version_tag | body_length | body` where body is itself
  length-prefixed strings + numeric scalars.
- HelloAck embedding wraps the topology bytes through
  `base64_encode` before placing them as a JSON `String`. Base64
  alphabet is JSON-safe.

**Boundary:** Binary, length-prefixed.

**Escape rule today:** None needed — the format is
length-prefixed, so embedded delimiters in payload bytes do not
restructure the frame.

**Hypothetical exploit chain:** None — confirmed safe.

**Call site:** As above.

**Fix slice:** None — recorded for completeness, per the audit's
"every protocol surface is enumerated" acceptance criterion.

---

## Surfaces audited and explicitly de-scoped

- **gRPC `Status::*(message)`**: tonic 0.14 percent-encodes the
  description into the `grpc-message` HTTP/2 trailer per the gRPC
  spec. The wire surface is safe assuming tonic is correct. No
  finding — but a regression test asserting "no raw CRLF/NUL ever
  reaches the trailer" is recommended in the F-04 follow-up.
- **gRPC inbound metadata pass-through**: `service_impl.rs` does
  not write to outbound metadata; all metadata operations are
  read-only `request.metadata().get(...)`. No injection surface.
- **Snapshot redactor** (`tests/support/parser_hardening/secret_redactor.rs`):
  test-side only, not a runtime serialization boundary.
- **HMAC auth header verification**
  (`server/hmac_auth.rs`): inbound parsing only; no outbound
  emission.
- **TLS handshake / certificate generation paths**
  (`server/tls.rs`, `wire/tls.rs`): no user-supplied strings
  reach the certificate fields — DNS names come from operator
  config.
- **MCP server**
  (`crates/reddb-server/src/mcp/server.rs`): uses
  `json_to_string` so it inherits F-01. Listed at F-01 rather
  than as its own finding because the surface composes 1:1 with
  HelloAck / PayloadReply.
- **Backup / restore handlers** (`handlers_backup.rs`): emits
  collection names via `JsonValue::String` — F-01 covers it.
- **Connection-string parsing internal grammar**: deferred to
  the existing `docs/security/parser-limits.md` and the
  parser-hardening corpus. Only the *error message* path
  (F-06) is in scope here.

## Cross-references

- Parent: **#173** (PRD: Hardening — serialization-boundary input
  sanitization across all RedDB protocol surfaces).
- This slice: **#174** (the audit doc itself).
- Companion fix slices in #173:
  - **#175** — `HeaderEscapeGuard` (HTTP header values). Out of
    scope of this audit's findings; the codebase does not let
    user-supplied bytes reach response-header values today (the
    `HttpResponse` struct hardcodes Content-Type, Content-Length,
    Connection). When that changes, this audit must be revisited.
  - **#176** — `AuditFieldEscaper` (F-03).
  - **#177** — `SerializedJsonField` (F-01, F-08, F-10, F-11,
    F-12, F-13, F-14).
  - **#178** — `ConnStringSanitizer` (F-06).
  - **#179** — CI lint script.
- Follow-up issues filed for the high-severity findings that need
  their own slice (F-02, F-04, F-05): see the GitHub issue tracker
  for the latest cross-link.

## Notes for the next pass

- The two-encoder split (`crate::serde_json::Value` vs.
  `crate::utils::json::JsonValue` vs. `grpc::scan_json::write_json_string`)
  is the largest single source of risk in the workspace. The
  #177 fix should converge to one encoder.
- `Display` vs. `Debug` discipline in tracing macros is the second
  largest. The mechanical `%` → `?` migration is cheap and the
  taint-wrapper-only approach is the correct long-term answer.
- The PG wire surface (F-02) is the only path with a *truly*
  unguarded NUL injection today. It is the highest-priority
  follow-up.
- Length-prefixed binary surfaces (F-15) are the gold standard
  and should be the model when adding new protocol envelopes.

## Method limitations

This audit was performed by reading the source tree without
running tests, traffic, or fuzz harnesses. Each finding's
"hypothetical exploit chain" is reasoned, not demonstrated. The
typed-guard fix slices each ship their own regression-test corpus
per the #173 PRD; landing those tests is what converts these
findings from "audited" to "verified-fixed".
