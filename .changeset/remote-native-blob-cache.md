---
"@reddb-io/cli": minor
"@reddb-io/client": minor
"@reddb-io/sdk": minor
---

Expose the native Blob Cache over authenticated HTTP with tenant-scoped namespaces, hard TTL, binary values, existence probes and key/prefix/tag/namespace invalidation. The remote JavaScript client now uses application-authorized endpoints for tag invalidation and namespace flush, without requiring an operator credential. Align cache policy types with the implemented HTTP contract.
