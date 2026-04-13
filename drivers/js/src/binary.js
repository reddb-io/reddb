/**
 * Locate the `red` binary on disk.
 *
 * Resolution order:
 *   1. REDDB_BINARY_PATH env var (escape hatch).
 *   2. node_modules/reddb/bin/red (where postinstall.js downloads it).
 *   3. fallback: just `red` and let the OS resolve via PATH.
 */

import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve, join } from 'node:path'

const HERE = dirname(fileURLToPath(import.meta.url))
const PACKAGE_ROOT = resolve(HERE, '..')

function defaultBinaryName() {
  if (typeof process !== 'undefined' && process.platform === 'win32') {
    return 'red.exe'
  }
  // Bun/Deno also expose process.platform; this branch covers all three.
  return 'red'
}

/** Returns the absolute path to the `red` binary, or `'red'` as a fallback. */
export function resolveBinaryPath() {
  if (typeof process !== 'undefined' && process.env && process.env.REDDB_BINARY_PATH) {
    return process.env.REDDB_BINARY_PATH
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
