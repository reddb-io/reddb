# Dependency Graph

RedDB tracks migration dependencies as a directed acyclic graph (DAG). Every
node is a migration; every directed edge means "this migration must be applied
before that one." The engine enforces the graph at both registration time
(cycle detection) and application time (ordering).

---

## Manual dependencies: `DEPENDS ON`

You declare an explicit dependency with `DEPENDS ON` in `CREATE MIGRATION`:

```sql
CREATE MIGRATION add_score_index
DEPENDS ON add_score_column
AS
  CREATE INDEX idx_users_score ON users (score);
```

Multiple dependencies:

```sql
CREATE MIGRATION build_leaderboard_view
DEPENDS ON add_score_column, add_rank_column
AS
  CREATE VIEW leaderboard AS
    SELECT id, name, score, rank
    FROM users
    ORDER BY score DESC;
```

Explicit edges are stored in `red_migration_deps` with `inferred = false`.

---

## Automatic dependency inference

When you register a migration, RedDB scans the body before storing it.
The scanner looks for these SQL keywords followed by a collection name:

| Keyword | Context |
|---|---|
| `FROM` | `SELECT ... FROM table_name` |
| `INTO` | `INSERT INTO table_name` |
| `TABLE` | `ALTER TABLE table_name`, `CREATE INDEX ... ON table_name` |
| `UPDATE` | `UPDATE table_name` |
| `JOIN` | `... JOIN table_name ON ...` |

For each collection name found, the engine queries `red_migrations` to find
all previously registered migrations that also reference the same collection.

- **Zero matches**: no inferred edge is created.
- **Exactly one match**: an edge is created automatically. The row is inserted
  into `red_migration_deps` with `inferred = true`.
- **Two or more matches**: the dependency is ambiguous. No edge is
  auto-created. A warning is returned:

```
WARNING: migration 'backfill_scores' touches collection 'users', which is also
touched by ['add_score_column', 'add_legacy_field']. Dependency is ambiguous —
add an explicit DEPENDS ON clause if ordering matters.
```

You can then add an explicit dependency:

```sql
CREATE MIGRATION backfill_scores
DEPENDS ON add_score_column
AS
  UPDATE users SET score = 0 WHERE score IS NULL;
```

### What the scanner detects

The scanner is a keyword-level SQL parser — not a full semantic parser. It
finds collection names by their syntactic position after the keywords above.
It does not:

- Resolve subqueries to their referenced collections.
- Follow CTEs or named subselects.
- Detect collections referenced inside function calls.
- Detect collections referenced by synonyms, aliases, or views.

**Practical implication**: if your migration body contains a subquery that
touches an important collection, auto-inference may miss the dependency.
Always add an explicit `DEPENDS ON` when a dependency is semantically
important but might not be detectable from the top-level keywords.

**Example of a dependency the scanner misses:**

```sql
-- The scanner sees: UPDATE invoices
-- It does NOT see: FROM orders (inside the subquery)
CREATE MIGRATION backfill_invoice_amounts
AS
  UPDATE invoices
  SET total = (
    SELECT sum(amount) FROM orders WHERE order_id = invoices.id
  );
```

In this case, add an explicit dependency:

```sql
CREATE MIGRATION backfill_invoice_amounts
DEPENDS ON add_orders_amount_column
AS
  UPDATE invoices
  SET total = (
    SELECT sum(amount) FROM orders WHERE order_id = invoices.id
  );
```

---

## Cycle detection

The DAG constraint is enforced at `CREATE MIGRATION` time, not at apply time.
The moment a `DEPENDS ON` clause or an inferred edge would create a cycle,
the registration fails and the migration is not stored.

**Example:**

```sql
CREATE MIGRATION m_a AS ...;
CREATE MIGRATION m_b DEPENDS ON m_a AS ...;
CREATE MIGRATION m_a_v2 DEPENDS ON m_b AS ...;
-- ^ This would create a cycle if m_a_v2 is a rename of m_a — but since
--   m_a already exists and m_a_v2 is a new name, this is fine.

-- The cycle would look like this:
CREATE MIGRATION m_c DEPENDS ON m_b AS ...;
-- And then:
ALTER MIGRATION m_a ADD DEPENDS ON m_c; -- hypothetical
-- which would create: m_a → m_b → m_c → m_a — rejected.
```

When cycle detection fires:

```
ERROR: cycle detected in migration dependency graph: add_index → backfill_data → add_index
```

The error message includes the full cycle path. Fix it by removing the
dependency that closes the cycle.

---

## `EXPLAIN MIGRATION` for dependency inspection

`EXPLAIN MIGRATION <name>` shows the full dependency chain resolved at
query time:

```sql
EXPLAIN MIGRATION build_leaderboard_view;
```

```
name              : build_leaderboard_view
status            : pending
dependency_chain  : [add_score_column, add_rank_column]

dependencies:
  - add_score_column  (applied, explicit)
  - add_rank_column   (pending, explicit)
```

`EXPLAIN MIGRATION *` returns the full topological ordering of all pending
migrations:

```
pending migrations (topological order):
  1. add_rank_column         (no dependencies)
  2. build_leaderboard_view  (depends on: add_score_column ✓, add_rank_column)
```

The `✓` marker indicates a dependency that is already applied.

---

## Querying `red_migration_deps` directly

The dependency table is a first-class collection. Query it with `SELECT`:

```sql
-- All explicit dependencies
SELECT migration_id, depends_on_id
FROM red_migration_deps
WHERE inferred = false;

-- All inferred dependencies
SELECT migration_id, depends_on_id
FROM red_migration_deps
WHERE inferred = true;

-- Full upstream chain for a specific migration
SELECT m.name, d.depends_on_id, d.inferred
FROM red_migration_deps d
JOIN red_migrations m ON m.name = d.migration_id
WHERE d.migration_id = 'build_leaderboard_view';

-- Migrations with no dependencies (roots of the DAG)
SELECT name FROM red_migrations
WHERE name NOT IN (SELECT migration_id FROM red_migration_deps)
AND status = 'pending';

-- Migrations that are ready to apply right now
-- (all dependencies satisfied, none still pending)
SELECT m.name
FROM red_migrations m
WHERE m.status = 'pending'
AND NOT EXISTS (
  SELECT 1
  FROM red_migration_deps d
  JOIN red_migrations dep ON dep.name = d.depends_on_id
  WHERE d.migration_id = m.name
  AND dep.status != 'applied'
);
```

---

## Topological sort: Kahn's algorithm

When you run `APPLY MIGRATION *`, the engine applies Kahn's algorithm over
the pending set:

1. Build an in-degree map: for each pending migration, count how many of
   its dependencies are also pending (not yet applied).
2. Enqueue all migrations with in-degree 0 (no pending dependencies).
3. Pop from the queue, apply the migration, decrement the in-degree of all
   migrations that depend on it.
4. Enqueue any migration whose in-degree has reached 0.
5. Repeat until the queue is empty.

Since cycles are rejected at `CREATE MIGRATION` time, step 5 always
terminates — you cannot reach a state where pending migrations exist but
none can be applied because they all wait on each other.

---

## DAG visualization

You can generate a DOT-format visualization of the dependency graph by
querying the system tables and formatting the output:

```sql
SELECT
  'digraph migrations {' AS line
UNION ALL
SELECT
  '  "' || depends_on_id || '" -> "' || migration_id || '";'
FROM red_migration_deps
UNION ALL
SELECT '}';
```

Pipe the output to `dot -Tsvg > migrations.svg` to render the graph.

---

## Common mistakes

**Registering migrations without checking for ambiguity.** If you have
three migrations that all touch `users`, each new migration will produce
an ambiguity warning. Add explicit `DEPENDS ON` to the migrations where
ordering matters.

**Assuming auto-inference catches subquery references.** It does not. Always
add explicit deps for migrations that depend on the output of another
migration via a subquery.

**Creating migrations that form near-cycles through aliased tables.** The
scanner matches collection names literally — if your migration uses an alias
(`FROM users u`), the scanner reads `users`, not `u`. This is usually
correct, but verify with `EXPLAIN MIGRATION` if you are unsure.
