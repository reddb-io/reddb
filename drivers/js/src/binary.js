/**
 * Locate the `red` binary for SDK / CLI use.
 *
 * SDK lookup (`resolveSdkBinary`):
 *   1. `REDDB_BIN` env var (the canonical override per ADR 0006).
 *   2. `REDDB_BINARY_PATH` env var (legacy alias, deprecation window).
 *   3. `<package>/bin/red[.exe]` — where postinstall.js dropped it.
 *   4. Otherwise throw an actionable error.
 *
 *   PATH is **never** consulted. The wire-format coupling between the
 *   SDK and the embedded engine is too tight to silently bind to
 *   whatever `red` happens to be on PATH (see ADR 0006).
 *
 * CLI lookup (`resolveCliBinary`):
 *   1. `REDDB_BIN` env var.
 *   2. `<package>/bin/red[.exe]`.
 *   3. PATH-resolved bare `red[.exe]` — appropriate for the CLI which
 *      *targets* PATH.
 */

import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve, join } from 'node:path'

import { resolveBin } from './internal/bin-resolver/index.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const PACKAGE_ROOT = resolve(HERE, '..')

function defaultBinaryName() {
  if (typeof process !== 'undefined' && process.platform === 'win32') {
    return 'red.exe'
  }
  return 'red'
}

/** SDK runtime lookup. Throws actionable error when binary cannot be located. */
export function resolveSdkBinary() {
  const legacy = process.env?.REDDB_BINARY_PATH
  if (typeof legacy === 'string' && legacy !== '' && !process.env?.REDDB_BIN) {
    return legacy
  }
  return resolveBin({
    name: defaultBinaryName(),
    packageRoot: PACKAGE_ROOT,
    envVar: 'REDDB_BIN',
  })
}

/** CLI runtime lookup. Allowed to fall back to PATH per ADR 0006. */
export function resolveCliBinary() {
  const override = process.env?.REDDB_BIN
  if (typeof override === 'string' && override !== '') {
    return override
  }
  const local = join(PACKAGE_ROOT, 'bin', defaultBinaryName())
  if (existsSync(local)) {
    return local
  }
  return defaultBinaryName()
}

/** Used by postinstall.js to know where to drop the downloaded binary. */
export function packageBinaryDir() {
  return join(PACKAGE_ROOT, 'bin')
}
