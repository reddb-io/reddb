---
"@reddb-io/cli": patch
---

Opening a store no longer takes a full checkpoint to build an index a user declared. `CREATE INDEX` over rows already on disk leaves the tree to the next checkpoint; if the server stopped before one ran, the next open took that checkpoint itself, covering everything the WAL replayed, before answering anything, health probes included. On a 4.8 GB store that was 26 minutes. Statements already read the collection while such an index is unbuilt, so the open now leaves it to the next checkpoint. Projections the runtime cannot read without are still built on open.
