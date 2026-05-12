# CLI: red sql --param ergonomics for parameterized queries [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/375

Labels: needs-triage

GitHub issue number: #375

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

CLI ergonomics for parameterized queries. `red sql` accepts parameters from the command line or a JSON file:

```
red sql 'SELECT * FROM users WHERE id = $1' --param 1
red sql 'SEARCH SIMILAR $1 IN embeddings K 5' --param @vec.json
red sql 'INSERT INTO articles (title, body) VALUES ($1, $2)' --param 'AI Safety' --param @body.txt
```

`@file` syntax loads a JSON file (vectors, json objects, large text). Plain values are auto-typed (int / float / string / bool / null).

Sends params over the HTTP transport (#358) using the JSON envelope.

## Acceptance criteria

- [ ] `--param <value>` flag accepted (repeatable).
- [ ] `@file` syntax loads JSON file content as the parameter.
- [ ] Auto-typing rules documented (int, float, bool, null, string fallback).
- [ ] `--param-type vec/text/bytes/...` override flag for ambiguous cases.
- [ ] Integration test for each parameter form.
- [ ] `docs/api/cli.md` updated.

## Blocked by

- #358
