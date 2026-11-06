---
"@reddb-io/cli": patch
---

Reconcile duplicate runtime metadata during explicit canonical-config WAL recovery. Preserve missing index and view definitions, verify equivalent queue-group identities, keep canonical planner observations, and reject conflicting definitions, delivery state, references and overlapping metadata WAL.
