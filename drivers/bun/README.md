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

<!-- contract-matrix:begin -->
## Public-surface support

> Generated from [`docs/conformance/public-surface-contract-matrix.json`](/docs/conformance/public-surface-contract-matrix.json) by `scripts/gen-docs-from-matrix.mjs`. Do not edit between the markers by hand — run `node scripts/gen-docs-from-matrix.mjs --write`. The matrix is the source of truth; this block can never claim more than it, and CI (`docs-matrix`) fails on drift.
>
> Driver-helper (SDK Helper Spec v1.0) support for every public promise. A helper not marked supported here is not promised by this driver.

| Promise | driver_helpers |
| --- | --- |
| **PSC-001** — RedDB is one multi-model database (tables, graph, KV, timeseries, probabilistic, vector, queue, documents) backed by a single file. | ✅ supported |
| **PSC-002** — MATCH supports node, edge, label, property, and LIMIT projections. | ✅ supported |
| **PSC-003** — GRAPH algorithms accept semantic identifiers, limits, ordering, and return stable rich rows. | ❌ unsupported |
| **PSC-004** — INSERT creates rows, documents, and native timeseries points. | ✅ supported |
| **PSC-005** — HLL/SKETCH/FILTER expose write and read commands for cardinality, frequency, and membership. | ⚠️ partial |
| **PSC-006** — Timeseries stores timestamped metrics with tags and supports query/readback. | ⚠️ partial |
| **PSC-007** — Documents are first-class: create, read, update, delete, and SQL analytics over JSON. | ✅ supported |
| **PSC-008** — KV helpers expose get/put/delete; get of a missing key returns null, delete reports affected. | ✅ supported |
| **PSC-009** — Queue helpers expose create/push/peek/pop/len/purge with FIFO semantics; empty pop is not an error. | ✅ supported |
| **PSC-010** — Transactions are imperative (begin/commit/rollback) plus a run(callback) form; empty SQL rejects with INVALID_ARGUMENT. | ✅ supported |
| **PSC-011** — SQL aggregate, projection, expression, and mutation behaviour matches ordinary SQL expectations where advertised. | ✅ supported |
| **PSC-012** — Server transports expose the same query contract as embedded (HTTP, RedWire, gRPC parity). | ✅ supported |
| **PSC-013** — Official drivers implement the SDK Helper Spec v1.0 conformance suite (all 22 §12 case IDs). | ✅ supported |
| **PSC-014** — ASK / SEARCH semantic surfaces return ranked results with stable shape. | ⚠️ partial |

_Status legend: ✅ supported · ⚠️ partial (known gaps) · ❌ unsupported._
<!-- contract-matrix:end -->

## SQL quotation compatibility

SQL double quotes delimit identifiers; single quotes delimit text. For example,
`SELECT "title" FROM articles WHERE author = 'alice'` reads the `title` column.
Bind application values as parameters. JSON object/array strings keep double
quotes. See the [SQL quotation migration guide](../../docs/query/sql-quoting.md)
before upgrading applications that used double-quoted SQL text literals.
