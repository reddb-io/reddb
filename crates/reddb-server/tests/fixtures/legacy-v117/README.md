# Synthetic RedDB 1.17.0 paged-store fixture

This fixture contains only synthetic data. It was created using the Linux
amd64 image `ghcr.io/reddb-io/reddb@sha256:edb785a532abe9b527755d4087b0f9bea7df46bb248a3b617981c8a89d26580d`
and a temporary test-only administrator. The server created the table
`compatibility_fixture (id INT, name TEXT)` and inserted `(1, 'alpha')`,
`(2, 'beta')`, and `(3, 'gamma')`. The same image reopened the store and
returned those three values before a clean shutdown. The retained WAL contains
only its empty header and the legacy DWB sidecar is empty.

`data.rdb` SHA-256:
`b34695b9cfe986d5a8f04d7376c54e0958eba7952fd9e23737b762c7e1ec5263`.
The files contain no customer data or production credentials. Tests copy this
directory to a temporary location before invoking any migration or opening it.
