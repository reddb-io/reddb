# VCS Quickstart

Use this when a Collection needs a named checkpoint and a way to inspect data
after a change. The Collection is the universal container; Git-for-data VCS is
the semantic layer for time travel and checkpoints.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
CREATE TABLE releases (id INT, version TEXT);
INSERT INTO releases (id, version) VALUES (1, 'v1');
CHECKPOINT 'initial release' AUTHOR 'Ada <ada@reddb.io>';
INSERT INTO releases (id, version) VALUES (2, 'v2');
SELECT version FROM releases ORDER BY id;
```

First meaningful result: the final query shows the working set after a named
checkpoint and a later write.

Where to go next: [Git for Data Overview](/vcs/overview.md),
[Command Reference](/vcs/commands.md), and [Walkthrough](/vcs/walkthrough.md).
