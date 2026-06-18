# Test Compose Topologies

This directory is the test-owned Docker Compose layer for external environment validation.

Rules:

- `tests/` and test harness scripts may depend on files here
- `examples/` must stay independent and manual/documentation-focused
- changes here should optimize for deterministic automation, not for tutorial readability
- every RedDB service must set `REDDB_STORAGE_PRESET`, `REDDB_STORAGE_PROFILE`,
  `REDDB_STORAGE_PACKAGING`, and `REDDB_REPLICA_COUNT` explicitly

Current profiles:

- `min.yml`
- `replica.yml`
- `full.yml`
- `remote.yml`
- `backup.yml`
- `pitr.yml`
- `serverless.yml`

There is intentionally no `cluster.yml` fixture yet. Add one only when the
cluster runtime has a concrete symmetric node/discovery topology to validate.

Typical usage:

```bash
make test-env PROFILE=replica
make test-env PROFILE=remote
make test-env-rust PROFILE=replica
```
