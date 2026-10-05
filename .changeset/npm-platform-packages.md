---
"@reddb-io/cli": minor
"@reddb-io/sdk": minor
---

Drop the `postinstall` download from `@reddb-io/cli` and `@reddb-io/sdk`. The `red` binary now ships as per-platform npm packages (`@reddb-io/red-linux-x64`, `-linux-x64-musl`, `-linux-arm64`, `-linux-arm64-musl`, `-linux-arm`, `-darwin-x64`, `-darwin-arm64`, `-win32-x64`) declared as `optionalDependencies`, so the package manager installs only the one for your OS/CPU/libc. Installs run no script and need no network beyond the registry, and work with `--ignore-scripts`. Don't install with `--omit=optional`. `REDDB_BIN` still overrides; `REDDB_SKIP_POSTINSTALL`, `REDDB_POSTINSTALL_VERSION` and `REDDB_POSTINSTALL_REPO` are gone.
