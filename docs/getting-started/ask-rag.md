# ASK and RAG Quickstart

Use this when a natural-language answer should be grounded in database context.
The Collection is the universal container; ASK/RAG is the semantic layer over
retrieved rows, documents, graph paths, and vector hits.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
CREATE TABLE incidents (id TEXT, status TEXT, summary TEXT);
INSERT INTO incidents (id, status, summary) VALUES ('INC-001', 'open', 'checkout latency increased after deploy');
EXPLAIN ASK 'show incidents matching INC-001 with status' STRICT OFF LIMIT 5;
```

First meaningful result: the explain plan verifies the ASK/RAG request shape
without requiring provider credentials. Configure a provider before running the
same `ASK` without `EXPLAIN`.

Where to go next: [ASK Command](/query/search-commands.md#ask),
[Ask Your Database](/guides/ask-your-database.md), and
[RAG in 20 lines](/guides/rag-in-20-lines.md).
