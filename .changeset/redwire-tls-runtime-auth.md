---
"@reddb-io/cli": patch
---

Standalone RedWire TLS now uses the runtime authentication store. Valid session tokens work over `reds://`, and authenticated servers reject anonymous connections instead of treating the TLS listener as an unauthenticated server.
