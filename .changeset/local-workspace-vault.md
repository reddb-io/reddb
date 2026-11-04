---
"@reddb-io/cli": minor
---

Add a private local workspace service to the RedDB binary, backed by the shared native/WASM Vault and transaction engine. Local hosts can atomically save connection metadata and encrypted credentials, retain metadata access while locked, and use OS-keychain opening credentials without exposing the private database as a user query target.
