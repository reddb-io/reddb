# Row-Level Security (RLS)

RedDB implements PostgreSQL-style row-level security: arbitrary
boolean predicates that gate every read and mutation on a table,
evaluated per-row against session state.

## Quick reference

```sql
CREATE POLICY policy_name
  ON table_name
  [FOR SELECT|INSERT|UPDATE|DELETE|ALL]
  [TO role_name]
  USING (predicate_expression);

DROP POLICY [IF EXISTS] policy_name ON table_name;

ALTER TABLE table_name ENABLE  ROW LEVEL SECURITY;
ALTER TABLE table_name DISABLE ROW LEVEL SECURITY;
```

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
