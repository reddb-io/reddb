# Quickstart: Version Control (Time-Travel)

Commit a snapshot of your data, keep writing, then read the past. RedDB's
**VCS** layer opts a `collection` (the universal container) into versioning so
you can `CHECKPOINT` a commit and query `AS OF` any branch.

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

## 2. Create a versioned collection

```sql
CREATE TABLE releases (id INT, name TEXT, status TEXT);
ALTER TABLE releases SET VERSIONED = true;
INSERT INTO releases (id, name, status) VALUES (1, 'reddb', 'draft');
```

## 3. Commit a checkpoint

`CHECKPOINT` records the current state as a commit on the `main` branch:

```sql
CHECKPOINT 'cut draft release' AUTHOR 'Release Bot <rel@reddb.io>';
```

## 4. Change the data, then time-travel

Update the live row, then compare "now" against the committed snapshot:

```sql
UPDATE releases SET status = 'published' WHERE id = 1;
SELECT name, status FROM releases;
SELECT name, status FROM releases AS OF BRANCH 'main';
```

```text
-- live now:
 name  | status
-------+----------
 reddb | published

-- AS OF BRANCH 'main' (the checkpoint):
 name  | status
-------+-------
 reddb | draft
```

## 5. Your first meaningful result

The commit log shows the checkpoint you just made:

```sql
SELECT message, height FROM red.commits ORDER BY height DESC LIMIT 3;
```

## Where to go next

- [VCS overview](/vcs/overview.md) — branches, commits, and merges
- [VCS commands](/vcs/commands.md) — `CHECKPOINT`, `CHECKOUT`, `MERGE`, and more
- [VCS walkthrough](/vcs/walkthrough.md) — a longer end-to-end tour
