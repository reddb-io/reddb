# Graph Quickstart

Use this when the meaningful result is a relationship path. The Collection is
the universal container; the graph model is the semantic layer.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
INSERT INTO social NODE (label, name) VALUES ('User', 'Ada');
INSERT INTO social NODE (label, name) VALUES ('User', 'Grace');
MATCH (n:User) RETURN n.name LIMIT 2;
```

First meaningful result: the final query returns graph node properties through
`MATCH`.

Where to go next: [Graphs](/data-models/graphs.md) and
[Graph Commands](/query/graph-commands.md).
