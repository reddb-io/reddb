---
"@reddb-io/cli": patch
---

**UPDATE-heavy tables reclaim superseded versions again after a migration or VCS commit** (#2895): every VCS commit pins its snapshot horizon for the store's lifetime, and those pins are restored on every open. Every `APPLY MIGRATION` records such a commit. The paced MVCC collector and `VACUUM` used that global horizon for every collection, so after the first commit no superseded row version created afterwards could ever be reclaimed, in any collection. On a staging control store, heartbeat tables updated every 15–60 s kept 12k–24k physical versions for 1–6 live rows, and the file reached 4.2 GiB in 3.5 days.

Commit pins now protect only the history that `AS OF` can address: versioned collections and internal `red.*` / `red_*` metadata. Other collections reclaim against open transactions and captured statement snapshots only, so open snapshots keep their MVCC guarantees. The decision is made inside the publication fence that also serializes `vcs.set_versioned`. Existing files need no migration: their retained history becomes reclaimable once the new binary runs.

Indexed point lookups on those tables were slowed by the same history: the paged HASH lookup also visits every resident version. With reclamation working again, they read O(1) stored records. `EXPLAIN` also stops reporting the whole collection as the `index_seek` estimate: the seek now carries its predicate's estimate.
