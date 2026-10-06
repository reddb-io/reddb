---
"@reddb-io/client": patch
---

`query()` now returns the documented `{ statement, affected, columns, rows }` over every transport. Over HTTP it returned the server envelope's bare `result` (`{ columns, records, stats }`, no `rows`, and `affected`/`statement` dropped); over RedWire a parameterized query returned the raw envelope. `rows` are plain objects keyed by column, so `kv`, `documents` and the quickstart now work. RedWire sends every query as `QueryWithParams` (with an empty list when nothing is bound): the legacy `Query` frame returned only a summary and `QueryBinary` returned all-NULL columns for a one-row table on 1.23.4. HTTP `insert()` now sends the row under `fields`, as the server requires. `bookmark` and `stats` are passed through when the server sends them.
