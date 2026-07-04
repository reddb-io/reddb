# Quickstart: Documents

Store schema-free JSON and query its fields directly. The **document** model
is a semantic layer over a `collection` (the universal container): you write
whole JSON bodies, and RedDB lets you filter and project their fields as if
they were columns.

## 1. Start RedDB

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

Connect with `red connect 127.0.0.1:55055` (or POST to
`http://127.0.0.1:5000/query`).

## 2. Insert JSON documents

Each document carries an arbitrary JSON `body`:

```sql
INSERT INTO docs DOCUMENT VALUES ({"category":"ops","slug":"guide","title":"Runbook Guide"});
INSERT INTO docs DOCUMENT VALUES ({"category":"db","slug":"runbook","title":"DB Runbook"});
```

The `docs` collection is created on first write — no schema declaration needed.

## 3. Your first meaningful result

Query document fields by name, just like columns:

```sql
SELECT slug, title FROM docs WHERE category = 'ops' ORDER BY slug ASC;
```

```text
 slug  | title
-------+--------------
 guide | Runbook Guide
```

## Where to go next

- [Documents](/data-models/documents.md) — the full document model
- [Querying nested fields](/query/select.md) — dotted-path field access
- [INSERT reference](/query/insert.md) — `DOCUMENT` and other write forms
