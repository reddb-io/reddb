# Relational SQL Quickstart

Use this when your Collection has a fixed row shape and you want SQL filters,
ordering, and updates. The Collection is the universal container; the table
model is the semantic layer.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
CREATE TABLE app_users (id INT, name TEXT, plan TEXT, active BOOLEAN);
INSERT INTO app_users (id, name, plan, active) VALUES (1, 'Ada', 'pro', true), (2, 'Grace', 'free', true), (3, 'Linus', 'free', false);
SELECT name, plan FROM app_users WHERE active = true ORDER BY name;
```

First meaningful result: the final query returns the active users and their
plans.

Where to go next: [Tables & Rows](/data-models/tables.md), [SELECT](/query/select.md),
and [CREATE TABLE](/query/create-table.md).
