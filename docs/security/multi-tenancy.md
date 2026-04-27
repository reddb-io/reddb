# Multi-Tenancy

RedDB ships first-class multi-tenancy: a session-scoped tenant handle,
a declarative `TENANT BY (col)` clause on `CREATE TABLE`, and
auto-wired row-level security that keeps every read and write scoped
to the current tenant.

Tenant scoping is enforced by every policy via the `tenant_match: true`
condition or the implicit `current_tenant` prefix on resources — see
[Policies](policies.md) for the authorization vocabulary.

## Three isolation patterns

| Pattern | Mechanism | Storage | Use when |
|---------|-----------|---------|----------|
| **Row-based declarative** | `CREATE TABLE ... TENANT BY (col)` | Shared collection | Default SaaS; low overhead, many small tenants |
| **Row-based manual** | `CREATE POLICY` + `SET TENANT` | Shared collection | Custom predicates, multiple discriminators |
| **Schema-based** | `CREATE SCHEMA; CREATE TABLE acme.users (...)` | Namespaced | Strong isolation, per-tenant migrations |
| **Collection-per-tenant** | `users_acme`, `users_globex` | Separate collections | Per-tenant scale / retention policies |

## Session tenant handle

Every connection carries a thread-local tenant id. Set it via SQL or
from a transport middleware after resolving the tenant from auth
claims (JWT `tenant` claim, HTTP header, subdomain, mTLS cert OID…).

```sql
SET TENANT 'acme';
SHOW TENANT;             -- returns 'acme'
RESET TENANT;            -- clears
SET TENANT NULL;         -- equivalent to RESET
```

Scalar functions read the handle:

| Function | Returns |
|----------|---------|
| `CURRENT_TENANT()` | Thread-local tenant id or NULL |
| `CURRENT_USER()` / `SESSION_USER()` / `USER` | Authenticated username or NULL |
| `CURRENT_ROLE()` | Role name (lowercase) or NULL |

## Declarative tenancy

`TENANT BY (col)` declares the tenant discriminator column. RedDB
automatically:

1. Records the mapping so INSERT can auto-fill the column
2. Installs an implicit RLS policy `__tenant_iso` equivalent to
   `USING (col = CURRENT_TENANT())` for every action
3. Enables row-level security on the table

```sql
CREATE TABLE users (
  id INT,
  email TEXT,
  tenant_id TEXT
) TENANT BY (tenant_id);

-- alternative (explicit WITH clause)
CREATE TABLE users (...)
  WITH (tenant_by = 'tenant_id');
```

### Dotted paths — JSON-native tenancy

`TENANT BY (col)` also accepts dotted paths so you can store the
tenant discriminator inside a JSON column, a graph node's
properties, a queue message's payload, or a timeseries tag map.

```sql
-- tenant inside a JSON column
CREATE TABLE events (id INT, meta TEXT)
  TENANT BY (meta.tenant);

-- same pattern works on any non-table kind via RLS policies
CREATE POLICY tenant_vecs ON VECTORS OF articles
  USING (metadata.tenant = CURRENT_TENANT());

CREATE POLICY tenant_jobs ON MESSAGES OF jobs
  USING (payload.tenant = CURRENT_TENANT());
```

INSERT auto-fill handles the nested case too:

```sql
SET TENANT 'acme';

-- no root column supplied → auto-creates {"tenant": "acme"}
INSERT INTO events (id) VALUES (1);

-- user provides the root but omits the tenant key → merge
INSERT INTO events (id, meta) VALUES (2, '{"trace_id": "abc"}');
-- stored: {"trace_id": "abc", "tenant": "acme"}

-- user provides the full path → trusted, no overwrite (admin
-- bulk-load from CSV on behalf of multiple tenants)
INSERT INTO events (id, meta) VALUES (3, '{"tenant": "globex"}');
```

The dotted-path resolver is lenient: columns declared as `TEXT`
but containing JSON strings are parsed transparently, so you don't
have to change column types to adopt tenancy.

### Auto-fill on INSERT

```sql
SET TENANT 'acme';

-- tenant_id omitted → auto-filled from CURRENT_TENANT()
INSERT INTO users (id, email) VALUES (1, 'a@b');
-- stored as (1, 'a@b', 'acme')

-- admin can target another tenant by naming the column explicitly
INSERT INTO users (id, email, tenant_id) VALUES (2, 'x@y', 'globex');
```

If the user omits the tenant column and no `SET TENANT` is active,
the INSERT fails loud:

```
Error: INSERT into tenant-scoped table 'users' requires an active
tenant — run SET TENANT '<id>' first or name column 'tenant_id'
explicitly
```

### Auto-filter on SELECT / UPDATE / DELETE

```sql
SET TENANT 'acme';
SELECT * FROM users;                      -- only tenant_id='acme' rows
UPDATE users SET email='c@d' WHERE id=1;  -- gated by RLS
DELETE FROM users WHERE id=1;              -- gated by RLS

SET TENANT 'globex';
SELECT * FROM users;                      -- only globex rows

RESET TENANT;
SELECT * FROM users;                      -- zero rows (policy hides all)
```

## Retrofit onto existing tables

```sql
-- day 1 — table exists without tenancy
CREATE TABLE orders (id INT, amount DECIMAL, org TEXT);
INSERT INTO orders VALUES (1, 100, 'acme'), (2, 200, 'globex');

-- day 2 — enable tenancy without dropping data
ALTER TABLE orders ENABLE TENANCY ON (org);

-- day 3 — use normally
SET TENANT 'acme';
SELECT * FROM orders;     -- only (1, 100, 'acme')

-- later — turn off
ALTER TABLE orders DISABLE TENANCY;
```

## Persistence across restart

Tenant-table markers are persisted to the internal `red_config`
collection (`tenant_tables.{table}.column`). On boot, RedDB replays
every marker and re-installs the auto-policy + in-memory registry.

## Integration patterns

### HTTP / REST

Bind the tenant from a header in your middleware, then every downstream
SQL sees the scope automatically:

```rust
let tenant = headers
    .get("X-Tenant-Id")
    .and_then(|h| h.to_str().ok())
    .unwrap_or_default();
reddb::runtime::impl_core::set_current_tenant(tenant.to_string());
// ... handle request ...
reddb::runtime::impl_core::clear_current_tenant();
```

### gRPC

Extract from metadata in the interceptor; same thread-local API.

### PostgreSQL wire

Clients can issue `SET TENANT 'acme'` as the first statement after
authenticating — no driver changes needed.

### Connection pool

Configure the pool to run `SET TENANT '$tenant'` on connection checkout
based on the requesting user's context, and `RESET TENANT` on return.

## Manual RLS (advanced)

When the declarative form isn't enough (multiple discriminators,
custom predicates, shared-with-role rows), write the policy yourself:

```sql
CREATE POLICY tenant_iso_plus_shared
  ON documents
  USING (org = CURRENT_TENANT() OR visibility = 'public');

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
```

See [Row Level Security](rls.md) for the full policy grammar.

## Tenants are not entities

There is no `CREATE TENANT` command and no internal tenant registry.
A "tenant" is an opaque string carried by `current_tenant()`; it
springs into existence the moment a row is inserted with that value
and disappears when the last row referencing it is deleted. Two
consequences:

1. **No spelling protection** — `'acme'` and `'Acme'` are different
   tenants. RLS will happily isolate them as such. If you need
   canonical IDs, enforce them at the application layer or via a
   catalog table (below).
2. **No tenant lifecycle hooks** — there is no "tenant created" /
   "tenant deleted" event. Provisioning, billing, and quota tracking
   live in your application or a catalog table you manage.

### The catalog pattern

The recommended layout for a SaaS deployment:

```sql
-- Catalog: NOT tenant-scoped. The application owns it. One row per
-- customer. Use whatever discriminator you want (UUID, slug, …).
CREATE TABLE tenants (
  id          TEXT PRIMARY KEY,
  display_name TEXT,
  plan         TEXT,
  created_at   TIMESTAMP DEFAULT NOW()
);

-- Data tables: tenant-scoped. Reference the catalog by convention
-- (RedDB has no foreign keys yet — wire the integrity check in your
-- service layer).
CREATE TABLE orders (
  id        INT,
  amount    DECIMAL,
  tenant_id TEXT
) TENANT BY (tenant_id);
```

Provisioning a new customer becomes:

```sql
INSERT INTO tenants (id, display_name, plan) VALUES ('acme', 'Acme Corp', 'pro');

WITHIN TENANT 'acme' INSERT INTO orders (id, amount) VALUES (1, 100);
```

The `tenants` row gives admin tooling something to enumerate; the
catalog is invisible to RLS so admins always see every customer
regardless of their session tenant.

## Cold-start (no tenant bound)

When a connection has no `SET TENANT`, no `SET LOCAL TENANT`, and no
`WITHIN TENANT '…'` prefix on the statement:

| Operation                             | Result                                    |
|---------------------------------------|-------------------------------------------|
| `SELECT` on a `TENANT BY` table       | 0 rows (RLS deny-default)                 |
| `UPDATE` / `DELETE` on a `TENANT BY` table | Affects 0 rows (RLS gate)            |
| `INSERT` into a `TENANT BY` table without naming the tenant column | **Error** — loud failure |
| `INSERT` into a `TENANT BY` table naming the tenant column explicitly | Succeeds — uses the named value |
| Any operation on a non-tenant table   | Works normally                            |

Why deny-default: `current_tenant()` returns `NULL`, so the auto-policy
predicate becomes `tenant_id = NULL`. Per SQL three-valued logic that
evaluates to `UNKNOWN`, never `TRUE`, so no row passes the filter. The
behaviour matches every other RLS policy gated on a missing identity.

The loud INSERT error is intentional — silently writing rows with
`NULL` tenant ids would leak into a "global" bucket no other session
could see. Better to fail and force the caller to pick a tenant.

To bootstrap the first tenant from an unscoped admin session:

```sql
-- Either: name the column explicitly (no SET TENANT needed)
INSERT INTO orders (id, amount, tenant_id) VALUES (1, 100, 'acme');

-- Or: bind the tenant first
SET TENANT 'acme';
INSERT INTO orders (id, amount) VALUES (2, 200);
```

The catalog table is the natural seed point — it has no `TENANT BY`,
so admins can populate it freely and then switch into the new
tenant's scope to seed the data tables.

## Per-statement & transaction-scoped overrides

Three layers can carry a tenant binding, in order of precedence
(highest wins):

1. **`WITHIN TENANT '<id>' [USER '<u>'] [AS ROLE '<r>'] <stmt>`** —
   single-statement override. Stack-scoped via an RAII guard, so a
   pool-shared connection cannot leak the binding to the next
   request and an early `?` return still pops cleanly. Designed for
   stateless SaaS APIs where one connection serves many tenants.

   ```sql
   WITHIN TENANT 'acme' SELECT * FROM orders;
   WITHIN TENANT 'acme' UPDATE orders SET status='paid' WHERE id=1;
   WITHIN TENANT 'acme' USER 'filipe' AS ROLE 'admin'
     SELECT * FROM audit_log;          -- USER/ROLE project into
                                       -- CURRENT_USER / CURRENT_ROLE
                                       -- without granting privilege
   WITHIN TENANT NULL SELECT * FROM orders;   -- explicit clear
   ```

2. **`SET LOCAL TENANT '<id>'`** — transaction-local override. Only
   valid inside an active transaction; auto-evicted on `COMMIT` or
   `ROLLBACK`. Useful when several statements in a transaction must
   share a tenant binding without repeating `WITHIN` on each:

   ```sql
   BEGIN;
   SET LOCAL TENANT 'acme';
   INSERT INTO orders (id, amount) VALUES (3, 300);
   UPDATE orders SET status='paid' WHERE id=3;
   COMMIT;     -- override gone, session tenant resumes
   ```

3. **`SET TENANT '<id>'` / `RESET TENANT` / `SET TENANT NULL`** —
   session-level binding via thread-local. Persists for the
   connection's lifetime. Best when one connection serves one tenant
   for its whole lifecycle (worker per tenant, dedicated pool).

`USER` and `AS ROLE` overrides only affect the values returned by
`CURRENT_USER` / `CURRENT_ROLE()` inside SQL — they do **not** elevate
RBAC privileges. The connection's real identity (set via
`set_current_auth_identity` from a transport layer) still gates DDL,
vault access, and other admin operations.

## Multi-model coverage

| Model           | Tenant filter via `ON <coll>` policy | `WITHIN TENANT '…'` works |
|-----------------|--------------------------------------|---------------------------|
| Tables (SELECT/UPDATE/DELETE/INSERT) | ✓                              | ✓                         |
| JOINs of tenant-scoped tables        | ✓                              | ✓                         |
| Queue MESSAGES (LEN / POP / PEEK)    | ✓                              | ✓                         |
| Timeseries POINTS (SELECT)           | ✓ (`ON <ts>` form)             | ✓                         |
| Graph NODES / EDGES (MATCH)          | ✗ (executor does not gate)     | ✗                         |
| Vectors (SIMILAR / SEARCH)           | ✓ (post-filter)                | ✓                         |

The `CREATE POLICY ON NODES OF / ON POINTS OF / ON MESSAGES OF / ON
VECTORS OF` *kind-targeted* form is parsed and stored for every
collection kind, but only the queue MESSAGES read path queries the
kind-targeted policy registry today. Tables, timeseries, and vectors
read the *basic* `ON <coll>` (kind=Table) form — use that form for
SaaS-style tenant isolation. Graph MATCH currently ignores both forms
(tracked as a gap).

## Auto-index on the tenant column

Declaring `TENANT BY (col)` automatically creates a hash index named
`__tenant_idx_{table}` on the discriminator column. Since the
auto-policy adds `col = CURRENT_TENANT()` to every read/write, that
column is on the hot path of every query — without an index the
filter degrades to a full scan. The index is skipped when:

- the tenant column is dotted (`metadata.tenant`) — flat secondary
  indices don't cover nested paths today
- a user-defined index already covers the column as its leading key
- the index already exists (idempotent across boot rehydrate)

`ALTER TABLE … DISABLE TENANCY` and `DROP TABLE` clean it up.

## Debugging

| Statement              | Returns                                           |
|------------------------|---------------------------------------------------|
| `SHOW TENANT`          | The currently bound tenant id, or `NULL`          |
| `SELECT CURRENT_TENANT()` | Same value, usable in expressions              |
| `SELECT CURRENT_USER()`   | Projected user (override or auth identity)     |
| `SELECT CURRENT_ROLE()`   | Projected role (override or auth identity)     |

When a query unexpectedly returns zero rows on a `TENANT BY` table,
check `SHOW TENANT` first — an unbound session is the most common
cause and the deny-default explains the silent empty result.

## See also

- [Policies](policies.md) — IAM-style authorization, `tenant_match` condition
- [Row Level Security](rls.md)
- [Transactions & MVCC](../query/transactions.md)
- [CREATE TABLE](../query/create-table.md)
