---
"@reddb-io/cli": minor
---

**Feature: per-node community assignment from `GRAPH COMMUNITY` (#660).**
`GRAPH COMMUNITY ALGORITHM louvain RETURN ASSIGNMENTS` now emits one row per
node `{node_id, community_id}` — the node→community map needed to colour or
visualise nodes by community. Without the `RETURN ASSIGNMENTS` clause the
historical per-community aggregate shape (`community_id`, `size`) is unchanged
(backward compatible). `LIMIT` caps the per-node rows.
