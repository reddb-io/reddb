# Example Environments

These Docker Compose files are the quickest way to exercise RedDB deployment modes locally.
They build the RedDB image from your checkout with Compose `build:` entries,
so they do not require access to `ghcr.io/reddb-io/reddb`.

Available profiles:

- `min`: one local server only
- `replica`: primary + one replica
- `full`: primary + two replicas
- `remote`: primary + replica + MinIO for remote snapshot/WAL tests
- `backup`: single remote-backed server + MinIO for backup flows
- `pitr`: single remote-backed primary + MinIO for restore-point flows
- `serverless`: single remote-backed node + MinIO for serverless-style readiness/warmup flows
- `cluster`: three symmetric cluster-shape members with stable identity/discovery env

Quick commands:

```bash
make env-up PROFILE=replica
make env-logs PROFILE=replica
make env-down PROFILE=replica
```

For automated validation, use the test-owned compose files under `testdata/compose/`.

Run validations:

```bash
make test-env PROFILE=replica
make test-env PROFILE=remote
make test-env PROFILE=serverless
```

The human-facing examples mount `examples/container-config.json` at
`/etc/reddb/config.json`. On first boot RedDB seeds missing keys into
`red.config`; later `SET CONFIG` values in the database win over that file.

The environment test harness does two things:

- shell/control-plane checks against the HTTP endpoints
- ignored Rust integration tests in `tests/integration_external_env.rs`

If you want to keep a stack running and only re-run the Rust suite:

```bash
KEEP_UP=1 make test-env-shell PROFILE=replica
make test-env-rust PROFILE=replica
```
