---
"@reddb-io/cli": patch
---

Restore durable database forks with each fork's data and lifecycle metadata stored in its own `.rdb` and WAL. Recover create, drop, detach, and promotion operations across crashes and cold boots, validate file ownership before deletion, and reject busy fork files without blocking. Audit logs remain separate.

Fork creation copies the current committed checkpoint; historical `AT LSN` branching is not supported. Promotion returns the replacement database path and requires reopening the server on that path.
