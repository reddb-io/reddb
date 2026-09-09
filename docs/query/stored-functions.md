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
| Cumulative execution work | 1,000,000 instrumented units per CALL |
| Cooperative execution deadline | 5,000 ms per CALL |

The result limit is checked after statement execution. It is **not** a hard limit
on scanned rows or peak memory. Loops and recursive calls remain unsupported.
Native integration tests do not establish every transport/SDK's behavior. Clean
reopen is not evidence of power-loss recovery.

### Cooperative execution budgets

Each CALL shares one budget across its body statements and return validation.
Operators can tighten the default ceilings with ordinary configuration:

```sql
SET CONFIG functions.execution.work_max = 100000;
SET CONFIG functions.execution.timeout_ms = 1000;
```

Values above the defaults are capped at the defaults; zero rejects CALL rather
than disabling the budget. Configuration is read once when CALL starts. These
settings do not change ordinary SQL execution or the persisted function format.
The existing configuration authorization rules apply.

Work units count body statements, MVCC-visible candidates visited by the instrumented table
scan callbacks (before predicate filtering), indexed candidate processing, join
build/probe/pair iterations, affected rows reported by statements, and TABLE
return rows normalized by CALL. Accounting is cumulative and depends on the
chosen execution plan; it is not a count of CPU instructions or physical reads.

Multimodel paths share that same counter. Exact vector search charges visible
candidate-ID discovery and candidate processing before distance evaluation.
TurboQuant charges the filled lanes before each block of up to 32 vectors, then
charges candidate conversion and exact reranking. Graph materialization charges
every visible candidate inspected in each of its two passes over all collections,
including non-graph entities. It collects IDs and fetches/processes payloads only
for the current pass's kind (nodes or edges), charging that processing separately.
Pattern matching charges seed nodes, partial matches, edge candidates and projected
matches. Hybrid fusion charges its input-map entries and fused candidates.
A small LIMIT or an empty final result does not exempt input work from accounting.
An exhausted call returns `stored function: execution work_max exceeded` or
`stored function: execution timeout_ms exceeded`, including both limits.

Instrumented sequential table scans, universal collection scans and aggregate
input scans stop at the next candidate that exceeds the work budget. Runtime
joins check before each build/probe/pair step; CROSS JOIN under CALL grows its
result incrementally instead of reserving the whole Cartesian product first.
An interrupted scan returns an error, never a successful partial result.
Statement boundaries force a clock check; charged loops sample elapsed time
at least every 256 work units. Exhaustion rolls back the call's writes, including
when a later statement fails after earlier writes. In an existing transaction,
the caller's earlier writes and savepoints remain usable. The budget is removed
before COMMIT/ROLLBACK so cleanup can complete.

Coverage is intentionally incomplete: individual vector distance calculations,
TurboQuant query rotation/LUT preparation, index readiness/rebuild, graph adjacency
list construction, mutation internals, invisible-version traversal, index candidate
discovery/batch fetching, scalar built-ins, sorting and aggregate finalization are
not yet fully interruptible. Graph analytics/shortest-path APIs outside admitted
CALL graph patterns do not acquire this budget. Their elapsed time
is checked when they return to an instrumented boundary; mutation counts are
charged after execution. Blocking I/O and lock waits cannot be preempted by these
checks. Commit/rollback time is outside the execution deadline. This is therefore
**not a hard wall-clock or peak-memory guarantee** and does not implement client
cancel requests for CALL.

CALL uses sequential fallbacks for the table/aggregate scan paths that would
otherwise start workers without inheriting the thread-local execution context.
Graph materialization filters entity kinds before copying payloads, including in
mixed collections. Budgeted materialization collects only matching IDs under the
segment lock and fetches entities in batches of 256 before evaluating RLS outside that lock; it
preserves the captured MVCC view. Unbudgeted graph materialization retains its
existing scan path. Parallel budget propagation remains pending. Ordinary SQL retains its existing
parallel paths. Cost sketch: N visited candidates add N budget checks and about
N/256 clock samples, with no per-candidate budget allocation, locks or atomics;
ordinary scan callbacks bypass per-row accounting. Reading the two limits uses
the existing configuration accessors, each scanning `red_config`; long config
histories add lookup cost. This is an implementation cost estimate, not a
measured throughput claim. Exact vector search adds about 2N work units for N
candidates; TurboQuant adds one callback per scoring block plus candidate/rerank
checks. For N visible entities and G graph entities, graph materialization charges
about 2N + G units before pattern expansion, while retaining the existing two
collection passes. Vector ID lists, TurboQuant score buffers and graph
adjacency lists still use O(input size) memory; these checks are not a memory cap.

Functions are a foundation for later collection rules and declarative endpoints.
This slice does not implement either, nor does it establish performance parity
with SQLite, PostgreSQL, Cassandra or SurrealDB.
