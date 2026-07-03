# PG-Wire real-client conformance harness

`run.sh` starts a local RedDB server with the PG listener enabled, proxies the
PG-Wire traffic for frame auditing, and runs three real PostgreSQL clients:

- `psycopg_client.py` in the pinned `PSYCOPG_IMAGE`
- `pgx_client.go` in the pinned `PGX_IMAGE`
- `JdbcClient.java` in the pinned `PGJDBC_IMAGE`

The script ends by running `assert_extended.py` against the proxy log, so a
green run proves that each client completed successfully and that the extended
protocol frames were observed.

Run it locally after building `red`:

```sh
cargo build --locked --bin red
RED_BIN=target/debug/red bash tests/pgwire_clients/run.sh
```

## Adding a client

1. Add the client source file under `tests/pgwire_clients/`.
2. Add a digest-pinned image variable near the top of `run.sh`, following the
   existing `*_IMAGE` defaults. Use a multi-arch manifest digest when possible.
3. Add one `run_client "<name>" "$<IMAGE_VAR>" '<command>'` line. Keep the name
   stable because CI failure output uses it to identify the failing client.
4. Set a unique `application_name` in the client connection string.
5. Add the expected statements for that application name to `assert_extended.py`.
6. Run `bash tests/pgwire_clients/run.sh` and confirm the final frame audit passes.
