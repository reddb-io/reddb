# Foreign Data Wrappers (FDW)

Foreign Data Wrappers let you query external data sources as if they
were native RedDB tables. A `ForeignDataWrapper` trait handles the
protocol; a `ForeignTableRegistry` maps SQL identifiers to wrappers.

## Quick usage

```sql
-- register a source (built-in CSV wrapper)
CREATE SERVER local_csv
  FOREIGN DATA WRAPPER csv
  OPTIONS (base_path '/data/imports');

-- map a CSV file as a foreign table
CREATE FOREIGN TABLE sales_2026 (
  id       INT,
  customer TEXT,
  total    DECIMAL
)
  SERVER local_csv
  OPTIONS (filename 'sales_2026.csv', header 'true');

-- read it like any table
SELECT customer, sum(total)
FROM sales_2026
GROUP BY customer
ORDER BY 2 DESC
LIMIT 10;

-- tear down
DROP FOREIGN TABLE sales_2026;
DROP SERVER local_csv;
```

## CSV wrapper

Ships built-in. RFC 4180 compliant (quoted fields, escaped quotes,
CRLF line endings).

### Server options

| Option | Default | Description |
|--------|---------|-------------|
| `base_path` | — (required) | Directory containing CSV files |

### Table options

| Option | Default | Description |
|--------|---------|-------------|
| `filename` | — (required) | Path relative to `base_path` |
| `header` | `'false'` | First row is a header; skip on read |
| `delimiter` | `','` | Field separator |
| `quote` | `'"'` | Quote character |

### Example with options

```sql
CREATE SERVER warehouse
  FOREIGN DATA WRAPPER csv
  OPTIONS (base_path '/mnt/warehouse');

CREATE FOREIGN TABLE pipe_delim_table (
  sku    TEXT,
  price  DOUBLE,
  stock  INT
)
  SERVER warehouse
  OPTIONS (
    filename 'inventory.psv',
    header 'true',
    delimiter '|'
  );

SELECT * FROM pipe_delim_table WHERE stock > 0;
```

## Read semantics

The read-path rewriter intercepts table references and dispatches to
`ForeignTableRegistry::scan(name)` before the native collection lookup.
Scan results are materialised into `UnifiedRecord`s and then processed
by the standard query pipeline:

- `WHERE` filters apply post-scan (no pushdown in Phase 3.2)
- `LIMIT` / `OFFSET` apply post-scan
- Aggregates, GROUP BY, ORDER BY all work
- Joins with native tables work (foreign side read first, then joined)

Joins / subqueries that reference foreign tables route through the
standard executor — the intercept happens at the top-level table
resolution step.

## What's not yet in

These are known gaps in Phase 3.2 — implemented as infrastructure
without wrappers:

| Wrapper | Status |
|---------|--------|
| `postgres_fdw` | Trait ready, no implementation |
| `mysql_fdw` | Trait ready, no implementation |
| `s3_parquet_fdw` | Trait ready, no implementation |
| Filter / projection pushdown | Read-path reads everything, post-filters in the executor |
| INSERT / UPDATE / DELETE into foreign tables | Scan-only in Phase 3.2 |

## Writing a custom wrapper (Rust)

```rust
use reddb::storage::fdw::{ForeignDataWrapper, ForeignTableDefinition};
use reddb::RedDBResult;
use reddb::storage::unified::entity::UnifiedEntity;

struct MyWrapper { /* connection config */ }

impl ForeignDataWrapper for MyWrapper {
    fn name(&self) -> &str { "mywrapper" }

    fn scan(
        &self,
        table: &ForeignTableDefinition,
    ) -> RedDBResult<Vec<UnifiedEntity>> {
        // fetch from remote source, build entities
        todo!()
    }
}

// register at startup
let registry = runtime.foreign_tables();
registry.register_wrapper("mywrapper", Arc::new(MyWrapper { ... }));
```

## See also

- [CSV Import / COPY](../query/maintenance.md#csv-import--copy)
- [CREATE TABLE](../query/create-table.md)
