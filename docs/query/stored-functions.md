# Stored RQL functions

Stored functions let application developers name and persist database operations.
RedDB supplies the evaluator, catalog, permissions and transaction boundary; the
application supplies each function body. This initial implementation is a bounded,
sequential subset of RQL, invoked with `CALL`.

```sql
CREATE FUNCTION total(price INTEGER, quantity INTEGER)
RETURNS INTEGER EFFECT PURE AS 'SELECT $1 * $2';

CALL total(12, 3);

CREATE FUNCTION adjust(delta INTEGER, item INTEGER)
RETURNS TABLE (id INTEGER, amount INTEGER) EFFECT WRITE AS
'UPDATE accounts SET amount = amount + $1 WHERE id = $2;
 SELECT id, amount FROM accounts WHERE id = $2';

CALL adjust(5, 1);
SHOW FUNCTION adjust;
SHOW FUNCTIONS;
DROP FUNCTION IF EXISTS total;
```

Create `accounts` as a TABLE with `id` and `amount` INTEGER columns before calling
`adjust`. `ALTER FUNCTION` takes the entire signature, effect and body and requires
an existing function. `CREATE FUNCTION` rejects duplicate names. Names are unique
within the current tenant; there is no argument-type overloading.

Arguments are positional expressions. Signature names describe `$1`, `$2`, etc.;
they are not automatically local variables. Each body statement may use a subset
of those arguments. Use the normal parameterized query API for `CALL f($1, $2)`;
values are substituted in the AST, never interpolated into the body source.

Arguments and results use the collection contract type normalizer. They accept
NULL; this syntax does not yet declare NOT NULL parameters. A scalar result must
have exactly one row and one column, exposed as `value`. A TABLE result projects
the named fields from the final statement and validates their declared types.
There is no VOID result in this slice. Invalid results fail before committing
writes performed by the call.

## Effects and transactions

- `PURE`: source-free scalar SELECT expressions using admitted built-ins.
- `READ`: admitted table, join, graph and vector reads, without mutation.
- `WRITE`: those reads plus INSERT, UPDATE, DELETE, QUEUE PUSH and KV PUT/DELETE.

The compiler rejects unknown or volatile built-ins, external embedding requests,
transaction controls, DDL, nested CALL, table-valued functions, window expressions,
expression subqueries, AS OF and EXPAND. Vector inputs are literal vectors or
stored references. This effect declaration is an admission boundary, not a claim
that every RQL feature has an effect annotation.

READ and WRITE calls start a transaction if the caller has none. Within an existing
transaction they use an internal savepoint. A failed body or return validation
rolls back the call's writes; the caller's earlier writes remain. Successful calls
inside a transaction remain subject to the caller's COMMIT or ROLLBACK. Transaction
isolation and the durability configuration remain those of the ordinary engine.
Function DDL requires autocommit; transactional catalog changes are not supported.

CALL requires EXECUTE on the named function. Every body statement also checks the
invoker's ordinary collection, column and row permissions. There is no definer
privilege elevation. SHOW FUNCTIONS filters its entries using EXECUTE permissions.
The existing IAM/legacy-RBAC mode determines the default permissions; deploying a
restrictive policy requires configuring that mode and grants.

## Persistence, replacement and export

Definitions compile at CREATE/ALTER and at runtime opening. An invalid persisted
catalog prevents opening. CALL takes an Arc to the current compiled definition;
an ALTER affects later calls while an already-started call retains its definition.
CALL results bypass the result cache. SHOW FUNCTION returns canonical CREATE DDL
that can be replayed through the same public query API to import a definition.

The initial catalog uses a reserved internal `red_config` entry. SET CONFIG,
configuration export and inline CONFIG access do not expose or modify that entry.
Raw access to internal storage is a trusted administrative interface; applications
must not grant tenants direct access to internal `red_*` collections. This is not
a separate security boundary around a process that already owns the embedded DB.

Cost sketch: a call clones one compiled-definition Arc and binds O(body AST size)
values, followed by the normal cost of its statements and one transaction boundary.
DDL copies/serializes the catalog (at most 4 MiB) and calls the existing database
flush. A catalog persistence error makes further function operations fail until
runtime reopening; the DDL outcome is indeterminate. Catalog snapshots are append-only; repeated DDL consumes storage until the
catalog gains compaction in a later slice. This is a control-plane implementation,
not a measured DDL throughput optimization.

## Bounds and current limits

| Resource | Initial limit |
|---|---|
| Body source | 65,536 bytes and 1,024 lexer tokens |
| Body statements | 32 |
| Parameters / declared TABLE result columns | 32 each |
| Query nesting | 32 |
| Expression/projection nesting | 64 |
| Materialized result per statement | 10,000 rows |
| Catalog | 1,024 functions, 4 MiB serialized |

The result limit is checked after statement execution. It is **not** a hard limit
on scanned rows, elapsed time or peak memory. Loops and recursive calls are
unsupported; execution deadlines, cumulative work/memory accounting and interrupt
checks remain future work. Native integration tests do not establish every
transport/SDK's behavior. Clean reopen is not evidence of power-loss recovery.

Functions are a foundation for later collection rules and declarative endpoints.
This slice does not implement either, nor does it establish performance parity
with SQLite, PostgreSQL, Cassandra or SurrealDB.
