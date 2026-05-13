# `bulk_insert` packs payloads into batched row writes [DONE]

Local issue number: #459

GitHub issue: not found in `reddb-io/reddb`.

## Result

Implemented the #459 performance slice for local stdio `bulk_insert`.

After #458, stdio no longer used per-row `execute_query` for row inserts
because the response must include inserted ids. This slice preserves that
id contract and batches non-transactional row payloads through
`RuntimeEntityPort::create_rows_batch` in chunks of 500 rows instead of
calling the single-row path once per payload.

The transactional path is unchanged: each payload still becomes a pending
`INSERT` in the open stdio transaction's write set.

## Verification

- `cargo test -q -p reddb-io-server bulk_insert --lib -- --test-threads=1`
- `cargo test -q -p reddb-io --test integration_rpc_stdio insert -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
- `git diff --check`
