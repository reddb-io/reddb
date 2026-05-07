# null: CLI: red migrate-from-redis dual-write tool (executes #185 playbook)

## Parent

#188

## What to build

`red migrate-from-redis` CLI subcommand that automates the three-phase migration playbook documented in #185:
1. **Phase dual-write**: every Redis write also writes to RedDB cache. Reads from Redis. Asserts cache consistency.
2. **Phase cutover**: reads flip to RedDB. Redis becomes write-only fallback for emergencies.
3. **Phase decommission**: stops writes to Redis. Tears down Redis instance (operator confirms).

Invocation:
```
red migrate-from-redis --source redis://primary:6379 --target grpc://reddb:5050 --phase dual-write --namespace my-app
red migrate-from-redis --source ... --target ... --phase cutover --namespace my-app --validate-shadow
red migrate-from-redis --source ... --target ... --phase decommission --namespace my-app --i-am-sure
```

Uses `redis-rs` for source. Uses RedDB gRPC protocol (or @reddb-io/client patterns) for target. Implements consistency validation in shadow mode.

## Acceptance criteria

- [ ] Three phases each invokable, each prints clear status + next-phase hint.
- [ ] End-to-end integration test with redis docker + reddb instance + verifying:
  - dual-write: Redis stays the read source-of-truth, RedDB cache grows
  - cutover: reads flip, Redis still writable
  - decommission: stops dual-writes, prints summary
- [ ] Validation-shadow option counts divergence + dumps diff to a report file.
- [ ] Idempotent: re-running a phase doesn't break state.
- [ ] Confirmation prompt for decommission (or --i-am-sure flag).
- [ ] Documentation example in #185 migration guide.

## Blocked by

- https://github.com/reddb-io/reddb/issues/196

