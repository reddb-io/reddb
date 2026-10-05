/**
 * Locate the `red` binary for SDK / CLI use.
 *
 * SDK lookup (`resolveSdkBinary`):
 *   1. `REDDB_BIN` env var (the canonical override per ADR 0006).
 *   2. `REDDB_BINARY_PATH` env var (legacy alias, deprecation window).
 *   3. The `@reddb-io/red-<platform>` package npm installed for this host
 *      (see `platform-binary.js`).
 *   4. Otherwise throw an actionable error.
 *
 *   PATH is **never** consulted. The wire-format coupling between the
 *   SDK and the embedded engine is too tight to silently bind to
 *   whatever `red` happens to be on PATH (see ADR 0006).
 *
 * CLI lookup (`resolveCliBinary`):
 *   1. `REDDB_BIN` env var.
 *   2. The `@reddb-io/red-<platform>` package.
 *   3. PATH-resolved bare `red[.exe]` — appropriate for the CLI which
 *      *targets* PATH.
 */

import { platformPackageNames, resolvePlatformBinary } from './platform-binary.js'

function defaultBinaryName() {
  if (typeof process !== 'undefined' && process.platform === 'win32') {
    return 'red.exe'
  }
  return 'red'
}

/** SDK runtime lookup. Throws actionable error when binary cannot be located. */
export function resolveSdkBinary() {
  const override = process.env?.REDDB_BIN
  if (typeof override === 'string' && override !== '') {
    return override
  }
  const legacy = process.env?.REDDB_BINARY_PATH
  if (typeof legacy === 'string' && legacy !== '') {
    return legacy
  }
  const packaged = resolvePlatformBinary()
  if (packaged) {
    return packaged
  }
  const wanted = platformPackageNames()[0]
  throw new Error(
    `reddb: binary "${defaultBinaryName()}" not found.\n` +
      `  expected:    the ${wanted} package (an optionalDependency of @reddb-io/sdk)\n` +
      `  override:    set REDDB_BIN=/path/to/${defaultBinaryName()}\n` +
      `  fix:         reinstall without --no-optional / --omit=optional, and make sure\n` +
      `               ${process.platform}/${process.arch} is a supported platform;\n` +
      `               or build it: cargo build --release --bin red`,
  )
}

/** CLI runtime lookup. Allowed to fall back to PATH per ADR 0006. */
export function resolveCliBinary() {
  const override = process.env?.REDDB_BIN
  if (typeof override === 'string' && override !== '') {
    return override
  }
  return resolvePlatformBinary() ?? defaultBinaryName()
}
