# Multi-Tenancy

RedDB ships first-class multi-tenancy: a session-scoped tenant handle,
a declarative `TENANT BY (col)` clause on `CREATE TABLE`, and
auto-wired row-level security that keeps every read and write scoped
to the current tenant.

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

## See also

- [Row Level Security](rls.md)
- [Transactions & MVCC](../query/transactions.md)
- [CREATE TABLE](../query/create-table.md)
