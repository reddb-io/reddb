---
"@reddb-io/client": minor
---

Drop the `postinstall` script from `@reddb-io/client`. The package is pure JavaScript and never needed the `red_client` binary it downloaded, so installs no longer touch the network, no longer depend on the platform/architecture, and work with `--ignore-scripts`. The `REDDB_CLIENT_BIN`, `REDDB_SKIP_POSTINSTALL`, `REDDB_POSTINSTALL_VERSION` and `REDDB_POSTINSTALL_REPO` overrides are gone; install `@reddb-io/cli` if you want the `red_client` CLI.
