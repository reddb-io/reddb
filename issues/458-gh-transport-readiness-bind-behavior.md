# Make transport readiness and bind behavior explicit [AFK]

GitHub issue: https://github.com/reddb-io/reddb/issues/458

Labels: enhancement, ready-for-agent

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#449

## What to build

Make server startup behavior predictable when HTTP, gRPC, RedWire, or other listeners fail to bind. Explicitly requested transports should fail fast. Implicit/default listeners should degrade without killing requested transports, while health/readiness reports active and failed listeners.

## Acceptance criteria

- [ ] Explicit `--http-bind`, `--grpc-bind`, or `--wire-bind` failure exits/fails startup with a clear error.
- [ ] Implicit/default listener bind failure does not kill a successfully requested transport.
- [ ] Health/readiness exposes active listeners and failed listener reasons.
- [ ] Logs clearly distinguish fatal explicit bind failure from non-fatal implicit bind degradation.
- [ ] Tests simulate port collision and verify both explicit-fatal and implicit-degrade behavior.
- [ ] Docs describe the startup contract.

## Blocked by

None - can start immediately

