# Documents Quickstart

Use this when each entity is a JSON document and the shape can evolve. The
Collection is the universal container; the document model is the semantic layer.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
CREATE DOCUMENT support_docs;
INSERT INTO support_docs DOCUMENT (body) VALUES ('{"ticket":"INC-001","status":"open","service":"checkout"}');
SELECT ticket, status, service FROM support_docs WHERE ticket = 'INC-001';
```

First meaningful result: the final query returns the incident document fields
without requiring a table migration.

Where to go next: [Documents](/data-models/documents.md) and
[INSERT](/query/insert.md).
