# Row-Level Security (RLS)

RedDB implements PostgreSQL-style row-level security: arbitrary
boolean predicates that gate every read and mutation on a table,
evaluated per-row against session state.

## Quick reference

```sql
CREATE POLICY policy_name
  ON [<kind> OF] table_or_collection_name
  [FOR SELECT|INSERT|UPDATE|DELETE|ALL]
  [TO role_name]
  USING (predicate_expression);

DROP POLICY [IF EXISTS] policy_name ON collection_name;

ALTER TABLE collection_name ENABLE  ROW LEVEL SECURITY;
ALTER TABLE collection_name DISABLE ROW LEVEL SECURITY;
```

`<kind>` is one of `NODES`, `EDGES`, `VECTORS`, `MESSAGES`, `POINTS`,
`DOCUMENTS`, or `TABLE` (default). Kind-scoped policies only gate
the matching entity kind — a `NODES` policy never filters a table
SELECT on the same collection.

Policies are inert until `ENABLE ROW LEVEL SECURITY` is run on the
table. Once enabled, **every** matching action is filtered by every
matching policy (OR-combined per action, AND-folded into the user's
`WHERE`).

## Gate model

RedDB's RLS uses PG's permissive-default evaluation:

- For a given `(table, role, action)`, collect every policy that
  matches.
- Combine predicates with `OR` — a row passes if **any** policy
  accepts it.
- AND the combined predicate into the user's `WHERE` before execution.
- Zero matching policies ⇒ zero rows visible (restrictive default).

When RLS is disabled on a table, policies remain defined but are
ignored.

## Examples

### Per-user ownership

Users see only their own rows:

```sql
CREATE POLICY own_rows
  ON documents
  USING (owner_id = CURRENT_USER());

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
```

### Role-scoped policies

Different policies for different roles:

```sql
-- analysts see everything non-draft
CREATE POLICY analyst_read
  ON documents
  FOR SELECT
  TO analyst
  USING (status != 'draft');

-- editors see their drafts + published
CREATE POLICY editor_read
  ON documents
  FOR SELECT
  TO editor
  USING (status = 'published' OR owner_id = CURRENT_USER());

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
```

A logged-in `analyst` gets only the analyst policy. A logged-in
`editor` gets only the editor policy. An unauthenticated session
matches neither — zero rows.

### Multi-tenant with shared public rows

Each tenant sees its own rows **plus** rows marked public:

```sql
CREATE POLICY tenant_or_public
  ON articles
  USING (org = CURRENT_TENANT() OR visibility = 'public');

ALTER TABLE articles ENABLE ROW LEVEL SECURITY;
```

### Action-specific policy

Read and delete gated differently:

```sql
CREATE POLICY read_own
  ON orders
  FOR SELECT
  USING (customer_id = CURRENT_USER());

CREATE POLICY cancel_own_pending
  ON orders
  FOR DELETE
  USING (customer_id = CURRENT_USER() AND status = 'pending');

ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
```

### Per-entity-kind policies

RLS works across every data model, not just tables. Target the kind
explicitly with `ON <kind> OF <collection>`:

```sql
-- graph nodes
CREATE POLICY own_nodes ON NODES OF social
  USING (properties.owner = CURRENT_USER());

-- graph edges
CREATE POLICY my_relations ON EDGES OF social
  USING (properties.visibility = 'public');

-- vector collections (metadata-scoped)
CREATE POLICY tenant_vecs ON VECTORS OF articles
  USING (metadata.tenant = CURRENT_TENANT());

-- queue messages (payload-scoped)
CREATE POLICY my_jobs ON MESSAGES OF jobs
  USING (payload.user_id = CURRENT_USER());

-- timeseries points (tag-scoped)
CREATE POLICY my_hosts ON POINTS OF metrics
  USING (tags.host IN (SELECT host FROM my_hosts));

ALTER TABLE jobs ENABLE ROW LEVEL SECURITY;
```

The predicate evaluates against the entity's native fields —
`properties` for nodes/edges, `metadata` for vectors, `payload`
JSON for queue messages, `tags` for timeseries points. Dotted
paths work on any of them (same resolver as
[Multi-Tenancy dotted paths](multi-tenancy.md#dotted-paths—json-native-tenancy)).

Legacy `TABLE` policies on the same collection still apply to
non-tabular reads for backwards compatibility — `CREATE TABLE ...
TENANT BY (col)` installs its auto-policy under `TABLE` and the
evaluator applies it to any kind.

## Session context in policies

Policies typically reference one of the built-in session scalars:

| Scalar | Source |
|--------|--------|
| `CURRENT_USER()` / `SESSION_USER()` / `USER` | Auth thread-local (username) |
| `CURRENT_ROLE()` | Auth thread-local (role name) |
| `CURRENT_TENANT()` | Session tenant handle |

These are marked `Volatile` in the function catalog so the planner
never constant-folds them across rows.

## Relationship with declarative tenancy

`CREATE TABLE ... TENANT BY (col)` installs a reserved policy named
`__tenant_iso` automatically. You can add your own policies alongside
it; RLS OR-combines them:

```sql
CREATE TABLE articles (...) TENANT BY (org);
-- implicit policy __tenant_iso: USING (org = CURRENT_TENANT())

CREATE POLICY public_reads
  ON articles
  FOR SELECT
  USING (visibility = 'public');
-- now a reader sees: own-tenant rows OR public rows
```

See [Multi-Tenancy](multi-tenancy.md) for the declarative form.

## Observability

`ALTER TABLE ... ENABLE ROW LEVEL SECURITY` persists the toggle to
`red_config.rls.enabled.{table}` so it survives restart. Policy
definitions live in memory for now — re-run `CREATE POLICY` on boot
if you need them preserved.

## Limitations

- No `WITH CHECK` separate-from-`USING` clause yet. INSERT gating is
  handled via auto-fill (for tenancy) or by policies whose `USING`
  predicate evaluates in the insert path.
- Policies are in-memory; persistence of `CREATE POLICY` definitions
  across restart is on the roadmap.
- `BYPASSRLS` privilege for admin users is planned.

## See also

- [Multi-Tenancy](multi-tenancy.md)
- [Transactions & MVCC](../query/transactions.md)
- [Auth Overview](overview.md)
