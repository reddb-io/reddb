# Bun RedWire client

```ts
import { connect } from '@reddb-io/client-bun'

const db = await connect('127.0.0.1:6380')
try {
  const result = await db.queryParsed('SELECT id,name FROM users WHERE id=$1', [1])
  console.log(result.result.records.map(record => record.values))
} finally {
  db.close()
}
```

On servers advertising `FEATURE_PARAMS`, bound and unbound queries both return
the canonical query envelope: records are at `result.records`, write counts at
`affected_rows` (when nonzero), and the command at `statement_type` (for writes).
`query()` returns its JSON string; `queryParsed()` parses it. This also applies
to `query(sql, [])`. Previously unbound queries returned only a summary, which
omitted records; consumers of that summary must update their field paths.

Older servers without `FEATURE_PARAMS` retain the legacy summary response for
unbound queries. Binding values to such servers raises `PARAMS_UNSUPPORTED`.

Run the shared SDK smoke test and this package's actual TCP-client regression:

```sh
REDDB_BINARY_PATH=/absolute/path/to/red bun run test
```
