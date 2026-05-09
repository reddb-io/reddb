# KV foundation — PUT / GET / DELETE + default kv_default + all transports + drivers [AFK]

## Parent

#238

## What to build

Foundational tracer that proves the Redis-flavor KV verb DSL end-to-end. Ship `PUT`, `GET`, and `DELETE` as engine-native SQL statements, auto-create a default `kv_default` collection on first use without an explicit `CREATE TABLE … KV`, and surface the verbs across every transport (gRPC, HTTP, pgwire, MCP) and every official driver (Node, Python, Rust). The atomic-ops surface (INCR / CAS) lands in follow-ups; this slice establishes the parser → runtime → transports → drivers backbone they will all extend.

Scope guard: this issue is **normal KV only**. It must not implement Config or Vault semantics. Config and Vault are separate keyed Collection models under #314.

`PUT` accepts `EXPIRE <duration>` and `IF NOT EXISTS` clauses on this slice — both translate to existing engine semantics (`WITH TTL <ms>` and `ON CONFLICT DO NOTHING` respectively).

## Acceptance criteria

- [ ] Parser accepts `PUT <key> = <value> [EXPIRE <duration>] [IF NOT EXISTS]`, `GET <key>`, `DELETE <key>`. Snapshot tests pinned in the parser-hardening style used for queue / time-series / graph DSLs.
- [ ] Bare-key form (`PUT name = …`) routes to a default `kv_default` collection auto-created on first PUT. `red.config.kv.default_collection = false` disables the auto-create for production deploys that prefer explicit collections.
- [ ] Dotted-key form (`PUT sessions.<id> = …`) routes to a named `sessions` KV collection when one exists. Errors loudly when the collection does not exist and auto-create is disabled.
- [ ] Existing `INSERT INTO <kv-collection> (key, value) VALUES (…)` and `SELECT value FROM <kv-collection> WHERE key = ?` paths continue to work untouched.
- [ ] `KvAtomicOps` runtime module exists with `set / get / delete` methods at minimum (the INCR / CAS methods land in #2 / #3). The interface is the only seam new transports / drivers depend on.
- [ ] gRPC, HTTP (`PUT /collections/<coll>/kv/<key>`, `GET …`, `DELETE …`), pgwire (simple-query), and MCP (existing `reddb_kv_set` / `reddb_kv_get` extended; new `reddb_kv_delete`) all round-trip the same envelope shape for these three verbs.
- [ ] Node, Python, and Rust drivers expose `db.kv.put / get / delete` methods that match the wire shape.
- [ ] Integration test: each transport executes the same `PUT → GET → DELETE` scenario; output envelopes are equivalent.
- [ ] TTL eviction: `PUT key = value EXPIRE 1s`, sleep > 1s, `GET key` returns null. Verifies the engine's existing TTL sweep catches the new shape.
- [ ] No regression in existing KV collection / `INSERT … WITH TTL` / `SELECT … WHERE key = ?` paths.

## Blocked by

None - can start immediately
