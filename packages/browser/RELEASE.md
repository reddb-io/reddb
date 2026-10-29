RedDB Browser 0.7.0 adds persisted catalog inspection and host-scoped, read-only SQL for embedded database explorers such as RedUI.

`database.catalog()` reads actual table/column contracts. `database.readQuery(sql, params, scopes)` admits only canonical table SELECT and filters source rows by a host-owned table/field/value scope before the caller's WHERE, ORDER BY and LIMIT. Missing or duplicate table scopes and mutations are rejected. Normal query/transaction semantics and native file formats are unchanged.

This is a local file-owner inspection boundary, not server authentication or RLS. Applications retain their ordinary file/operation locks. Catalog inspection and scoped reads obey publication-health and aborted-transaction fences.

The JS/Worker/WASM package is self-contained. Applications bundle and cache its assets locally; no RedDB server, account, API key or CDN is required.
