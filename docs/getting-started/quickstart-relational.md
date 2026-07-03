# Quickstart: Relational SQL

Go from an empty database to a grouped SQL report in under five minutes. The
**relational** model gives a `collection` typed columns, constraints, and
familiar `SELECT ... GROUP BY` — the model is the semantic layer; the
`accounts` collection below is the universal container underneath it.

## 1. Start RedDB

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

Then open a session with `red connect 127.0.0.1:55055` (or POST each statement
to `http://127.0.0.1:5000/query`). Every statement below is plain RQL.

## 2. Create a typed collection

```sql
CREATE TABLE accounts (id TEXT PRIMARY KEY, username TEXT UNIQUE, status TEXT NOT NULL DEFAULT = 'active', tier TEXT DEFAULT = 'basic', score FLOAT) WITH timestamps = true;
```

## 3. Insert rows

```sql
INSERT INTO accounts (id, username, status, score) VALUES ('u1', 'alice', 'active', 91.5);
INSERT INTO accounts (id, username, status, tier, score) VALUES ('u2', 'bob', 'suspended', 'enterprise', 72.0);
INSERT INTO accounts (id, username, status, tier, score) VALUES ('u3', 'carol', 'active', 'pro', 88.25);
```

## 4. Your first meaningful result

Filter, project, and order like any SQL database:

```sql
SELECT username, tier FROM accounts WHERE status = 'active' ORDER BY username ASC;
```

```text
 username | tier
----------+------
 alice    | basic
 carol    | pro
```

Then aggregate:

```sql
SELECT status, count(*) AS total FROM accounts GROUP BY status ORDER BY status ASC;
```

```text
 status    | total
-----------+------
 active    | 2
 suspended | 1
```

## Where to go next

- [Tables & Rows](/data-models/tables.md) — the full relational model
- [CREATE TABLE reference](/query/create-table.md) — constraints, TTL, indexes
- [INSERT reference](/query/insert.md) — every write form
