# Test Compose Topologies

This directory is the test-owned Docker Compose layer for external environment validation.

Rules:

- `tests/` and test harness scripts may depend on files here
- `examples/` must stay independent and manual/documentation-focused
- changes here should optimize for deterministic automation, not for tutorial readability

Current profiles:

- `min.yml`
- `replica.yml`
- `full.yml`
- `remote.yml`
- `backup.yml`
- `pitr.yml`
- `serverless.yml`

Typical usage:

```bash
make test-env PROFILE=replica
make test-env PROFILE=remote
make test-env-rust PROFILE=replica
```
