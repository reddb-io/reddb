# Parse error: distinguish "unknown token" from "token not supported here" [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implemented as a focused vertical slice. Parser errors now have a structured
`UnsupportedToken` kind for lexer-recognized keywords that are not supported at the
current parser position, without rendering the rejected token in an `expected:` list.

## Parent

#445

## Acceptance criteria

- [x] When a lexer-known token appears in an unsupported parser position, the error message distinguishes that case from "token not in the lexer at all".
- [x] The "expected" list no longer contains the rejected token.
- [x] `CREATE VECTOR`, `CREATE DOCUMENT`, `CREATE GRAPH`, and `CREATE COLLECTION` produce the new variant.
- [x] Parser unit tests cover the new error variant across CREATE, DROP, and ALTER positions.

## Verification

- `cargo test -q -p reddb-io-server --lib unsupported_recognized -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
